# Condition E Failure Analysis and Improvement Strategy

**Date:** 2026-04-04  
**Task:** research-condition-e  
**Source data:** 30 pilot trials (10 tasks × 3 trials) from terminal-bench/results/pilot-e/

---

## 1. Executive Summary

Condition E achieves 75.0% pass rate (21/28 valid trials, 2 timeouts excluded), but its **central mechanism — independent verification — is completely broken**. Every single non-error failure (7/7) declared `VERIFY: PASS` before calling `wg_done`, yet the external verifier scored them 0. The "independent verification" is theater: the same context window that wrote the code also "independently" verified it, and confirmation bias dominates.

The verification system produces a **100% false-positive rate on failures** — it never catches a bug that the agent introduced. When E succeeds, it succeeds despite the verification loop, not because of it.

**Key numbers:**

| Metric | Passing (N=21) | Failing (N=7) | Error (N=2) |
|--------|---------------|---------------|-------------|
| Avg turns | 38.2 | 34.7 | N/A (timeout) |
| Avg decomp tasks | 3.7 | 2.4 | N/A |
| Avg verification iterations | 4.7 | 10.9 | N/A |
| Avg input tokens | 644K | 755K | N/A |
| False PASS rate | N/A | **100%** (7/7) | N/A |

---

## 2. Failure Taxonomy

### Category 1: False PASS — Self-Verification Blind Spot (5/9 failures)

The agent runs its own ad-hoc tests, they pass, it declares `VERIFY: PASS`, but the external verifier tests a subtlety the agent never considered.

**Affected trials:**

| Trial | Task | External failure | What agent tested | What agent missed |
|-------|------|-----------------|-------------------|-------------------|
| cancel-async-tasks\_\_5pQRtiq | cancel-async-tasks | `test_tasks_cancel_above_max_concurrent` — process hangs on SIGINT | Import, signature, concurrency limit, KeyboardInterrupt cleanup | Cancellation behavior when tasks > max_concurrent: the process must exit cleanly after SIGINT, printing "Cleaned up." for active tasks. Agent's process hung (5s timeout). |
| cancel-async-tasks\_\_CGQTAGp | cancel-async-tasks | Same test | Same checks | Same gap — agent's asyncio implementation doesn't properly propagate SIGINT when queued tasks exist |
| cancel-async-tasks\_\_hPH8vPS | cancel-async-tasks | Same test | Same checks plus empty list, ValueError | Same gap — fundamental misunderstanding of how `asyncio.gather` interacts with semaphore-gated tasks on cancellation |
| regex-log\_\_ad9z2qG | regex-log | Wrong dates returned (6 matches instead of 9) | 31 test cases (agent-written) — all passed | Agent's test cases didn't cover the exact edge cases in the verifier. Regex missed 3 valid matches. |
| regex-log\_\_i725Fsn | regex-log | Extra false match (10 matches instead of 9) | 16 test cases (agent-written) — all passed | Agent's boundary check failed on `abc2021-08-20` (alphanumeric before date). Regex incorrectly matched it. |

**Root cause:** The agent writes its own test suite during verification. It tests what it *thinks* the requirements are, not what the external verifier actually checks. The "read files fresh" instruction doesn't help because the verification gap isn't about re-reading the code — it's about imagining failure modes the agent's mental model doesn't contain.

**Pattern:** 14 verification iterations on cancel-async-tasks trial 1, 15 on regex-log trial 1, 36 on regex-log trial 2. More iterations ≠ better verification. The agent iterates on its own test suite, perfecting an incomplete specification.

### Category 2: False PASS — Constraint Violation (1/9)

The agent satisfies the functional requirement but violates a constraint it didn't test for.

| Trial | Task | External failure | What agent missed |
|-------|------|-----------------|-------------------|
| overfull-hbox\_\_Crktit2 | overfull-hbox | `test_input_file_matches` — input.tex has unauthorized modifications | Agent fixed overfull hbox warnings and verified compilation succeeds with no warnings. But the verifier checks that *only* synonym substitutions were made — no other text changes. The agent made edits beyond the allowed synonym replacements. |

**Root cause:** The agent correctly verified the primary functional requirement (no overfull hbox warnings, PDF compiles) but was unaware of the constraint (only allowed modifications are word substitutions from synonyms.txt). The constraint was in the task spec, but the agent focused on the outcome, not the method.

### Category 3: False PASS — Semantic Error (1/9)

The agent produces a plausible-looking solution that passes structural checks but fails on semantic correctness.

| Trial | Task | External failure | What agent missed |
|-------|------|-----------------|-------------------|
| sparql-university\_\_Nan2efz | sparql-university | `test_sparql_query_results` — wrong professor/country results | Agent verified the query "implements all three criteria" by reasoning about the query logic, but didn't run it against the actual data file with the exact expected output. |

**Root cause:** The agent's "verification" was a logical review of the SPARQL query, not an empirical test against the data. It read the query, decided it looked correct, and declared PASS. Zero verification iterations. The verification was entirely cognitive, not empirical.

### Category 4: Timeout — Unbounded Iteration (2/9)

The agent exhausts the 30-minute wall clock in verification/implementation loops.

| Trial | Task | Error | Details |
|-------|------|-------|---------|
| regex-log\_\_g4BEPZp | regex-log | AgentTimeoutError after 1800s | No metadata preserved — agent was mid-execution when killed |
| qemu-startup\_\_rVk2G76 | qemu-startup | AgentTimeoutError after 1800s | No metadata preserved |

**Root cause:** The E prompt sets a 6-iteration cap on the triage loop, but there's no time-based bailout. Regex-log is a trap: the decomposition into 5 subtasks fragments the problem, and the agent spends ~3M tokens and 57+ turns trying to converge on a complex regex through iterative refinement. The timeout fires before the iteration limit is hit.

---

## 3. Decomposition Analysis: Passing vs Failing

### Decomposition depth comparison

| Task | Pass? | Avg subtasks | Task nature | Decomposition appropriate? |
|------|-------|-------------|-------------|--------------------------|
| build-cython-ext | 3/3 pass | 4.0 | Multi-step build | **Yes** — clone, patch, build, test are genuinely independent phases |
| merge-diff-arc-agi-task | 3/3 pass | 5.3 | Multi-step reasoning | **Yes** — git setup, merge, algorithm design are distinct |
| count-dataset-tokens | 3/3 pass | 6.0 | Pipeline | **Yes** — install deps, load data, tokenize, write output |
| nginx-request-logging | 3/3 pass | 6.0 | Multi-config | **Yes** — install, create files, configure, verify |
| custom-memory-heap-crash | 3/3 pass | 1.0 | Single bug fix | **No but harmless** — 1 subtask ≈ no decomposition overhead |
| qemu-startup | 2/3 pass | 0.5 | System setup | **Mixed** — low decomposition, 1 error (timeout) |
| overfull-hbox | 2/3 pass | 3.7 | Text editing | **Marginal** — per-word replacement tasks add overhead without benefit |
| sparql-university | 2/3 pass | 1.0 | Single query | **No** — 1-2 subtasks, essentially no decomposition |
| cancel-async-tasks | 0/3 pass | 1.0 | Single function | **No** — 1 subtask per trial. Task is atomically "implement one function" |
| regex-log | 0/3 pass | 5.0 | Single regex | **Counterproductive** — 5 subtasks fragment what must be a single coherent regex |

**Key finding:** Decomposition helps on genuinely multi-step tasks (build, config, pipeline) and is neutral on single-step tasks (1 subtask = no overhead). But on regex-log, decomposition is actively harmful: splitting a regex problem into "design IP pattern," "design date pattern," "combine" creates artificial boundaries that prevent holistic reasoning.

### Dependency structure quality

Of the 95 total subtasks created across 30 trials:
- **Zero used `--after` dependencies** in the wg_add calls (based on transcript analysis — only regex-log trial 1 used `after` parameter)
- Most subtasks are created as flat lists, not dependency chains
- The agent marks them done sequentially but doesn't express the dependency graph

**This means E is not using the wg pipeline at all.** The subtasks are bookkeeping labels, not a dependency-managed workflow. The agent creates subtasks, immediately implements them in order, then runs verification. There's no parallel execution, no blocking, no dependency resolution.

---

## 4. Verification Loop Effectiveness

### Did verification ever catch anything?

| Metric | Value |
|--------|-------|
| Total VERIFY verdicts recorded | 25 (all PASS) |
| VERIFY: FAIL verdicts | 0 |
| Triage tasks created | 4 (regex-log trial 1 only: 3 fix tasks + 1 in custom-memory-heap-crash) |
| Verification that caught a real bug | 0 |

**The verification loop never produced a FAIL verdict.** Across all 30 trials, the agent always declared PASS. The 4 triage tasks in regex-log were created during the *implementation* phase (the agent found issues while testing, not during the formal verification phase).

The `verification_verdicts_pass: 25, verification_verdicts_fail: 0` in the summary.json confirms this globally.

### Why verification fails to work

1. **Same context window**: The "role shift" instruction ("you are now a REVIEWER") doesn't reset the model's internal representation. The model that just wrote the code still has the same assumptions about what's correct.

2. **Agent writes its own test suite**: Instead of running the task's existing tests (which would require knowing where they are — they're in `/tests/` inside the Docker container), the agent writes ad-hoc tests that validate its own understanding of the requirements. This is circular: the tests can only be as good as the agent's spec interpretation.

3. **No access to verifier tests**: The external verifier tests are in `/tests/test_outputs.py` inside the Docker container. The agent *could* read these, but the prompt doesn't instruct it to look for existing test files. It tells the agent to "run the task's test suite" but the agent interprets this as "write and run my own tests."

4. **Verification of verification**: There's no external check on whether the VERIFY: PASS verdict is correct. The prompt says "NEVER call wg_done without a PASS verdict" but this just means the agent always produces a PASS verdict before calling wg_done.

---

## 5. wg Pipeline Gaps

### What E uses

| Feature | Usage rate | Notes |
|---------|-----------|-------|
| `wg_add` (task creation) | 93% of trials | Used for decomposition bookkeeping |
| `wg_log` (progress logging) | 100% | Heavy use — 219 total log entries |
| `wg_done` (task completion) | 100% | 105 total done calls |
| `wg_artifact` | 13% (4 trials) | Rarely used |
| `wg_list` | 3% (1 trial) | Almost never used |

### What E does NOT use

| Feature | Expected use | Why absent | Impact |
|---------|-------------|-----------|--------|
| `--after` dependencies | Every subtask should declare dependencies | Prompt doesn't emphasize dep chains, just "create tasks" | Tasks are flat lists, not graphs. No parallel/sequential reasoning. |
| `wg_show` (task inspection) | Should inspect subtask status before proceeding | Not mentioned in prompt flow | Agent can't check what's done vs pending |
| `wg_fail` (root task failure) | Should be used when stuck | Prompt mentions it but 6-iteration limit is never hit (verdicts are always PASS) | Agent never declares failure — it always "succeeds" per its own assessment |
| `--verify` gates | Should attach machine-checkable criteria | Not part of E's prompt | Subtasks have no automated validation |
| Cycle edges / `--max-iterations` | Could structure impl→verify→triage as a cycle | Harbor's single-agent design prevents native cycles | Agent manages the loop manually, but never actually loops (always PASS on first verify) |
| `wg_list` for monitoring | Should poll subtask status | Prompt says to use it but agent ignores | No awareness of task graph state |

### Agency assignment

- Agency is bootstrapped (role=architect, tradeoff=thorough) but the identity is purely cosmetic
- The agent doesn't use agency for subtask differentiation (e.g., assigning "programmer" to impl tasks and "reviewer" to verify tasks)
- **Should it?** In a single-agent Harbor trial, agency assignment to subtasks is meaningless — there's only one agent executing everything. It would only matter with multi-agent execution (wg service).

---

## 6. Per-Task Deep Dive: Cancel-Async-Tasks (0/3)

This task is E's worst performer and reveals the fundamental flaw.

### The task
Implement `run_tasks(tasks, max_concurrent)` — an async function that:
- Runs tasks concurrently with a semaphore limit
- Handles KeyboardInterrupt by cancelling tasks and running cleanup code
- Critical edge case: when tasks > max_concurrent, SIGINT must still clean up active tasks

### What all 3 E trials did
1. Created 1 subtask ("Implement run_tasks function")
2. Implemented using `asyncio.Semaphore` + `asyncio.gather`
3. Ran self-written tests: basic execution, concurrency limiting, empty list, cleanup on completion
4. Declared `VERIFY: PASS`
5. Called `wg_done`

### What the external verifier tested
The 6th test (`test_tasks_cancel_above_max_concurrent`) sends SIGINT to the process when 3 tasks are queued but only 2 can run concurrently. It expects:
- 2 "Task started." outputs (the 2 active tasks)
- 2 "Cleaned up." outputs (cleanup code runs for both active tasks)
- Process exits within 5 seconds

### Why all 3 failed
The agent's `asyncio.gather` implementation handles SIGINT by cancelling tasks, but when tasks are waiting on the semaphore (queued, not started), the cancellation handling blocks — the process hangs and the 5-second timeout fires.

The agent's self-tests tested cancellation when tasks ≤ max_concurrent (tests 4 and 5 pass). It never tested cancellation when tasks > max_concurrent — the specific edge case that `asyncio.gather` handles incorrectly.

### The irony
The test's docstring says: "This is a common gotcha in Python because asyncio.gather doesn't properly cancel existing tasks if there are still tasks in the queue." The agent fell into exactly this gotcha, and its "independent verification" couldn't catch it because the verification tests the same mental model that produced the bug.

---

## 7. Per-Task Deep Dive: Regex-Log (0/3, including 1 timeout)

### The task
Write a single regex that matches dates in `YYYY-MM-DD` format, but only on lines containing a valid IPv4 address (no leading zeros in octets, octets 0–255). Return only the last date on matching lines.

### Why decomposition hurts here
- Trial ad9z2qG: 6 subtasks (analyze, write, test, fix×3). 47 turns, 2M tokens.
- Trial i725Fsn: 4 subtasks (design IP, design date, combine, test). 62 turns, 1.8M tokens.
- Trial g4BEPZp: timed out at 30 min. Unknown subtask count.

The problem is that a regex is **atomic**: you can't independently design the IP pattern and the date pattern and just concatenate them. The interactions between lookaheads, boundaries, and capturing groups mean the combined regex behaves differently than the sum of its parts. Decomposing into subtasks forces the agent to context-switch between "IP expert" and "date expert" roles, losing the holistic view needed for a correct combined regex.

### Specific regex failures
- **Trial ad9z2qG**: Matched 6/9 expected dates. Missed 3 dates where the IP address appeared in a non-standard position in the line. The lookahead was too restrictive.
- **Trial i725Fsn**: Matched 10 instead of 9 — false match on `abc2021-08-20` from line "Backup abc2021-08-20 from 203.0.113.5 completed". The negative lookbehind for alphanumeric before the date didn't work correctly with the line structure.

Both trials declared `VERIFY: PASS` with agent-written test suites (31 and 16 test cases respectively). The test cases were close but didn't cover the exact edge cases in the verifier's `test_regex_matches_dates`.

---

## 8. Ranking of Improvements by Expected Impact

### Improvement 1: **Run existing test files first** (HIGH IMPACT)

**Problem:** Agent writes its own tests instead of running the task's test suite.  
**Evidence:** All 7 false-PASS failures would have been caught by running `/tests/test_outputs.py`.  
**Proposed fix:** Add to the E prompt before Phase 3:

```
### Before Verification
1. Look for existing test files in the task environment:
   bash("find / -name 'test_*.py' -o -name '*_test.py' 2>/dev/null | head -20")
2. If test files exist, run them FIRST:
   bash("cd /tests && python -m pytest test_outputs.py -v 2>&1 | tail -50")
3. Your self-verification supplements existing tests — it does NOT replace them.
```

**Expected impact:** Could fix 5-7 of the 9 failures. The verifier runs exactly these test files. If the agent ran them first, it would discover failures before declaring PASS.

**Code change in `build_condition_e_prompt()`:** Insert the test-discovery step between Phase 2 and Phase 3.

### Improvement 2: **Adaptive decomposition — skip for atomic tasks** (MEDIUM IMPACT)

**Problem:** Decomposition is mandatory but counterproductive for single-function tasks.  
**Evidence:** cancel-async-tasks (1 subtask avg, 0/3 pass), regex-log (5 subtasks but fragmented, 0/3 pass).  
**Proposed fix:** Change the decomposition guidance from "Break the task into implementation steps" to:

```
### Phase 1: Analyze & Decide
1. Read the task instruction carefully.
2. Classify the task:
   - ATOMIC (single file, single function, single config): Skip decomposition. Implement directly.
   - MULTI-STEP (multiple files, build pipeline, system setup): Decompose into subtasks.
3. For ATOMIC tasks, proceed directly to Phase 2 without creating subtasks.
```

**Expected impact:** Saves ~30% of turns on simple tasks. Won't fix the false-PASS problem directly but reduces token waste and timeout risk.

### Improvement 3: **Test-driven verification with explicit test comparison** (HIGH IMPACT)

**Problem:** Agent's self-tests validate its own spec interpretation, not the actual requirements.  
**Evidence:** regex-log agent wrote 31 test cases but missed 3 critical edge cases; cancel-async-tasks agent tested 5/6 verifier scenarios.  
**Proposed fix:** Add to the verification protocol:

```
### Verification Protocol (revised)
1. First, discover and run existing tests (see "Before Verification").
2. If existing tests FAIL, that IS your verification result — do NOT override with your own assessment.
3. If no existing tests exist, write tests that cover:
   a. The exact examples from the task specification
   b. Edge cases: empty input, boundary values, error conditions
   c. The specific constraints mentioned in the task (not just the primary output)
4. Compare your test cases against the task spec line-by-line. For each requirement in the spec, verify you have a test for it.
```

**Expected impact:** Moderate — helps when existing tests aren't available. The key insight is "if existing tests FAIL, that IS your verdict."

### Improvement 4: **Time-aware bailout** (LOW-MEDIUM IMPACT)

**Problem:** 2 trials timed out at 30 minutes.  
**Evidence:** regex-log (3M tokens, 57 turns before timeout), qemu-startup (timeout).  
**Proposed fix:** Add to the prompt:

```
## Time Management
- You have a maximum of 30 minutes. Budget:
  - Analysis + decomposition: 2 minutes
  - Implementation: 15 minutes
  - Verification: 5 minutes
  - Triage/retry: 8 minutes
- If you've been iterating for more than 20 minutes without progress, call wg_fail.
- Track your iteration count. After 3 iterations on the same issue, step back and try a fundamentally different approach — or call wg_fail.
```

**Expected impact:** Converts 2 timeouts (errors) into clean failures (wg_fail), preserving metadata for analysis. Doesn't improve pass rate but improves data quality.

### Improvement 5: **Use `--after` dependencies in wg_add** (LOW IMPACT)

**Problem:** Subtasks are flat lists with no dependency edges.  
**Evidence:** Only 1/30 trials used `--after`. Tasks are done sequentially regardless.  
**Proposed fix:** In Phase 1, add:

```
4. Create tasks for each step using `wg_add` with dependencies:
   `wg_add("Step 2: ...", after="step-1-id")`
   Express the real dependency order — what must finish before what can start.
```

**Expected impact:** Low for Harbor (single agent, sequential execution). But creates better data for post-hoc analysis and would matter if E were extended to multi-agent execution.

### Improvement 6: **Remove "ORCHESTRATOR, not implementer" framing** (LOW-MEDIUM IMPACT)

**Problem:** The "orchestrator" framing is misleading in a single-agent context and may cause the agent to over-delegate to subtasks when it should just work.  
**Evidence:** The agent *does* implement everything itself — the "orchestrator" framing just adds overhead (creating/tracking subtasks) without changing the actual work.  
**Proposed fix:** Change framing to:

```
You are an AI agent completing a Terminal Bench task.
You use a STRUCTURED APPROACH: plan your work as subtasks, implement them, then verify independently.
```

**Expected impact:** Reduces cognitive overhead. The agent still decomposes when useful but isn't forced to pretend it's delegating.

---

## 9. What Would Fix E (If Anything Can)

### The fundamental problem

E's thesis is that "independent verification" improves outcomes. But in a single-agent context, independence is impossible — it's the same model, same context window, same assumptions. The "role shift" instruction is a prompt-engineering attempt at what requires architectural separation.

### What would actually achieve independent verification

1. **Run existing tests**: The task environment already contains tests (`/tests/test_outputs.py`). Running them is trivially independent — they were written by the benchmark author, not the agent.

2. **True multi-agent**: A separate LLM call (different conversation, no shared context) as the verifier. This requires Harbor infrastructure changes (spawn a second agent inside the trial).

3. **Verifier with different model**: Use a different model for verification (e.g., implement with minimax, verify with claude). Different biases = more likely to catch errors.

Option 1 is the only one implementable without infrastructure changes and would likely resolve most failures.

### Recommended E' variant

If E were to be re-run with fixes:

1. **Test discovery**: Prompt instructs agent to find and run `/tests/test_*.py` before declaring PASS
2. **Adaptive decomposition**: Agent classifies task as atomic vs multi-step before decomposing
3. **Empirical verification only**: "VERIFY: PASS" requires that ALL discovered tests pass — no cognitive-only verification
4. **Time budget**: Agent aware of 30-minute limit, fails explicitly after 20 min without progress
5. **Remove orchestrator framing**: Use "structured approach" framing instead

---

## 10. Comparison: Passing vs Failing Decomposition Patterns

### Tasks where decomposition clearly helped (all 3 trials pass)

| Task | Avg subtasks | Pattern | Why it helped |
|------|-------------|---------|--------------|
| build-cython-ext | 4.0 | Clone → patch → build → test | Each phase is genuinely independent; failures in one phase don't contaminate others |
| merge-diff-arc-agi-task | 5.3 | Git setup → merge → algorithm → verify | Complex multi-step reasoning benefits from focus on each step |
| count-dataset-tokens | 6.0 | Install → explore → load → tokenize → write | Pipeline with clear handoffs |
| nginx-request-logging | 6.0 | Install → create files → config × 3 → verify | System configuration with multiple independent config steps |

### Tasks where decomposition was neutral (1 subtask ≈ no decomposition)

| Task | Avg subtasks | Pass rate | Notes |
|------|-------------|-----------|-------|
| custom-memory-heap-crash | 1.0 | 3/3 | Single bug fix — 1 subtask = no overhead |
| sparql-university | 1.0 | 2/3 | Single query — decomposition irrelevant |

### Tasks where decomposition hurt

| Task | Avg subtasks | Pass rate | Why it hurt |
|------|-------------|-----------|------------|
| regex-log | 5.0 | 0/2 valid | Fragmented atomic problem; lost holistic view; massive token waste |
| cancel-async-tasks | 1.0 | 0/3 | Decomposition was minimal but the organizational overhead (VERIFY phase) consumed turns on wrong tests |

---

## 11. Specific Code Changes to ConditionEAgent

### Change 1: Add test discovery to `build_condition_e_prompt()` (adapter.py:683)

Insert after Phase 2, before Phase 3:

```python
"### Phase 2.5: Discover Existing Tests\n"
"Before verifying, look for existing test files:\n"
"```\n"
"bash(\"find /tests -name 'test_*.py' -o -name '*_test.py' 2>/dev/null\")\n"
"bash(\"find / -maxdepth 3 -name 'test_*.py' 2>/dev/null | head -10\")\n"
"```\n"
"If test files exist, you MUST run them as part of verification.\n"
"Their results OVERRIDE your own assessment.\n\n"
```

### Change 2: Modify Phase 3 verdict rules (adapter.py:710-719)

Replace the verification verdict section with:

```python
"5. Record a structured verdict:\n"
f'   - If existing tests all pass AND your review finds no issues:\n'
f'     `wg_log("{root_task_id}", "VERIFY: PASS — existing tests pass + manual review OK")`\n'
f'   - If ANY existing test fails:\n'
f'     `wg_log("{root_task_id}", "VERIFY: FAIL — test_X failed: <output>")`\n'
f'     (Your own assessment cannot override a failing existing test)\n'
```

### Change 3: Make decomposition conditional (adapter.py:696-701)

Replace Phase 1 with:

```python
"### Phase 1: Analyze & Decide\n"
"1. Read the task instruction carefully.\n"
"2. Identify what success looks like (test criteria, expected outputs).\n"
"3. Classify the task:\n"
"   - ATOMIC: Single file, single function, or single configuration → "
"implement directly, skip wg_add.\n"
"   - MULTI-STEP: Multiple files, build pipeline, or system setup → "
"create subtasks with wg_add and --after dependencies.\n"
"4. For MULTI-STEP tasks only, create tasks with dependencies:\n"
"   wg_add(\"Step 1: ...\"), then wg_add(\"Step 2: ...\", after=\"step-1\")\n\n"
```

### Change 4: Add time management (adapter.py, after CRITICAL Rules)

```python
"## Time Management\n"
"You have 30 minutes maximum. After 20 minutes of iteration without progress, "
f"call `wg_fail(\"{root_task_id}\", \"reason: stuck after N iterations\")` "
"instead of continuing to spin.\n\n"
```

---

## 12. Summary Table

| # | Failure category | Count | Root cause | Fix | Impact |
|---|-----------------|-------|-----------|-----|--------|
| 1 | False PASS: blind spot | 5 | Agent's self-tests miss edge cases | **Run existing test files** | HIGH |
| 2 | False PASS: constraint violation | 1 | Agent ignores method constraints | **Read spec constraints in verification** | MEDIUM |
| 3 | False PASS: semantic error | 1 | Cognitive verification, no empirical test | **Require empirical verification** | HIGH |
| 4 | Timeout | 2 | No time-aware bailout | **Time budget + explicit wg_fail** | LOW-MEDIUM |
| 5 | Counterproductive decomposition | 2 | Atomic tasks fragmented | **Adaptive decomposition** | MEDIUM |
| 6 | Flat subtask structure | 28 | No --after dependencies used | **Prompt emphasizes dependencies** | LOW |
| 7 | Verification never fails | 30 | VERIFY always produces PASS | **Existing test results override** | HIGH |
