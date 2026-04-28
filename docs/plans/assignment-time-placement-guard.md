# Design: Assignment-Time Placement Guard for Orphaned Tasks

## Problem

When `auto_place` is enabled, tasks added via `wg add` without `--after` or `--before` flags bypass the placement pipeline entirely. The merged placement+assignment step in `build_auto_assign_tasks` (Phase 2) already supplies the active-tasks context to the LLM so it *can* suggest placement edges — but nothing distinguishes an unlinked task that *needs* placement from one that *has already been placed*. The LLM gets the same prompt regardless, and there's no guard to ensure the placement decision is actually exercised for floating tasks.

Result: orphaned tasks get assigned and executed but sit disconnected from the workflow graph, invisible to downstream consumers and unblockable by upstream work.

## Design Questions (Answered)

### 1. Detection: What counts as "orphaned"?

**Definition:** A task is _placement-eligible_ when ALL of the following hold:

- It is **not a system task** (no `.assign-*`, `.flip-*`, `.evaluate-*`, `.place-*`, `.create-*` prefix)
- Its `after` list is empty OR contains **only** system tasks (`.assign-<self>`)
- Its `before` list is empty
- It does **not** carry the `placed` tag (which is stamped by the placement code after edges are applied — see `coordinator.rs:1213`)
- It does **not** carry a proposed `standalone` tag (see Question 4 below)

Why not "zero dependents"? Because a task with zero `after` but non-zero `before` was explicitly placed as an entry point that feeds later work. Only tasks with *no edges in either direction* (modulo system scaffolding) are truly floating.

Edge case: a task with `after: [".assign-my-task"]` technically has one dependency, but that's auto-scaffolded by Phase 1. The guard must filter out system-task deps when checking emptiness.

### 2. Where the guard lives: coordinator (Phase 2 entry), not a separate pass

**Decision: Inside the existing Phase 2 loop in `build_auto_assign_tasks`, at the point where `active_tasks_context` is built.**

Rationale:
- The guard is a *gating check* on whether placement context gets injected into the assignment prompt. It belongs at the call site that decides the prompt shape, not in a separate coordinator pass.
- Putting it in the assigner module (`assignment.rs`) would mean the assigner needs graph-level knowledge about edge state — that's the coordinator's concern.
- A separate pre-assignment pass would add an extra graph scan per tick with no benefit.

**Concrete change:** In `coordinator.rs`, replace the current `auto_place` check:

```rust
// Current:
let active_tasks_context = if config.agency.auto_place {
    super::assignment::build_active_tasks_context(graph, &source_id)
} else {
    String::new()
};

// Proposed:
let needs_placement = config.agency.auto_place && is_placement_eligible(graph, &source_id);
let active_tasks_context = if needs_placement {
    super::assignment::build_active_tasks_context(graph, &source_id)
} else {
    String::new()
};
```

A new helper `is_placement_eligible(graph, task_id) -> bool` encapsulates the detection logic from Question 1. It lives in `coordinator.rs` (or a small `placement.rs` helper module) since it queries graph topology.

When `auto_place` is true but the task already has real edges or carries `placed`/`standalone`, the assignment prompt stays slim (no active-tasks section, no placement JSON field) — saving tokens and latency.

### 3. Prompt structure: conditional section, not a separate prompt

**Decision: Keep the existing integrated prompt structure.** The current design in `build_assignment_prompt` already handles this correctly:

- When `active_tasks_context` is non-empty → "Active Tasks" section + "Placement Instructions" section + `placement` JSON field in response schema
- When `active_tasks_context` is empty → none of the above

The only change is **upstream**: the coordinator now passes a non-empty context *only when the task is placement-eligible*, rather than for *all* tasks when `auto_place` is on.

Additionally, when the guard fires (task is placement-eligible), add a **placement urgency hint** to the prompt:

```
## Placement Notice
This task has no dependency edges. You MUST provide a `placement` field.
If the task genuinely has no dependencies (it is standalone), set:
  "placement": {"after": [], "before": [], "standalone": true}
```

This explicit instruction prevents the LLM from lazily returning `"placement": null` for an unlinked task.

The `PlacementDecision` struct gains an optional `standalone: bool` field (default `false`), deserialized from the LLM response.

### 4. Handling genuinely standalone tasks

**Decision: Mark with a `standalone` tag so the guard doesn't re-fire.**

When the LLM returns `placement.standalone == true`:
1. Add the `standalone` tag to the task (alongside the existing `placed` tag logic)
2. Do **not** add any edges
3. Log the decision: `"Placement: confirmed standalone (no edges needed)"`

The `standalone` tag is the circuit breaker. `is_placement_eligible` checks for it and returns `false`, so subsequent assignment cycles (e.g., after resurrection) won't re-prompt for placement unless the tag is explicitly removed.

If the coordinator resurrects a task (reopens `.assign-*`), it should **not** strip the `standalone` tag — the placement decision persists across assignment cycles. Manual override: `wg tag remove <task> standalone` to force re-evaluation.

### 5. Configurability

**Decision: Gated by the existing `auto_place` config flag.** No new flag needed.

- `auto_place = false` (default): No placement guard, no placement in assignment prompt. Existing behavior.
- `auto_place = true`: The placement guard activates. Floating tasks get the expanded prompt. Already-placed and standalone tasks get the slim prompt.

This is backward-compatible. The only behavioral change when `auto_place = true` is that the prompt is now *selectively* expanded (only for tasks that need it) rather than *always* expanded.

For workflows that intentionally create floating tasks, users have two escape hatches:
1. Add `--tag standalone` at creation time: `wg add "My floating task" --tag standalone`
2. Disable `auto_place` globally

No new config flag is introduced because the granularity of `auto_place` + per-task `standalone` tag is sufficient. Adding a `placement_guard` flag on top of `auto_place` would create confusing state.

### 6. Performance impact

**Improved, not degraded.**

Current behavior with `auto_place = true`:
- *Every* task gets the active-tasks context injected, even tasks that already have edges and don't need placement.
- Cost: ~50-200 extra tokens per assignment call (active task list) × number of assignments per tick.

Proposed behavior:
- Only placement-eligible (floating) tasks get the expanded prompt.
- Tasks with existing edges skip placement entirely → shorter prompt → cheaper call.
- Net effect: **reduced cost** for the common case (tasks created with `--after`), same cost for the uncommon case (floating tasks).

The `is_placement_eligible` check is O(1) per task (check `after`, `before`, `tags` fields on the in-memory `Task` struct). Negligible.

## Implementation Summary

### Data changes

1. **`PlacementDecision`** (in `assignment.rs`): Add `#[serde(default)] pub standalone: bool`
2. **No new config fields.**
3. **No schema changes** to `graph.jsonl` (tags are already free-form strings).

### Code changes

1. **`coordinator.rs`**: New `is_placement_eligible(graph, task_id) -> bool` function.
2. **`coordinator.rs`** Phase 2: Replace `if config.agency.auto_place` with `if config.agency.auto_place && is_placement_eligible(...)`.
3. **`coordinator.rs`** Phase 2 (after verdict): When `verdict.placement.standalone == true`, add `standalone` tag.
4. **`assignment.rs`** `build_assignment_prompt`: When `active_tasks_context` is non-empty, append the "Placement Notice" hint about standalone.
5. **Tests**: 
   - Unit test for `is_placement_eligible` (various edge combinations)
   - Unit test for `PlacementDecision` deserialization with `standalone: true`
   - Integration test: task with edges → no placement section in prompt
   - Integration test: floating task → placement section present
   - Integration test: standalone tag → no placement section on re-assignment

### Migration / backward compatibility

- **No breaking changes.** The `standalone` field on `PlacementDecision` defaults to `false`, so existing LLM responses without it parse correctly.
- **Existing `placed` tag** continues to work as before (applied after edges are added).
- **Tasks already in the graph** without the `standalone` tag: if they're floating and `auto_place` is on, they'll get the placement prompt on their next assignment cycle — this is the desired behavior (catch previously-missed orphans).
- The guard is **additive** — it narrows when placement is attempted, never removes existing placement capability.

## Sequence Diagram

```
User: wg add "Fix bug" (no --after, no --before)
  │
  ▼
Phase 1: scaffold .assign-fix-bug (blocking edge)
  │
  ▼
Phase 2: process .assign-fix-bug
  │
  ├── is_placement_eligible("fix-bug")?
  │     after = [".assign-fix-bug"] → filter system → empty
  │     before = [] → empty
  │     no "placed" tag, no "standalone" tag
  │     → TRUE
  │
  ├── Build active_tasks_context (non-empty)
  │
  ├── build_assignment_prompt (with placement section + notice)
  │
  ├── LLM returns verdict:
  │     { agent_hash, placement: { after: ["other-task"], before: [], standalone: false } }
  │
  ├── Apply edges: fix-bug.after += ["other-task"]
  │   Add "placed" tag
  │
  └── Mark .assign-fix-bug as Done

--- OR if standalone ---

  ├── LLM returns verdict:
  │     { agent_hash, placement: { after: [], before: [], standalone: true } }
  │
  ├── No edges added
  │   Add "placed" tag + "standalone" tag
  │   Log: "Placement: confirmed standalone"
  │
  └── Mark .assign-fix-bug as Done
```

## Open Questions (Non-blocking)

1. **Should `wg add --standalone` be a CLI shortcut for `--tag standalone`?** Ergonomic but adds CLI surface. Defer to implementation task.
2. **Should the guard log a warning when it fires?** Yes — `[coordinator] Placement guard: task 'X' has no edges, expanding assignment prompt`. Useful for debugging.
3. **Rate limiting:** If many floating tasks arrive at once, they all get expanded prompts. The existing 10-second Phase 2 time budget handles this — deferred tasks retry next tick.
