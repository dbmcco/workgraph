# Design: Unified Agent Lifecycle State Machine

**Author:** Committee v2 Researcher C1
**Date:** 2026-03-04
**Context:** Extends `docs/design/liveness-detection.md` (Round 1 consensus)

## 1. Complete State Enumeration

The unified lifecycle has **11 states**, mapped across two dimensions: the **task** (what work needs doing) and the **agent** (what process is doing it).

| # | State | Description | Agent Process? | Counts Against max_agents? |
|---|-------|-------------|----------------|---------------------------|
| 1 | **Draft** | Created but not published/visible to coordinator | No | No |
| 2 | **Blocked** | Waiting on dependency completion | No | No |
| 3 | **Ready** | All deps met, eligible for dispatch | No | No |
| 4 | **InProgress** | Agent actively working | Yes (alive) | Yes |
| 5 | **Stuck** | Agent alive but unresponsive (detected by liveness) | Yes (alive, unresponsive) | Yes |
| 6 | **Waiting** | Agent voluntarily parked (`wg wait`), process exited | No (exited cleanly) | **No** |
| 7 | **Done** | Completed successfully | No | No |
| 8 | **Failed** | Agent couldn't complete | No | No |
| 9 | **Abandoned** | Permanently dropped, won't be retried | No | No |
| 10 | **Paused** | Administratively frozen (not dispatched regardless of deps) | No | No |
| 11 | **Resuming** | Being resurrected from Waiting/Done (transient state) | Spawning | Briefly yes |

### Key Distinctions

- **Blocked vs Waiting:** Blocked = structural (deps not met, automatic). Waiting = agent-initiated (agent called `wg wait`, chose to park on a condition it specified). Blocked is graph-level; Waiting is agent-level.
- **Stuck vs InProgress:** Stuck is detected by the liveness system (stale_ticks >= threshold). It's a coordinator-observed state, not self-reported. The agent doesn't know it's stuck.
- **Draft vs Paused:** Draft = never been published. Paused = was active but administratively frozen. Both prevent dispatch, but Paused preserves loop state and status.
- **Resuming:** A transient state lasting seconds. Covers the window between "coordinator decides to resume" and "agent is alive and InProgress." Prevents double-dispatch.

## 2. State Transition Map

### All Valid Transitions

```
From          → To            | Trigger
──────────────┬───────────────┼──────────────────────────────────────
Draft         → Ready         | `wg resume` / `wg publish` (if deps met)
Draft         → Blocked       | `wg resume` / `wg publish` (if deps NOT met)
Draft         → Abandoned     | `wg abandon`
Draft         → Paused        | (already effectively paused; explicit flag set)
──────────────┼───────────────┼──────────────────────────────────────
Blocked       → Ready         | Last blocking dep completed
Blocked       → Abandoned     | `wg abandon`
Blocked       → Paused        | `wg pause` (admin freeze)
Blocked       → Failed        | Dep failed + no retry path
──────────────┼───────────────┼──────────────────────────────────────
Ready         → InProgress    | Coordinator dispatches agent
Ready         → Blocked       | New dep added (e.g., `wg add --before`)
Ready         → Paused        | `wg pause`
Ready         → Abandoned     | `wg abandon`
──────────────┼───────────────┼──────────────────────────────────────
InProgress    → Done          | Agent calls `wg done`
InProgress    → Failed        | Agent calls `wg fail` / process crashes
InProgress    → Stuck         | Liveness detector: stale_ticks >= threshold
InProgress    → Waiting       | Agent calls `wg wait` (voluntary park)
InProgress    → Paused        | `wg pause` (admin; agent is killed)
InProgress    → Abandoned     | `wg abandon` (admin; agent is killed)
──────────────┼───────────────┼──────────────────────────────────────
Stuck         → InProgress    | Agent produces new stream events (false positive)
Stuck         → Done          | Triage verdict: kill-done
Stuck         → Ready         | Triage verdict: kill-restart (agent killed, task reopened)
Stuck         → Failed        | Triage verdict: kill-fail (max retries exceeded)
Stuck         → Abandoned     | `wg abandon`
──────────────┼───────────────┼──────────────────────────────────────
Waiting       → Resuming      | Wait condition met / `wg resume <task>`
Waiting       → Ready         | Wait condition met (if no session to resume)
Waiting       → Failed        | Wait condition cannot be met (timeout, dep failed)
Waiting       → Abandoned     | `wg abandon`
Waiting       → Paused        | `wg pause`
──────────────┼───────────────┼──────────────────────────────────────
Resuming      → InProgress    | Agent process alive, begins producing events
Resuming      → Failed        | Resume failed (session expired, crash on start)
Resuming      → Ready         | Resume failed but task retryable
──────────────┼───────────────┼──────────────────────────────────────
Done          → Ready         | `wg retry` / loop edge fires (cycle iteration)
Done          → Resuming      | `wg resume` with --continue (extend completed work)
Done          → Abandoned     | `wg abandon`
──────────────┼───────────────┼──────────────────────────────────────
Failed        → Ready         | `wg retry`
Failed        → Abandoned     | `wg abandon` / max retries exceeded
──────────────┼───────────────┼──────────────────────────────────────
Abandoned     → Ready         | `wg retry --force` (explicit un-abandon)
──────────────┼───────────────┼──────────────────────────────────────
Paused        → Ready         | `wg resume` (if deps met)
Paused        → Blocked       | `wg resume` (if deps NOT met)
Paused        → Draft         | (revert to draft, rare)
Paused        → Abandoned     | `wg abandon`
```

## 3. State Diagram (ASCII)

```
                          ┌──────────┐
                          │  DRAFT   │
                          └────┬─────┘
                     publish/  │  \abandon
                     resume    │   \
                    ┌──────────▼─┐  ▼
              ┌────►│  BLOCKED   │  ABANDONED ◄── (from any state)
              │     └──────┬─────┘      ▲
              │   dep met  │            │ retry --force
              │            ▼            │
   new dep  ┌─┴────────────────┐       │
   ────────►│     READY        │◄──────┘
            └────────┬─────────┘
                     │ dispatch
                     ▼
             ┌───────────────┐    wg wait     ┌──────────┐
             │  IN PROGRESS  │───────────────►│ WAITING  │
             └──┬──┬──┬──────┘                └────┬─────┘
                │  │  │                    condition│met
      wg done  │  │  │ stale_ticks               ▼
               │  │  │ >= threshold      ┌──────────────┐
               ▼  │  ▼                   │  RESUMING    │
        ┌──────┐  │  ┌───────┐           └──────┬───────┘
        │ DONE │  │  │ STUCK │                  │
        └──┬───┘  │  └──┬────┘                  │ agent alive
           │      │     │ triage                 ▼
    loop/  │      │     ├─► kill-done ──► DONE   │
    retry  │      │     ├─► kill-restart → READY │
           ▼      │     └─► resumed ──► IN PROGRESS
        READY     │
                  │ crash / wg fail
                  ▼
              ┌────────┐
              │ FAILED │──── retry ──► READY
              └────────┘

                     ┌────────┐
              ──────►│ PAUSED │  (from Ready, Blocked, InProgress, Waiting)
        wg pause     └───┬────┘
                         │ wg resume
                         ▼
                   Ready or Blocked (depending on deps)
```

## 4. Invalid/Impossible Transitions

| Transition | Why Invalid |
|-----------|-------------|
| Draft → InProgress | Must pass through Ready first (coordinator dispatches from Ready) |
| Draft → Done | Cannot complete work that was never started |
| Blocked → InProgress | Must transition through Ready when deps are met |
| Blocked → Done | Cannot complete a blocked task directly |
| Ready → Done | Must go through InProgress (agent does work) |
| Ready → Failed | No agent has attempted it yet |
| Ready → Stuck | No agent exists to be stuck |
| Ready → Waiting | No agent exists to voluntarily park |
| Done → InProgress | Must go through Ready or Resuming (prevents accidental re-entry) |
| Done → Blocked | Completed tasks don't revert to blocked |
| Done → Failed | Already completed; if later invalidated, use a new task |
| Failed → InProgress | Must go through Ready (retry resets state) |
| Failed → Blocked | Retry goes to Ready; deps are re-evaluated there |
| Abandoned → InProgress | Must go through Ready via explicit retry --force |
| Stuck → Waiting | Stuck agent can't voluntarily park (it's unresponsive) |
| Stuck → Blocked | Doesn't make sense; kill-restart → Ready handles this |
| Waiting → InProgress | Must go through Resuming (prevents race conditions) |
| Resuming → Waiting | Just resumed; if needs to park again, goes InProgress → Waiting |
| Paused → InProgress | Must go through Ready first |
| Paused → Done | Cannot complete work while paused |
| InProgress → Blocked | Agent is working; if dep added, handle after completion |
| InProgress → Ready | Agent crash goes to Failed, not back to Ready directly |

## 5. Relationship to Current Status Enum in graph.rs

### Current `Status` enum (graph.rs:96-104):
```rust
pub enum Status {
    Open,        // maps to → Ready (deps met) or Blocked (deps not met)
    InProgress,  // stays InProgress
    Done,        // stays Done
    Blocked,     // stays Blocked
    Failed,      // stays Failed
    Abandoned,   // stays Abandoned
}
```

### Current `AgentStatus` enum (service/registry.rs:26-41):
```rust
pub enum AgentStatus {
    Starting,    // maps to → InProgress (or Resuming)
    Working,     // maps to → InProgress
    Idle,        // no direct task-state equivalent
    Stopping,    // transient, no task-state equivalent
    Done,        // maps to → Done
    Failed,      // maps to → Failed
    Dead,        // triggers → Stuck or Failed transition
}
```

### What Needs to Change

**In `Status` (task state):**

1. **Split `Open` into `Ready` + `Draft`:** Currently `Open` is overloaded. A task with `status: Open` and `paused: true` is effectively Draft. A task with `status: Open` whose deps are met is Ready. Making these explicit states eliminates the need for the `paused` boolean flag and the implicit readiness-check logic scattered across the codebase.

2. **Add `Waiting`:** New state for agent-parked tasks. Needs fields: `wait_condition` (what the agent is waiting for), `session_id` (for `--resume`), `checkpoint_summary` (fallback). Could be stored as metadata on the task rather than fields on `Task` struct.

3. **Add `Stuck`:** New state for liveness-detected hung agents. The coordinator sets this; agents never self-report as stuck. Might be better as a field (`stuck: bool` or `stuck_since: Option<DateTime>`) on InProgress tasks rather than a separate enum variant, since the task is still logically InProgress from the agent's perspective.

4. **Consider `Resuming` as transient:** Could be a field on the task (`resuming: bool`) rather than a status, since it lasts seconds. Or track it only in the coordinator's in-memory state (not persisted).

### Recommended Approach

```rust
pub enum Status {
    #[default]
    Draft,         // NEW: was Open + paused
    Blocked,       // existing (unchanged)
    Ready,         // NEW: was Open (deps met, not paused)
    InProgress,    // existing (unchanged)
    Waiting,       // NEW: agent voluntarily parked
    Done,          // existing (unchanged)
    Failed,        // existing (unchanged)
    Abandoned,     // existing (unchanged)
}
```

**Not added as enum variants (use fields instead):**
- `Stuck` → field `stuck_since: Option<DateTime<Utc>>` on tasks with `status: InProgress`
- `Resuming` → coordinator-internal transient state, not persisted
- `Paused` → the `paused: bool` field remains orthogonal to status (any status can be paused, though it only matters for Ready/Blocked)

**Migration path for `Open`:**
- `Open` with `paused: true` → `Draft`
- `Open` with deps not met → `Blocked`
- `Open` with deps met → `Ready`
- Add backwards-compatible deserializer (like the existing `pending-review` → `Done` mapping at graph.rs:119)

### AgentStatus Changes

```rust
pub enum AgentStatus {
    Starting,    // unchanged
    Working,     // unchanged
    Idle,        // unchanged
    Stopping,    // unchanged
    Done,        // unchanged
    Failed,      // unchanged
    Dead,        // unchanged
    Parked,      // NEW: agent exited via wg wait, task is Waiting
    Stuck,       // NEW: liveness detected unresponsive (mirrors task stuck_since)
}
```

## 6. The 'Waiting' State — Deep Dive

### Blocked vs Waiting

| Dimension | Blocked | Waiting |
|-----------|---------|---------|
| **Who decides** | Graph structure (automatic) | Agent (voluntary) |
| **Trigger** | Dependency not complete | `wg wait --on <condition>` |
| **Resolution** | Automatic when dep completes | Condition met or `wg resume` |
| **Agent process** | Never existed | Existed, exited cleanly |
| **Session state** | N/A | Saved (session_id + checkpoint) |
| **Slot consumed** | No | **No** (key benefit) |
| **Cost while waiting** | $0 | $0 (vs $1.80-4.80/30min if kept alive) |
| **Context on resume** | N/A | Preserved via --resume or checkpoint |

### Wait Conditions

An agent might wait on:
1. **Task completion:** "Wait until task X is done" → resolved by graph event
2. **Time:** "Wait until 2pm" → resolved by `not_before`-style timer
3. **External event:** "Wait for CI to pass" → resolved by webhook or polling
4. **Human input:** "Wait for user review" → resolved by `wg resume`

### Data Model for Waiting

```rust
// On the Task:
pub struct WaitState {
    /// What the agent is waiting for (human-readable + machine-parseable)
    pub condition: WaitCondition,
    /// Claude session ID for --resume (if Claude executor)
    pub session_id: Option<String>,
    /// Checkpoint summary for non-Claude or expired-session fallback
    pub checkpoint_summary: Option<String>,
    /// When the agent parked
    pub parked_at: DateTime<Utc>,
    /// Optional timeout (auto-fail if condition not met by this time)
    pub timeout: Option<DateTime<Utc>>,
}

pub enum WaitCondition {
    TaskComplete(String),     // wait for task ID to complete
    NotBefore(DateTime<Utc>), // time-based wait
    HumanInput,               // waiting for user
    Custom(String),           // freeform condition description
}
```

## 7. Implementation Considerations

### Backward Compatibility

The `Open` → `Ready`/`Draft` split is the biggest breaking change. Mitigation:
1. Keep deserializing `"open"` as `Ready` (backward-compatible read)
2. Serialize new states with new names
3. The existing `paused: bool` field continues to work during transition
4. Provide `wg migrate` command for explicit graph migration

### Coordinator Impact

The coordinator currently checks `status == Open && deps_met && !paused && assigned.is_none()` to find dispatchable tasks. With the new model: `status == Ready && !paused && assigned.is_none()`. Simpler.

### Transition Guards

Some transitions need guards:
- `InProgress → Done`: Only the assigned agent (or admin) can mark done
- `* → Abandoned`: Admin-only operation
- `Stuck → *`: Only the coordinator/triage system (not the stuck agent)
- `Waiting → Resuming`: Only coordinator (prevents races)

## 8. Open Questions for Committee Discussion

1. **Should Stuck be a status or a field?** Making it a status is cleaner for the state machine but means we need `Stuck → InProgress` (false positive recovery). Making it a field (`stuck_since`) keeps the task logically InProgress but requires checking the field in addition to status.

2. **Should Ready be computed or stored?** Currently readiness is computed (Open + deps met). Storing `Ready` as explicit status means the graph must recompute on every dep change. Risk: stale Ready status if deps are added after status is set. Alternatively, keep computing readiness but rename `Open` to convey "not yet started."

3. **Paused as orthogonal flag vs status?** Current `paused: bool` works for any status. Making Paused a status means you lose the underlying status info. Recommend keeping as flag.

4. **Resuming duration:** How long before Resuming → Failed (resume timeout)? Suggest 60s default.
