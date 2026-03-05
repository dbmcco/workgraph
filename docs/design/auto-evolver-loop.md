# Auto-Evolver Loop: Closing the Agency Feedback Cycle

## Status: Design (March 2026)

## Summary

The agency lifecycle is a four-step loop: assign, execute, evaluate, evolve. Steps 1-3 are automated (`auto_assign`, `auto_evaluate`), but step 4 — evolution — is always manual (`wg evolve`). This means evaluations accumulate without being acted on, and users who don't know about `wg evolve` never benefit from the data their agents are generating.

This design closes the loop. The system bootstraps itself, evaluates itself, and evolves itself. No manual intervention required. Power users can still override everything.

---

## 1. Trigger Conditions

**Decision: Evaluation-count threshold with time-based minimum interval.**

The evolver triggers when **both** conditions are met:

1. **New evaluations since last evolution >= `auto_evolve_threshold`** (default: 10)
2. **Time since last evolution >= `auto_evolve_interval_minutes`** (default: 120)

Rationale for these defaults:
- 10 evaluations provides enough signal for meaningful evolution without noise. At typical workloads (5-20 tasks/day), this means evolving 1-3 times per day.
- The 2-hour minimum interval prevents thrashing — evolution needs time to be tested before evolving again.
- A pure time-based trigger would fire even when nothing has happened. A pure count-based trigger could fire every few minutes in burst workloads. The combination prevents both failure modes.

**Reactive trigger:** When average evaluation score drops below `auto_evolve_reactive_threshold` (default: 0.4) over the last 5 evaluations, evolution triggers immediately (ignoring the time interval but still requiring >= 5 new evaluations). This catches sudden performance regressions.

**Where the trigger lives:** In the coordinator tick loop (`coordinator.rs`), checked after the auto-evaluate phase. The coordinator already tracks graph state and has access to evaluations — adding a "should we evolve?" check is natural.

### Trigger Implementation

```rust
// In coordinator_tick(), after build_auto_evaluate_tasks():
if config.agency.auto_evolve {
    if should_trigger_evolution(dir, config) {
        build_auto_evolve_task(dir, graph, config);
    }
}
```

The `should_trigger_evolution()` function:

1. Reads `.workgraph/agency/evolver-state.json` for `last_evolved_at` and `last_evolved_eval_count`
2. Counts current evaluations in `agency/evaluations/`
3. Computes `new_evals = current_count - last_evolved_eval_count`
4. Checks time elapsed since `last_evolved_at`
5. Checks reactive threshold if `new_evals >= 5`
6. Returns true if conditions met

---

## 2. Coordinator Integration

**Decision: Evolution runs as a `.evolve-*` meta-task, dispatched like `.assign-*` and `.evaluate-*`.**

### Why a meta-task, not a coordinator-level action

The compactor is a coordinator-level action (lightweight, fast, no graph representation needed). Evolution is different:

- It's expensive (spawns an LLM, opus-class model, ~$0.10-0.30 per run)
- It can take 30-60 seconds
- It produces structured output that needs parsing and application
- It has safety gates (deferred operations, self-mutation detection)
- It benefits from the same observability as other tasks (logs, artifacts, status)

Making it a task means it shows up in `wg list`, can be inspected with `wg show`, and its output is logged. Making it a coordinator-level action would hide it.

### Meta-task structure

```
.evolve-{timestamp}
  title: "Evolve agency (auto, 15 new evaluations)"
  tags: [agency, evolution, eval-scheduled]
  verify: "Evolution applied or no operations proposed"
```

The coordinator creates at most one `.evolve-*` task at a time. If a previous `.evolve-*` is still open/in-progress, no new one is created.

### Execution

The `.evolve-*` task is dispatched to an agent like any other task. The agent runs the equivalent of `wg evolve --strategy all --budget 5`. The `--budget 5` default for auto-evolve keeps changes incremental — large sweeping changes are reserved for manual `wg evolve`.

The evolver agent uses the model configured via `evolver_model` (default: model routing for `DispatchRole::Evolver`). If an `evolver_agent` identity is configured, it's assigned to the task.

### Post-evolution bookkeeping

After the `.evolve-*` task completes, the coordinator:

1. Updates `.workgraph/agency/evolver-state.json` with `last_evolved_at` and `last_evolved_eval_count`
2. Logs a summary of operations applied

This is handled in the coordinator tick loop's completion-processing phase, the same way it handles `.evaluate-*` completion.

### Interaction with the compactor

The compactor (`docs/design/compactor-agent-spec.md`) is a periodic lightweight LLM call that produces a context artifact. The evolver is a periodic heavyweight LLM call that modifies agency primitives. They are independent:

- The compactor runs every ~10 turns, takes seconds, and is non-blocking
- The evolver runs every ~10 evaluations (potentially every ~10 tasks), takes 30-60s, and runs as a task
- The compactor's Evaluation Digest layer feeds information to the coordinator about agent performance, which indirectly influences when the evolver triggers (because the coordinator creates better tasks → better evaluations → evolver sees different signal)
- They don't conflict on any shared resources

The compactor does NOT trigger evolution. Evolution is triggered by evaluation count, not compaction.

### Interaction with coordinator-as-graph-citizen

The coordinator-as-graph-citizen design (`docs/design/coordinator-as-graph-citizen.md`) proposes evaluating coordinator turns and evolving the coordinator prompt. Auto-evolve subsumes the evolution trigger described in that design:

- Coordinator evaluations land in the same `agency/evaluations/` directory
- The evolver sees them alongside worker evaluations
- Coordinator roles and tradeoffs are valid mutation targets
- No separate "coordinator evolution" trigger is needed — auto-evolve handles all roles

The coordinator-as-graph-citizen design's Phase 4 (Prompt Evolution) should reference this auto-evolve design as the trigger mechanism.

---

## 3. Auto-Bootstrapping the Agency

**Decision: Silent auto-init on first `wg service start` when `auto_evolve` is enabled (or when any `auto_*` agency flag is set).**

### The problem

Currently, users must run `wg agency init` before the agency system works. But the ideal experience is:

```bash
wg service start
wg add "Implement feature X"
# ... agents get assigned identities, evaluated, and improved over time
```

### The solution

When the coordinator tick loop runs and detects that agency is not initialized (no `agency/cache/roles/` directory), it auto-initializes:

```rust
// In coordinator_tick(), before any agency-related phases:
if config.agency.auto_assign || config.agency.auto_evaluate || config.agency.auto_evolve {
    if !dir.join("agency/cache/roles").exists() {
        agency_init::run(dir)?;
        eprintln!("[coordinator] Auto-initialized agency system");
    }
}
```

This calls the same `agency_init::run()` that `wg agency init` uses. It's idempotent — running it when already initialized is a no-op.

### Progressive capability activation

The agency system has three levels of automation, each building on the previous:

| Level | Config | What happens | Prerequisite |
|-------|--------|-------------|-------------|
| 0 | All defaults | Agents spawn without identity. No evaluation. No evolution. | None |
| 1 | `auto_assign = true` | Agents get identities assigned by LLM. | Agency initialized |
| 2 | `auto_evaluate = true` | Completed tasks get evaluated. Performance data accumulates. | Level 1 |
| 3 | `auto_evolve = true` | Roles and tradeoffs improve automatically from evaluation data. | Level 2 |

**`wg agency init` enables levels 1-2 automatically** (it sets `auto_assign = true` and `auto_evaluate = true`). Level 3 (`auto_evolve`) defaults to false because evolution modifies agency primitives — a step that warrants explicit opt-in the first time.

### Zero-config path

For users who want everything automatic from the start:

```bash
wg config --auto-evolve true
wg service start
```

Setting `auto_evolve = true` implies `auto_assign` and `auto_evaluate` should also be true. If they aren't, the auto-init flow enables them:

```rust
// In the auto-evolve trigger check:
if config.agency.auto_evolve && !config.agency.auto_evaluate {
    config.agency.auto_evaluate = true;
    config.agency.auto_assign = true;
    config.save(dir)?;
}
```

### The invisible principle in practice

A new user's experience with `auto_evolve = true`:

1. `wg service start` — agency auto-initializes (starter roles, tradeoffs, agents)
2. First task completes — `.assign-*` assigns a "Careful Programmer" identity
3. Task evaluated — `.evaluate-*` scores it
4. After 10 evaluated tasks — `.evolve-*` proposes mutations based on accumulated data
5. Evolved roles/tradeoffs feed into subsequent assignments
6. The user never sees any of this unless they look (`wg agency stats`, `wg role list`)

---

## 4. Safety and Guardrails

### 4.1 Budget limit

Auto-evolve uses `--budget 5` by default (configurable via `auto_evolve_budget`). This keeps each evolution cycle incremental. Manual `wg evolve` has no default budget.

Rationale: An unbounded auto-evolve could retire critical roles or create a flood of new ones. Capping at 5 operations per cycle means the system changes gradually.

### 4.2 Rate limiting

The `auto_evolve_interval_minutes` (default: 120) prevents evolution from running too frequently. Even if evaluations arrive rapidly, evolution won't trigger more than once per interval.

Additionally, the coordinator creates at most one `.evolve-*` task at a time. If a previous one is still running, the trigger is suppressed.

### 4.3 Self-mutation deferral

The existing deferred approval system (`src/commands/evolve/deferred.rs`) is used unchanged for auto-evolve:

- Operations targeting the evolver's own role or tradeoff → deferred to `evolve-review-*` task requiring human approval
- Operations targeting outcomes with `requires_human_oversight` → deferred
- Bizarre ideation on outcomes → deferred

Auto-evolve does NOT bypass any deferral gate. The same safety rules apply whether evolution is manual or automatic.

### 4.4 Rollback detection

After an auto-evolve cycle applies mutations, subsequent evaluations measure the impact. If the rolling average score drops by >0.1 from the pre-evolution baseline over the next 10 evaluations, the coordinator logs a warning:

```
[coordinator] Warning: avg eval score dropped from 0.78 to 0.65 after evolve-run-20260305-1430.
  Consider: wg evolve --strategy retirement or manual review.
```

**No automatic rollback.** Automatic rollback is dangerous because:
- Score drops could be caused by harder tasks, not worse roles
- Reverting mutations destroys lineage data
- The retirement strategy in the next evolution cycle will naturally address underperformers

Instead, the evolver state file records the pre-evolution baseline score, and the next evolution cycle receives this context in its prompt: "Scores dropped after the last evolution — prioritize retirement of underperforming entities."

### 4.5 Strategy selection

Auto-evolve uses a lighter strategy mix than manual evolve:

- **Default auto strategy**: `mutation` + `gap-analysis` + `retirement` + `motivation-tuning`
- **Excluded from auto**: `crossover`, `bizarre-ideation`, `randomisation`, `component-mutation`

The excluded strategies are more experimental and higher-variance. They're available via manual `wg evolve --strategy all` or `wg evolve --strategy bizarre-ideation` for users who want them.

Configurable via `auto_evolve_strategy` (default: `"auto"`, which maps to the safe subset above). Set to `"all"` to include experimental strategies.

### 4.6 Minimum evaluation quality gate

Auto-evolve only triggers when at least `auto_evolve_min_evals_per_role` (default: 3) evaluations exist for at least one role. This prevents evolution from running on insufficient data (e.g., a single evaluation with a fluke score).

### 4.7 Evolver state file

`.workgraph/agency/evolver-state.json`:

```json
{
  "last_evolved_at": "2026-03-05T14:30:00Z",
  "last_evolved_eval_count": 45,
  "last_evolved_avg_score": 0.78,
  "evolution_history": [
    {
      "run_id": "run-20260305-143022",
      "timestamp": "2026-03-05T14:30:22Z",
      "operations_applied": 3,
      "pre_avg_score": 0.75,
      "strategy": "auto"
    }
  ]
}
```

This file is read/written by the coordinator. The `evolution_history` is capped at the last 20 entries to prevent unbounded growth.

---

## 5. Configuration Surface

### New fields in `AgencyConfig`

```rust
/// Enable automatic evolution when evaluation data accumulates
#[serde(default)]
pub auto_evolve: bool,

/// Minimum new evaluations before triggering auto-evolve (default: 10)
#[serde(default = "default_auto_evolve_threshold")]
pub auto_evolve_threshold: u32,

/// Minimum minutes between auto-evolve runs (default: 120)
#[serde(default = "default_auto_evolve_interval_minutes")]
pub auto_evolve_interval_minutes: u32,

/// Maximum operations per auto-evolve cycle (default: 5)
#[serde(default = "default_auto_evolve_budget")]
pub auto_evolve_budget: u32,

/// Strategy for auto-evolve: "auto" (safe subset) or "all" (default: "auto")
#[serde(default = "default_auto_evolve_strategy")]
pub auto_evolve_strategy: String,

/// Score drop threshold that triggers reactive evolution (default: 0.4)
#[serde(default = "default_auto_evolve_reactive_threshold")]
pub auto_evolve_reactive_threshold: f64,
```

### CLI configuration

```bash
wg config --auto-evolve true                      # enable
wg config --auto-evolve-threshold 15              # trigger after 15 evals
wg config --auto-evolve-interval-minutes 60       # minimum 1 hour between runs
wg config --auto-evolve-budget 3                  # max 3 operations per cycle
wg config --auto-evolve-strategy all              # include experimental strategies
wg config --auto-evolve-reactive-threshold 0.3    # reactive trigger at 0.3
```

### Default configuration after `wg agency init`

`wg agency init` does NOT enable `auto_evolve` by default. It enables `auto_assign` and `auto_evaluate`, which are prerequisites. Users opt into auto-evolve explicitly:

```bash
wg agency init
wg config --auto-evolve true
```

Or the zero-config path:

```bash
wg config --auto-evolve true   # implies auto_assign + auto_evaluate
wg service start               # auto-inits agency if needed
```

---

## 6. Implementation Plan

### Phase 1: Trigger and meta-task creation

**Files to modify:**
- `src/config.rs` — Add `auto_evolve`, `auto_evolve_threshold`, `auto_evolve_interval_minutes`, `auto_evolve_budget`, `auto_evolve_strategy`, `auto_evolve_reactive_threshold` to `AgencyConfig` with defaults
- `src/commands/service/coordinator.rs` — Add `should_trigger_evolution()` and `build_auto_evolve_task()` functions, call them in the tick loop after the auto-evaluate phase
- `src/cli.rs` — Add `--auto-evolve`, `--auto-evolve-threshold`, etc. to `wg config`

**New file:**
- None. The evolver state file (`.workgraph/agency/evolver-state.json`) is written at runtime.

**Validation:**
- Unit tests for `should_trigger_evolution()` with various state combinations
- Integration test: create a workgraph with 10+ evaluations, verify `.evolve-*` task gets created
- Integration test: verify rate limiting (no second `.evolve-*` within interval)

### Phase 2: Auto-bootstrap

**Files to modify:**
- `src/commands/service/coordinator.rs` — Add auto-init check at start of tick loop
- `src/commands/agency_init.rs` — No changes needed (already idempotent)

**Validation:**
- Integration test: `auto_evolve = true` with no agency dir → agency gets initialized
- Integration test: `auto_evolve = true` implies `auto_assign` + `auto_evaluate`

### Phase 3: Evolver state tracking and reactive trigger

**Files to modify:**
- `src/commands/service/coordinator.rs` — After `.evolve-*` completes, update evolver state file; add reactive trigger logic
- `src/commands/evolve/mod.rs` — Accept `--auto` flag that applies the safe strategy subset and budget defaults

**Validation:**
- Integration test: evolver state file is created and updated correctly
- Integration test: reactive trigger fires when scores drop below threshold
- Integration test: score-drop warning is logged when post-evolution scores decrease

### Phase 4: Post-evolution context injection

**Files to modify:**
- `src/commands/evolve/prompt.rs` — Include evolution history summary in evolver prompt ("last run applied N operations, scores went from X to Y")

**Validation:**
- Verify the evolver prompt includes history context
- Qualitative: does the evolver make better decisions with evolution history?

---

## 7. Interaction with Other Designs

### Compactor agent (`docs/design/compactor-agent-spec.md`)

The compactor and auto-evolver are both periodic maintenance processes triggered by the coordinator, but they don't interact directly:

| | Compactor | Auto-Evolver |
|---|---|---|
| **Trigger** | Turn count or ops growth | Evaluation count |
| **Execution** | Lightweight LLM call (haiku) | Meta-task (opus) |
| **Output** | `compactor/context.md` | Modified role/tradeoff YAML files |
| **Blocking** | No (background thread) | No (runs as a task) |
| **Frequency** | Every ~10 turns | Every ~10 evaluations |

They share the pattern of "periodic coordinator maintenance" and could share infrastructure (the `should_compact` / `should_evolve` checks are structurally similar), but they're independent processes.

### Coordinator-as-graph-citizen (`docs/design/coordinator-as-graph-citizen.md`)

That design proposes coordinator turn evaluation and prompt evolution. Auto-evolve provides the trigger mechanism for coordinator prompt evolution:

- Coordinator evaluations flow into `agency/evaluations/` alongside worker evaluations
- Auto-evolve sees all evaluations and evolves all roles (including coordinator roles)
- The coordinator-as-graph-citizen design's Phase 4 (Prompt Evolution, Section 2.4) should use auto-evolve as its trigger rather than defining a separate evolution trigger

The key insight from that design that applies here: evaluation frequency for coordinators should be configurable separately from auto-evolve threshold. The coordinator might be evaluated every 5 turns, generating evaluations faster than workers. The auto-evolve threshold should count all evaluations uniformly.

### Auto-create (`auto_create` in AgencyConfig)

`auto_create` triggers the agent creator to expand the primitive store (new components, outcomes, tradeoffs). Auto-evolve modifies existing primitives. They complement each other:

- `auto_create` expands the search space (more building blocks)
- `auto_evolve` optimizes within the search space (better configurations)

They should not run simultaneously. If both are enabled, the coordinator should alternate: create → evaluate new primitives → evolve. The simplest implementation: `auto_create` and `auto_evolve` never trigger in the same tick. `auto_create` takes priority when its threshold is met, because new primitives need evaluation before they can be evolved.

---

## 8. The Invisible-by-Design Principle

This design follows the invisible-by-design principle as its north star. Every decision above serves one goal: a user who runs `wg service start` and adds tasks should get improving agent performance over time without knowing the agency system exists.

### What the user sees (if they look)

```bash
$ wg list
...
.evolve-20260305-1430  done   "Evolve agency (auto, 12 new evaluations)"
...

$ wg agency stats
Role Leaderboard:
  Programmer-v3  0.87  (gen 3, evolved from Programmer-v2)
  Architect      0.82
  Reviewer-v2    0.79  (gen 2, evolved from Reviewer)
```

### What the user doesn't see (unless they ask)

- Agency auto-initialized on first service start
- Identities assigned to every task
- Every completed task evaluated
- Every 10 evaluations, roles and tradeoffs evolved
- Underperformers retired, new variants created from high performers
- The evolver's own identity subject to the same governance (with human deferral)

### The escape hatches

| User want | How |
|-----------|-----|
| Disable auto-evolve | `wg config --auto-evolve false` |
| Run evolution manually | `wg evolve` (works regardless of auto-evolve setting) |
| Review pending mutations | `wg evolve deferred-list` |
| See what changed | `wg role lineage <id>`, `wg agency stats` |
| Override strategy | `wg config --auto-evolve-strategy all` |
| Increase/decrease frequency | `--auto-evolve-threshold`, `--auto-evolve-interval-minutes` |

The system defaults to doing the right thing. Manual intervention is always available but never required.
