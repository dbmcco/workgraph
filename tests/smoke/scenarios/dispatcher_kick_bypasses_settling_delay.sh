#!/usr/bin/env bash
# Scenario: dispatcher_kick_bypasses_settling_delay
#
# Regression check for dispatcher-poll-lower (2026-04-27): user-initiated
# state mutations (`wg add --immediate`, `wg publish`, `wg unclaim`,
# `wg service resume`) MUST send a `KickDispatcher` IPC that bypasses the
# settling delay so the dispatcher ticks within sub-second.
#
# Before this fix, the dispatcher waited the full settling_delay (2s) after
# every GraphChanged IPC, on top of the safety-timer poll (default 30s, was
# 60s in some configs). The user perceived 30-60s of dead air after every
# state-changing CLI command.
#
# This scenario boots a fresh daemon, runs `wg add --immediate`, and asserts
# that the daemon log shows "KickDispatcher received, ticking immediately"
# AND a coordinator tick fires within 1 second of the IPC arriving. If the
# kick path regresses (e.g. callers revert to notify_graph_changed), this
# scenario fails because the log line is gone or the tick is delayed.

set -u

HERE="$(cd "$(dirname "$0")" && pwd)"
. "$HERE/_helpers.sh"

require_wg

scratch=$(make_scratch)
cd "$scratch"

# Initialize with claude scaffold (no LLM calls happen in this scenario;
# the daemon just needs to boot and accept IPC).
if ! wg init -m claude:opus >init.log 2>&1; then
    loud_fail "wg init failed: $(tail -5 init.log)"
fi

# --no-chat-agent: skip spawning the chat coordinator so we test pure
# dispatcher behavior. --max-agents 0: no real worker spawns either.
start_wg_daemon "$scratch" --max-agents 0 --no-chat-agent
graph_dir="$WG_SMOKE_DAEMON_DIR"
daemon_log="$graph_dir/service/daemon.log"

# Wait for the daemon's first tick to land in the log so we have a baseline.
for _ in $(seq 1 30); do
    if grep -q "Coordinator tick #1" "$daemon_log" 2>/dev/null; then
        break
    fi
    sleep 0.2
done
if ! grep -q "Coordinator tick #1" "$daemon_log" 2>/dev/null; then
    loud_fail "daemon never logged its first tick. log tail:\n$(tail -30 "$daemon_log" 2>/dev/null || echo '<no log>')"
fi

# Snapshot the pre-add tick count so we can detect the post-kick tick.
pre_kick_ticks=$(grep -c "Coordinator tick #" "$daemon_log" 2>/dev/null || echo 0)

# Fire the kick by adding an immediately-published task. The wg add path
# must call notify_kick (not notify_graph_changed) when paused=false.
if ! wg add "smoke-kick-test" --immediate >add.log 2>&1; then
    loud_fail "wg add failed: $(tail -5 add.log)"
fi

# Allow up to 1 second for the IPC to be processed and the tick to fire.
# If the kick path is broken, the next tick won't come until either the
# settling delay (2s) or the safety-timer poll (5s default).
for _ in $(seq 1 10); do
    if grep -q "KickDispatcher received" "$daemon_log" 2>/dev/null; then
        break
    fi
    sleep 0.1
done

if ! grep -q "KickDispatcher received" "$daemon_log" 2>/dev/null; then
    loud_fail "no 'KickDispatcher received' log entry within 1s of wg add --immediate. log tail:\n$(tail -20 "$daemon_log" 2>/dev/null || echo '<no log>')"
fi

post_kick_ticks=$(grep -c "Coordinator tick #" "$daemon_log" 2>/dev/null || echo 0)
if (( post_kick_ticks <= pre_kick_ticks )); then
    loud_fail "expected coordinator tick after KickDispatcher; pre=$pre_kick_ticks, post=$post_kick_ticks. log tail:\n$(tail -20 "$daemon_log" 2>/dev/null || echo '<no log>')"
fi

echo "PASS: kick path delivered immediate dispatcher tick (pre_ticks=$pre_kick_ticks, post_ticks=$post_kick_ticks)"
exit 0
