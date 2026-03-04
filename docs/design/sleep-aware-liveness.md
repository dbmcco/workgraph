# Design: Sleep-Aware Agent Liveness Detection

## Problem

When the laptop hibernates/sleeps and resumes, agent connections may be lost, leaving processes that appear alive (PID exists) but are actually stuck. The current coordinator only detects dead agents by PID exit — it cannot detect an agent whose process is alive but is hung due to a broken connection post-sleep.

There is already a `STREAM_STALE_THRESHOLD_MS` (5 min) warning in `src/commands/service/triage.rs:29`, but it only logs a warning — it takes no action. More critically, it doesn't distinguish "system was asleep" from "agent is genuinely stuck while the system was awake."

## Current State

### Existing detection (`src/commands/service/triage.rs`)
- **PID-based**: `is_process_alive(pid)` via `kill(pid, 0)` — detects crashed/OOMed agents
- **Stream staleness**: `check_stream_liveness()` reads last event timestamp from `stream.jsonl`, warns if >5min stale while PID alive — but takes no action
- **Triage system**: When an agent is detected dead (process exited), an LLM (haiku) assesses output.log and issues `done`/`continue`/`restart` verdict
- **Heartbeat auto-bump**: Coordinator auto-bumps heartbeat for any agent whose PID is alive (`cleanup_dead_agents` line 92-94), making the heartbeat useless for detecting stuck-but-alive agents

### Existing config (`src/config.rs`)
- `agent.heartbeat_timeout`: 5 min default — used by `wg dead-agents` command, NOT by the coordinator's triage cleanup
- `agency.auto_triage`: enables LLM-based triage on dead agents
- `agency.triage_model`: model for triage (default: "haiku")
- `agency.triage_timeout`: timeout for triage calls (default: 30s)

### Coordinator poll loop (`src/commands/service/mod.rs`)
- Tick-based: runs on `poll_interval` (default 60s) or on `GraphChanged` IPC events
- Each tick calls `cleanup_and_count_alive()` → `triage::cleanup_dead_agents()`

## Design

### Core Insight: Monotonic Clock Gap Detection

The system clock jumps forward during sleep, but `Instant` (monotonic clock) pauses. By comparing wall-clock elapsed time against monotonic elapsed time, we can detect sleep gaps:

```
wall_elapsed  = Utc::now() - last_tick_wall
mono_elapsed  = Instant::now() - last_tick_mono
sleep_duration = wall_elapsed - mono_elapsed   // positive = system slept
```

If `sleep_duration > threshold` (e.g., 30s), the system was asleep. This is portable (works on Linux and macOS) and requires no platform-specific APIs.

### Liveness Algorithm

On each coordinator tick:

1. **Detect sleep gap**: Compare wall-clock vs monotonic elapsed since last tick
2. **If system just woke up** (sleep gap > 30s):
   - Log the detected sleep duration
   - Reset all stream-staleness timers (set "awake-since" to now)
   - Skip stuck-agent detection for this tick — agents may need a moment to reconnect
   - On the *next* tick after wake, start a grace period (configurable, default 2 min)
3. **If system has been awake** (no recent sleep, and past grace period):
   - For each alive agent, check stream staleness: `now_ms - last_event_ms > stale_threshold`
   - `stale_threshold` is configurable (default: 10 min), measured in *awake time* only
   - If stale: escalate to stuck-agent handler

### Stuck Agent Handler

When an agent is detected as stuck (PID alive, no stream activity for `stale_threshold` awake-time):

**Phase 1 — Check if the process is actually doing anything:**
```
/proc/<pid>/stat → check state (S=sleeping, R=running, D=disk sleep, Z=zombie, T=stopped)
/proc/<pid>/io  → check if read/write bytes have changed since last check
```
If the process is in `Z` (zombie) or `T` (stopped/traced), treat as dead immediately.
If I/O counters haven't changed between two consecutive ticks, the process is truly stuck.

**Phase 2 — Lightweight triage (reuse existing system):**

The existing `run_triage()` in `triage.rs` already does exactly what we need: reads the output log, calls haiku, gets a `done`/`continue`/`restart` verdict. The only difference is that the agent's process is still alive.

Action based on verdict:
- **"done"**: Kill the process, mark task done (agent finished work but hung on cleanup)
- **"continue"**: Kill the process, mark task open for reassignment with recovery context
- **"restart"**: Kill the process, mark task open for fresh start

The kill should be graceful: SIGTERM first, wait 5s, then SIGKILL (existing `kill_process_graceful()` does this).

### New Data Structures

```rust
/// Tracks awake-time for sleep-aware liveness detection.
/// Lives in the daemon's main loop (not persisted).
struct SleepTracker {
    /// Wall-clock time of last tick
    last_tick_wall: DateTime<Utc>,
    /// Monotonic time of last tick
    last_tick_mono: Instant,
    /// If Some, system recently woke up — grace period ends at this Instant
    wake_grace_until: Option<Instant>,
    /// Per-agent: last known I/O read_bytes from /proc/<pid>/io
    agent_io_bytes: HashMap<String, u64>,
}
```

### Configuration

New fields in `[agent]` section of `config.toml`:

```toml
[agent]
# Existing
heartbeat_timeout = 5  # minutes, used by `wg dead-agents` CLI

# New
stale_threshold = 10       # minutes of awake-time with no stream activity before intervention
wake_grace_period = 2      # minutes after wake before checking liveness
sleep_gap_threshold = 30   # seconds of wall-vs-mono divergence to detect sleep
```

These are added to the existing `AgentConfig` struct in `config.rs`.

### Integration Points

#### 1. Daemon main loop (`src/commands/service/mod.rs`, ~line 1220)

Add `SleepTracker` to the daemon state. Before each tick:
```rust
let sleep_tracker = &mut daemon_state.sleep_tracker;
let wall_elapsed = Utc::now() - sleep_tracker.last_tick_wall;
let mono_elapsed = sleep_tracker.last_tick_mono.elapsed();
let sleep_gap = wall_elapsed.num_seconds() - mono_elapsed.as_secs() as i64;

if sleep_gap > config.agent.sleep_gap_threshold as i64 {
    logger.info(&format!("System sleep detected: ~{}s gap", sleep_gap));
    sleep_tracker.wake_grace_until = Some(Instant::now() + Duration::from_secs(config.agent.wake_grace_period * 60));
    sleep_tracker.agent_io_bytes.clear();
}

sleep_tracker.last_tick_wall = Utc::now();
sleep_tracker.last_tick_mono = Instant::now();
```

#### 2. Triage cleanup (`src/commands/service/triage.rs`)

Add a new `StuckReason` variant and extend `detect_dead_reason()`:

```rust
enum DeadReason {
    ProcessExited,
    StuckAlive { last_stream_ms: i64, io_stale: bool },
}
```

The `cleanup_dead_agents()` function receives a `&SleepTracker` parameter. When not in grace period:
- Check stream staleness per existing `check_stream_liveness()`
- Check `/proc/<pid>/io` for I/O progress (Linux only; skip on macOS)
- If both stale → `StuckAlive`

For `StuckAlive`, run triage as with `ProcessExited`, but also kill the process after applying the verdict.

#### 3. Stream events (`src/stream_event.rs`)

No changes needed. The existing `Heartbeat` event type and turn/tool events already provide the activity signal. Native executor already emits these.

#### 4. `wg dead-agents` CLI

Add a `--check-stuck` flag that performs the stuck-alive check on demand (useful for debugging without waiting for the coordinator).

### Platform Considerations

| Feature | Linux | macOS | Windows |
|---------|-------|-------|---------|
| Monotonic vs wall clock gap | `Instant` vs `Utc::now()` | Same | Same |
| `/proc/<pid>/io` I/O check | Yes | No — use `rusage` or skip | N/A |
| `/proc/<pid>/stat` state | Yes | No — use `ps` or skip | N/A |
| `kill(pid, 0)` PID check | Yes | Yes | `is_process_alive` returns true |
| SIGTERM/SIGKILL | Yes | Yes | TerminateProcess |

The I/O check is a refinement, not a requirement. The core algorithm (monotonic gap + stream staleness) works cross-platform.

For macOS, `/proc` doesn't exist. The I/O staleness check can be skipped — stream staleness alone (no new events in `stream.jsonl`) is sufficient. The monotonic clock gap detection works identically.

### Failure Modes & Edge Cases

1. **Agent reconnects after sleep**: Grace period (2 min) allows agents to resume. If they produce stream events during grace, they're fine.

2. **Agent stuck on long tool call** (e.g., cargo build taking 20 min): Stream events include `ToolStart`/`ToolEnd`. A `ToolStart` without a matching `ToolEnd` means a tool is running. Could extend staleness check to account for in-progress tools. Initial implementation: just use a generous default threshold (10 min).

3. **False positive — agent doing CPU work with no I/O**: The `/proc/<pid>/io` check catches this (bytes will change). But more importantly, agents produce stream events regularly (turns, tool calls). 10 min with zero events is a strong stuck signal.

4. **Coordinator itself sleeps**: The daemon's main loop uses the same monotonic clock. After wake, it detects the gap on its first loop iteration and applies the grace period.

5. **Multiple rapid sleep/wake cycles**: Each wake resets the grace period. Agents that survive multiple cycles are likely fine.

6. **Race: agent finishes during triage**: Check task status after triage but before killing. If task is already `Done`/`Failed`, skip the kill.

### Rollout

1. **Phase 1**: Sleep gap detection + grace period + logging only (no kills). Ship this first to validate the detection heuristic with real data.
2. **Phase 2**: Stuck detection with triage + kill. Gate behind `auto_triage` config (already exists).
3. **Phase 3**: Optional I/O refinement for Linux. Add `--check-stuck` to CLI.

### Summary

The key insight is that `Instant` (monotonic) pauses during sleep while `Utc::now()` jumps forward. This gives us a reliable, portable, zero-dependency sleep detector. Combined with the existing stream staleness check and triage system, we get sleep-aware liveness detection with minimal new code:

- ~50 lines for `SleepTracker` struct + gap detection in daemon loop
- ~30 lines extending `detect_dead_reason()` for stuck-alive case
- ~20 lines config additions
- Reuse existing `run_triage()` and `kill_process_graceful()` for intervention

No new dependencies. No platform-specific APIs required (though `/proc` refinements help on Linux).
