# Self-Healing Task Graph: Automatic Failure Diagnosis and Remediation

## Status: Design (March 2026)

## Summary

When a task fails, the system currently marks it as failed and stops. The human must notice, diagnose, and fix. This design closes that gap: the coordinator diagnoses failures, selects a remediation strategy, creates remediation tasks wired into the graph, and retries automatically.

The key insight: **remediation is a graph operation, not a special case.** A `.remediate-*` task is just another task — it runs, produces output, and when it completes, the original task becomes ready again with the remediation's context injected.

---

## Worked Example: `spark-deep-synthesis`

This failure motivates every decision below. The task tried to read 97 documents in a single agent context and hit "Prompt is too long."

**What happened:**
1. Agent spawned for `spark-deep-synthesis`
2. Agent tried to read all 97 input documents
3. Claude CLI returned "Prompt is too long" error
4. Agent called `wg fail spark-deep-synthesis --reason "Prompt is too long"`
5. Task sat in `Failed` status until a human noticed

**What should have happened:**
1. Coordinator detects `spark-deep-synthesis` failed
2. Diagnosis LLM call reads the failure reason + output log tail
3. Diagnosis classifies this as **context overflow** → strategy: **task decomposition**
4. Coordinator creates `.remediate-spark-deep-synthesis` with description:
   ```
   The task spark-deep-synthesis failed with "Prompt is too long" because it tried
   to process 97 documents in a single agent session. Decompose this task:
   1. Create N parallel subtasks that each read a subset of documents
   2. Create a synthesis task that depends on all subtasks
   3. Wire the synthesis task --before spark-deep-synthesis
   ```
5. `.remediate-spark-deep-synthesis` runs, creates the fan-out subtasks
6. When `.remediate-*` completes, `spark-deep-synthesis` is retried with the decomposed results as context
7. On retry, the task description includes a `## Remediation Context` section explaining the decomposition

This example is referenced throughout the design to ground abstract decisions.

---

## 1. Failure Classification Taxonomy

Diagnosis is a **lightweight LLM call** (like triage), not a full agent task. The coordinator already has everything needed: the failure reason string and the agent's output log tail. A haiku/sonnet-class call is sufficient to classify the failure.

### Classification Categories

| Category | Signal Patterns | Remediation Strategy | Example |
|----------|----------------|---------------------|---------|
| **Transient** | `rate limit`, `timeout`, `connection reset`, `503`, `SIGKILL` (with no output) | Auto-retry with backoff | Network blip during API call |
| **Context overflow** | `Prompt is too long`, `context window`, `token limit` | Task decomposition | `spark-deep-synthesis` reading 97 docs |
| **Build failure** | `cargo test failed`, `compilation error`, `type mismatch`, exit code from verify command | Prerequisite injection (fix task) | Test regression from upstream change |
| **Missing dependency** | `file not found`, `No such file`, `command not found`, `API key not set` | Prerequisite injection | Task needs a file another task should have created |
| **Agent confusion** | `I'm not sure how to`, agent loops, empty output, wrong files modified | Description rewrite | Ambiguous task description |
| **Unfixable** | `wg fail` with explicit "cannot complete" language, permissions error, fundamental design issue | Escalation to human | Task requires human judgment |

### Diagnosis Flow

```
Task fails → coordinator reads failure_reason + output log tail (last 50KB)
           → lightweight LLM call (DispatchRole::Diagnostician)
           → returns JSON: { "category": "...", "confidence": 0.9,
                             "reasoning": "...", "suggested_action": "..." }
           → coordinator applies remediation strategy for that category
```

The LLM prompt includes:
- Task ID, title, description (truncated to 2000 chars)
- `failure_reason` field from the task
- Last 50KB of agent output log (reusing `read_truncated_log` from triage)
- Task's `retry_count` and `max_retries`
- Tags and verify command (for context)

### Diagnosis Prompt Structure

```
You are a failure diagnostician for a software development task coordinator.

A task has failed. Examine the failure reason and agent output to classify the failure.

## Task
- ID: {task_id}
- Title: {task_title}
- Description: {task_desc_truncated}
- Retry count: {retry_count}
- Verify command: {verify_cmd}

## Failure Reason
{failure_reason}

## Agent Output (last 50KB)
{output_log_tail}

Respond with ONLY a JSON object:
{
  "category": "<transient|context_overflow|build_failure|missing_dependency|agent_confusion|unfixable>",
  "confidence": <0.0-1.0>,
  "reasoning": "<one paragraph explaining why this category>",
  "suggested_action": "<concrete next step for remediation>"
}
```

### Confidence Threshold

If `confidence < 0.6`, the system escalates to human rather than attempting automatic remediation. This prevents the system from making bad guesses on ambiguous failures.

### Why Not a Full Agent Task?

Diagnosis must be fast (seconds, not minutes) and cheap. It's a classification task, not a creative task. The triage system already proves this pattern works — triage uses a single LLM call to classify agent death outcomes (done/continue/restart). Diagnosis follows the same pattern but classifies failure types instead.

For `spark-deep-synthesis`: the failure reason "Prompt is too long" is unambiguous. A haiku-class model can classify this as `context_overflow` with high confidence in under 2 seconds.

---

## 2. Remediation Strategies

Each failure category maps to a specific remediation strategy. The strategy determines what the `.remediate-*` task does and how it wires back into the graph.

### 2.1 Auto-Retry (Transient Failures)

**No remediation task created.** The coordinator simply retries the failed task with exponential backoff.

**Mechanism:**
1. Coordinator identifies `category: transient`
2. If `retry_count < max_transient_retries` (default: 3):
   - Reset task to `Open` (like `wg retry`)
   - Set a `wait_until` timestamp: `now + backoff_seconds`
   - Log: "Transient failure, auto-retrying in {N}s (attempt {M}/{max})"
3. If retries exhausted: escalate to human

**Backoff schedule:** 30s, 120s, 300s (configurable via `transient_backoff_schedule`).

**Integration with existing `WaitSpec`:** The `wait_until` field already exists on tasks (from `wg-wait-design.md`). Transient retry reuses this mechanism — the coordinator's existing phase 2.7 (`evaluate_waiting_tasks`) handles the un-waiting.

For `spark-deep-synthesis`: This wouldn't apply — "Prompt is too long" is not transient.

### 2.2 Task Decomposition (Context Overflow)

**Remediation task decomposes the original into smaller pieces.**

**Mechanism:**
1. Create `.remediate-{task-id}` with description explaining:
   - What failed and why (context overflow)
   - The original task's description
   - Instructions to decompose: create N subtasks that split the input, plus an integration task
   - The integration task should be wired `--before {task-id}` so its output feeds back
2. Wire `.remediate-{task-id}` into the graph (see Section 3)
3. When `.remediate-*` completes, its subtasks run, the integration task synthesizes, and the original task retries with synthesized context

**Remediation task description template:**
```
## Remediation: Context Overflow

Task `{task_id}` ("{task_title}") failed with: {failure_reason}

### Original Task Description
{original_description}

### What to do
The original task exceeded the context window. Decompose it:
1. Analyze the task to identify natural partition points
2. Create parallel subtasks using `wg add "Part N: ..." --after .remediate-{task-id}`
3. Create a synthesis task that depends on all parts: `wg add "Synthesize: ..." --after part-1,part-2,...`
4. Wire the synthesis task before the original: the original task will retry after synthesis completes

### Constraints
- Each subtask should be small enough for a single agent session
- Subtasks should be independent (no file conflicts)
- The synthesis task must produce a single coherent output
```

For `spark-deep-synthesis`: The remediation agent would create 5-10 `read-batch-N` tasks (each reading ~10-20 documents), plus a `synthesize-spark` task that merges the batches, wired `--before spark-deep-synthesis`.

### 2.3 Prerequisite Injection (Build Failure / Missing Dependency)

**Remediation task fixes the precondition that caused failure.**

**Mechanism:**
1. Create `.remediate-{task-id}` with description containing:
   - The build error or missing dependency details
   - The specific error output from the agent's log
   - Instructions to fix the underlying issue
2. The remediation task runs and makes the fix (e.g., fixes a compile error, creates a missing file)
3. The original task retries with the fix in place

**Build failure example:**
```
## Remediation: Build Failure

Task `implement-auth` failed because `cargo test` shows 3 failures in the auth module.
Error output:
  test auth::test_token_refresh ... FAILED
  test auth::test_token_expiry ... FAILED
  ...

Fix the failing tests. The errors may be caused by:
- A regression from a parallel task's changes
- An incomplete implementation in the original task
- A test that depends on state from another module

Run `cargo test` to reproduce, fix the failures, commit.
```

**Missing dependency example:**
```
## Remediation: Missing Dependency

Task `generate-report` failed because `data/metrics.json` does not exist.
This file should have been produced by an upstream task.

Check if the upstream task that produces `data/metrics.json` completed successfully.
If not, create it. If so, investigate why the file is missing.
```

### 2.4 Description Rewrite (Agent Confusion)

**Remediation task rewrites the original task's description.**

**Mechanism:**
1. Create `.remediate-{task-id}` with instructions to:
   - Read the original task description and the agent's confused output
   - Identify what was ambiguous or misleading
   - Rewrite the description with clearer instructions
   - Apply the rewrite using `wg edit {task-id} -d "new description"`
2. When the remediation completes, the original task retries with the improved description

**This is the trickiest strategy** because the remediation agent must understand both what the task intended and why the previous agent got confused. The diagnosis LLM's `suggested_action` field provides guidance.

For `spark-deep-synthesis`: This wouldn't apply — the description was clear, the problem was scale.

### 2.5 Escalation (Unfixable / Low Confidence)

**No remediation task. Alert the human.**

**Mechanism:**
1. Log a prominent message: `[coordinator] ESCALATION: Task '{task-id}' failed and automatic remediation cannot help: {reasoning}`
2. Add a log entry to the task: "Escalated to human — automatic remediation declined (category: {category}, confidence: {confidence})"
3. Pause the task (`task.paused = true`) to prevent the circuit breaker from firing on repeated failures
4. If `wg msg` is available to a human channel, send a message

**Escalation triggers:**
- Diagnosis confidence < 0.6
- Category is `unfixable`
- Max remediation attempts reached (see Section 5)
- Budget cap exceeded (see Section 5)

---

## 3. The `--before` Wiring Pattern

This is the core graph operation. Remediation works by inserting a task into the dependency graph between a failed task and its retry.

### 3.1 Wiring Procedure

When the coordinator decides to remediate task `T`:

1. **Create the remediation task:**
   ```
   .remediate-{T}  (status: Open)
   ```

2. **Add dependency edge:** `.remediate-{T}` → `T`
   - This means `T` cannot become ready until `.remediate-{T}` is done
   - Implemented as: `T.after.push(".remediate-{T}")`

3. **Reset `T` for retry:**
   - `T.status = Open` (from Failed)
   - `T.assigned = None` (clear old agent)
   - `T.failure_reason = None` (clear)
   - Keep `T.retry_count` (don't reset — this is a retry)
   - Append `## Remediation Context` to `T.description` with summary of what the remediation will do
   - The `converged` tag is cleared (if present)

4. **`T` waits:** Because `T.after` now includes `.remediate-{T}`, the coordinator won't dispatch `T` until the remediation completes.

5. **Remediation runs:** The coordinator dispatches `.remediate-{T}` like any other task. It may create subtasks (for decomposition) or directly fix the issue.

6. **Remediation completes:** `.remediate-{T}` → Done. `T`'s dependencies are satisfied. `T` becomes ready and gets dispatched.

7. **Context injection:** When building `T`'s prompt for retry, the coordinator includes `.remediate-{T}`'s output as dependency context (this already happens for any `--after` dependency via the existing context injection system).

### 3.2 Multiple Remediation Attempts

If `T` fails again after remediation:

1. Create `.remediate-{T}-2` (suffix indicates attempt number)
2. Wire: `.remediate-{T}-2` → `T`
3. The previous `.remediate-{T}` stays in the graph as Done (its output is still useful context)
4. `T` now has two after-dependencies, both satisfied

This naturally accumulates context: each retry of `T` sees all prior remediation outputs.

### 3.3 Remediation Task Properties

```rust
Task {
    id: format!(".remediate-{}", task_id),  // or .remediate-{task_id}-{attempt}
    title: format!("Remediate ({}): {}", category, original_title),
    description: Some(remediation_description),
    status: Status::Open,
    tags: vec!["remediation".to_string(), "agency".to_string()],
    after: vec![],  // No dependencies — runs immediately
    before: vec![task_id.to_string()],  // Blocks the original task
    max_retries: Some(1),  // Remediation itself gets one retry
    ..Task::default()
}
```

**System task prefix (`.`):** Remediation tasks use the dot prefix (like `.evaluate-*`, `.assign-*`, `.verify-flip-*`) to mark them as system-generated. This prevents remediation tasks from being remediated themselves (see Section 5).

### 3.4 spark-deep-synthesis: Full Wiring

```
Before remediation:
  spark-deep-synthesis (Failed)

After diagnosis + remediation creation:
  .remediate-spark-deep-synthesis (Open)
  spark-deep-synthesis (Open, blocked by .remediate-spark-deep-synthesis)

After .remediate-* runs and creates subtasks:
  .remediate-spark-deep-synthesis (Done)
  read-batch-1 (Open)
  read-batch-2 (Open)
  ...
  read-batch-5 (Open)
  synthesize-spark (Open, blocked by read-batch-1..5)
  spark-deep-synthesis (Open, blocked by synthesize-spark via .remediate-*)

Wait — there's a subtlety. The `.remediate-*` task creates subtasks, but how do those subtasks block `spark-deep-synthesis`? The remediation agent uses `wg add "synthesize-spark" --before spark-deep-synthesis`. This adds `synthesize-spark` to `spark-deep-synthesis.after`. The remediation agent is responsible for wiring its subtasks correctly.

After everything resolves:
  .remediate-spark-deep-synthesis (Done)
  read-batch-1..5 (Done)
  synthesize-spark (Done)
  spark-deep-synthesis (Ready → dispatched → succeeds with synthesized context)
```

---

## 4. Integration with Existing Systems

### 4.1 FLIP (Faithfulness via Likelihood-Inverted Probing)

**Complementary, not overlapping.**

| | FLIP | Remediation |
|---|---|---|
| **Detects** | Completed but wrong (low quality) | Couldn't complete at all |
| **Trigger** | Task reaches Done | Task reaches Failed |
| **Action** | Creates `.verify-flip-*` task | Creates `.remediate-*` task |
| **Outcome** | May fail the task (→ then remediation kicks in) | Fixes preconditions for retry |

**Interaction chain:** A task completes → FLIP scores it low → `.verify-flip-*` fails the task → the task is now Failed → remediation diagnoses it as `build_failure` → creates `.remediate-*` to fix the issue. FLIP and remediation form a pipeline where FLIP catches quality issues and remediation handles the resulting failures.

### 4.2 Evaluation

**Yes, `.remediate-*` tasks should be evaluated** (when `auto_evaluate` is enabled).

The quality of a diagnosis matters. A remediation that correctly identifies and fixes the root cause is valuable signal. A remediation that creates a bad decomposition wastes compute. Evaluation scores for `.remediate-*` tasks feed the evolution system (see 4.4).

However, `.remediate-*` tasks should use the existing `eval-scheduled` tag mechanism — the coordinator auto-creates `.evaluate-remediate-*` tasks just like for any other task.

### 4.3 Mandatory Validation (PendingValidation)

**PendingValidation could gate remediation retries** but should not by default.

If validation is enabled (`validation = "external"`), the flow becomes:
```
remediation completes → original task retries → agent calls wg done
  → PendingValidation → validator runs → approve/reject
```

The remediation itself should NOT go through PendingValidation (same as `.evaluate-*` tasks — system tasks get `validation = "none"` to prevent infinite regress).

### 4.4 Auto-Evolver

**Remediation patterns are evolution signal.**

When remediation works:
- The diagnosis category + remediation strategy that succeeded is positive signal
- The agent role that failed the original task gets negative signal (but qualified — context overflow isn't the agent's fault)

When remediation fails:
- The diagnosis was wrong, or the remediation strategy was inadequate
- This is negative signal for the diagnostician model choice

**Evolver integration:** The evolver should examine remediation patterns:
- "Tasks assigned to role X keep failing with context_overflow → role X's task-sizing heuristics need improvement"
- "Build failures from role Y always involve the same module → role Y needs better testing discipline"

This is a natural extension of the evolver's existing evaluation analysis. No special evolver changes needed — remediation outcomes flow through the standard evaluation pipeline.

---

## 5. Safety Guardrails

### 5.1 Max Remediation Attempts

**Default: 3 attempts per task** (configurable via `max_remediation_attempts`).

After 3 failed remediations, the task is escalated to human and paused. The coordinator logs:
```
[coordinator] ESCALATION: Task 'spark-deep-synthesis' exhausted 3 remediation
attempts. Categories tried: context_overflow, context_overflow, agent_confusion.
Pausing task — run `wg resume spark-deep-synthesis` after manual investigation.
```

**Counting:** Each `.remediate-{task-id}` creation (not each retry of the remediation task itself) counts as one attempt. If `.remediate-spark-deep-synthesis` fails and is retried via its own `max_retries: 1`, that's still one remediation attempt for `spark-deep-synthesis`.

### 5.2 Budget Cap

**Remediation cost should not exceed the original task's cost.** This is hard to enforce precisely (we don't know the original task's cost until it runs), so we use a proxy:

- **Token budget:** Total tokens consumed by diagnosis + remediation tasks ≤ `remediation_budget_multiplier` × original task's token usage (default: 2.0)
- **Attempt budget:** No more than `max_remediation_attempts` remediation cycles

In practice, diagnosis calls are cheap (~1K tokens each), and remediation tasks are similar in scope to the original. The multiplier of 2.0 gives generous room while preventing runaway costs.

**Implementation:** When the original task has `token_usage` recorded (from its failed run), the coordinator checks cumulative remediation token usage before creating a new remediation attempt. If the budget is exceeded, escalate instead.

If the original task has no recorded token usage (first run failed immediately), skip the budget check — there's no baseline to compare against.

### 5.3 Escalation Rules

Escalation happens when any of these are true:
1. `remediation_attempts >= max_remediation_attempts`
2. Cumulative remediation tokens > `remediation_budget_multiplier × original_tokens`
3. Diagnosis confidence < 0.6
4. Diagnosis category = `unfixable`
5. Task is `Abandoned` (respect human intent — never remediate abandoned tasks)

On escalation:
- Task is paused (`task.paused = true`)
- Log entry added with full diagnostic context
- If a notification channel is configured, send an alert

### 5.4 Preventing Remediation Loops

**System tasks (dot-prefixed) are never remediated.** This includes:
- `.remediate-*` — don't remediate the remediator
- `.evaluate-*` — don't remediate evaluations
- `.assign-*` — don't remediate assignments
- `.verify-flip-*` — don't remediate FLIP verifications
- `.evolve-*` — don't remediate evolution

This check already exists in `build_flip_verification_tasks()` (line 1366: `is_system_task(source_task_id)`) and is reused here.

### 5.5 Don't Remediate Abandoned Tasks

If `task.status == Abandoned`, skip remediation entirely. The human made an intentional decision to stop this task.

### 5.6 Interaction with Circuit Breaker

The existing circuit breaker (`should_circuit_break` in `triage.rs`) pauses tasks after N rapid agent deaths. Remediation should respect the circuit breaker:

- If a task is paused by the circuit breaker, don't attempt remediation
- Remediation only triggers on `Failed` status, not on `Open` tasks paused by the circuit breaker
- The circuit breaker counts "unclaimed" and "Triage:" log entries — remediation log entries use "Remediation:" as prefix, so they don't trigger the circuit breaker

---

## 6. Configuration

### New Fields in Config

```toml
[coordinator]
# Enable automatic failure diagnosis and remediation
auto_remediate = false          # opt-in (like auto_evaluate)

# Maximum remediation attempts per task before escalation
max_remediation_attempts = 3

# Token budget: remediation cost ≤ multiplier × original task cost
remediation_budget_multiplier = 2.0

# Transient failure retry policy
max_transient_retries = 3
transient_backoff_schedule = [30, 120, 300]  # seconds between retries
```

### Model Routing

Add `Diagnostician` to the `DispatchRole` enum:

```rust
/// Failure diagnosis (classifying why a task failed)
Diagnostician,
```

**Default tier: `sonnet`** — diagnosis needs to understand error messages and code context, but doesn't need opus-level reasoning. Sonnet balances quality and cost for this classification task.

```toml
[models]
diagnostician = { model = "sonnet" }
```

The remediation *task* itself uses the standard `TaskAgent` model (it's a regular task dispatched by the coordinator).

### CLI Configuration

```bash
wg config --auto-remediate true                     # enable
wg config --max-remediation-attempts 5              # increase limit
wg config --remediation-budget-multiplier 3.0       # increase budget
wg config --max-transient-retries 5                 # more transient retries
wg config --transient-backoff-schedule "10,30,60"   # faster retries
```

### Feature Flag Interaction

```
auto_remediate = true requires: (nothing — works independently)
auto_remediate + auto_evaluate = true: remediation tasks get evaluated
auto_remediate + auto_assign = true: remediation tasks get identity assignment
auto_remediate + auto_evolve = true: remediation patterns feed evolution
```

Unlike `auto_evolve` (which implies `auto_evaluate`), `auto_remediate` has no prerequisites. It works even without the agency system — it just creates tasks and retries.

---

## 7. Coordinator Integration

### Where in the Tick Loop

Remediation fits into the coordinator tick loop as a new phase after triage (which handles dead agents) and before auto-assign (which prepares tasks for dispatch):

```
Phase 1:   cleanup_dead_agents (triage)
Phase 2:   cycle iteration, cycle failure restart, wait evaluation, resurrection
Phase 2.9: AUTO-REMEDIATION ← new phase
Phase 3:   auto-assign
Phase 4:   auto-evaluate
Phase 4.5: FLIP verification
Phase 5:   check ready tasks
Phase 6:   spawn agents
```

**Why Phase 2.9?** Remediation must run:
- After triage (phase 1): triage may set tasks to Failed, which triggers remediation
- After cycle evaluation (phase 2.5-2.6): cycle restarts may clear failures, removing the need for remediation
- Before auto-assign (phase 3): remediation creates new tasks that need assignment
- Before spawn (phase 6): remediation may change task readiness

### Implementation Sketch

```rust
// Phase 2.9: Auto-remediation — diagnose failed tasks and create remediation tasks
if config.coordinator.auto_remediate {
    graph_modified |= build_auto_remediation_tasks(dir, &mut graph, &config);
}
```

```rust
fn build_auto_remediation_tasks(
    dir: &Path,
    graph: &mut WorkGraph,
    config: &Config,
) -> bool {
    let failed_tasks: Vec<String> = graph
        .tasks()
        .filter(|t| t.status == Status::Failed)
        .filter(|t| !is_system_task(&t.id))           // no remediating system tasks
        .filter(|t| t.status != Status::Abandoned)     // respect human intent
        .filter(|t| !t.paused)                         // respect circuit breaker
        .filter(|t| !has_pending_remediation(graph, &t.id))  // no duplicate remediation
        .filter(|t| remediation_attempts(graph, &t.id) < config.coordinator.max_remediation_attempts)
        .map(|t| t.id.clone())
        .collect();

    if failed_tasks.is_empty() {
        return false;
    }

    let mut modified = false;
    for task_id in failed_tasks {
        match diagnose_and_remediate(dir, graph, config, &task_id) {
            Ok(true) => { modified = true; }
            Ok(false) => { /* escalated, no graph change */ }
            Err(e) => {
                eprintln!("[coordinator] Remediation failed for '{}': {}", task_id, e);
            }
        }
    }
    modified
}
```

### `has_pending_remediation`

Checks if a `.remediate-{task-id}` (or `.remediate-{task-id}-N`) task exists that is Open or InProgress. Prevents creating duplicate remediation tasks.

### `remediation_attempts`

Counts existing `.remediate-{task-id}*` tasks in the graph (regardless of status). Returns the number of remediation attempts already made.

### `diagnose_and_remediate`

1. Read the task's `failure_reason` and output log
2. Call `run_lightweight_llm_call(config, DispatchRole::Diagnostician, &prompt, 30)`
3. Parse the diagnosis JSON
4. If `confidence < 0.6` or `category == "unfixable"`: escalate, return `Ok(false)`
5. If `category == "transient"`: auto-retry (no task creation), return `Ok(true)`
6. Create `.remediate-{task-id}` with category-specific description
7. Wire it into the graph
8. Reset the original task to Open
9. Log: "[coordinator] Remediation: created .remediate-{task-id} (category: {category})"
10. Return `Ok(true)`

---

## 8. Task Schema Impact

No new fields on `Task` are required. Remediation uses existing mechanisms:

| Mechanism | Existing Field | Usage |
|-----------|---------------|-------|
| Blocking retry | `task.after` | Add `.remediate-*` as dependency |
| Retry tracking | `task.retry_count` | Already incremented by retry |
| Failure info | `task.failure_reason` | Read by diagnostician |
| System task detection | `is_system_task()` | Prevent remediation loops |
| Wait for backoff | `WaitSpec` / `wait_until` | Transient retry backoff |
| Pausing | `task.paused` | Escalation pauses the task |

The only new data is the remediation task itself, which is a regular `Task` node in the graph.

---

## 9. Observability

### Log Messages

All remediation actions are logged to the task's log:

```
Remediation diagnosis: category=context_overflow, confidence=0.95
Remediation: created .remediate-spark-deep-synthesis (context_overflow)
Remediation: auto-retrying transient failure in 30s (attempt 2/3)
ESCALATION: 3 remediation attempts exhausted, task paused
```

### Coordinator Log

```
[coordinator] Remediation: diagnosed 'spark-deep-synthesis' as context_overflow (0.95)
[coordinator] Remediation: created .remediate-spark-deep-synthesis
[coordinator] Remediation: transient retry for 'api-call-task' (attempt 2/3, waiting 120s)
[coordinator] Remediation: escalating 'impossible-task' (unfixable, confidence 0.88)
```

### Querying Remediation History

```bash
# See all remediation tasks
wg list --tag remediation

# See remediation for a specific task
wg list --filter ".remediate-spark"

# See the diagnosis in the remediation task's description
wg show .remediate-spark-deep-synthesis
```

---

## 10. Implementation Plan

### Phase 1: Diagnosis infrastructure

**Files to modify:**
- `src/config.rs` — Add `auto_remediate`, `max_remediation_attempts`, `remediation_budget_multiplier`, `max_transient_retries`, `transient_backoff_schedule` to `CoordinatorConfig`. Add `Diagnostician` to `DispatchRole` with default tier `sonnet`.
- `src/cli.rs` — Add `--auto-remediate`, `--max-remediation-attempts`, etc. to `wg config`

**New file:**
- `src/commands/service/remediation.rs` — Diagnosis prompt, LLM call, JSON parsing, category classification

**Validation:**
- Unit tests for diagnosis prompt construction
- Unit tests for JSON extraction (reuse `extract_triage_json` pattern)
- Unit tests for confidence thresholds and escalation decisions

### Phase 2: Remediation task creation and wiring

**Files to modify:**
- `src/commands/service/coordinator.rs` — Add Phase 2.9 with `build_auto_remediation_tasks()`
- `src/commands/service/remediation.rs` — Add remediation task creation for each category, graph wiring logic

**Validation:**
- Integration test: failed task → remediation task created → original task becomes Open but blocked
- Integration test: remediation completes → original task becomes ready
- Integration test: system tasks are not remediated
- Integration test: max attempts enforcement

### Phase 3: Transient retry with backoff

**Files to modify:**
- `src/commands/service/remediation.rs` — Transient retry logic using `WaitSpec`
- Integration with existing `evaluate_waiting_tasks` in coordinator

**Validation:**
- Integration test: transient failure → task goes to Waiting → becomes Open after backoff
- Integration test: transient retries exhausted → escalation

### Phase 4: Budget enforcement and observability

**Files to modify:**
- `src/commands/service/remediation.rs` — Token budget calculation, cumulative tracking
- Logging improvements for remediation history

**Validation:**
- Integration test: budget exceeded → escalation instead of remediation
- Verify log messages match expected format

---

## 11. Open Questions

1. **Should the diagnostician see previous remediation attempts?** For attempt 2+, the diagnostician should know what was already tried and what the outcome was. The suggested approach: include `.remediate-{task-id}` task descriptions and outcomes in the diagnosis prompt for subsequent attempts. This prevents the system from trying the same strategy twice.

2. **Remediation for cycle members:** If a task in a cycle fails and `restart_on_failure` is true, the cycle restart mechanism (phase 2.6) already handles reactivation. Remediation should NOT run for cycle members with `restart_on_failure = true` — the cycle's own retry logic takes precedence. For cycle members with `restart_on_failure = false`, remediation can apply normally.

3. **Concurrent remediation:** If multiple tasks fail simultaneously, the coordinator creates multiple `.remediate-*` tasks. If they modify the same files, this creates conflicts. The existing "same files = sequential edges" guidance applies — the diagnostician could detect file overlap and serialize remediation tasks. For v1, this is acceptable to punt: the remediation agent should handle conflicts via git merge, and if it can't, it fails and the system escalates.

4. **Remediation across repos:** In a multi-repo setup (future `cross-repo-communication.md`), a failure in repo A might need a fix in repo B. This is out of scope for v1 — cross-repo remediation is escalated to human.
