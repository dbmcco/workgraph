# Coordinator as Graph Citizen: Evaluation, Assignment, and Prompt Evolution per Turn

## Status: Design (March 2026)

## Summary

The coordinator currently sits above the graph as a privileged entity — it manages tasks but is never itself managed. This design makes the coordinator a first-class graph citizen, subject to the same lifecycle machinery (assignment, evaluation, prompt evolution) as every other agent. Each coordinator "turn" (processing a user message and responding) becomes a unit of work that is represented, evaluated, and improved through the graph.

This builds on three prior design documents:
- `coordinator-as-regular-agent.md` — Coordinator as a regular looping agent with context compaction
- `coordinator-agent-prompt.md` — System prompt, context injection, tool definitions
- `self-hosting-architecture.md` — End-to-end self-hosting vision

Those documents address *how* the coordinator runs (executor path, compaction, prompts). This document addresses *how the coordinator is governed* — evaluation, assignment, and prompt evolution.

## 1. Current State

### What the coordinator does today

The coordinator is implemented across two files:

1. **`coordinator.rs` (3351 lines)** — The tick loop: cleanup dead agents, cycle iteration, wait evaluation, resurrection, auto-assign, auto-evaluate, FLIP verification, spawn agents for ready tasks. Pure Rust, no LLM.

2. **`coordinator_agent.rs` (1630 lines)** — A persistent Claude CLI subprocess with stream-json I/O, crash recovery, event logging, and context injection. This is the LLM session that interprets user messages.

### What's missing

| Capability | Worker Agents | Coordinator |
|-----------|---------------|-------------|
| Assignment (which agent profile?) | `build_auto_assign_tasks()` → lightweight LLM call | Hardcoded — always the same prompt |
| Evaluation (was the work good?) | `.evaluate-*` tasks, 4-dimension rubric, FLIP | None |
| Prompt evolution | `wg evolve` → mutation, crossover, gap-analysis | Static system prompt in `build_system_prompt()` |
| Graph visibility | Task node with logs, artifacts, status | Hidden daemon detail |
| Identity (role + tradeoff) | Agency system: role + tradeoff → agent hash | None — no agency identity |

The coordinator is the only entity in the system that creates and governs agents but is not itself governed. This asymmetry means:
- Bad coordinator decisions (poor decomposition, wrong dependencies, unclear descriptions) go undetected
- The coordinator prompt never improves from experience
- In a multi-coordinator world, there's no basis for choosing which coordinator handles what

## 2. Proposed Architecture

### 2.1 Coordinator Turn as Graph Node

Each coordinator turn is a discrete unit of work. A "turn" is defined as: the coordinator receives a trigger (user message, graph event, periodic tick), processes it, and produces output (tasks created, messages sent, responses given).

**Representation: A cycle task with `--max-iterations`.**

```
.coordinator-turn (cycle, max_iterations = unlimited)
  └── Each iteration = one coordinator invocation
      - Input: user message(s), graph state, event log
      - Output: tasks created, responses sent, graph mutations
      - Metadata: token usage, latency, decisions made
```

This fits the existing cycle machinery. The coordinator task is the cycle header. Each turn is an iteration. The task is never "done" in the normal sense — it runs indefinitely (or until the service stops).

**Why a cycle, not individual tasks per turn:**
- Individual tasks per turn would create hundreds of nodes, polluting the graph
- A cycle with iterations is the idiomatic workgraph pattern for repeating work
- Iteration metadata (token_usage, loop_iteration) already exists on tasks
- The cycle can use `--max-iterations` for cost-capped sessions

**Turn metadata** is recorded in the task log:

```
[2026-03-05T10:00:00Z] Turn 47: processed 1 user message, created 3 tasks
  (auth-research, auth-impl, auth-test), responded to user.
  Tokens: 12k in / 3k out. Latency: 4.2s.
```

### 2.2 Evaluation Per Turn

Each coordinator turn is evaluated, but not by spawning a full agent per turn (that would be prohibitively expensive). Instead:

**Lightweight inline evaluation** — the same mechanism used for assignment (`run_lightweight_llm_call`). After each turn, the coordinator tick:

1. Captures the turn's inputs and outputs
2. Builds an evaluation prompt
3. Runs a single LLM call (haiku-class model, ~500 tokens out)
4. Records the evaluation

**Evaluation frequency** is configurable to manage cost:

```toml
[coordinator]
eval_frequency = "every_5"  # Options: "every", "every_5", "every_10", "sample_20pct", "none"
```

- `every` — evaluate every turn (highest signal, highest cost)
- `every_5` — evaluate every 5th turn (default, good balance)
- `every_10` — every 10th turn
- `sample_20pct` — random 20% sample
- `none` — disable coordinator evaluation

#### Evaluation Rubric

The coordinator evaluation uses a different rubric than worker agents, because the work is different:

| Dimension | Weight | Description |
|-----------|--------|-------------|
| `decomposition` | 30% | Did it break the request into the right tasks? Right granularity? Right boundaries? |
| `dependency_accuracy` | 25% | Were dependencies set correctly? No missing edges? No unnecessary serialization? Same-file tasks sequential? |
| `description_quality` | 20% | Are task descriptions clear, complete, and actionable? Do they include validation criteria? |
| `user_responsiveness` | 15% | Was the response to the user helpful, accurate, and appropriately detailed? |
| `efficiency` | 10% | Did the coordinator avoid unnecessary work? Redundant tasks? Over-decomposition? |

**Evaluation prompt template:**

```
You are evaluating a coordinator turn. The coordinator received input and produced output.

## Input
- User message: {user_message}
- Graph state at turn start: {graph_summary}
- Recent events: {events}

## Output
- Tasks created: {tasks_created_with_descriptions}
- Dependencies set: {dependency_edges}
- Response to user: {user_response}
- Other actions: {other_mutations}

## Evaluation Criteria
Score each dimension 0.0-1.0:

1. **Decomposition (30%)**: Were the tasks well-scoped? Right number? Right boundaries?
2. **Dependency accuracy (25%)**: Correct edges? No cycles that shouldn't be there? Same-file work serialized?
3. **Description quality (20%)**: Clear? Actionable? Validation criteria included?
4. **User responsiveness (15%)**: Helpful? Accurate? Right level of detail?
5. **Efficiency (10%)**: Minimal unnecessary work? No redundant tasks?

Output JSON:
{
  "score": <float 0.0-1.0>,
  "dimensions": {
    "decomposition": <float>,
    "dependency_accuracy": <float>,
    "description_quality": <float>,
    "user_responsiveness": <float>,
    "efficiency": <float>
  },
  "notes": "<brief explanation of strengths and weaknesses>"
}
```

**Where evaluations are stored:** Same location as worker evaluations — `.workgraph/agency/evaluations/eval-coordinator-turn-{N}-{timestamp}.json`. This feeds into the same `wg agency stats` pipeline.

#### Deferred Deep Evaluation

For important turns (those that create >5 tasks, or where user explicitly requests review), a full `.evaluate-coordinator-turn-{N}` task can be created and dispatched to an evaluator agent. This is the same pattern as worker evaluation but triggered selectively rather than on every turn.

### 2.3 Assignment

In a single-coordinator world, assignment is trivial: the one coordinator gets all coordination work. But the architecture should support multi-coordinator scenarios from the start.

#### Assignment Flow

```
                    ┌─────────────────────────┐
                    │    Service Daemon         │
                    │  (pure Rust, no LLM)      │
                    └────────────┬──────────────┘
                                 │
                    ┌────────────▼──────────────┐
                    │  Coordinator Assignment    │
                    │  (lightweight LLM call)    │
                    │                            │
                    │  Input:                    │
                    │  - User message/event type │
                    │  - Available coordinators  │
                    │  - Recent performance      │
                    │                            │
                    │  Output:                   │
                    │  - Which coordinator       │
                    └────────────┬──────────────┘
                                 │
              ┌──────────────────┼──────────────────┐
              ▼                  ▼                   ▼
       Coordinator A      Coordinator B       Coordinator C
       (Architect focus)  (Ops focus)         (General)
```

**Who assigns the coordinators?** The service daemon, using the same lightweight LLM assignment mechanism already used for worker tasks. The daemon is the one entity that *must* be privileged — it's the bootstrapping base case (see Section 2.5).

**Coordinator agent profiles** are regular agency entities:

```yaml
# .workgraph/agency/roles/coordinator-architect.yaml
name: "Architect Coordinator"
description: "Coordinates architectural and design work. Excels at decomposing complex systems into well-bounded tasks with correct dependency structures."
skills:
  - system-design
  - task-decomposition
  - dependency-analysis
desired_outcome: "Well-structured task graphs that enable parallel execution with minimal rework"

# .workgraph/agency/roles/coordinator-ops.yaml
name: "Operations Coordinator"
description: "Coordinates operational work: deployments, monitoring, incident response. Prefers small, fast tasks with clear rollback plans."
skills:
  - operations
  - incident-response
  - monitoring
desired_outcome: "Rapid, safe operational changes with explicit rollback procedures"
```

These are paired with tradeoffs (Careful, Fast, Thorough) to produce coordinator agents, exactly like worker agents. The assignment LLM sees the user message and coordinator catalog and picks the best match.

**Single-coordinator shortcut:** When only one coordinator agent profile exists, skip the assignment LLM call entirely. This is the common case and avoids unnecessary cost.

### 2.4 Prompt Evolution

The coordinator's prompt is currently hardcoded in `build_system_prompt()` (`coordinator_agent.rs`). The proposed design makes it a living document that evolves through the same feedback loop as worker agent prompts.

#### Where the prompt lives

```
.workgraph/agency/
├── roles/
│   ├── coordinator-{hash}.yaml       # Coordinator role (evolved)
│   └── ...
├── coordinator-prompt/
│   ├── base-system-prompt.md         # Base coordinator system prompt
│   ├── behavioral-rules.md           # "Never implement", decomposition rules
│   ├── common-patterns.md            # Few-shot examples
│   └── evolved-amendments.md         # Amendments from evolution
└── evolver-skills/
    └── coordinator-evolution.md       # Strategy-specific guidance for coordinator evolution
```

The coordinator prompt is composed from:
1. **Base system prompt** — The core identity and instructions (from `base-system-prompt.md`)
2. **Behavioral rules** — Hard constraints ("never implement code")
3. **Common patterns** — Few-shot examples of good coordinator behavior
4. **Evolved amendments** — Additions/modifications from the evolution loop
5. **Role identity** — From the assigned coordinator agent's role + tradeoff

This is the same composition pattern used for worker agents (role skills → resolved into prompt), but with coordinator-specific content.

#### Evolution Feedback Loop

```
┌─────────────┐     ┌───────────┐     ┌────────────┐     ┌──────────┐
│ Coordinator │────>│ Turn gets │────>│ Evaluation  │────>│ Evolve   │
│   acts      │     │ evaluated │     │ accumulates │     │ prompt   │
│             │     │ (inline   │     │ (10+ evals  │     │ (modify  │
│ (turn N)    │     │  or deep) │     │  triggers)  │     │  rules)  │
└─────────────┘     └───────────┘     └────────────┘     └──────────┘
       ▲                                                      │
       └──────────────────────────────────────────────────────┘
                    evolved prompt feeds into next turn
```

**When evolution triggers:** Evolution runs when sufficient evaluations have accumulated. The existing `wg evolve` command works unchanged — it reads evaluations for all agents (including coordinator agents) and proposes mutations.

**What can evolve:**
- Common patterns (better few-shot examples based on what worked)
- Behavioral rules (new rules discovered from evaluation feedback)
- Decomposition heuristics (better sizing/boundary guidance)
- Description templates (what makes a good task description)

**What cannot evolve (hard constraints):**
- "Never implement code" — This is a structural constraint, not a behavioral preference
- Tool access list — Determined by security policy, not performance
- Graph interaction semantics — `wg add`, `wg edit`, etc. are stable API

The distinction is encoded in the prompt composition: `base-system-prompt.md` and `behavioral-rules.md` are immutable anchors; `evolved-amendments.md` and `common-patterns.md` are mutable.

#### Coordinator-Specific Evolution Strategies

Add a new evolver skill document:

```markdown
# Coordinator Evolution Strategy (.workgraph/agency/evolver-skills/coordinator-evolution.md)

## What to look for in coordinator evaluations

1. **Recurring low decomposition scores**: The coordinator may be creating too many or too few tasks.
   - If over-decomposing: add a rule about minimum task size
   - If under-decomposing: add examples of good decomposition

2. **Dependency errors**: Missing edges or unnecessary serialization.
   - Add heuristics: "When tasks touch the same file, they MUST be sequential"
   - Add anti-patterns: "Don't create a linear pipeline when tasks are independent"

3. **Poor description quality**: Tasks without validation criteria, vague scope.
   - Strengthen the description template
   - Add examples of good vs. bad descriptions

4. **User responsiveness issues**: Over-verbose or under-informative responses.
   - Adjust tone/length guidance

## Operations available
- modify_role: Adjust the coordinator role's skills, description, or outcome
- create_role: Create a specialized coordinator variant (e.g., ops-focused)
- modify_tradeoff: Adjust coordinator tradeoff constraints
```

### 2.5 Bootstrapping

The bootstrapping problem: the coordinator can't be assigned by a coordinator. Who starts the system?

**The service daemon is the base case.**

The service daemon (`src/commands/service/mod.rs`) is pure Rust code with no LLM. It is the one permanently privileged entity in the system. Its responsibilities at bootstrap:

1. **Create the coordinator task** (if it doesn't exist) — a cycle task tagged `coordinator-loop`
2. **Assign the initial coordinator** — if only one coordinator agent exists, assign it directly (no LLM call). If multiple exist, use a lightweight LLM call (same as worker assignment).
3. **Spawn the coordinator agent** — via the normal executor path
4. **Monitor and restart** — detect coordinator crashes, trigger compaction/restart

The daemon's dispatching logic (the tick loop in `coordinator_tick()`) remains pure Rust. It doesn't need an LLM to decide *whether* to spawn agents — that's deterministic. The LLM is only needed for *which* agent to assign, and even that can be skipped in the common single-agent case.

**Bootstrap sequence:**

```
1. `wg service start`
2. Daemon checks for coordinator task → creates if missing
3. Daemon checks for coordinator agent profiles → uses default if none
4. Daemon spawns coordinator via normal executor path
5. Coordinator begins processing user messages and graph events
6. After N turns, coordinator evaluations accumulate
7. `wg evolve` (manual or scheduled) proposes prompt improvements
8. Next coordinator era uses evolved prompt
```

### 2.6 Self-Similar Architecture

The system is self-similar at every level:

```
Level 0: Service Daemon (pure Rust, no LLM)
  │
  ├── Assigns coordinators (lightweight LLM call or direct)
  ├── Monitors coordinator health
  └── Triggers compaction/restart
  │
Level 1: Coordinator Agent (LLM session)
  │
  ├── Assigned by daemon (via agency system)
  ├── Evaluated per turn (lightweight or deep)
  ├── Prompt evolves (via wg evolve)
  └── Each turn is a graph iteration
  │
Level 2: Worker Agents (LLM sessions)
  │
  ├── Assigned by coordinator/daemon (via agency system)
  ├── Evaluated on completion (auto-evaluate)
  ├── Prompt evolves (via wg evolve)
  └── Each task is a graph node
  │
Level 3: Meta-Agents (evaluators, assigners, evolvers)
  │
  ├── Assigned by daemon (fixed profiles)
  ├── Could be evaluated (configurable, disabled by default)
  └── Infinite regress is stopped by configuration
```

**Stopping infinite regress:** The `eval-scheduled` tag and the `is_system_task()` check already prevent meta-tasks from spawning meta-meta-tasks. The same mechanism applies to coordinator evaluation — coordinator eval tasks are tagged `evaluation` and `agency`, so they don't get auto-evaluated themselves.

## 3. Overhead Analysis

### Cost per turn

| Component | Model | Tokens | Cost (approx.) |
|-----------|-------|--------|----------------|
| Coordinator turn | opus | ~15k in, 4k out | ~$0.15 |
| Inline evaluation | haiku | ~2k in, 500 out | ~$0.001 |
| Deep evaluation (selective) | haiku | ~5k in, 1k out | ~$0.005 |
| Assignment (multi-coordinator) | haiku | ~1k in, 200 out | ~$0.0005 |

**Overhead of evaluation: ~0.7% per turn** (inline eval cost / turn cost). This is negligible.

**Overhead of assignment:** Zero in single-coordinator mode (direct assignment). ~0.3% per turn in multi-coordinator mode. Also negligible.

**Overhead of evolution:** Evolution runs periodically (not per-turn), so its cost amortizes across many turns. A typical evolution run costs ~$0.10 and improves all subsequent turns.

### When to disable

For cost-sensitive projects or rapid prototyping:

```toml
[coordinator]
evaluation = false        # Disable coordinator evaluation entirely
assignment = "direct"     # Skip assignment LLM, use default coordinator
```

This reduces the coordinator to today's behavior while preserving the architecture for when governance is desired.

## 4. Implementation Approach

### Phase 1: Coordinator Identity (Low Risk)

**Goal:** Give the coordinator an agency identity without changing behavior.

1. Create a coordinator role in `.workgraph/agency/roles/`:
   ```yaml
   name: "Coordinator"
   description: "Persistent coordinator that interprets user intent and manages the task graph"
   skills:
     - task-decomposition
     - dependency-analysis
     - user-communication
   desired_outcome: "Well-structured task graphs that enable parallel execution"
   ```

2. Pair with a tradeoff to create a coordinator agent profile

3. Move `build_system_prompt()` content into the role's skill files

4. `build_system_prompt()` reads from skill files instead of hardcoded string

**No behavioral change.** The coordinator still runs identically, but its prompt comes from the agency system.

### Phase 2: Turn Recording (Low Risk)

**Goal:** Record coordinator turns as graph iterations.

1. Create the coordinator task on service start:
   ```rust
   if !graph.get_task(".coordinator").is_some() {
       graph.add_node(Node::Task(Task {
           id: ".coordinator".to_string(),
           title: "Coordinator".to_string(),
           tags: vec!["coordinator-loop".to_string()],
           cycle_config: Some(CycleConfig { max_iterations: None, .. }),
           ..Default::default()
       }));
   }
   ```

2. After each coordinator turn (in `process_chat_inbox` or the coordinator agent), log the turn metadata:
   - Tasks created
   - User response summary
   - Token usage
   - Latency

3. Increment `loop_iteration` on each turn.

**Minimal behavioral change.** The coordinator task is visible in the graph but doesn't affect dispatching.

### Phase 3: Inline Evaluation (Medium Risk)

**Goal:** Evaluate coordinator turns using lightweight LLM calls.

1. Add `eval_frequency` to coordinator config
2. After eligible turns, build the evaluation prompt from turn metadata
3. Run `run_lightweight_llm_call` with the coordinator evaluation rubric
4. Record the evaluation in `.workgraph/agency/evaluations/`
5. Propagate scores to the coordinator agent's performance record

**Behavioral change:** Small additional latency (100-500ms) on evaluated turns. No user-visible change.

### Phase 4: Prompt Evolution (Medium Risk)

**Goal:** Make the coordinator prompt evolvable.

1. Decompose `build_system_prompt()` into composable files (base, rules, patterns, amendments)
2. Add `coordinator-evolution.md` to evolver skills
3. Wire `wg evolve` to include coordinator evaluations
4. The evolver proposes modifications to `evolved-amendments.md` and `common-patterns.md`

**Behavioral change:** After evolution, the coordinator behaves differently (hopefully better). This is the same risk as worker agent evolution — mitigated by lineage tracking and `--dry-run`.

### Phase 5: Multi-Coordinator Assignment (Higher Risk)

**Goal:** Support multiple coordinator profiles with dynamic assignment.

1. Create multiple coordinator role variants
2. Add coordinator assignment to the daemon's dispatch logic
3. Route user messages to the best-matching coordinator

**Behavioral change:** Different coordinator agents may handle different messages. Requires the multi-coordinator design (separate task dependency) to be implemented first.

## 5. Interaction with Prior Designs

### coordinator-as-regular-agent.md

That design addresses *how* the coordinator runs (era-based compaction, regular executor path). This design addresses *how the coordinator is governed* (evaluation, assignment, evolution). They are complementary:

- **When compaction triggers:** Each era boundary is a natural evaluation point. The compaction task's quality can be evaluated (as that doc already describes).
- **Era N+1 gets evolved prompt:** After evolution, the next coordinator era uses the updated prompt.
- **Compaction agent is also governable:** The compaction agent's role/tradeoff feeds into the same evolution loop.

### Multi-coordinator design (pending)

This design provides the governance layer for multiple coordinators. The multi-coordinator design (tree-structured shared context) provides the coordination layer. They combine:

- **This design:** How coordinators are assigned, evaluated, and improved
- **Multi-coordinator design:** How coordinators share context and divide work

### Existing agency system

This design makes the coordinator a consumer of the agency system, not an extension of it. No new agency primitives are needed:

- **Roles** — Already support coordinator roles (just create one)
- **Tradeoffs** — Already work for coordinators
- **Agents** — Coordinator agents are role + tradeoff pairs, same as worker agents
- **Evaluations** — Same storage, different rubric dimensions
- **Evolution** — Same evolver, different skill document

The only new configuration is:

```toml
[coordinator]
eval_frequency = "every_5"   # How often to evaluate turns
assignment = "auto"          # "auto" (LLM), "direct" (single coordinator), "none"
```

## 6. Recommendation

**Implement Phases 1-3 first.** These are low-risk, independent of other designs, and provide immediate value:

- Phase 1 (identity) removes hardcoded prompts and makes the coordinator visible in the agency system
- Phase 2 (turn recording) provides observability into coordinator behavior
- Phase 3 (inline evaluation) starts generating the data needed for evolution

Phases 4-5 require more accumulated evaluation data and can be deferred until Phases 1-3 prove their value.

**Key insight:** The coordinator is already governed by the tick loop. Making that governance explicit (evaluation + evolution) is a matter of wiring, not architecture. The agency system already has all the primitives needed — the coordinator just needs to participate.

## 7. Open Questions

1. **Should coordinator evaluation block the next turn?** No. Evaluation is async — it runs after the turn completes and doesn't delay the next user interaction. The evaluation result is available for the *evolver*, not for the immediate next turn.

2. **How does evaluation interact with the coordinator agent's self-assessment?** The coordinator could include a self-assessment in its turn output ("I think I decomposed this well because..."). This could inform the evaluation but shouldn't replace it — external evaluation prevents self-serving bias.

3. **Can the coordinator opt into deeper evaluation for high-stakes turns?** Yes. The coordinator agent could tag a turn as `needs-deep-eval`, and the daemon would create a full `.evaluate-coordinator-turn-{N}` task instead of using inline evaluation.

4. **How much historical context should the evaluator see?** For inline evaluation: just the current turn (inputs + outputs). For deep evaluation: the last 5 turns for trend detection. The evaluator should not need the full conversation history — that's the coordinator's job.

5. **Should evolved amendments have a cooling-off period?** After evolution proposes a change, should it be applied immediately or held for review? Recommendation: apply immediately but track lineage, so it can be reverted if subsequent evaluations degrade.
