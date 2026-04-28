#!/usr/bin/env bash
# Phase 1 live smoke: session handler lock.
#
# Verifies:
#   1. wg nex --chat X creates .handler.pid
#   2. Clean exit (/quit) removes .handler.pid
#   3. Second concurrent wg nex --chat X is refused (while first live)
#   4. wg session status reports the holder correctly
#   5. wg session release asks cleanly; handler exits at next boundary
#   6. Stale lock (kill -9 of handler) is recovered on next acquire
#   7. Release marker isn't stuck (new handler doesn't immediately exit)
set -euo pipefail

tmp=$(mktemp -d)
trap "pkill -P $$ 2>/dev/null || true; rm -rf $tmp" EXIT
cd "$tmp"
export WG_DIR="$tmp/.workgraph"
wg init --no-agency >/dev/null 2>&1
chat_dir="$WG_DIR/chat/lock-smoke"
mkdir -p "$chat_dir"
lock_path="$chat_dir/.handler.pid"
release_marker="$chat_dir/.handler.release-requested"

fail() { echo "FAIL: $*"; exit 1; }
pass() { echo "PASS: $*"; }

echo "=== Test 1: lock file created on --chat startup ==="
# Prime the inbox so autonomous handler has something to respond to.
# Then the handler stays alive briefly processing; we check the lock
# file during that window.
cat > "$chat_dir/inbox.jsonl" <<EOF
{"id":1,"request_id":"req-1","role":"user","content":"Say hi in one word.","timestamp":"2026-04-19T00:00:00Z"}
EOF
# Spawn handler in background. Autonomous mode so it exits on EndTurn.
wg nex --chat lock-smoke --autonomous --no-mcp --max-turns 3 >/dev/null 2>/dev/null &
handler_pid=$!
# Poll for lock file (up to 10s).
for i in {1..50}; do
  if [ -f "$lock_path" ]; then break; fi
  sleep 0.2
done
[ -f "$lock_path" ] || fail "lock file was not created within 10s"
pass "lock file created at $lock_path"

echo
echo "=== Test 2: lock file contents report correct pid + kind ==="
lock_pid=$(sed -n 1p "$lock_path")
lock_kind=$(sed -n 3p "$lock_path")
[ "$lock_pid" = "$handler_pid" ] || fail "lock pid ($lock_pid) != handler pid ($handler_pid)"
[ "$lock_kind" = "chat-nex" ] || fail "lock kind is '$lock_kind', expected 'chat-nex'"
pass "lock contents pid=$lock_pid kind=$lock_kind"

echo
echo "=== Test 3: wg session status reports holder ==="
status=$(wg session status lock-smoke 2>&1)
echo "  status output: $status"
echo "$status" | grep -q "pid=$handler_pid" || fail "status missing pid"
echo "$status" | grep -q "live" || fail "status missing 'live'"
pass "session status correctly reports live holder"

echo
echo "=== Test 4: concurrent --chat on same session is refused ==="
# Second handler must fail immediately. Capture stderr to verify message.
set +e
err=$(wg nex --chat lock-smoke --autonomous --no-mcp --max-turns 1 'ping' 2>&1 >/dev/null)
rc=$?
set -e
[ $rc -ne 0 ] || fail "second handler should have failed but exited 0"
echo "$err" | grep -q "already owned" || fail "error should mention 'already owned': $err"
pass "second handler correctly refused (rc=$rc)"

echo
echo "=== Test 5: wait for the first handler to finish, lock is removed ==="
# The first handler should exit after EndTurn (autonomous + max-turns=3).
wait $handler_pid 2>/dev/null || true
sleep 0.5
[ ! -f "$lock_path" ] || fail "lock file should be removed after clean exit"
pass "lock file removed on clean exit"

echo
echo "=== Test 6: stale lock (dead pid) is recovered on next acquire ==="
# Write a fake lock pointing at a dead pid.
echo "999999" > "$lock_path"
echo "2020-01-01T00:00:00Z" >> "$lock_path"
echo "chat-nex" >> "$lock_path"
# New handler should detect stale and recover.
cat > "$chat_dir/inbox.jsonl" <<EOF
{"id":2,"request_id":"req-2","role":"user","content":"one word.","timestamp":"2026-04-19T00:00:00Z"}
EOF
wg nex --chat lock-smoke --autonomous --no-mcp --max-turns 3 >/dev/null 2>"$tmp/recovery.err" &
h2_pid=$!
for i in {1..50}; do
  if [ -f "$lock_path" ]; then
    p=$(sed -n 1p "$lock_path")
    if [ "$p" = "$h2_pid" ]; then break; fi
  fi
  sleep 0.2
done
new_pid=$(sed -n 1p "$lock_path" 2>/dev/null || echo "")
[ "$new_pid" = "$h2_pid" ] || fail "expected new pid $h2_pid in lock, got '$new_pid'"
grep -q "recovering stale lock" "$tmp/recovery.err" || fail "stale-recovery log line missing"
pass "stale lock recovered (new pid=$h2_pid, old was 999999)"

# Clean up handler 2.
wait $h2_pid 2>/dev/null || true
sleep 0.3
[ ! -f "$lock_path" ] || fail "lock should be removed after h2 exit"
pass "clean exit again"

echo
echo "=== Test 7: wg session release triggers clean exit at turn boundary ==="
# Start an interactive non-autonomous handler — it'll wait on inbox forever.
# Release should make it exit at the next turn boundary.
cat > "$chat_dir/inbox.jsonl" <<EOF
{"id":3,"request_id":"req-3","role":"user","content":"one word.","timestamp":"2026-04-19T00:00:00Z"}
EOF
wg nex --chat lock-smoke --no-mcp --max-turns 20 >/dev/null 2>/dev/null &
h3_pid=$!
# Wait for lock.
for i in {1..50}; do
  [ -f "$lock_path" ] && break
  sleep 0.2
done
[ -f "$lock_path" ] || fail "h3 didn't start"
# Give it a moment to process the first message.
sleep 3
# Ask to release, wait up to 15s.
echo "  requesting release..."
release_out=$(wg session release lock-smoke --wait 15 2>&1)
echo "$release_out" | grep -q "released" || {
  # wait anyway in case it's slow
  wait $h3_pid 2>/dev/null || true
}
# Confirm handler actually exited.
if kill -0 $h3_pid 2>/dev/null; then
  sleep 2
  if kill -0 $h3_pid 2>/dev/null; then
    kill $h3_pid 2>/dev/null || true
    fail "handler did not exit after release"
  fi
fi
[ ! -f "$lock_path" ] || fail "lock file lingered after release"
pass "release command worked: handler exited cleanly, lock released"

echo
echo "=== Test 8: release marker is cleared — next handler doesn't immediately quit ==="
# After release, a new handler starting up should acquire fine AND not see
# a stale release marker (which would make it exit on first turn).
cat > "$chat_dir/inbox.jsonl" <<EOF
{"id":4,"request_id":"req-4","role":"user","content":"one word.","timestamp":"2026-04-19T00:00:00Z"}
EOF
wg nex --chat lock-smoke --autonomous --no-mcp --max-turns 3 >/dev/null 2>/dev/null &
h4_pid=$!
wait $h4_pid 2>/dev/null || true
# Confirm it actually ran (outbox should have a response).
if jq -e 'select(.request_id == "req-4")' "$chat_dir/outbox.jsonl" >/dev/null 2>&1; then
  pass "new handler ran to completion (no stale release-marker triggered early exit)"
else
  fail "new handler didn't produce a response; stale marker may have killed it early"
fi
[ ! -f "$release_marker" ] || fail "release marker should be gone"

echo
echo "=== ALL PHASE 1 CHECKS PASSED ==="
