# Coordinator-Compactor Binary System: Architecture

## 1. Problem Statement

The coordinator agent currently depends on Claude Code's internal context window
management. When the process crashes, it loses all in-session context and must
reconstruct state from a crude summary (last 10 chat messages, truncated to 500
chars each, plus a graph status snapshot). Between crashes, the context window
fills up and Claude Code's internal compaction decides what to keep — opaque to
the system and not portable across model runtimes.

The coordinator should be model-agnostic: no dependence on `--resume`, session
IDs, or any runtime's internal context management. Instead, context should be an
explicit, system-managed artifact produced by a **compactor** process.

## 2. Current Coordinator Architecture

### 2.1 Process Model

The coordinator is a **single long-lived `claude --print --input-format stream-json`
process** spawned by the service daemon (`coordinator_agent.rs:1208-1277`).

- **System prompt**: Static, ~4KB, injected once at process start via `--system-prompt`
- **Per-turn context injection**: On each user message, `build_coordinator_context()`
  builds a dynamic context block (~500-2000 tokens) prepended to the user message
- **Crash recovery**: On restart, `build_crash_recovery_summary()` injects last 10
  chat messages (truncated to 500 chars each) plus a full graph state refresh
- **History rotation**: Keeps last 200 messages per JSONL file, rotated on restart

### 2.2 Context Injection (build_coordinator_context, line 1431)

On each turn, the coordinator receives:
- **Graph summary**: Task counts by status (done/in-progress/open/blocked/failed)
- **Recent events**: Up to 20 events drained from a shared `EventLog` ring buffer
  (task completions, agent spawns, failures)
- **Active agents**: Agent IDs, tasks, uptime
- **Attention needed**: Failed tasks with failure reasons

This is compact but **stateless** — it shows the current snapshot, not what
decisions led to this state or what patterns have emerged.

### 2.3 Data Already Stored in the Workgraph

The system stores far more than the coordinator currently uses:

| Data Source | Location | Content | Size (this project) |
|---|---|---|---|
| Chat inbox | `.workgraph/chat/inbox.jsonl` | User messages | 372 msgs, 159KB |
| Chat outbox | `.workgraph/chat/outbox.jsonl` | Coordinator responses + full_response with tool calls | 368 msgs, 764KB |
| Operations log | `.workgraph/log/operations.jsonl` | Every graph mutation (add/done/fail/edit) | 16,463 entries |
| Task graph | `.workgraph/graph.jsonl` | Full task state including logs, artifacts, deps | All tasks |
| Agent registry | `.workgraph/agents/` | Agent spawn/completion records, PIDs, output files | Per-agent YAML |
| Messages | `.workgraph/messages/<task-id>.jsonl` | Inter-task/agent messages | Per-task |
| Evaluations | `.workgraph/agency/evaluations/*.json` | LLM-scored task evaluations (7 dimensions) | 901 evaluations |
| Output captures | `.workgraph/output/<task-id>/` | `artifacts.json`, `changes.patch`, `log.json` per task | Per-completed-task |
| Assignment records | `.workgraph/agency/assignments/` | Agent selection decisions with reasoning | Per-assignment |

**Key insight**: The workgraph already has **far greater vision into its own past**
than any single LLM session window. The compactor's job is to make this data
*usable* by distilling it into context that fits in a coordinator turn.

## 3. What the Compactor Produces

### 3.1 Layered Memory Architecture

The compactor produces three layers with different update frequencies:

#### Layer 1: Rolling Narrative (~2000 tokens, updated every 5-10 turns)

A structured summary of recent activity:

```
## Session Summary (last updated: 2026-03-05T14:30:00Z, turns 45-52)

### Active Workstreams
- TUI improvements: 3 tasks in-progress (add-scrollbars, fix-layout, add-keybinds),
  2 blocked on add-scrollbars
- Coordinator refactor: research phase complete, design task ready

### Recent Decisions
- Chose to parallelize TUI tasks since they touch different files
  (scrollbars→render.rs, keybinds→event.rs)
- Deferred database migration task per user request ("not yet")
- Retried fix-layout after first agent OOM'd (triage: "continue")

### Patterns Observed
- User prefers fan-out with integration tasks over sequential pipelines
- Tasks touching render.rs take ~15min avg, event.rs ~8min
- 2 of last 5 failures were OOM on large files (>200KB)

### Open Questions
- User mentioned "auth system" but hasn't given specifics yet
- Should we increase max_agents? Current utilization: 3/4 slots busy 80% of time
```

#### Layer 2: Persistent Facts (~500 tokens, updated infrequently)

Key-value facts that remain stable across sessions:

```
## Project Memory
- project_type: rust_tui_application
- build_cmd: cargo build && cargo install --path .
- test_cmd: cargo test
- primary_files: src/tui/ (30+ files), src/commands/service/ (10 files)
- user_preferences:
  - decomposition_style: fan_out_with_integrator
  - description_detail: high (always include ## Validation)
  - response_style: concise, no emojis
- recurring_issues:
  - OOM on large files (>200KB) with opus model
  - render.rs merge conflicts when parallelized
```

#### Layer 3: Evaluation Digest (~500 tokens, updated after evaluation batches)

Distilled from the 901+ evaluation records:

```
## Agent Performance Digest
- avg_score: 0.79 across 901 evaluations
- top_dimensions: correctness (0.87), style_adherence (0.85)
- weak_dimensions: completeness (0.72), coordination_overhead (0.74)
- recommendation: Task descriptions should be more explicit about edge cases
  (completeness gap). Agents over-decompose (coordination overhead).
- model_comparison: opus avg 0.81, haiku avg 0.74 (evaluator: haiku)
```

### 3.2 Output Format

A single file: `.workgraph/compactor/context.md`

Structured markdown with YAML frontmatter for metadata:

```yaml
---
version: 3
last_compacted: "2026-03-05T14:30:00Z"
turns_covered: 52
compactor_model: "haiku"
token_estimate: 3000
---
```

Followed by the three layers concatenated. The coordinator reads this file at
startup (replacing crash recovery) and optionally on each turn.

## 4. Mechanical Design

### 4.1 Trigger: Periodic, Not Blocking

The compactor runs **asynchronously** as a periodic task, not blocking the
coordinator's response loop:

- **Trigger**: Every N coordinator turns (configurable, default 10) OR when
  the operations log has grown by >100 entries since last compaction
- **Implementation**: A workgraph system task `.compact-coordinator-context`
  created by the service daemon
- **Execution**: Runs as a lightweight LLM call (like triage), not a full
  agent spawn — uses `run_lightweight_llm_call()` with the triage/evaluator
  model (typically haiku)
- **Latency**: 5-15 seconds for haiku; coordinator doesn't wait for it

### 4.2 Data Flow

```
┌─────────────────────────────────────────────────────┐
│                    Workgraph Storage                  │
│                                                       │
│  chat/inbox.jsonl ─────┐                             │
│  chat/outbox.jsonl ────┤                             │
│  log/operations.jsonl ─┤    ┌──────────────┐        │
│  graph.jsonl ──────────┼───▶│   Compactor   │        │
│  agency/evaluations/ ──┤    │  (haiku LLM)  │        │
│  output/<task>/ ───────┤    └──────┬───────┘        │
│  messages/<task>.jsonl ┘           │                  │
│                                    ▼                  │
│                    compactor/context.md                │
│                           │                           │
│                           ▼                           │
│                    ┌──────────────┐                   │
│                    │  Coordinator  │                   │
│                    │  (any model)  │                   │
│                    └──────────────┘                   │
└─────────────────────────────────────────────────────┘
```

### 4.3 Compactor Input Assembly

The compactor receives a prompt built from:

1. **Previous context.md** (the compactor's own last output — self-referential)
2. **New chat messages** since last compaction (inbox + outbox, full content)
3. **Operations log delta** (graph mutations since last compaction)
4. **Evaluation summaries** for recently completed tasks
5. **Current graph snapshot** (status counts, dependency structure)

This is bounded: the compactor prompt should stay under 20K tokens even for
busy sessions. The operations log is the largest source — if it exceeds 5K
tokens, it gets summarized (counts by operation type + notable events).

### 4.4 Compactor Prompt Structure

```
You are the workgraph compactor. Your job is to produce a structured context
summary that a coordinator agent will use to make decisions.

## Previous Context
<contents of compactor/context.md, or "(first compaction)" if none>

## New Chat Messages (since last compaction)
<inbox + outbox messages, chronologically interleaved>

## Graph Mutations (since last compaction)
<operations log entries>

## Recent Evaluations
<evaluation summaries for tasks completed since last compaction>

## Current Graph State
<task counts, active agents, failed tasks>

## Instructions
Produce an updated context.md with three sections:
1. Rolling Narrative: What happened, what decisions were made, what's in progress
2. Persistent Facts: Stable project knowledge (update only if new info contradicts)
3. Evaluation Digest: Updated agent performance patterns

Keep total output under 3000 tokens. Prioritize recency: recent events in detail,
older events in summary. Remove information that is no longer relevant (completed
workstreams, resolved issues).

Output ONLY the markdown content (with YAML frontmatter). No commentary.
```

## 5. Long-Lived Process vs Fresh-Per-Turn

### 5.1 Comparison

| Aspect | Long-lived (current) | Fresh-per-turn |
|---|---|---|
| Context management | Runtime-internal (opaque) | System-managed (compactor) |
| Crash recovery | Lossy (10 messages, truncated) | Clean (compactor context.md) |
| Model lock-in | Process tied to one runtime | Any model, any invocation style |
| Latency | Fast (in-session) | ~3-5s cold start per turn |
| Token efficiency | Accumulates bloat over time | Fixed context budget per turn |
| Tool state | Tools persist across turns | Tools re-initialized each turn |
| Observability | Opaque (what's in context?) | Transparent (context.md is readable) |

### 5.2 Recommendation: Hybrid — Fresh-per-Turn with Warm Cache

**Long-term target: fresh-per-turn with compacted context.**

Each coordinator turn becomes:
1. Read `compactor/context.md` (~3000 tokens)
2. Read current graph state via `build_coordinator_context()` (~500-2000 tokens)
3. Append the user message
4. Invoke the model (any model, any runtime)
5. Parse the response, execute tool calls, write outbox

This gives full model agnosticism and transparent context management. The
coordinator is stateless except for the compacted context file.

**Migration path (3 phases):**

1. **Phase 1**: Add compactor alongside existing long-lived process. Compactor
   runs periodically, writes context.md. Crash recovery uses context.md instead
   of the current truncated-history approach. Long-lived process continues as-is.

2. **Phase 2**: Replace crash recovery entirely with context.md injection. The
   long-lived process still runs, but restarts become seamless — just inject
   context.md as the first message. Test with intentional crashes to validate.

3. **Phase 3**: Replace long-lived process with per-turn invocations. Each user
   message triggers a fresh model call with context.md + graph state + user
   message. This unlocks multi-model coordination (different model per turn
   based on complexity).

### 5.3 Minimum Context Window Size

The design requires approximately:
- System prompt: ~4K tokens (current, already exists)
- Compactor context: ~3K tokens (configurable)
- Per-turn graph state: ~1-2K tokens (existing `build_coordinator_context`)
- User message: variable, typically ~100-500 tokens
- Response budget: ~2-4K tokens

**Minimum viable: 16K context window.** Works with Haiku, Sonnet, GPT-4o-mini,
Gemini Flash, and most local models (Llama 3.x 8B+). The 200K+ windows of
Opus/Sonnet are not needed.

## 6. Autopoietic Loop

The system is autopoietic (self-creating/self-maintaining) because:

```
User request
    │
    ▼
Coordinator ──creates tasks──▶ Task Graph
    ▲                              │
    │                         agents work
    │                              │
    │                              ▼
    │                        Task Results
    │                         (artifacts,
    │                          evaluations,
    │                          messages)
    │                              │
    │                              ▼
    │◀──compacted context──── Compactor
    │                         reads results,
    │                         distills patterns
    │
    ▼
Coordinator uses compacted context to create BETTER tasks
```

### 6.1 Self-Improvement Feedback Loops

1. **Decision quality feedback**: The evaluation system scores completed tasks.
   The compactor distills these scores into the Evaluation Digest. The
   coordinator sees "completeness scores are low" and writes more detailed
   task descriptions → scores improve.

2. **Pattern recognition**: The compactor notices "tasks touching render.rs
   conflict when parallelized" and records it as a Persistent Fact. The
   coordinator reads this and stops parallelizing render.rs tasks.

3. **Resource optimization**: The compactor tracks agent utilization and
   failure rates. The coordinator adjusts decomposition granularity and
   max-agents recommendations.

4. **User preference learning**: The compactor distills user interaction
   patterns (correction frequency, preferred decomposition style, response
   verbosity) into Persistent Facts.

### 6.2 Evaluation System Integration

Today: evaluations are scored and stored but not fed back to the coordinator.
The coordinator has no awareness of which decisions produced good or bad outcomes.

With the compactor:
- Every evaluation batch gets summarized in the Evaluation Digest
- Trends are surfaced: "last 5 tasks with description length <100 chars scored
  0.65 avg vs 0.82 for >300 chars"
- The coordinator receives this as actionable context, not raw data

## 7. Model Agnosticism

### 7.1 Hard Constraints

- No `--resume` or session persistence
- No runtime-specific context management (Claude's /memory, GPT's memory, etc.)
- All context is explicit in files the system controls
- The compactor and coordinator can be different models
- The system works with **any model that supports text-in, text-out** (even
  non-chat-completion APIs, with appropriate wrapping)

### 7.2 Executor Abstraction

The current `spawn_claude_process()` already uses a generic command pattern.
For per-turn invocations, this becomes `run_lightweight_llm_call()` (already
exists for triage/evaluation) with the coordinator role:

```rust
run_lightweight_llm_call(
    config,
    DispatchRole::Coordinator,
    &prompt,  // context.md + graph_state + user_message
    timeout,
)
```

The config already supports model selection per role. The compactor uses
`DispatchRole::Compactor` (or reuses `Triage` since it's a similar lightweight
call).

## 8. Implementation Phases

### Phase 1: Compactor MVP (low risk, high value)
- Add `compactor/` module alongside existing coordinator
- Implement compactor prompt assembly from existing data sources
- Run compactor every 10 turns via daemon tick
- Write output to `.workgraph/compactor/context.md`
- Use context.md for crash recovery (replace `build_crash_recovery_summary`)
- **Validates**: compactor output quality, latency, data flow
- **Risk**: None — additive, doesn't change coordinator's live behavior

### Phase 2: Context Injection
- Inject context.md into each coordinator turn (alongside existing per-turn context)
- A/B test: coordinator quality with and without compactor context
- Tune compactor prompt and output format based on coordinator behavior
- **Validates**: Whether compactor context improves coordinator decisions

### Phase 3: Per-Turn Coordinator
- Replace long-lived process with per-turn invocations
- Each user message triggers: read context.md → build prompt → call model → parse response
- Support multi-model: use haiku for simple status queries, opus for complex planning
- **Validates**: Full model agnosticism, latency acceptability

### Phase 4: Self-Improvement
- Compactor tracks its own quality (does the coordinator use the context it provides?)
- Coordinator can request specific context ("compactor, what was the failure pattern on auth tasks?")
- Evaluation digest drives task description templates
- **Validates**: Autopoietic feedback loop produces measurable improvement

## 9. Open Questions

1. **Compactor token budget**: 3000 tokens is a starting point. Should this be
   configurable? Should it auto-scale based on graph complexity?

2. **Stale context detection**: How does the coordinator know if compactor output
   is stale? YAML frontmatter includes timestamp; if >30 minutes old and >50
   operations have occurred, trigger emergency compaction.

3. **Multi-turn tool use in per-turn mode**: In Phase 3, the coordinator needs
   to run `wg` commands. Per-turn means each tool call is a separate model
   invocation. This increases latency but is already how triage works. Acceptable?
   Or keep a mini multi-turn loop within a single "turn"?

4. **Compactor disagreement**: What if the compactor drops context the coordinator
   later needs? The raw data is always available — the coordinator can fall back
   to reading JSONL files directly for specific queries. The compactor is a cache,
   not a gate.
