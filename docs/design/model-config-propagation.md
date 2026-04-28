# Design: Model/Config Propagation from Seed Tasks

**Task:** design-model-config  
**Date:** 2026-04-13  
**Status:** Proposed  
**Depends on:** [research-model-propagation](../research/model-propagation-subgraphs.md)

## Problem Statement

When a user creates a task tree where different branches should use different models or endpoints, every subtask must currently have `--model` set explicitly. There is no inheritance. An agent spawned with `WG_MODEL=qwen3-local` that fans out subtasks via `wg add --after parent` creates children that fall back to the coordinator's default model — not the parent's model.

**Use case:** Mixed deployments — one task tree runs on a cheap local model, another on Opus — without the user or agents manually specifying `--model` on every `wg add`.

## Current Model Selection Flow

The model for a spawned agent is resolved in `spawn/execution.rs:1340` via a tiered hierarchy:

```
task.model > agent.preferred_model > executor.model > role_model (config) > coordinator.model
```

Each tier is checked in order; the first non-`None` value wins. The resolved model is:
1. Passed to the executor as a CLI flag (`--model`)
2. Set as `WG_MODEL` env var on the spawned agent process

**The gap:** When an agent creates subtasks via `wg add`, neither `WG_MODEL` nor the parent task's model is consulted. The child's `model` field is `None` unless explicitly set with `--model`. The child falls through to the coordinator default at spawn time.

Fields on `Task` that could participate in inheritance (`src/graph.rs:296-312`):
- `model: Option<String>` — preferred model
- `provider: Option<String>` — provider override
- `endpoint: Option<String>` — named endpoint
- `agent: Option<String>` — agency agent hash
- `context_scope: Option<String>` — context scope override

## Design Alternatives

### Alternative A: Env-Based Propagation (Recommended)

**Mechanism:** `wg add` reads `WG_MODEL` (and optionally `WG_ENDPOINT`, `WG_LLM_PROVIDER`) from the environment when no explicit `--model`/`--endpoint` is given. Since spawned agents already have these env vars set, subtasks automatically inherit the parent's effective configuration.

**Data model changes:** None. The `model` field on the new task is set from the env var, which makes it look exactly like an explicitly-set model. No new fields or schema changes.

**Resolution algorithm:**
```
if --model was passed on CLI:
    task.model = resolve_model_input(cli_model)
elif WG_MODEL env var is set:
    task.model = WG_MODEL
else:
    task.model = None  (falls through to coordinator default at spawn time)
```

Same pattern for `--endpoint`/`WG_ENDPOINT` and `--provider`/`WG_LLM_PROVIDER`.

**Code change** (`src/commands/add.rs`, after line 252):
```rust
let resolved_model_str: Option<String> = if let Some(m) = model {
    Some(resolve_model_input(m, dir)?)
} else if let Ok(env_model) = std::env::var("WG_MODEL") {
    // Inherit model from parent agent's environment
    Some(env_model)
} else {
    None
};
```

**Pros:**
- ~5 lines of code per field
- Works immediately — agents already have `WG_MODEL` set
- Zero schema changes, zero migration
- Explicit `--model` always overrides (natural opt-out)
- System tasks (`.assign-*`, `.flip-*`) are unaffected since they're created by the coordinator, not agents
- The inherited model is **visible on the task** (stored in `task.model`), so `wg show` displays it

**Cons:**
- Only works in agent context (human `wg add` from terminal won't inherit unless they set env vars manually)
- Inheritance is implicit — no field distinguishes "explicitly set" from "inherited from parent env"
- Propagation is "viral" — once a model enters the env, all descendants inherit unless overridden

### Alternative B: Graph-Based Ancestor Walk

**Mechanism:** When `wg add --after parent-id` is used without `--model`, look up the `--after` parents in the graph and copy the first parent's model.

**Data model changes:** None (same as Alt A — sets `task.model` directly).

**Resolution algorithm:**
```
if --model was passed on CLI:
    task.model = resolve_model_input(cli_model)
elif any --after parent has a model:
    task.model = first parent's model  (or: nearest ancestor with model set)
else:
    task.model = None
```

**Code change** (`src/commands/add.rs`, inside `modify_graph` closure):
```rust
let inherited_model = if model.is_none() {
    effective_after.iter()
        .filter_map(|parent_id| graph.get_task(parent_id))
        .find_map(|parent| parent.model.clone())
} else {
    None
};
```

**Pros:**
- Works in both agent and human contexts
- Deterministic — always resolved from graph state, not runtime environment
- Visible in graph data

**Cons:**
- **Diamond ambiguity:** multiple parents with different models — which wins? First-listed is arbitrary. Would need a tiebreaking rule or error.
- **Surprise behavior change:** all users who use `--after` get automatic model inheritance, which may not be wanted. A subtask intended to use the default model would silently inherit the parent's.
- Requires `--no-inherit-model` or `--model default` escape hatch
- More invasive — changes `wg add` semantics for everyone

### Alternative C: Explicit `TaskConfig` with Inheritance Tracking

**Mechanism:** Add a new `inherited_config` field to `Task` that records which config fields were inherited vs. explicitly set. Introduce a `--inherit-config` flag on `wg add`.

**Data model changes:**
```rust
/// Configuration inherited from seed task (via --inherit-config or auto-propagation)
#[serde(default, skip_serializing_if = "Option::is_none")]
pub inherited_config: Option<InheritedConfig>,

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct InheritedConfig {
    pub source_task: String,       // ID of the seed task
    pub model: Option<String>,
    pub endpoint: Option<String>,
    pub provider: Option<String>,
    pub context_scope: Option<String>,
}
```

**Resolution at spawn time:** Modify `resolve_model_and_provider` to include an `inherited_model` tier between agent and executor:
```
task.model > inherited_config.model > agent.preferred_model > executor.model > role_model > coordinator.model
```

**CLI:**
```bash
# Explicit: inherit config from a specific seed task
wg add "Subtask" --after seed-task --inherit-config seed-task

# Or: `--inherit-config auto` walks the --after chain
wg add "Subtask" --after parent --inherit-config auto
```

**Pros:**
- Full provenance tracking — you can see where config came from
- Explicit opt-in — no surprise behavior changes
- Distinguishes "inherited" from "explicitly set" in the data model
- Clean override semantics: explicit `--model` beats inherited config

**Cons:**
- Schema change with migration concerns
- More CLI surface area
- Adds complexity to the already 5-tier model resolution
- Agents must be taught to pass `--inherit-config` in their task descriptions

### Alternative D: Coordinator Spawn-Time Inheritance

**Mechanism:** Instead of setting the model at task creation time, the coordinator resolves it at spawn time by walking the dependency graph.

**Data model changes:** None.

**Resolution algorithm** (in `spawn_agents_for_ready_tasks`):
```
if task.model is set:
    use task.model
elif any ancestor task has model set:
    use nearest ancestor's model  (BFS up the --after chain)
else:
    fall through to existing cascade
```

**Pros:**
- No changes to `wg add` or task creation
- Works for all tasks regardless of how they were created
- Late resolution means the graph state at spawn time is authoritative

**Cons:**
- Model isn't visible on the task until after spawning — `wg show` won't show the effective model beforehand
- Ancestor walk could be expensive for deep graphs (though graphs are small in practice)
- Adds complexity to an already-complex spawn path
- Doesn't compose well: inserting an "ancestor walk" tier into the 5-tier resolution creates ordering ambiguity (is ancestor model higher or lower priority than agent.preferred_model?)

## Comparison Matrix

| Criterion | A: Env-Based | B: Graph Walk | C: InheritedConfig | D: Spawn-Time |
|-----------|:---:|:---:|:---:|:---:|
| Code complexity | ~10 LOC | ~25 LOC | ~150 LOC | ~40 LOC |
| Schema changes | None | None | New field + migration | None |
| Works for humans | No (env-only) | Yes | Yes (explicit flag) | Yes |
| Works for agents | Yes (automatic) | Yes (automatic) | Needs prompting | Yes (automatic) |
| Model visible pre-spawn | Yes | Yes | Yes (separate field) | No |
| Diamond merge handling | N/A (env is single) | Ambiguous | Explicit source | Ambiguous |
| Opt-out mechanism | `--model X` | Needs `--no-inherit` | Don't pass flag | Needs `--no-inherit` |
| Backward compat | Full | Breaking (behavior) | Full | Full |
| Provenance tracking | None | None | Full | None |

## Recommended Approach: A + C Hybrid (Phased)

### Phase 1: Env-Based Propagation (ship first)

Implement Alternative A. This is the highest-value, lowest-risk change:

1. **~10 lines of code** in `src/commands/add.rs`
2. Covers the primary use case (agent-created subtask trees)
3. Zero schema changes, zero migration, zero breaking behavior
4. Natural opt-out: `--model X` overrides the env var

**What to propagate:**
- `WG_MODEL` → `task.model` (most important)
- `WG_ENDPOINT` → `task.endpoint` (for custom endpoint routing)

**What NOT to propagate automatically:**
- `WG_LLM_PROVIDER` — already embedded in model strings via `provider:model` format; separate propagation would be redundant and confusing
- Agency agent hash — agent assignment is handled by the `.assign-*` pipeline and should not be inherited
- `context_scope` — this is a per-task concern, not a tree-wide default

**Opt-out:** Agents can suppress inheritance with `--model default` (a new sentinel that explicitly sets `task.model = None`). Alternatively, the prompt/template for task descriptions could instruct agents not to inherit when a different model is desired.

**Track provenance (lightweight):** Add a log entry when env-based inheritance occurs:
```rust
if inherited_from_env {
    log_entries.push(LogEntry::now(
        format!("Model inherited from parent environment: {}", env_model)
    ));
}
```
This gives `wg show` visibility into the inheritance chain without schema changes.

### Phase 2: Explicit `--inherit-config` (future, if needed)

If the env-based approach proves insufficient (e.g., humans need inheritance, or provenance tracking becomes critical), add Alternative C as an explicit opt-in mechanism:

```bash
# Human creates a tree root with specific config
wg add "Local model experiments" --model qwen3-local --id local-seed

# Subsequent tasks explicitly inherit
wg add "Sub-experiment A" --after local-seed --inherit-config local-seed
wg add "Sub-experiment B" --after local-seed --inherit-config local-seed
```

This phase adds the `InheritedConfig` field and resolution tier. It's additive and non-breaking.

### Not Recommended

- **Alternative B (graph walk in `wg add`)**: Behavior change that surprises users. Diamond ambiguity is a footgun. The benefit over A is marginal (only helps humans, who can set env vars or wait for Phase 2).
- **Alternative D (spawn-time walk)**: Late resolution means the model isn't visible before spawning, making debugging and `wg show` less useful. Also adds complexity to the spawn path which is already the most complex code path.

## CLI UX Examples

### Phase 1 (Env-Based) — The Happy Path

```bash
# User creates a seed task with a specific model
$ wg add "Benchmark qwen3 on our test suite" --model openrouter:qwen/qwen3-235b
Created: benchmark-qwen3-on-our-test-suite

# Coordinator spawns an agent with WG_MODEL=openrouter:qwen/qwen3-235b
# Agent creates subtasks — they auto-inherit qwen3

# Inside the agent:
$ wg add "Run unit tests" --after benchmark-qwen3-on-our-test-suite
# → task.model = "openrouter:qwen/qwen3-235b" (from WG_MODEL env)

$ wg add "Run integration tests" --after benchmark-qwen3-on-our-test-suite
# → task.model = "openrouter:qwen/qwen3-235b" (from WG_MODEL env)

# Agent wants one subtask on a different model:
$ wg add "Analyze results with Opus" --after run-unit-tests,run-integration-tests \
    --model opus
# → task.model = "opus" (explicit override wins)
```

### Mixed Deployment Scenario

```bash
# User sets up two branches with different models
$ wg add "Feature A: fast iteration" --model haiku
$ wg add "Feature B: deep analysis" --model opus

# Each branch's subtasks inherit their seed model automatically
# Feature A's subtree runs on haiku, Feature B's on opus
# No manual --model needed on any subtask

# Verify with:
$ wg list --json | jq '.[] | {id, model}'
```

### Overriding Inherited Model

```bash
# Inside an agent running with WG_MODEL=haiku:
$ wg add "Complex sub-problem" --after my-parent --model sonnet
# Explicit --model overrides the env var

# To explicitly use coordinator default (suppress inheritance):
$ wg add "Use default model" --after my-parent --model default
# Sentinel value: sets task.model = None, falls through to coordinator
```

## Edge Cases

### 1. Diamond Merge — Parents with Different Models

```
seed-haiku ──► task-a (haiku) ──┐
                                 ├──► merge-task (???)
seed-opus  ──► task-b (opus)  ──┘
```

**In Phase 1 (env-based):** Not a problem. The agent creating `merge-task` runs with a single `WG_MODEL` env var (from its own parent). If the merge task is created by the user, no env var is set and it falls through to coordinator default.

**In Phase 2 (explicit):** `--inherit-config` takes a specific source task ID, so the user explicitly chooses which branch to inherit from. No ambiguity.

### 2. System Tasks (`.assign-*`, `.flip-*`, `.compact-*`)

System tasks are created by the coordinator process, not by agents. The coordinator does NOT have `WG_MODEL` set to a task-specific value (it uses its own coordinator model). System tasks will correctly continue using their role-based model resolution (`DispatchRole::Assigner`, `DispatchRole::Verification`, etc.).

**No special handling needed.** The existing system task model resolution is orthogonal to this design.

### 3. Cycles (Loop Tasks)

When a cycle resets (all members → `open`, increment `loop_iteration`), the `model` field on each member is preserved. Env-based inheritance set the model at creation time, so re-execution uses the same model.

If a cycle member's model should change between iterations (e.g., escalation), `wg edit --model` or the existing `try_escalate_model` triage path handles this. No conflict with inheritance.

### 4. Human-Created Tasks

Phase 1 (env-based) doesn't help humans creating tasks from a normal terminal. Workarounds:
- `WG_MODEL=haiku wg add "Task" --after parent` (explicit env var)
- `wg add "Task" --after parent --model haiku` (explicit flag)
- Phase 2 adds `--inherit-config` for this case

This is acceptable because the primary pain point is agent-created subtask trees (hundreds of tasks), not human-created tasks (tens of tasks where explicit `--model` is manageable).

### 5. Model Escalation Interaction

The triage system (`src/commands/service/triage.rs`) escalates models on task failure (e.g., haiku → sonnet → opus). An inherited model is stored in `task.model` and is treated identically to an explicitly-set model. Escalation overwrites `task.model` with the next tier and records the old model in `tried_models`. No conflict.

### 6. `--model default` Sentinel

To suppress env-based inheritance, a `default` sentinel value tells `wg add` to set `task.model = None`:

```rust
let resolved_model_str: Option<String> = if let Some(m) = model {
    if m == "default" {
        None  // Explicit opt-out of inheritance
    } else {
        Some(resolve_model_input(m, dir)?)
    }
} else if let Ok(env_model) = std::env::var("WG_MODEL") {
    Some(env_model)
} else {
    None
};
```

### 7. Endpoint Propagation and API Keys

When `WG_ENDPOINT` is propagated to `task.endpoint`, the endpoint config (including API keys) is resolved at spawn time from `config.toml`'s `[llm_endpoints]` section. The key is NOT stored on the task — only the endpoint name. This is safe: the endpoint name is a pointer, not a credential.

## Migration / Backward Compatibility

### Phase 1
- **No schema changes.** The `Task` struct is unchanged.
- **No behavioral changes for explicit `--model` users.** The env var is only consulted when `--model` is not passed.
- **Behavioral change for agents:** Subtasks created by agents will now inherit the parent's model instead of being `None`. This is the desired behavior and the entire point of the feature. However, it could change the effective model for subtasks that previously fell through to coordinator default.
- **Rollback:** Remove the env var check from `add.rs`. Tasks already created with inherited models retain their `task.model` value but this is harmless (they'd have gotten that model at spawn time anyway).
- **Config flag to disable:** Add `inherit_model_from_env = true` to `[coordinator]` config. Set to `false` to restore old behavior. Default: `true`.

### Phase 2
- **Schema change:** New `inherited_config` field on `Task`. Old tasks without the field deserialize to `None` via `#[serde(default)]`. Forward-compatible.
- **New CLI flag:** `--inherit-config`. Purely additive, no breaking changes.
- **Resolution tier insertion:** The new `inherited_config` tier slots between `task.model` and `agent.preferred_model`. Existing behavior for tasks without `inherited_config` is unchanged.

## Implementation Plan

### Phase 1 Checklist
1. `src/commands/add.rs`: Read `WG_MODEL` when `--model` not provided
2. `src/commands/add.rs`: Read `WG_ENDPOINT` when `--endpoint` not provided  
3. `src/commands/add.rs`: Handle `--model default` sentinel
4. `src/commands/add.rs`: Add log entry for env-based inheritance
5. `src/config.rs`: Add `inherit_model_from_env` config option (default `true`)
6. Tests: Unit test for env-based model inheritance in `add.rs`
7. Tests: Integration test showing subtask model propagation through agent chain
8. Docs: Update `wg add --help` to mention env-based inheritance
9. Docs: Update agent guide to explain model propagation behavior

### Phase 2 Checklist (Deferred)
1. `src/graph.rs`: Add `InheritedConfig` struct and field
2. `src/commands/add.rs`: Implement `--inherit-config` flag
3. `src/commands/spawn/execution.rs`: Add inherited config tier to resolution
4. `src/commands/show.rs`: Display inherited config provenance
5. Tests and docs
