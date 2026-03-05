# Integration Roadmap: From Design to Implementation

**Date:** 2026-03-05
**Task:** synthesis-report-outstanding
**Sources:** 8 design/research documents, 1 archive review report

---

## 1. Design Inventory

### 1.1 Coordinator Architecture (4 documents)

| Document | Location | Completeness | Summary |
|----------|----------|-------------|---------|
| Coordinator as Regular Agent | `docs/design/coordinator-as-regular-agent.md` | Complete design | Coordinator becomes a regular looping agent with era-based context compaction. Eliminates ~800 lines of special-entity code. 4-phase migration (A→D). |
| Coordinator as Graph Citizen | `docs/design/coordinator-as-graph-citizen.md` | Complete design | Coordinator subject to evaluation, assignment, and prompt evolution per turn. 5-phase rollout. Builds on the regular-agent design. |
| Compactor Agent Spec | `docs/design/compactor-agent-spec.md` | Complete spec (implementation-ready) | Detailed module spec: `src/service/compactor.rs` (~300 lines), `src/commands/compact.rs` (~50 lines). Config changes, prompt template, data flow, validation criteria all specified. |
| Coordinator-Compactor Architecture | `docs/design/coordinator-compactor-architecture.md` | Complete design | Binary system: compactor distills workgraph data into `context.md`, coordinator reads it. Layered memory (rolling narrative + persistent facts + evaluation digest). 4-phase migration to full model agnosticism. |

**Relationship:** These four documents form a coherent stack. The compactor spec is the concrete implementation plan for what the architecture doc describes. The regular-agent design explains *how* the coordinator runs, and the graph-citizen design explains *how it's governed*. They share the same phasing — compactor first, then regular-agent migration, then governance.

### 1.2 UX / TUI (1 document)

| Document | Location | Completeness | Summary |
|----------|----------|-------------|---------|
| Temporal Navigation | `docs/design/temporal-navigation.md` | Complete design | Unified streams-and-epochs abstraction. 5 phases: iteration history → chat archive boundary → search → multi-coordinator tabs → iteration diff. ~930 lines new code, 4 new UI elements. |

**Relationship:** Temporal navigation depends on the compactor (epochs are defined by compaction boundaries for chat). Phase 4 (multi-coordinator tabs) depends on the coordinator-as-graph-citizen multi-coordinator work. Phases 1-3 are independent.

### 1.3 Model / Provider (2 documents)

| Document | Location | Completeness | Summary |
|----------|----------|-------------|---------|
| OpenRouter Integration | `docs/research/openrouter-integration.md` | Complete research with implementation plan | HTTP client works, tool-use works. 6 gaps: endpoint-aware key resolution, endpoint→role binding, CLI endpoint management, key file support, streaming, native executor per-role routing. 4-phase plan. |
| Model Provider Audit | `docs/research/model-provider-audit.md` | Complete audit with implementation plan | Full inventory of all 30+ dispatch points. 6 hardcoded coupling points identified. 4 subtasks defined: unify fallbacks, wire provider to triage, dynamic TUI choices, deprecation warnings. |

**Relationship:** These are complementary. The provider audit identifies *what needs to change* across the codebase. The OpenRouter research identifies *what's needed for a specific provider*. The audit's subtask 2 (wire provider to triage/checkpoint) directly enables the OpenRouter work.

### 1.4 Archive Review (1 document)

| Document | Location | Completeness | Summary |
|----------|----------|-------------|---------|
| Archive Review | `docs/reports/archive-review-human-interaction.md` | Complete | Catalogs 11 design documents and ~80 archived tasks across messaging, lifecycle, integration, and notification clusters. Identifies 15 open threads. |

---

## 2. Implementation Readiness

### Ready to build now (detailed spec exists, minimal unknowns)

| Design | What's ready | Estimated scope | Key files to create/modify |
|--------|-------------|----------------|---------------------------|
| **Compactor Agent (Phase 1)** | Full module spec, prompt template, config schema, validation criteria | ~400 lines new, ~90 lines modified | New: `src/service/compactor.rs`, `src/commands/compact.rs`. Modify: `src/config.rs`, `coordinator_agent.rs`, `coordinator.rs` |
| **Temporal Nav Phase 1** (iteration history) | Data model changes, TUI render logic specified | ~100 lines | `src/graph.rs`, `src/tui/viz_viewer/render.rs` |
| **Provider Audit Subtask 1** (unify fallbacks) | All dispatch points enumerated with exact file:line | ~50 lines changed | `src/commands/evaluate.rs`, `src/commands/service/triage.rs`, `src/commands/service/coordinator.rs`, `src/config.rs` |
| **Provider Audit Subtask 3** (dynamic TUI model choices) | Exact locations and replacement strategy documented | ~80 lines | `src/tui/viz_viewer/state.rs`, `src/commands/setup.rs` |
| **OpenRouter Phase 1** (wire endpoints to client) | Gap analysis complete, exact functions identified | ~100 lines | `src/commands/native_exec.rs`, `src/service/llm.rs` |

### Needs minor design refinement before building

| Design | What's ready | What's missing | Estimated gap |
|--------|-------------|----------------|--------------|
| **Compactor Phase 2** (per-turn injection) | Architecture specified | Tuning parameters, A/B comparison methodology | Small — can discover during Phase 1 validation |
| **Provider Audit Subtask 2** (wire provider to triage) | Dispatch points identified | Utility function design for provider-aware dispatch | Small — straightforward wrapper around existing `run_lightweight_llm_call` |
| **Temporal Nav Phase 2** (chat archive boundary) | UI spec complete | Compaction epoch tracking integration details | Small — depends on compactor Phase 1 for epoch semantics |
| **Coordinator identity** (graph-citizen Phase 1) | Role YAML, prompt decomposition described | Exact file decomposition of `build_system_prompt()` content | Small — mechanical extraction |

### Needs significant design work before building

| Design | What exists | What's missing | Risk |
|--------|-----------|----------------|------|
| **Coordinator as regular agent (Phase C)** | Architecture described | Detailed executor integration, daemon refactor spec | Medium — architectural change to coordinator process model |
| **Per-turn coordinator (Phase D / Compactor Phase 3)** | Architecture described | Multi-turn tool-use in `run_lightweight_llm_call`, latency validation | High — requires solving tool-use loop in per-turn model |
| **Multi-coordinator assignment** (graph-citizen Phase 5) | Conceptual design | Coordinator routing logic, context partitioning | High — depends on Phase 1-3 validation |
| **Temporal Nav Phase 3** (search) | Spec for trigram index, UI overlay | Trigram index implementation, incremental updates | Medium — ~400 lines, external dependency decisions |

---

## 3. Dependency Map

```
                    Provider Unification
                    ┌─────────────────────────────────────────┐
                    │ Subtask 1: Unify hardcoded fallbacks     │
                    │ Subtask 2: Wire provider to triage       │──┐
                    │ Subtask 3: Dynamic TUI model choices     │  │
                    │ Subtask 4: Deprecation warnings           │  │
                    └──────────────────┬──────────────────────┘  │
                                       │                          │
                    OpenRouter Integration                        │
                    ┌──────────────────▼──────────────────────┐  │
                    │ Phase 1: Wire endpoints to client        │◀─┘
                    │ Phase 2: CLI endpoint management          │
                    │ Phase 3: Endpoint-aware role routing      │
                    │ Phase 4: Streaming                        │
                    └─────────────────────────────────────────┘


                    Compactor + Coordinator
                    ┌─────────────────────────────────────────┐
                    │ Compactor Phase 1: MVP                   │
                    │   (module, prompt, config, crash recovery)│
                    └──────────────┬──────────────────────────┘
                                   │
              ┌────────────────────┼────────────────────────┐
              │                    │                         │
              ▼                    ▼                         ▼
   Compactor Phase 2      Coordinator Identity     Temporal Nav Phase 2
   (per-turn injection)   (graph-citizen Ph 1-2)   (chat archive boundary)
              │                    │
              │                    ▼
              │            Coordinator Evaluation
              │            (graph-citizen Ph 3)
              │                    │
              ▼                    ▼
   Coordinator as Regular Agent   Prompt Evolution
   (regular-agent Phase B-C)      (graph-citizen Ph 4)
              │
              ▼
   Per-Turn Coordinator ──────────────────────────────▶ Multi-Coordinator
   (regular-agent Phase D /                              (graph-citizen Ph 5)
    compactor Phase 3)


                    Temporal Navigation (mostly independent)
                    ┌─────────────────────────────────────────┐
                    │ Phase 1: Iteration history               │  (independent)
                    │ Phase 2: Chat archive boundary           │  (needs compactor Ph 1)
                    │ Phase 3: Search                          │  (independent)
                    │ Phase 4: Multi-coordinator tabs          │  (needs multi-coord)
                    │ Phase 5: Iteration diff                  │  (needs Phase 1)
                    └─────────────────────────────────────────┘
```

### Critical path

The longest dependency chain is:

**Compactor Phase 1 → Compactor Phase 2 → Coordinator as Regular Agent → Per-Turn Coordinator → Multi-Coordinator**

This is the path from current state to full model-agnostic, self-governing coordinator. Each step is independently valuable and shippable.

### Independence

These streams are **fully independent** of each other and can proceed in parallel:

1. Provider unification + OpenRouter
2. Compactor + Coordinator evolution
3. Temporal navigation Phase 1 (iteration history)
4. Temporal navigation Phase 3 (search)

---

## 4. Integration Risks

### 4.1 Coordinator refactor touches the same files from multiple designs

The compactor spec, coordinator-as-regular-agent, and coordinator-as-graph-citizen all modify `src/commands/service/coordinator_agent.rs` and `src/commands/service/coordinator.rs`. These are the two largest files in the codebase (~1630 and ~3351 lines respectively).

**Mitigation:** Strict serialization of work on these files. The compactor Phase 1 is additive (new module + small changes to existing files), so it should go first. The regular-agent refactor is subtractive (removes ~800 lines), so it should come after the compactor is validated.

### 4.2 Compactor design has two competing visions

`coordinator-as-regular-agent.md` describes era-based compaction where each coordinator era produces `era-{N}-summary.md`. The `compactor-agent-spec.md` describes a single rolling `context.md` updated periodically. These are different approaches:

- **Era-based**: Compaction happens at era boundaries (coordinator exit). Output is per-era.
- **Rolling**: Compaction happens every N turns. Output is a single continuously-updated file.

**Resolution:** The rolling compactor (compactor-agent-spec.md) is the simpler, more incremental approach and should be built first. Era-based compaction is a natural extension when the coordinator migrates to the regular-agent model — each era boundary triggers a compaction.

### 4.3 Provider unification could break existing setups

Changing hardcoded fallbacks from `"haiku"` / `"sonnet"` to `resolve_model_for_role()` changes behavior if users haven't configured `[models.*]` sections. The resolution order must preserve backward compatibility.

**Mitigation:** Set tier-based defaults in `resolve_model_for_role()` that match the current hardcoded values (triage → haiku, flip_inference → sonnet, etc.). Only behavior that changes is if the user has explicitly set `[models.triage]` — which is the desired behavior.

### 4.4 Temporal navigation Phase 2 depends on compactor semantics

The chat archive boundary is defined by "compaction epochs." If the compactor design changes significantly, Phase 2's epoch definition changes too.

**Mitigation:** Phase 1 (iteration history) has no dependency on the compactor. Build Phase 1 first, then Phase 2 after compactor Phase 1 ships.

### 4.5 Native executor and OpenRouter gaps overlap

The model provider audit identifies that the native executor doesn't use `resolve_model_for_role()`. The OpenRouter research identifies the same gap from a different angle. Both need the same fix.

**Mitigation:** Treat these as one workstream. Provider audit subtask 2 enables OpenRouter Phase 1 naturally.

---

## 5. Recommended Phases

### Phase A: Foundation (parallel tracks, no dependencies between them)

**Track 1: Provider Unification**
1. Subtask 1: Unify hardcoded model fallbacks through `resolve_model_for_role()` — ~50 lines
2. Subtask 3: Dynamic model choices in TUI and setup wizard — ~80 lines
3. Subtask 4: Deprecation warnings for legacy `agency.*_model` fields — ~30 lines

**Track 2: Compactor MVP**
1. Compactor Phase 1: New `src/service/compactor.rs` module, `wg compact` command, crash recovery replacement — ~400 lines new

**Track 3: TUI Iteration History**
1. Temporal Nav Phase 1: `IterationSnapshot` data model, iteration tag on `LogEntry`, TUI collapsed/expanded iterations — ~100 lines

These three tracks touch different files and can run fully in parallel.

**Deliverables:**
- All model dispatch uses unified `resolve_model_for_role()` path
- Compactor produces `context.md` and replaces crash recovery
- Cycle iterations show per-iteration history in TUI

### Phase B: Integration Layer (depends on Phase A)

**Track 1: Provider + OpenRouter**
1. Subtask 2: Wire provider to triage/checkpoint dispatch — ~60 lines
2. OpenRouter Phase 1: Wire endpoints to client creation — ~100 lines
3. OpenRouter Phase 2: CLI endpoint management (`--add-endpoint`, `--remove-endpoint`) — ~80 lines

**Track 2: Compactor Tuning + Coordinator Identity**
1. Compactor Phase 2: Per-turn context injection — ~30 lines changed
2. Coordinator Identity (graph-citizen Phase 1): Extract prompt to agency role files — ~50 lines + YAML files
3. Turn Recording (graph-citizen Phase 2): Coordinator task as cycle node, turn metadata logging — ~100 lines

**Track 3: TUI Temporal Features**
1. Temporal Nav Phase 2: Chat archive boundary with epoch navigation — ~130 lines
2. Temporal Nav Phase 3: `wg search` CLI and TUI search overlay — ~400 lines

Tracks 1 and 2 are independent. Track 3's Phase 2 depends on Track 2's compactor tuning (for epoch semantics).

**Deliverables:**
- OpenRouter usable for all dispatch roles via config
- Compactor injects context every turn; coordinator visible in graph
- Chat has archive boundaries; search works across all streams

### Phase C: Governance (depends on Phase B)

1. Coordinator Evaluation (graph-citizen Phase 3): Inline evaluation per turn using lightweight LLM — ~200 lines
2. Coordinator Prompt Evolution (graph-citizen Phase 4): Decompose prompt into evolvable files, wire `wg evolve` — ~150 lines
3. OpenRouter Phase 3: Endpoint-aware role routing — ~100 lines

**Deliverables:**
- Coordinator turns are evaluated; prompt improves from experience
- Full provider abstraction — any model can serve any role

### Phase D: Architecture Evolution (depends on Phase C)

1. Coordinator as Regular Agent (Phase B-C from that doc): Remove special-entity code, use normal executor path — net -550 lines
2. Per-Turn Coordinator (Phase D / Compactor Phase 3): Replace long-lived process with per-turn invocations — requires multi-turn tool-use in `run_lightweight_llm_call`

**Deliverables:**
- Coordinator uses the same executor path as worker agents
- Full model agnosticism — coordinator can run on any model/provider

---

## 6. Quick Wins

These can each be done in a single task right now, with minimal risk:

### 6.1 Unify hardcoded model fallbacks
**What:** Replace 5 hardcoded model fallbacks (`"haiku"`, `"sonnet"`, `"opus"`) in evaluate.rs, triage.rs, and coordinator.rs with `resolve_model_for_role()`.
**Why:** Eliminates the most common provider coupling point. Users who set `[models.triage]` etc. will actually see their config used.
**Files:** `src/commands/evaluate.rs` (lines 594-603), `src/commands/service/triage.rs` (line 406), `src/commands/service/coordinator.rs` (line 2056), `src/config.rs` (add tier defaults)
**Verify:** `cargo test` passes; `wg config show` displays all role models correctly

### 6.2 Dynamic model choices in TUI
**What:** Replace hardcoded `vec!["opus", "sonnet", "haiku"]` in 5 TUI locations with model registry lookup.
**Why:** Users who add models via the registry or TUI add-endpoint form see them in dropdowns.
**Files:** `src/tui/viz_viewer/state.rs` (5 locations), `src/commands/setup.rs` (line 224-226)
**Verify:** `cargo build` passes; TUI config tab shows models from registry

### 6.3 Iteration history on cycle tasks
**What:** Add `IterationSnapshot` struct, `iteration` field on `LogEntry`, snapshot on cycle iteration. Display in TUI detail panel.
**Why:** Cycles currently lose per-iteration context. This is the most structurally meaningful TUI improvement.
**Files:** `src/graph.rs` (add structs, ~50 lines), `src/tui/viz_viewer/render.rs` (detail panel, ~50 lines)
**Verify:** `cargo test` passes; cycle tasks in TUI show collapsed iterations

### 6.4 `wg compact` command (compactor bootstrap)
**What:** Create `src/service/compactor.rs` with `build_compactor_prompt()` and `run_compaction()`. Create `src/commands/compact.rs` for `wg compact` CLI.
**Why:** Enables manual compaction and validates the compactor design before wiring into the daemon loop.
**Files:** New: `src/service/compactor.rs` (~300 lines), `src/commands/compact.rs` (~50 lines). Modify: `src/config.rs`, `src/cli.rs`, `src/main.rs`
**Verify:** `wg compact` runs and produces `.workgraph/compactor/context.md`; `cargo test` passes

### 6.5 Wire endpoint API keys to client creation
**What:** Make `create_client()` in native_exec.rs and `call_openai_native()` in llm.rs check `config.llm_endpoints.endpoints` for matching endpoint by provider, using the endpoint's `api_key` and `url`.
**Why:** Unblocks OpenRouter usage without manual env var exports. Endpoints configured via TUI become functional.
**Files:** `src/commands/native_exec.rs`, `src/service/llm.rs`
**Verify:** Configure an endpoint via TUI or TOML; native executor uses its API key without `OPENROUTER_API_KEY` env var

---

## 7. Open Questions

### Requiring decisions before implementation

1. **Compactor vs. era-based compaction: which first?** The compactor spec (rolling `context.md`) and the regular-agent design (per-era `era-N-summary.md`) describe different approaches. **Recommendation:** Build rolling compactor first; era-based is a refinement that comes with the regular-agent migration.

2. **Coordinator model: should it default to opus?** The graph-citizen design suggests configurable with project model as default. The regular-agent design suggests coordinator-specific override. **Recommendation:** Keep the existing `[coordinator] model` field. If unset, fall back to `agent.model`. No change needed.

3. **OpenRouter caching cost:** OpenRouter doesn't support Anthropic prompt caching. Using it for high-volume dispatch (triage, evaluation) costs more. **Decision needed:** Should per-role provider routing be the default, or should there be a cost-awareness mechanism?

4. **Temporal navigation search: external dependency?** Phase 3 proposes a trigram index with no external dependencies. An alternative is using an existing crate (e.g., `tantivy` for full-text search). **Decision needed:** Custom trigram vs. crate dependency.

### Answerable through implementation experience

5. **Compactor token budget:** 3000 tokens is the starting point. Should it auto-scale? Will be answered by running the compactor on real projects.

6. **Coordinator evaluation frequency:** `every_5` is the proposed default. May need tuning based on cost/signal ratio.

7. **Per-turn coordinator latency:** The per-turn model (Compactor Phase 3) adds cold-start latency. Whether this is acceptable depends on coordinator workload patterns — answerable through Phase 1-2 operation.

### Already decided (from design documents)

- Compactor uses `run_lightweight_llm_call()`, not a full agent spawn
- Coordinator evaluation uses a 5-dimension rubric (decomposition, dependency_accuracy, description_quality, user_responsiveness, efficiency)
- Provider unification uses the existing `DispatchRole` enum — no new roles needed
- Temporal navigation uses per-stream epochs, not a global timeline
- Cycles use `IterationSnapshot` for per-iteration archival
