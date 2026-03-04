# Design: `wg wait` Command and Condition System

**Author:** Researcher D1 (committee-v2-researcher-6)
**Date:** 2026-03-04
**Context:** Extends Phase 4 of `docs/design/liveness-detection.md`

## 1. Command Design

### Syntax

```
wg wait <task-id> --until <condition> [--checkpoint <message>]
```

The `<task-id>` is the task the calling agent is working on (its own task). The `--until` flag specifies when to resume.

### Supported Conditions

| Condition | Syntax | Example |
|-----------|--------|---------|
| Task completion | `task:<id>=<status>` | `--until "task:research-1=done"` |
| Timer | `timer:<duration>` | `--until "timer:30m"` |
| Human input | `human-input` | `--until "human-input"` |
| Message received | `message` | `--until "message"` |
| File changed | `file:<glob>` | `--until "file:src/lib.rs"` |

Duration format: `<n>s`, `<n>m`, `<n>h` (seconds, minutes, hours).

### Composable Conditions

Conditions are composable with `AND` (`,`) and `OR` (`|`):

```bash
# Resume when BOTH conditions are true (AND)
wg wait my-task --until "task:dep-a=done,task:dep-b=done"

# Resume when EITHER condition is true (OR)
wg wait my-task --until "task:dep-a=done|timer:2h"
```

**Why simple string DSL instead of a full expression language:** The primary consumers are AI agents, not humans writing complex queries. Agents need to express "wait for X" or "wait for X or timeout" — that's it. A heavier DSL (nested parens, NOT, etc.) adds parsing complexity with near-zero practical benefit. If we later need complex conditions, we can add a `--until-expr` flag with a richer grammar without breaking the simple syntax.

**No `AND` + `OR` mixing in one expression.** A single `--until` is either all-AND (commas) or all-OR (pipes). This avoids precedence ambiguity. For truly complex conditions, agents can decompose the wait into multiple steps.

### Condition Data Model

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WaitCondition {
    /// Wait for a task to reach a specific status
    TaskStatus { task_id: String, status: Status },
    /// Wait for a duration to elapse
    Timer { resume_after: String },  // ISO 8601 timestamp (computed at wait time)
    /// Wait for a human to send a message on the task
    HumanInput,
    /// Wait for any message on the task (from any source)
    Message,
    /// Wait for a file to change (mtime check)
    FileChanged { path: String, mtime_at_wait: u64 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum WaitSpec {
    /// All conditions must be true
    All(Vec<WaitCondition>),
    /// Any condition being true is sufficient
    Any(Vec<WaitCondition>),
}
```

This is stored on the Task struct:

```rust
pub struct Task {
    // ... existing fields ...

    /// Wait condition set by `wg wait` — coordinator checks and resumes when met
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait_condition: Option<WaitSpec>,

    /// Session ID from the executor, used for `--resume` on wake
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,

    /// Checkpoint summary written by agent before parking
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<String>,
}
```

### New Task Status: `Waiting`

Add `Waiting` to the `Status` enum:

```rust
pub enum Status {
    Open,
    InProgress,
    Waiting,     // NEW: agent parked, condition pending
    Done,
    Blocked,
    Failed,
    Abandoned,
}
```

`Waiting` is NOT terminal — the coordinator transitions it back to `Open` (or directly to `InProgress` for `--resume` executors) when the condition is met. `Waiting` tasks are excluded from:
- Ready-task polling (they have an explicit condition to check)
- Stuck-agent detection (no agent running)
- `max_agents` count (slot is freed)

### New Agent Status: `Parked`

Add `Parked` to `AgentStatus`:

```rust
pub enum AgentStatus {
    Starting,
    Working,
    Idle,
    Stopping,
    Parked,    // NEW: agent exited cleanly via wg wait
    Done,
    Failed,
    Dead,
}
```

`Parked` agents do NOT count against `max_agents`. The agent process has exited — this status is purely bookkeeping so we know the agent wasn't killed or failed; it voluntarily parked.

## 2. Agent-Side Flow

### What happens when an agent calls `wg wait`

```
Agent calls: wg wait my-task --until "task:dep-a=done" --checkpoint "Completed phase 1. Waiting for dep-a to provide API schema."
```

**Step-by-step:**

1. **Validate** the condition syntax and that the task exists and is `InProgress`
2. **Extract session_id** from the agent's `stream.jsonl` (the `system` event contains it)
3. **Write checkpoint** to the task's `checkpoint` field (from `--checkpoint` or auto-generated from last log entry)
4. **Store wait condition** on the task's `wait_condition` field
5. **Transition task** from `InProgress` → `Waiting`
6. **Update agent registry**: set agent status to `Parked`
7. **Log**: "Agent parked. Waiting for: task:dep-a=done"
8. **Print**: Instructions for the agent to exit cleanly

```
Parked task 'my-task'. Condition: task:dep-a=done
Checkpoint saved. You should now exit cleanly.
```

**The agent process should exit after calling `wg wait`.** The `wg wait` command itself is synchronous and returns immediately — it does NOT block. It's a "park and exit" command, not a "block until ready" command.

**Why exit instead of suspend?**
- Suspending a Claude process is not supported by the CLI
- A suspended process holds a slot, consuming resources
- The liveness doc's cost analysis shows keeping alive costs $1.80-$4.80/30min vs ~$0 for park+resume
- Exit + resume is the proven pattern from the liveness committee's research

### Auto-checkpoint

If `--checkpoint` is not provided, `wg wait` auto-generates a checkpoint from:
1. The last 3 log entries on the task
2. The last tool call from `stream.jsonl` (if available)

This ensures there's always something to inject on resume, even if the agent forgets to write a summary.

## 3. Coordinator-Side Flow

### Condition Checking

The coordinator already has a main loop that polls for ready tasks. Add a new phase: **check waiting tasks**.

```
Coordinator tick:
  1. Check for dead agents (existing)
  2. Check waiting tasks for satisfied conditions (NEW)
  3. Poll for ready tasks and spawn agents (existing)
```

**Condition evaluation per type:**

| Condition | Check method | Cost |
|-----------|-------------|------|
| `TaskStatus` | Read graph, check `task.status` | O(1) graph lookup, ~free |
| `Timer` | Compare `resume_after` timestamp with `now()` | Trivial |
| `HumanInput` | Check task messages for messages with `actor != agent-*` since wait time | Read messages file |
| `Message` | Check task messages for any message since wait time | Read messages file |
| `FileChanged` | Compare `mtime` of file with stored `mtime_at_wait` | `fs::metadata()` stat call |

**All checks are poll-based, evaluated on each coordinator tick** (default: every 5-10 seconds). Event-driven would be more efficient but adds complexity for marginal gain — the coordinator already polls, and these checks are all O(1) or O(small).

**Why not event-driven for TaskStatus?** The coordinator could subscribe to graph changes and reactively wake waiters. But polling is simpler, and the latency difference (5-10s) is insignificant for the use cases. If we later add webhooks or CI integration, we can add event-driven wake as an optimization without changing the data model.

### Condition Satisfied → Resume

When all/any conditions in the `WaitSpec` are met:

1. **Clear** `wait_condition` from the task
2. **Transition** task from `Waiting` → `Open`
3. **Log**: "Wait condition satisfied: task:dep-a=done. Task ready for resume."
4. **The normal ready-task polling** picks it up on the next tick
5. **Coordinator spawns a new agent** with resume context (see §4)

## 4. Resume Flow

### Resume Strategy Selection

The coordinator checks the executor type and session availability:

```
if executor == "claude" && task.session_id.is_some() {
    // Try --resume first (zero replay cost, full context)
    spawn with: claude --resume <session_id> --print
    inject: "You previously called `wg wait` for [condition]. The condition is now satisfied. Continue your work."
} else {
    // Reincarnation: fresh agent with checkpoint context
    spawn with standard prompt + checkpoint injection
}
```

### Resume Prompt Injection

For `--resume` (Claude executor):

```
## Resume Context
You previously called `wg wait my-task --until "task:dep-a=done"`.
The condition is now satisfied: task dep-a completed at 2026-03-04T15:30:00Z.

Your checkpoint: "Completed phase 1. Waiting for dep-a to provide API schema."

Graph state delta since you parked:
- dep-a: done (artifact: docs/api-schema.json)
- dep-b: in-progress

Continue your work on 'my-task'.
```

For reincarnation (non-Claude executor or expired session):

```
## Previous Attempt Recovery (via wg wait)
A previous agent was working on this task and voluntarily parked.

Checkpoint: "Completed phase 1. Waiting for dep-a to provide API schema."

Wait condition (now satisfied): task:dep-a=done
dep-a completed at 2026-03-04T15:30:00Z, artifact: docs/api-schema.json

Progress log:
[last 5 log entries from the task]

Artifacts so far:
[list of task artifacts]

Continue from where the previous agent left off.
```

### Graph State Delta

On resume, inject a **brief** delta of what changed since the agent parked — NOT the full `wg context` dump. Specifically:
- Tasks that the wait condition referenced: their new status + artifacts
- Immediate dependencies that changed status
- Any messages received on the task during the wait

This addresses the token accumulation concern from Researcher E's liveness analysis: each resume should add O(100 tokens) of context, not O(10K).

### Session ID Capture

The `session_id` is already captured in `stream.jsonl` as part of the `system` event (see `src/stream_event.rs:23`). When `wg wait` is called:

1. Parse the agent's `stream.jsonl` to find the last `system` event
2. Extract `session_id`
3. Store on `task.session_id`

If `stream.jsonl` doesn't exist or has no session_id (non-Claude executor), `session_id` remains `None` and we fall back to reincarnation.

## 5. Nested Waits

**Yes, an agent can wait, resume, then wait again.** There is no depth limit on sequential waits.

### How It Works

Each `wg wait` call is independent:
1. Agent resumes from first wait
2. Agent does more work
3. Agent calls `wg wait` again with a new condition
4. Process repeats

The task goes through: `InProgress → Waiting → Open → InProgress → Waiting → Open → InProgress → Done`

### State Management

Each `wg wait` call **overwrites** the previous `checkpoint` and `wait_condition`. This is correct because:
- The resumed agent has the full context from the previous checkpoint (via `--resume` or prompt injection)
- The new checkpoint captures the current state, which subsumes the old one
- `session_id` may change between waits (new agent process = new session), but for `--resume`, we use the latest session

### Limits

- **No concurrent waits** from the same task (a task can only be in one state at a time)
- **No limit on sequential waits** — but each resume costs either ~$0 (Claude `--resume`) or ~$0.05-$0.12 (reincarnation), so it's not free. Agents should batch work between waits rather than wait-resume-wait in tight loops.
- **Practical depth**: In practice, agents will rarely wait more than 2-3 times per task. The graph structure (dependencies) handles most coordination; `wg wait` is for dynamic, runtime conditions that can't be expressed as static graph edges.

### Anti-Pattern: Wait Loops

If an agent finds itself wanting to wait in a tight loop (wait → check → wait → check), the work should instead be decomposed into separate tasks with proper graph edges. `wg wait` is not a polling mechanism — it's a parking mechanism.

## 6. CI/External Conditions (Future)

The initial implementation supports `task`, `timer`, `human-input`, `message`, and `file` conditions. For external conditions like CI status:

### Option A: Webhook Receiver (recommended for future)
- Add a `wg webhook` endpoint that accepts POST requests
- CI systems (GitHub Actions, etc.) POST to the endpoint on completion
- Coordinator checks a webhook event queue alongside graph polling
- Condition syntax: `webhook:<event-name>`

### Option B: Custom Script
- Condition syntax: `script:<path>` — coordinator runs the script, checks exit code
- Exit 0 = condition met, non-zero = not yet
- Simple but requires the script to be fast and idempotent

### Option C: External Task Bridge
- Create a task that represents the CI job
- Use `wg exec` to run a polling script that marks the task done when CI passes
- Agent waits on `task:ci-task=done`
- This works TODAY with no new features — just task decomposition

**Recommendation:** Start with Option C (no implementation needed) and add Option A when there's clear demand.

## 7. Implementation Plan

### Phase 1: Core `wg wait` (minimal)

1. Add `Waiting` to `Status` enum in `src/graph.rs`
2. Add `wait_condition`, `session_id`, `checkpoint` fields to `Task` struct
3. Add `Parked` to `AgentStatus` in `src/service/registry.rs`
4. Implement `src/commands/wait.rs`: parse conditions, validate, store, transition
5. Add CLI entry in `src/cli.rs` and `src/main.rs`
6. Coordinator: add condition checking in service main loop
7. Coordinator: resume logic (transition `Waiting` → `Open`, inject context)
8. Exclude `Waiting` tasks from ready-task polling and stuck-agent detection

**Estimated scope:** ~300-400 lines of new code across 6-8 files.

### Phase 2: Resume with `--resume`

1. Store `session_id` from `stream.jsonl` during `wg wait`
2. Coordinator: use `--resume <session_id>` for Claude executor on wake
3. Build graph state delta for resume prompt
4. Fallback to reincarnation if session expired

### Phase 3: External Conditions

1. Webhook receiver for CI integration
2. Script-based conditions
3. `wg wait --list` to show all waiting tasks and their conditions

## 8. Edge Cases

1. **Agent calls `wg wait` but doesn't exit:** The coordinator sees the task as `Waiting` but the PID is still alive. After a grace period (30s), log a warning. If PID is still alive after 2 min, SIGTERM it — the task is parked regardless.

2. **Condition references a non-existent task:** `wg wait` validates at call time. `task:nonexistent=done` fails immediately with an error.

3. **Waited-on task fails instead of completing:** If waiting on `task:X=done` and X fails, the condition is never met. The coordinator should detect this: if the referenced task is terminal and doesn't match the expected status, transition the waiting task to `Failed` with reason "Wait condition unsatisfiable: task X failed".

4. **Multiple tasks waiting on the same condition:** Fine — each task independently tracks its condition. When the condition is met, all waiting tasks resume.

5. **Circular wait:** A waits on B, B waits on A. Both tasks are `Waiting`, neither can make progress. The coordinator should detect this (check for cycles in wait_condition references) and fail both with "Circular wait detected."

6. **Session expiry during long wait:** Claude sessions may expire after extended periods. The `checkpoint` field is the safety net — always populated, always available for reincarnation.

7. **Timer condition with system sleep:** Use wall clock time (not monotonic) for timer conditions. If the system sleeps for 30 minutes during a 5-minute timer, the timer should be considered elapsed on wake. This is the right behavior — the external world advanced, and the intent was "resume after 5 real minutes."
