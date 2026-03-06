# Deep Synthesis: Unified Design Landscape

**Date:** 2026-03-06
**Task:** spark-v2-unified
**Sources:** 6 cluster synthesis artifacts (agency, coordinator, human, TUI/graph, providers, archive) + integration roadmap
**Scope:** 90+ design/research documents distilled from 1758 archived tasks

---

## 1. Design Landscape

### A. Multi-Provider Infrastructure (Maturity: HIGH)

The provider story is further along than it appears. The native executor (`src/executor/native/`) already has working Anthropic and OpenAI-compatible clients. OpenRouter works for tool-use. Per-role model routing via `DispatchRole` + `resolve_model_for_role()` exists. A model registry with cost/capability metadata is implemented (`src/models.rs`). Local models (Ollama, vLLM) work via the existing OpenAI client with `api_base` config.

**What works today:** Anthropic direct API (streaming, caching, tool-use), OpenRouter via OpenAI-compatible client (non-streaming), local models via same client, per-role model+provider routing for lightweight dispatch, TUI endpoint configuration.

**Gaps:**
- 6 hardcoded Claude fallbacks in evaluate.rs, triage.rs, coordinator.rs
- Endpoint API keys stored but not consumed by client creation
- Native executor ignores per-role provider config (uses global `[native_executor] provider`)
- No `Provider` trait abstraction (agents use concrete client types)
- No OpenAI client streaming (SSE)

**Cross-cluster dependencies:**
- Agency system needs provider routing for model quality tiers (SystemEvaluator dispatch role)
- Coordinator normalization needs provider abstraction to run on non-Claude models
- Cost tracking feeds into agency's cost-efficiency assignment scoring

### B. Coordinator & Architecture (Maturity: MEDIUM-HIGH)

Four interlocking designs form the coordinator transformation stack: compactor agent spec (rolling `context.md`, ~350 LOC, implementation-ready), coordinator-as-regular-agent (eliminate ~550 lines of special-entity code), coordinator-as-graph-citizen (self-evaluation + prompt evolution), and coordinator-chat-protocol (Phase 1 stub implemented).

The compactor is the clearest near-term win: a new `src/service/compactor.rs` module triggered every N turns, producing a 3-layer context artifact (rolling narrative ~2000 tokens + persistent facts ~500 tokens + evaluation digest ~500 tokens). This replaces the current crash-recovery mechanism and enables bounded coordinator context.

The liveness detection design (committee consensus, ~180 LOC) fixes sleep-related false positives using `clock_gettime(CLOCK_MONOTONIC)`. The agent activity protocol (~300 LOC) enriches stream events and fixes a token tracking bug.

**Gaps:** Two competing compaction visions (rolling vs. era-based) need resolution. The per-turn coordinator model needs multi-turn tool-use in `run_lightweight_llm_call()`. The coordinator-as-regular-agent transformation is a significant architectural change affecting the two largest files in the codebase (~1630 and ~3351 lines).

**Cross-cluster dependencies:**
- Provider abstraction required before coordinator can run on non-Claude models
- Compactor drives temporal navigation epoch semantics
- Agent activity protocol shared between coordinator and TUI observability

### C. Agency System (Maturity: MEDIUM)

The agency system (roles, motivations, agents, evaluations, evolution, federation) is production-deployed with 130+ federation tests. Key unimplemented designs:

- **Auto-evolver loop**: Triggers as `.evolve-*` meta-task when >=10 evaluations accumulate (2hr minimum interval). Safe strategy subset (mutation, gap-analysis, retirement, motivation-tuning). Reactive trigger at avg score <0.4. Runs as a graph task, not coordinator action.
- **Model quality tiers**: `SystemEvaluator` dispatch role for meta-tasks (~30 LOC). Automatic detection via `is_system_task()` dot-prefix check.
- **Cost tracking**: `cost_usd`/`token_usage` on Evaluation, cost-efficiency scoring formula, per-task budget constraints. All backward-compatible via `serde(default)`.
- **Executor weight tiers**: Expand exec_mode from binary (full/bare) to four tiers (shell/bare/light/full).

**Validation is the critical gap.** Six research documents converge: agents can mark work done without verification, evaluations are post-hoc and non-gating (48.5% score >=0.9, confirmed false positives from hallucination), and the `verify` field exists but is invisible to agents. Prompt-level guidance is the highest-impact fix.

**Cross-cluster dependencies:**
- Provider routing enables cost-aware assignment and right-sized execution
- Coordinator-as-graph-citizen makes coordinator subject to agency governance
- Validation prompt improvements are prerequisite for meaningful evaluation signal

### D. Human Connection (Maturity: LOW-MEDIUM)

Foundation is solid: task message queue (wg msg), coordinator chat (wg chat Phase 1), message discipline (unread blocks wg done) are implemented. The archive reveals notification code existed in `src/notify/*.rs` but was removed from active use.

The `NotificationChannel` trait design specifies `send_text`, `send_rich`, `send_with_actions`, and `listen` methods. A `NotificationRouter` handles event-type routing and escalation chains. 13 channels evaluated; Telegram is recommended first (inline keyboards, excellent `teloxide` crate).

The `wg ask` design enables cross-executor human input: blocking or async requests, `EventType::Question` for notification routing, agent parking/blocking with park-and-resume flow.

**Gaps:** No NotificationChannel trait implementation. Persistent coordinator agent (Phase 2 chat) needs coordinator-as-regular-agent. MCP integration (`rmcp` v0.16) would add web_search/web_fetch without building from scratch (12-16hr estimate).

**Cross-cluster dependencies:**
- Coordinator transformation needed for intelligent chat responses
- Native executor needs `wg_ask` tool in ToolRegistry
- Executor `prompt_mode` decoupling needed for non-Claude executors

### E. TUI & Graph System (Maturity: HIGH for core, MEDIUM for advanced)

Core graph features are mature: cycle detection via Tarjan's SCC (53 tests, ~1030 lines in `src/cycle.rs`), edge rename (`after`/`before`), loop convergence (`--converged`), multi-panel TUI layout.

**Critical bug:** File-locking audit identified a HIGH severity TOCTOU race in `graph.jsonl` -- flock held separately for load and save, not across read-modify-write. Lost updates are virtually guaranteed under 5-agent operation. Fix is ~50 lines (Option D: `mutate_graph()` wrapper).

**Ready to implement:**
- Outbound edge viz fix (wrong arrowhead direction, ~40 LOC)
- Cycle edge coloring (yellow for SCC edges, ~40 LOC)
- tui-textarea integration (already a dependency, replaces ~200 lines of custom code)
- Temporal navigation Phase 1 (iteration history for cycles)
- Dangling dependency resolution (fuzzy matching, ~330 LOC)
- Reopen-on-new-dep (auto-reopen stale tasks, ~120 LOC)
- `wg func` rename from `wg trace` (spec complete)

**Cross-cluster dependencies:**
- Temporal navigation Phase 2 depends on compactor for epoch semantics
- File locking fix is prerequisite for reliable multi-agent operation
- TUI node-specific chat depends on coordinator chat infrastructure

### F. Graph Safety (Maturity: HIGH)

Two new designs landed after the initial synthesis that form a complete safety architecture around the task graph. Combined with mandatory validation and liveness detection, they create a three-layer defense system: **prevention**, **detection**, and **recovery**.

#### Task Retraction & Cascade Control (`task-retraction-cascade.md`)

Motivated by the spark-v2 incident where a prematurely dispatched task created 17 child tasks before it could be stopped, requiring 26 manual cleanup steps. Introduces four new commands:

- **`wg retract <id>`** -- Undoes what a task *produced*: finds all tasks it created (via provenance lineage), kills their agents, abandons them, resets the originating task to open. Uses transitive provenance-based traversal.
- **`wg cascade-stop <id>`** -- Stops everything *downstream* of a task (via `--after` edges). Supports `--hold` (pause instead of abandon) for recoverable freezes.
- **`wg reset <id>`** -- Full clean-slate reset to open, regardless of current status. `--downstream` resets all dependents. `--retract` abandons created tasks first. `wg reset X --retract --downstream` replaces the entire 26-step spark-v2 incident with one command.
- **`wg hold/unhold <id>`** -- Atomic subtree pause/resume. Pauses a task plus all transitive dependents, kills in-progress agents.

Also adds **live dependency enforcement**: when `wg edit --add-after` adds a dep to an already-dispatched task, the system pauses the task and the coordinator's `recall_stale_agents()` kills and resets tasks with unmet deps. This prevents the exact race condition that caused the spark-v2 incident.

**Scope:** ~900 lines Rust + tests across ~9 files. No new task fields -- retraction computed from provenance. Key design decisions: no automatic git revert (append-only history), provenance records all retraction operations.

#### Self-Healing Task Graph (`self-healing-task-graph.md`)

When a task fails, instead of waiting for human intervention, the coordinator automatically diagnoses and remediates:

1. **Lightweight LLM diagnosis** (sonnet-class, ~2s) classifies the failure into 6 categories: transient, context overflow, build failure, missing dependency, agent confusion, unfixable
2. **Category-specific remediation**: auto-retry with backoff (transient), task decomposition via `.remediate-*` task (context overflow), prerequisite injection (build/missing dep), description rewrite (confusion), escalation to human (unfixable)
3. **Graph wiring**: `.remediate-*` tasks are wired `--before` the failed task, blocking retry until the fix completes. Multiple attempts accumulate context (`.remediate-T`, `.remediate-T-2`, etc.)

**Safety guardrails:** Max 3 remediation attempts per task, token budget cap (2x original cost), system tasks (dot-prefixed) never remediated, abandoned tasks never remediated, circuit breaker respected. Confidence threshold of 0.6 -- below this, escalate to human.

**Integration points:**
- FLIP can retroactively fail a task -> triggers remediation
- Validation rejection -> max rejections -> task Failed -> remediation kicks in
- `wg retract .remediate-X` cleans up bad remediation subtasks
- Remediation patterns feed evolution signal (repeated failures indicate role improvement needed)

#### Unified Safety Architecture

| Layer | Mechanism | Source Design |
|-------|-----------|---------------|
| **Prevention** | Live dep enforcement, dangling dep detection, auto-reopen on new dep, mandatory validation gates | task-retraction-cascade, dangling-dependency-resolution, reopen-on-new-dep, mandatory-validation |
| **Detection** | Liveness detection (clock drift + stream staleness), failure classification (LLM diagnosis), coordinator dep escalation | liveness-detection, self-healing-task-graph, dangling-dependency-resolution |
| **Recovery** | `wg retract`, `wg cascade-stop`, `wg reset`, `wg hold/unhold`, auto-remediation (`.remediate-*`), validation rejection + re-dispatch, stuck triage verdicts | task-retraction-cascade, self-healing-task-graph, mandatory-validation, liveness-detection |

The dot-prefix convention (`.remediate-*`, `.evaluate-*`, `.validate-*`, `.assign-*`, `.evolve-*`) serves as a universal safety boundary preventing infinite regress across all systems.

**Cross-cluster dependencies:**
- Liveness detection feeds stuck agent failures into remediation pipeline
- File locking fix is prerequisite for all graph mutation safety
- Retraction provides the "undo" that remediation lacks when it goes wrong
- Validation gates remediation retries (PendingValidation ensures quality before downstream unblock)

### G. Recovered Archive Ideas (Maturity: VARIES)

Mining 1758 archived tasks reveals:
- **Worktree isolation** (3 research docs): Per-agent git worktree isolation, critical for scaling parallel agents beyond the "same files = sequential edges" workaround
- **Trace function protocol**: CLI commands exist (`wg func list/apply`), plan validator built, extraction quality improvements specified
- **Provenance system**: Design extends existing `provenance.rs` with full operation logging for 12+ graph-mutating commands
- **Autopoietic gap**: Research found agents don't reliably decompose work despite instructions; prompt strengthening needed

---

## 2. Implementation Readiness Matrix

### Ready NOW (design complete, no blocking dependencies)

| Feature | Cluster | Effort | Impact |
|---------|---------|--------|--------|
| File locking fix (mutate_graph wrapper) | Infra | ~50 LOC | **CRITICAL** data loss prevention |
| Unify hardcoded model fallbacks | Infra | ~50 LOC | Enables all provider routing |
| Validation prompt improvements + surface task.verify | Infra | ~50 LOC | Highest-impact zero-risk change |
| Live dependency enforcement (wg edit detection) | Safety | ~30 LOC | Prevents spark-v2 race condition |
| Dangling dep detection (layers 1-3) | Safety | ~200 LOC | Prevents silent stalls from typos |
| Reopen-on-new-dep + wg reopen | Safety | ~120 LOC | Stale completion prevention |
| wg reset command | Safety | ~80 LOC | Clean-slate task reset foundation |
| SystemEvaluator dispatch role | Agency | ~30 LOC | Better meta-task evaluation |
| Liveness detection (sleep-aware) | Coordinator | ~180 LOC | Operational reliability |
| Agent activity protocol | Coordinator | ~300 LOC | Observability + token bug fix |
| Outbound edge viz fix | TUI | ~40 LOC | Rendering correctness |
| Cycle edge coloring | TUI | ~40 LOC | Visual clarity for cycles |

### Ready as foundational work (no prereqs, but are themselves prereqs)

| Feature | Cluster | Effort | Enables |
|---------|---------|--------|---------|
| wg cascade-stop + hold/unhold | Safety | ~180 LOC | Subtree control for incident recovery |
| wg retract (provenance lineage) | Safety | ~150 LOC | Undo task's side effects |
| Self-healing diagnosis + remediation | Safety | ~500 LOC | Automatic failure recovery |
| Compactor MVP | Coordinator | ~350 LOC | Context management, epoch navigation |
| NotificationChannel trait | Human | ~200 LOC | All external notification channels |
| Cost tracking data layer | Agency | ~200 LOC | Cost-aware assignment |

### Ready after one prerequisite

| Feature | Prerequisite | Effort |
|---------|-------------|--------|
| Wire endpoint API keys | Provider trait | ~100 LOC |
| Native executor per-role routing | Provider trait + unified fallbacks | ~100 LOC |
| Telegram channel | NotificationChannel trait | ~300 LOC |
| Webhook channel | NotificationChannel trait | ~100 LOC |
| Auto-evolver loop | Validation prompt improvements | ~400 LOC |
| Temporal nav Phase 2 (epochs) | Compactor MVP | ~130 LOC |

### Needs significant design work

| Feature | What's Missing | Risk |
|---------|---------------|------|
| Coordinator as regular agent | Detailed executor integration, daemon refactor | Medium |
| Mandatory validation (PendingValidation) | Lifecycle state machine coordination | Medium |
| Per-turn coordinator | Multi-turn tool-use in lightweight LLM call | High |
| Unified lifecycle state machine | Open->Ready/Draft split, cross-design coordination | Medium |

---

## 3. Conflict/Overlap Map

### Conflicts requiring resolution

1. **Compaction model**: Rolling `context.md` (compactor-agent-spec) vs. era-based `era-N-summary.md` (coordinator-as-regular-agent).
   **Resolution:** Build rolling compactor first. Era-based becomes a natural extension when coordinator migrates to regular-agent model.

2. **State machine expansion**: Three designs add states -- `Waiting` (wg-wait), `PendingValidation` (mandatory-validation), `Ready`/`Draft` split (unified-lifecycle).
   **Resolution:** For near-term, only implement what's needed. Defer `PendingValidation` and `Ready`/`Draft`. Add `Waiting` only when `wg wait` is built.

3. **Coordinator tick extensions**: Auto-evolver, `wg wait` conditions, and cycle re-activation all add checks to the coordinator tick.
   **Resolution:** Design a unified condition evaluation framework rather than ad-hoc additions. This is a shared architectural concern.

4. **Evaluation vs. validation sequencing**: Mandatory validation proposes validate-then-evaluate. Current system creates evaluate tasks directly.
   **Resolution:** Defer mandatory validation. Improve evaluation quality first through prompt improvements and `task.verify` surfacing. This provides 80% of the value at 10% of the complexity.

5. **Tool strategy for coordinator**: Bash-only (v1, immediate) vs. typed native tools (v2, requires native executor).
   **Resolution:** Not a true conflict -- phased. v1 ships now with claude executor; v2 ships when native executor is ready.

6. **Retraction vs. remediation scope**: `wg retract` undoes what a task *produced* (created tasks), while remediation fixes why a task *failed*. Both modify task state and coordinator tick.
   **Resolution:** They're complementary. Retraction provides the "undo" if remediation goes wrong. Build retraction (simpler graph operations) before remediation (LLM diagnosis calls).

7. **Live dep enforcement vs. auto-reopen**: Both handle graph topology changes invalidating task state, at different lifecycle stages. Live dep enforcement handles in-progress tasks; auto-reopen handles done tasks.
   **Resolution:** Build live dep enforcement first (handles the active race condition), then auto-reopen (handles the passive staleness case). Both modify `edit.rs` so must be serialized.

### Overlaps (complementary, not conflicting)

1. **Cost infrastructure**: model-cost-tracking (empirical data) and executor-weight-tiers (cost reduction) share cost data layer. Build tracking first, right-sizing on top.

2. **Provider routing**: OpenRouter integration and model-provider-audit identify identical gaps from different angles. Single workstream.

3. **Context management**: Compactor (coordinator) and checkpointing (workers) use same "summarize via lightweight LLM call" pattern. Share infrastructure.

4. **Validation guidance**: Four documents specify prompt changes. Use validation-synthesis R1-R7 as canonical source.

5. **Coordinator files**: Compactor, regular-agent, graph-citizen, and chat all modify `coordinator_agent.rs` and `coordinator.rs`. Strict serialization required.

6. **Logging/observability**: logging-gaps, provenance-system, and agent-activity-protocol form complementary observability stack. Provenance addresses mutation audit; activity addresses token tracking.

7. **Liveness -> Remediation pipeline**: Liveness detection kills stuck agents (task -> Failed) -> self-healing diagnoses failure -> creates `.remediate-*` task. The full stuck-agent recovery pipeline spans two designs that share the coordinator tick loop.

8. **Safety dot-prefix convention**: `.remediate-*`, `.evaluate-*`, `.validate-*`, `.assign-*`, `.evolve-*` all use the dot-prefix to prevent infinite regress. This is shared infrastructure across remediation, evaluation, validation, and evolution systems.

---

## 4. Critical Path

### Five tracks after shared foundations:

```
SHARED FOUNDATIONS (must be first):
  File locking fix ────────────> Reliable multi-agent operation
  Validation prompt fixes ─────> Meaningful evaluation signal
  Unify model fallbacks ───────> All provider + agency features

Track 1: GRAPH SAFETY (prevent/detect/recover from failures)
  Live dep enforcement ──> Reopen-on-dep ──> (prevention complete)
  wg reset ──> wg cascade-stop ──> wg retract ──> smoke-safety-ops
  Self-healing diagnosis ──> Mandatory validation ──> smoke-self-healing
  Dangling dep detection (parallel)

Track 2: COORDINATOR EVOLUTION (self-improving coordinator)
  Liveness detection ──> Activity protocol ──> Compactor MVP
       |
  Coordinator as regular agent ──> Coordinator as graph citizen

Track 3: AGENCY LOOP (close the self-improvement cycle)
  SystemEvaluator dispatch role ──> Cost tracking ──> Auto-evolver
  Executor weight tiers (parallel after SystemEvaluator)

Track 4: HUMAN CONNECTION
  NotificationChannel trait ──> Telegram channel
                            ──> Webhook channel

Track 5: TUI
  TUI fade fix (instant dot-task toggle)
  Edge viz fix ──> Cycle coloring ──> Dangling dep viz
```

### What must be built first (foundational ordering):

1. **File locking fix** -- TOCTOU race causes data loss under normal 5-agent operation. Every multi-agent feature is unreliable without this. ~50 LOC.

2. **Validation prompt improvements + surface task.verify** -- The agency self-improvement loop produces noisy signal when agents don't validate. Prompt fixes are highest-impact, lowest-effort. ~50 LOC total.

3. **Unify hardcoded model fallbacks** -- Every provider feature builds on `resolve_model_for_role()`. ~50 LOC with highest downstream enablement.

4. **Live dependency enforcement** -- Prevents the spark-v2 race condition. Highest safety value, lowest risk. ~30 LOC in edit.rs.

5. **wg reset** -- Foundation for cascade-stop, retract, and self-healing. ~80 LOC. The building block that makes the entire safety track possible.

6. **Liveness detection** -- Operational reliability. Feeds stuck agent failures into the self-healing pipeline. ~180 LOC.

7. **Compactor MVP** -- Coordinator context grows unbounded. Compactor enables the coordinator-as-regular-agent transformation and epoch-based temporal navigation. ~350 LOC.

### Parallel tracks after foundations:

- **Graph safety** (live dep enforcement -> reopen -> retract pipeline) depends on file locking fix
- **Human connection** (NotificationChannel -> Telegram -> wg ask) depends on file locking fix
- **TUI improvements** (edge viz, cycle coloring, fade fix) mostly independent
- **Agency improvements** (SystemEvaluator, auto-evolver) depend on validation fixes + unified fallbacks
- **Coordinator evolution** depends on liveness detection + compactor

### Final integration depends on ALL tracks:

The `integrate-spark-v3` task validates end-to-end that safety, agency, TUI, human connection, and coordinator improvements work together. It depends on all smoke tests and all track completions.
