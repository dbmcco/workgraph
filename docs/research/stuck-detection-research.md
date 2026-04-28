# Research: Agent Stuck Detection & Executor Process Management

## 1. Stuck Detection Systems (Complete Inventory)

The codebase has **four distinct stuck/liveness detection systems**:

### 1A. Dead-Agent Triage (Coordinator Tick — Phase 1)

**File:** `src/commands/service/triage.rs:139-169` (`detect_dead_reason`)  
**Called from:** `src/commands/service/coordinator.rs:3215-3219` (`cleanup_and_count_alive`)

**Heuristic:** Process liveness only (PID check).
- `kill(pid, 0)` via `src/service/mod.rs:25-27` (`is_process_alive`)
- PID reuse detection via `/proc/<pid>/stat` start-time comparison (`verify_process_identity`)
- Grace period: `config.agent.reaper_grace_seconds` (default 30s) — agents started less than this threshold are not reaped

**What happens when triggered:**
- Agent marked `Dead` in registry
- Task either triaged by LLM (if `auto_triage` enabled) or reset to `Open` with retry_count++
- Worktree cleaned up
- Token usage extracted from stream files

**Key insight:** This is purely process-based. It cannot detect a stuck *running* process — only a dead one. Heartbeat is **no longer used for detection** (comment at `triage.rs:153`). The coordinator auto-bumps heartbeats for all alive PIDs at `triage.rs:198-200`.

### 1B. Zero-Output Agent Detection (Coordinator Tick — Phase 1.3)

**File:** `src/commands/service/zero_output.rs` (entire file)  
**Called from:** `src/commands/service/coordinator.rs:3221-3241` (`sweep_zero_output_agents`)

**Heuristic:** Stream file content check. An agent is a "zero-output zombie" if:
1. PID is alive (`is_process_alive`)
2. Both `raw_stream.jsonl` and `stream.jsonl` are empty (0 bytes) or don't exist
3. Agent has been alive for >5 minutes (`ZERO_OUTPUT_KILL_THRESHOLD = 300s`)

**What happens when triggered:**
- Agent killed via `SIGKILL` (`kill_process_force`)
- Per-task circuit breaker: after 2 consecutive zero-output spawns (`MAX_ZERO_OUTPUT_RESPAWNS = 2`), task is `Failed` with tag `zero-output-circuit-broken`
- Global API-down detection: if >=50% of alive agents have zero output, spawning pauses with exponential backoff (60s → 15m cap)

**Key insight:** This catches API-level hangs (never got a response). But once an agent writes *any* bytes to the stream file, the zero-output detector ignores it permanently. An agent that gets one response then stalls forever is invisible to this system.

### 1C. Stream Staleness Warning (Triage — Advisory Only)

**File:** `src/commands/service/triage.rs:105-213` (`check_stream_liveness`)  
**Called from:** `triage.rs:202-213` (within `cleanup_dead_agents`, for alive agents)

**Heuristic:** Last stream event timestamp vs current time.
- `STREAM_STALE_THRESHOLD_MS = 5 * 60 * 1000` (5 minutes)
- Parses actual stream events to get the last event timestamp

**What happens when triggered:**
- **Warning only** (eprintln to daemon log). No action taken. No kill. No task reset.
- The warning reads: `[triage] WARNING: Agent {id} (task {tid}) PID alive but stream stale for {N}s`

**Key insight:** This is the closest thing to detecting an orchestrating agent waiting on a subprocess. It notices the stall but cannot distinguish "waiting on cargo build" from "genuinely stuck."

### 1D. TUI Dashboard Classification (UI Only)

**File:** `src/tui/viz_viewer/state.rs:1649-1687` (`DashboardAgentActivity::classify`)

**Heuristic:** Output file mtime.
- `Active`: output modified <30s ago
- `Slow`: output modified 30s–5m ago  
- `Stuck`: output modified >5m ago (300s threshold)
- `Exited`: terminal status (Done/Failed/Dead/Parked/Frozen/Stopping)

**Also:** TUI service health panel at `state.rs:9323-9343`:
- Checks `output_file` mtime, if >300s → toast warning "Agent stuck: {id} on {task} ({duration})"
- Deduped with key `stuck:{agent_id}`

**And:** "Stuck tasks" in service health at `state.rs:9360-9378`:
- Tasks with `status=InProgress` whose agent PID is dead
- These are surfaced in the TUI control panel for manual kill

**Key insight:** The TUI classification is purely cosmetic (visual indicator). The "stuck tasks" list in the service health panel combines dead-PID detection with manual intervention (kill button).

### 1E. Graph-Level Stuck Blocked Check

**File:** `src/check.rs:110-136` (`check_stuck_blocked`)

**Heuristic:** Tasks with `status=Blocked` where all dependencies have terminal status. These should have transitioned to `Open` but weren't.

**Key insight:** This is a graph consistency check, not an agent liveness check. It detects graph state bugs, not process issues.

---

## 2. Executor Process Management

### 2A. How Agents Are Spawned

**File:** `src/commands/spawn/execution.rs:29-622`

- Coordinator calls `spawn_agent_inner()` which:
  1. Claims the task (sets status=InProgress)
  2. Builds the prompt (assembles context, template, etc.)
  3. Creates a `run.sh` wrapper script
  4. Launches the wrapper via `Command::new("bash").arg("run.sh").spawn()`
  5. Captures the PID from `child.id()`
  6. Registers the agent in the registry with that PID

- The wrapper script optionally wraps the inner command with `timeout --signal=TERM --kill-after=30 <seconds>` based on `config.coordinator.agent_timeout` (default "30m")

### 2B. Process Tracking

- **Registry:** `src/service/registry.rs` — stores `AgentEntry` with PID, status, output_file, started_at, last_heartbeat
- **Liveness:** `kill(pid, 0)` — checks if PID exists, not whether the right process is there
- **Identity:** `/proc/<pid>/stat` start-time comparison — protects against PID reuse
- **No process tree awareness.** The system tracks only the top-level PID (the `run.sh` wrapper or the executor command). It has no concept of child processes.

### 2C. Heartbeat System (Vestigial)

**File:** `src/commands/heartbeat.rs`  
**CLI:** `wg heartbeat <agent-id>`

The heartbeat system exists but is **not used for stuck detection**:
- The triage module at `triage.rs:153` explicitly comments: "Process not running is the only signal — heartbeat is no longer used for detection"
- The coordinator **auto-bumps heartbeats** for all alive agents at `triage.rs:198-200`
- The `wg heartbeat` CLI and `wg dead-agents` commands still use heartbeat thresholds, but these are manual/diagnostic tools, not part of the coordinator loop

### 2D. Backgrounded/Long-Running Processes

**The executor does NOT track subprocess trees.** When an agent runs `cargo build`, `wg service start`, or any long command:
1. The agent process (Claude CLI, native executor) launches the command as a child
2. The agent's own PID stays alive (agent is waiting on the child)
3. The agent's output file does not update (agent is blocked waiting)
4. After 5 minutes of output silence, the TUI shows "stuck" and triage logs a warning
5. After 30 minutes (default `agent_timeout`), the `timeout` wrapper kills the agent

There is **no mechanism** for the agent to signal "I'm alive, just waiting on a subprocess."

---

## 3. The Core Problem: False Stuck Detection for Orchestrating Agents

An orchestrating agent (coordinator spawning sub-agents, running long CLI commands) hits the stuck detection in two ways:

### False Positive Path 1: Output Silence
1. Agent runs `wg service start` or `cargo build` (takes minutes)
2. Agent's stream file stops updating (agent is waiting, not producing LLM tokens)
3. After 5 minutes: TUI shows "stuck", triage logs warning
4. After 30 minutes: `timeout` wrapper kills the agent

### False Positive Path 2: Zero-Output on Slow Start
1. Agent starts up, API call hangs for 5+ minutes (rate limit, cold start)
2. Zero-output detector kills the agent
3. Respawn, same thing happens
4. Circuit breaker trips, task fails

### What Signals ARE Available
1. **PID alive** — top-level agent PID (`kill(pid, 0)`)
2. **Output file mtime** — when the agent last wrote to its output file
3. **Stream event timestamps** — when the agent last produced an LLM event
4. **No child process tracking** — `/proc/<pid>/children` or process group info is not used
5. **No file lock awareness** — `.git/index.lock` etc. not checked
6. **No disk I/O monitoring** — `/proc/<pid>/io` not checked

---

## 4. Proposed Approach: Subprocess-Aware Stuck Detection

### Quick Wins (Low effort, high impact)

**QW1: Expose child process liveness as a signal in triage**
- In `check_stream_liveness` (triage.rs), before emitting the stale warning, check if the agent PID has active child processes
- On Linux: read `/proc/<pid>/children` or walk `/proc/*/stat` for ppid matching
- If children are alive, suppress the stale warning or change it to an informational log
- Effort: ~50 lines in triage.rs

**QW2: Activity-based timeout extension for the output file heuristic**
- In the TUI `DashboardAgentActivity::classify` (state.rs:1665), and the stuck toast logic (state.rs:9323-9343), use child process liveness as a third input alongside status and output age
- If output is stale but children are alive → classify as `Slow` not `Stuck`
- Effort: ~30 lines in state.rs + helper function

**QW3: Agent heartbeat-via-wg-log**
- When an agent runs `wg log <task> "message"`, this writes to the graph but doesn't update the agent's output file. Add a side-effect: touch the agent's output file or write a heartbeat stream event when `wg log` is called.
- This gives agents a zero-cost way to signal liveness during long operations.
- Effort: ~20 lines in the log command

### Larger Architectural Changes

**A1: Process tree tracking in the registry**
- Extend `AgentEntry` with a `child_pids: Vec<u32>` or just a `has_active_children: bool`
- Triage writes this during its cleanup sweep (already iterates agents)
- Zero-output and stuck detection use it as an exemption signal
- Effort: ~100 lines across triage.rs, zero_output.rs, registry.rs

**A2: Agent activity bus (structured liveness signals)**
- Define a lightweight file-based protocol: agents write `{"type":"activity","detail":"waiting_on_subprocess","child_pid":12345}` to a known file (e.g., `agent_activity.jsonl` in their output directory)
- The coordinator reads this during triage alongside stream events
- This is the "agent signals I'm alive" mechanism
- Effort: ~200 lines (protocol definition, writer in agent prompt/tools, reader in triage)

**A3: Configurable stuck thresholds per task type**
- Allow `wg add --stuck-timeout 60m` or a task-level config for long-running orchestration tasks
- The coordinator respects this override instead of the global 5-minute threshold
- Effort: ~80 lines (graph schema + triage logic + CLI)

### Recommended Priority Order

1. **QW1** (child process check) — biggest bang for the buck, directly addresses the false positive
2. **QW3** (heartbeat-via-log) — gives agents an escape hatch today
3. **A3** (per-task timeout) — makes the system configurable without code changes
4. **A1** (process tree in registry) — proper architectural solution
5. **QW2** (TUI classification fix) — cosmetic but reduces user confusion
6. **A2** (activity bus) — full solution but heaviest lift

---

## 5. Related Issues with False Stuck Detection

1. **Agent timeout kills orchestrating agents**: Default 30m `agent_timeout` in config will SIGTERM then SIGKILL an agent running `cargo build` or waiting on sub-agents. Orchestrating agents need either a longer timeout or timeout exemption.

2. **Zero-output circuit breaker can trip on slow model providers**: If a model API is slow (5+ min for first response), the agent gets killed 3 times and the task fails. The circuit breaker doesn't distinguish "slow API" from "broken API."

3. **Auto-bumped heartbeats mask real staleness**: Since `triage.rs:198-200` auto-bumps heartbeats for all alive PIDs, the `wg heartbeat` and `wg dead-agents` CLI tools always show fresh heartbeats. The heartbeat field is effectively meaningless.

4. **Stream stale warning is fire-and-forget**: The warning at `triage.rs:206-212` logs to stderr but doesn't feed back into any detection/action system. It's invisible unless someone reads the daemon log.

---

## 6. File Reference Index

| System | File | Key Lines |
|--------|------|-----------|
| Dead-agent triage | `src/commands/service/triage.rs` | `139-169` (detect_dead_reason), `171-460` (cleanup_dead_agents) |
| Zero-output detection | `src/commands/service/zero_output.rs` | `22-23` (thresholds), `189-221` (check_zero_output), `238-447` (sweep) |
| Stream staleness | `src/commands/service/triage.rs` | `105-132` (check_stream_liveness), `198-213` (warning) |
| TUI activity classify | `src/tui/viz_viewer/state.rs` | `1649-1687` (DashboardAgentActivity) |
| TUI stuck toast | `src/tui/viz_viewer/state.rs` | `9323-9343` (output age check, 300s threshold) |
| TUI stuck tasks | `src/tui/viz_viewer/state.rs` | `9360-9378` (in-progress + dead PID) |
| Graph stuck-blocked | `src/check.rs` | `110-136` (check_stuck_blocked) |
| Process alive check | `src/service/mod.rs` | `25-27` (is_process_alive) |
| PID reuse detection | `src/service/mod.rs` | `verify_process_identity` |
| Agent spawn | `src/commands/spawn/execution.rs` | `29-622` (spawn_agent_inner) |
| Agent timeout | `src/commands/spawn/execution.rs` | `374-388` (timeout resolution) |
| Respawn throttle | `src/commands/service/coordinator.rs` | `2579-2660` (check_respawn_throttle) |
| Coordinator tick | `src/commands/service/coordinator.rs` | `3199-3376` (coordinator_tick) |
| Heartbeat (vestigial) | `src/commands/heartbeat.rs` | `6-18` (run_agent) |
| Config thresholds | `src/config.rs` | `2057-2058` (heartbeat_timeout=5m), `2110-2111` (agent_timeout=30m) |
| Dead-agents CLI | `src/commands/dead_agents.rs` | `43-163` (check/cleanup) |
