# Compactor Agent Implementation Spec

## Overview

The compactor is a lightweight, periodic LLM call that distills the workgraph's accumulated data into a structured context artifact (`compactor/context.md`). The coordinator reads this artifact to gain historical awareness without depending on any runtime's internal context management.

This spec covers: the compactor module, the context injection pipeline changes, the process architecture, evaluation integration, and a phased migration plan.

---

## 1. Compactor Agent Spec

### 1.1 Input: Data Sources

The compactor reads these data sources from `.workgraph/`:

| Source | Path | What it provides | Read strategy |
|---|---|---|---|
| Chat inbox | `chat/inbox.jsonl` | User messages since last compaction | Tail from `last_compacted_inbox_offset` |
| Chat outbox | `chat/outbox.jsonl` | Coordinator responses (summary + full_response) | Tail from `last_compacted_outbox_offset` |
| Operations log | `log/operations.jsonl` | Graph mutations (add/done/fail/edit/etc.) | Tail from `last_compacted_ops_offset` |
| Task graph | `graph.jsonl` | Current task state, deps, status counts | Full load (via `load_graph()`) |
| Evaluations | `agency/evaluations/*.json` | LLM-scored task evaluations (7 dimensions) | Files modified since last compaction |
| Previous context | `compactor/context.md` | Self-referential — the compactor's own prior output | Full read |

**Offset tracking**: The compactor metadata (YAML frontmatter in `context.md`) stores byte offsets into inbox/outbox/operations JSONL files. On the next run, it reads only new entries. This keeps the compactor input bounded regardless of total history size.

**Budget**: The compactor prompt should stay under 20K tokens. If new operations exceed 5K tokens, they are pre-summarized (counts by op type + notable events like failures) before being sent to the LLM.

### 1.2 Output: Artifact Format

**File**: `.workgraph/compactor/context.md`

```yaml
---
version: 1
last_compacted: "2026-03-05T14:30:00Z"
turns_covered: 52
compactor_model: "haiku"
token_estimate: 3000
inbox_offset: 159482
outbox_offset: 764321
ops_offset: 892145
---
```

Followed by three layers:

**Layer 1: Rolling Narrative** (~2000 tokens, updated every run)
- Active workstreams and their progress
- Recent decisions the coordinator made and why
- Observed patterns (failure modes, parallelism conflicts, timing)
- Open questions / pending user decisions

**Layer 2: Persistent Facts** (~500 tokens, updated only when new info contradicts)
- Project type, build/test commands
- User preferences (decomposition style, verbosity, etc.)
- Recurring issues (OOM patterns, file conflicts)

**Layer 3: Evaluation Digest** (~500 tokens, updated when new evaluations arrive)
- Aggregate scores, trends, weak dimensions
- Actionable recommendations for the coordinator

**Total budget**: ~3000 tokens. Configurable via `wg config --compactor-budget <tokens>`.

### 1.3 Trigger: When and How Often

The compactor runs as a **periodic lightweight LLM call** triggered by the service daemon, not blocking the coordinator response loop.

**Trigger conditions** (any of):
1. Every N coordinator turns (configurable, default 10)
2. Operations log has grown by >100 entries since last compaction
3. Coordinator restart (immediate compaction before injecting recovery context)
4. Manual: `wg compact` CLI command

**Tracking**: The daemon tracks `coordinator_turn_count` (already exists in coordinator.rs for checkpoint logic). After each coordinator response, the daemon checks if compaction is due.

### 1.4 Execution Model

The compactor uses `run_lightweight_llm_call()` (existing in `src/service/llm.rs`) — the same dispatch path used by triage, evaluation, and other lightweight calls. Not a full agent spawn.

```rust
run_lightweight_llm_call(
    config,
    DispatchRole::Compactor,  // new role
    &compactor_prompt,
    60,  // timeout_secs (generous; haiku typically responds in 5-15s)
)
```

**Async execution**: The daemon spawns the compactor call on a background thread. The coordinator does not wait for it. If the compactor is still running when the next trigger fires, the trigger is skipped (debounce).

---

## 2. Context Injection Pipeline

### 2.1 Current Architecture (what changes)

Current flow per coordinator turn (`coordinator_agent.rs:460-560`):

```
User message arrives
  → build_coordinator_context(dir, last_interaction, event_log)  // ~500-2000 tokens
  → prepend context to user message
  → write to coordinator stdin (stream-json)
```

Current crash recovery (`coordinator_agent.rs:675-728`):

```
Coordinator process restarts
  → build_crash_recovery_summary(dir)  // last 10 msgs truncated + graph state
  → inject as first user message
```

### 2.2 New Architecture

#### Per-turn injection (Phase 1+)

```
User message arrives
  → read compactor/context.md (if exists)          // ~3000 tokens (new)
  → build_coordinator_context(dir, ...)             // ~500-2000 tokens (unchanged)
  → combine: [compactor_context, graph_context, user_message]
  → write to coordinator stdin
```

The compactor context is **prepended** to the existing per-turn context, separated by a header:

```markdown
## Compactor Context (last updated: 2026-03-05T14:30:00Z, covers turns 1-52)

<contents of context.md layers>

## System Context Update (2026-03-05T14:35:00Z)

<existing build_coordinator_context output>
```

#### Crash recovery (Phase 1: replaces build_crash_recovery_summary)

```
Coordinator process restarts
  → trigger immediate compaction (blocking, max 30s timeout)
  → read compactor/context.md
  → build_coordinator_context(dir, ...)
  → inject as first user message: [compactor_context, graph_context, "You were restarted..."]
```

This replaces the current approach of reading last 10 messages truncated to 500 chars. The compactor produces a much richer recovery context because it has access to the full history.

### 2.3 Code Changes Required

#### `src/commands/service/coordinator_agent.rs`

| Function | Change | Lines |
|---|---|---|
| `build_crash_recovery_summary()` | Replace body: read `compactor/context.md` + `build_coordinator_context()`. If `context.md` doesn't exist yet, fall back to current behavior. | 675-728 |
| `agent_thread_main()` | Before crash recovery injection, trigger a blocking compaction call if `context.md` is stale (>30 min or >50 ops old). | ~420-430 |
| Context injection in message handling | After `build_coordinator_context()`, also read `compactor/context.md` and prepend it. | ~530-550 |

#### `src/config.rs`

| Change | Lines |
|---|---|
| Add `DispatchRole::Compactor` variant | 385-406 |
| Add `compactor: Option<RoleModelConfig>` field to `ModelRoutingConfig` | 478-510 |
| Add match arms in `get_role`, `get_role_mut`, `set_model`, `set_provider`, `Display`, `FromStr` | 514-570 |
| Add `DispatchRole::Compactor` to `ALL` array | 451-461 |
| Default model for Compactor: `"haiku"` (same as Triage) | 633 area |

#### `src/config.rs` — new compactor config fields

```rust
// Add to Config or ServiceConfig:
pub struct CompactorConfig {
    /// Maximum tokens for the compactor output (default: 3000)
    pub budget_tokens: Option<u32>,
    /// Coordinator turns between compactions (default: 10)
    pub turn_interval: Option<u32>,
    /// Operations log growth trigger (default: 100 entries)
    pub ops_interval: Option<u32>,
    /// Enable/disable compactor (default: true once configured)
    pub enabled: Option<bool>,
}
```

#### New module: `src/service/compactor.rs`

Core functions:

```rust
/// Build the compactor prompt from data sources.
/// Reads previous context.md, new chat messages, ops delta, evaluations, graph state.
pub fn build_compactor_prompt(dir: &Path) -> Result<String>

/// Run the compactor: build prompt, call LLM, write context.md.
/// Returns Ok(()) on success, Err on failure (non-fatal to the daemon).
pub fn run_compaction(config: &Config, dir: &Path) -> Result<()>

/// Read the current compactor context, if it exists.
/// Returns None if context.md doesn't exist or is unparseable.
pub fn read_compactor_context(dir: &Path) -> Option<String>

/// Check if compaction is needed based on turn count and ops growth.
pub fn should_compact(dir: &Path, turns_since_last: u32, config: &CompactorConfig) -> bool

/// Parse the YAML frontmatter from context.md to get metadata.
fn parse_context_metadata(content: &str) -> Option<CompactorMetadata>
```

#### `src/commands/service/coordinator.rs` (daemon loop)

After each coordinator turn completes, check if compaction is due:

```rust
// In the daemon's post-turn handling (after agent response is written to outbox):
coordinator_turn_count += 1;
if compactor::should_compact(dir, coordinator_turn_count - last_compaction_turn, &config.compactor) {
    // Spawn on background thread (non-blocking)
    let config = config.clone();
    let dir = dir.to_path_buf();
    thread::spawn(move || {
        if let Err(e) = compactor::run_compaction(&config, &dir) {
            eprintln!("[compactor] Failed: {}", e);
        }
    });
    last_compaction_turn = coordinator_turn_count;
}
```

#### New CLI command: `wg compact`

Manual trigger for the compactor. Useful for testing and for bootstrapping context on an existing project.

```
wg compact [--force]  # Run compaction now, regardless of interval
```

Implemented in `src/commands/compact.rs`, calls `compactor::run_compaction()`.

### 2.4 Size Budgeting

Per-turn context window usage:

| Component | Tokens | Source |
|---|---|---|
| System prompt | ~4000 | Static, `build_system_prompt()` |
| Compactor context | ~3000 | `compactor/context.md` (configurable) |
| Per-turn graph state | ~500-2000 | `build_coordinator_context()` |
| User message | ~100-500 | Variable |
| Response budget | ~2000-4000 | Model output |
| **Total** | **~10K-14K** | |

Fits comfortably in 16K context windows. 200K+ windows of Opus/Sonnet are not required for the coordinator.

---

## 3. Process Architecture: Fresh-Per-Turn vs Long-Lived

### 3.1 Decision: Keep Long-Lived, Add Compactor Alongside

**Phase 1-2**: Keep the current long-lived `claude --print --input-format stream-json` process. The compactor runs alongside it as a periodic lightweight call. This is the safe path — additive, no breaking changes.

**Phase 3 (future)**: Replace the long-lived process with per-turn invocations once the compactor has been validated. Each coordinator turn becomes a single `run_lightweight_llm_call()` with tool-use support.

### 3.2 Rationale

The long-lived process has advantages during Phase 1-2:
- Lower latency (no cold start per turn)
- Claude CLI handles tool execution natively (wg commands)
- The compactor context supplements the in-session memory, doesn't replace it

The per-turn model requires solving multi-turn tool use (the coordinator needs to run `wg add`, see the result, then respond). This is solvable but adds complexity. Deferring it to Phase 3 keeps the initial implementation simple.

### 3.3 Compactor Process Model

The compactor itself is always fresh-per-invocation — it's a single `run_lightweight_llm_call()` with no tool use, no multi-turn. Input goes in, context.md comes out. This is identical to how triage and evaluation work.

The compactor runs in the daemon's process (on a background thread), not as a separate binary or agent. It reads files from disk and calls the LLM — no workgraph task required.

---

## 4. Evaluation Integration

### 4.1 Compactor Reads Evaluations

The compactor reads evaluation files from `.workgraph/agency/evaluations/*.json`, filtered to those modified since `last_compacted`. Each evaluation has 7 scored dimensions (correctness, completeness, style_adherence, coordination_overhead, etc.).

The compactor distills these into the **Evaluation Digest** layer:
- Aggregate scores (mean, trend direction)
- Weak dimensions with concrete examples
- Correlation insights (e.g., "short descriptions correlate with low completeness scores")
- Recommendations the coordinator should follow

### 4.2 Compactor Output Gets Evaluated

The compactor's output quality can be measured by:

1. **Staleness check**: If `context.md` is >30 minutes old and >50 operations have occurred, the daemon logs a warning. Track how often this happens.

2. **Context utilization**: After the coordinator responds, check if the response references information from the compactor context (e.g., mentioning persistent facts, citing evaluation patterns). This is a heuristic — not an LLM-scored evaluation.

3. **Crash recovery quality**: Compare coordinator behavior after crash recovery with compactor context vs the old truncated-history approach. Qualitative assessment over time.

We do **not** add a formal LLM evaluation step for the compactor (that would be evaluating the evaluator's evaluator — diminishing returns). Instead, the compactor is evaluated indirectly through coordinator task quality: if the coordinator makes better decisions (higher evaluation scores on tasks it creates), the compactor is working.

### 4.3 Feedback Loop

```
Tasks complete → Evaluations scored → Compactor reads evaluations →
  Evaluation Digest updated → Coordinator reads digest →
  Coordinator creates better tasks → Tasks complete → ...
```

This closes the autopoietic loop. The coordinator doesn't see raw evaluation data (901+ JSON files); it sees the compactor's distilled digest. The compactor can surface patterns the coordinator would never notice from individual evaluations.

---

## 5. Migration Plan

### Phase 1: Compactor MVP (additive, no breaking changes)

**Scope**: Add the compactor module. Run it periodically. Write `context.md`. Replace crash recovery with `context.md` injection.

**Files to create**:
- `src/service/compactor.rs` — core compactor logic (~300 lines)
- `src/commands/compact.rs` — CLI command (~50 lines)

**Files to modify**:
- `src/config.rs` — add `DispatchRole::Compactor`, `CompactorConfig` struct, match arms
- `src/commands/service/coordinator_agent.rs` — `build_crash_recovery_summary()` reads `context.md`; per-turn injection reads `context.md`
- `src/commands/service/coordinator.rs` — daemon loop triggers compaction after N turns
- `src/commands/service/mod.rs` — export compactor module
- `src/service/mod.rs` — export compactor module
- `src/cli.rs` / `src/main.rs` — register `wg compact` command

**Validation**:
- `cargo test` passes (add unit tests for prompt assembly, metadata parsing, should_compact logic)
- Integration test: create a workgraph with history, run compaction, verify `context.md` is well-formed
- Manual test: start service, do some work, verify compactor runs and `context.md` appears
- Manual test: kill coordinator process, verify restart uses `context.md` for recovery

**Feature flag**: `CompactorConfig.enabled` (default: `true`). Can be disabled via `wg config --compactor-enabled false`.

**Risk**: None — the compactor is additive. If it fails, the coordinator falls back to the current crash recovery path.

### Phase 2: Context Injection Tuning

**Scope**: Inject `context.md` into every coordinator turn (not just crash recovery). Tune the compactor prompt based on observed coordinator behavior.

**Files to modify**:
- `src/commands/service/coordinator_agent.rs` — per-turn message injection prepends compactor context

**Validation**:
- Compare coordinator task creation quality with/without compactor context
- Monitor context.md for drift (does it accurately reflect project state?)
- Tune budget tokens and update frequency

**Risk**: Low — the compactor context is prepended alongside existing context. If it's unhelpful, the coordinator ignores it (same as any noisy context).

### Phase 3: Per-Turn Coordinator (future, not part of initial implementation)

**Scope**: Replace the long-lived `claude --input-format stream-json` process with per-turn `run_lightweight_llm_call()` invocations. Requires adding tool-use support to the lightweight call path.

**Files to modify**:
- `src/service/llm.rs` — add tool-use support (multi-turn loop within a single "turn")
- `src/commands/service/coordinator_agent.rs` — replace `spawn_claude_process()` with per-turn dispatch
- `src/commands/service/coordinator.rs` — adjust daemon to drive per-turn model calls

**Validation**:
- Coordinator produces equivalent task graphs as the long-lived process
- Latency is acceptable (3-5s per turn for simple queries, 10-15s for complex planning)
- Multi-model routing works (haiku for status queries, opus for planning)

**Risk**: Medium — this is an architectural change. The long-lived process's tool execution loop must be replicated. Defer until Phase 1-2 are validated.

### Phase 4: Self-Improvement (future)

**Scope**: Compactor tracks its own quality. Coordinator can request specific context.

**Validation**: Qualitative — does the coordinator demonstrably improve over time?

**Risk**: Low — purely additive.

---

## 6. Compactor Prompt Template

```
You are the workgraph compactor. Your job is to produce a structured context
summary that a coordinator agent will use to make decisions about task
orchestration.

The coordinator is a persistent agent that manages a task graph — it creates
tasks, sets dependencies, monitors agents, and communicates with a human user.
It needs to know: what happened recently, what decisions were made and why,
what patterns have emerged, and what's working or not working.

## Previous Context
{previous_context_md_or_"(first compaction)"}

## New Chat Messages (since last compaction)
{inbox_and_outbox_messages_interleaved_chronologically}

## Graph Mutations (since last compaction)
{operations_log_entries_or_summary_if_large}

## Recent Evaluations
{evaluation_summaries_for_recently_completed_tasks}

## Current Graph State
{task_counts_by_status, active_agents, failed_tasks}

## Instructions

Produce an updated context.md with YAML frontmatter and three sections:

1. **Rolling Narrative** (~2000 tokens): What happened, what decisions were
   made and why, what's in progress, what patterns emerged. Prioritize
   recency — recent events in detail, older events in summary. Remove
   information about completed workstreams that are no longer relevant.

2. **Persistent Facts** (~500 tokens): Stable project knowledge. Only update
   if new information contradicts existing facts. Include: project type,
   build commands, user preferences, recurring issues.

3. **Evaluation Digest** (~500 tokens): Aggregate agent performance. Trends
   in scores, weak dimensions, actionable recommendations. Only update if
   new evaluations arrived.

Keep total output under {budget_tokens} tokens. Output ONLY the markdown
content with YAML frontmatter. No commentary, no explanation, no code fences
around the entire output.
```

---

## 7. Summary of All Code Changes

### New files
| File | Purpose | Est. lines |
|---|---|---|
| `src/service/compactor.rs` | Core compactor logic: prompt assembly, LLM dispatch, context.md I/O | ~300 |
| `src/commands/compact.rs` | `wg compact` CLI command | ~50 |

### Modified files
| File | Change | Scope |
|---|---|---|
| `src/config.rs` | Add `DispatchRole::Compactor`, `CompactorConfig`, match arms, default model | ~40 lines added |
| `src/commands/service/coordinator_agent.rs` | `build_crash_recovery_summary()` uses context.md; per-turn injection reads context.md | ~30 lines changed |
| `src/commands/service/coordinator.rs` | Daemon triggers compaction after N turns | ~15 lines added |
| `src/commands/service/mod.rs` | Export compactor module | 1 line |
| `src/service/mod.rs` | Export compactor module | 1 line |
| `src/cli.rs` | Register `wg compact` subcommand | ~5 lines |
| `src/main.rs` | Route `wg compact` to handler | ~3 lines |
