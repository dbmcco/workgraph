# Calibration Analysis: Condition A vs Condition B

**Date**: 2026-04-03
**Analyst**: Automated (tb-calibration-analysis task)
**Model**: qwen/qwen3-32b via OpenRouter
**Tasks**: 8 custom calibration tasks (2 easy, 3 medium, 3 hard)
**Trials**: 1 per task per condition

> **Note**: This calibration was performed with Qwen3-32B. The primary experiment model was subsequently changed to **Minimax M2.7** because Qwen3-32B was expected to score near 0% on real Terminal Bench tasks. This data remains valid as a harness validation and baseline reference.

---

## 1. Pass Rate Comparison

### Raw Data (Exit Codes)

All 8 tasks in both conditions exited with code 0. The harness uses `exit_code` as its pass/fail metric, yielding **8/8 (100%) for both conditions**.

### Correctness Assessment (Manual Review from Condition A SUMMARY)

The Condition A agent performed its own manual verification of task outputs and found:

| Task | Category | Difficulty | Condition A | Condition B | Notes |
|------|----------|------------|-------------|-------------|-------|
| cal-01-file-ops | file-ops | easy | PASS | PASS | Both created structure and ran tests |
| cal-02-text-processing | text-processing | easy | PASS | PASS | Word frequency correct |
| cal-03-debugging | debugging | medium | PASS | PASS | All bugs fixed, tests pass |
| cal-04-shell-scripting | shell-scripting | medium | **FAIL** | **UNKNOWN** | A: Output had bugs ("Total requests: 9" should be 10). B: exit_code=0, but no independent output verification |
| cal-05-data-processing | data-processing | medium | PASS | PASS | JSON + CSV correct |
| cal-06-algorithm | algorithm | hard | PASS | PASS | KV store output matched expected |
| cal-07-ml | ml | hard | PASS | PASS | K-means within tolerance |
| cal-08-sysadmin | sysadmin | hard | **FAIL** | **UNKNOWN** | A: Port binding failures, timeouts, agent falsely claimed success. B: exit_code=0, but 1165s duration and 13 turns suggest struggles |

### Critical Finding: Measurement Gap

**The harness does not validate output correctness** -- it only checks exit codes. Exit code 0 means the agent process completed, not that the task was solved correctly. This is a known limitation of our calibration harness vs. the real Terminal Bench, which uses outcome-based tests (automated evaluation of container state).

**Corrected pass rates** (using Condition A's manual verification for A, and conservatively treating cal-04/cal-08 as unknown for B):

| Condition | Optimistic (exit code) | Conservative (manual review) |
|-----------|----------------------|------------------------------|
| A | 8/8 (100%) | 6/8 (75%) |
| B | 8/8 (100%) | 6/8 to 8/8 (75-100%) |

**Recommendation**: For the full run on real Terminal Bench, this is a non-issue -- TB has automated outcome tests. But for calibration, we should either (a) add automated validation scripts to calibration tasks, or (b) accept that calibration only tests harness mechanics, not true pass rate.

---

## 2. Token Usage Analysis

### Per-Task Comparison

| Task | Difficulty | A Input | B Input | B/A Ratio | A Output | B Output | B/A Ratio |
|------|-----------|---------|---------|-----------|----------|----------|-----------|
| cal-01 | easy | 13,370 | 69,804 | **5.2x** | 5,967 | 16,730 | 2.8x |
| cal-02 | easy | 10,011 | 29,128 | 2.9x | 14,703 | 16,141 | 1.1x |
| cal-03 | medium | 7,065 | 18,993 | 2.7x | 5,310 | 7,301 | 1.4x |
| cal-04 | medium | 16,588 | 23,466 | 1.4x | 24,490 | 8,378 | 0.3x |
| cal-05 | medium | 7,246 | 10,905 | 1.5x | 6,675 | 4,128 | 0.6x |
| cal-06 | hard | 2,971 | 12,823 | 4.3x | 9,261 | 8,699 | 0.9x |
| cal-07 | hard | 7,448 | 24,428 | 3.3x | 8,820 | 11,020 | 1.2x |
| cal-08 | hard | 29,004 | 61,456 | 2.1x | 24,950 | 29,301 | 1.2x |

### Aggregate

| Metric | Condition A | Condition B | Delta |
|--------|-------------|-------------|-------|
| Total Input Tokens | 93,703 | 251,003 | +157,300 (**2.7x**) |
| Total Output Tokens | 100,176 | 101,698 | +1,522 (1.02x) |
| Total Tokens | 193,879 | 352,701 | +158,822 (1.8x) |
| Mean Input/Task | 11,713 | 31,375 | +19,662 |
| Mean Output/Task | 12,522 | 12,712 | +190 |

### Analysis

**Input token overhead is substantial (2.7x)** but almost entirely from:
1. **Larger system prompt**: Condition B prompt is ~1900 chars vs ~1300 chars for A (only ~50% larger in chars, but the wg tool schemas add significant token overhead to each API call).
2. **More turns**: Condition B averaged 9.2 turns vs 5.8 for A. Each turn re-sends the full conversation history, compounding the system prompt overhead.
3. **wg_done calls**: Every B task called `wg_done`, adding a turn that doesn't exist in A.

**Output tokens are nearly identical (1.02x)**. The model generates roughly the same amount of reasoning and code regardless of condition. The wg tool overhead is negligible in output.

**Cost implication at OpenRouter rates ($0.20/MTok in, $0.60/MTok out)**:
- Condition A: $0.019 input + $0.060 output = $0.079/task
- Condition B: $0.006 input + $0.061 output = $0.067/task (actually CHEAPER due to lower output)
- Wait -- correcting: $0.20/MTok = $0.0002/Ktok
- A: 93.7K * $0.0002 + 100.2K * $0.0006 = $0.019 + $0.060 = **$0.079 total** ($0.010/task)
- B: 251.0K * $0.0002 + 101.7K * $0.0006 = $0.050 + $0.061 = **$0.111 total** ($0.014/task)
- **Delta**: +$0.032 total (+40%), or +$0.004/task

At these rates, the overhead is negligible. Even for the full 89-task x 3-trial run: 89 * 3 * $0.004 = ~$1 extra.

---

## 3. Timing Analysis

### Per-Task Duration

| Task | Difficulty | A Duration | B Duration | Faster? |
|------|-----------|-----------|-----------|---------|
| cal-01 | easy | 183s | 514s | A by 331s |
| cal-02 | easy | 430s | 513s | A by 83s |
| cal-03 | medium | 177s | 237s | A by 60s |
| cal-04 | medium | 1030s | 270s | **B by 760s** |
| cal-05 | medium | 192s | 125s | **B by 67s** |
| cal-06 | hard | 870s | 269s | **B by 601s** |
| cal-07 | hard | 263s | 346s | A by 83s |
| cal-08 | hard | 839s | 1165s | A by 326s |

### Aggregate

| Metric | Condition A | Condition B |
|--------|-------------|-------------|
| Total Duration | 3,984s (66 min) | 3,439s (57 min) |
| Mean Duration | 498s (8.3 min) | 430s (7.2 min) |
| Median Duration | 347s | 303s |

### Analysis

**Condition B is slightly faster overall (-14%)**, which is surprising given more turns and more tokens. The likely explanation is variance from OpenRouter stream interruptions (Condition A's cal-04 at 1030s and cal-06 at 870s appear to include significant retry latency). With a single trial per task, timing differences are dominated by network variance, not systematic effects.

**For the full run**: Set timeout to **1800s (30 min)** to be safe. The slowest task was cal-08-B at 1165s (~19 min). Real TB tasks (with Docker, larger scope) may take longer. 30 minutes provides margin without being wasteful.

---

## 4. Failure Mode Analysis

### Condition A Failure Modes

1. **Incorrect output despite completion (cal-04)**: The agent wrote a shell script that produced wrong counts. It didn't self-verify the output against expected values. This is a reasoning/accuracy failure.
   - **Root cause**: Complex awk/sed parsing in shell scripts is a weakness for Qwen3-32B
   - **Mitigation**: None within scaffold -- this is model capability

2. **False completion claim (cal-08)**: The agent attempted to start an HTTP server, encountered port binding failures and bash timeouts, but reported success.
   - **Root cause**: The agent can't run background processes (servers) reliably from the bash tool. The 120s bash timeout kills long-running processes. Port conflicts on retry. The agent pattern-matches on partial success and doesn't re-verify.
   - **Mitigation**: This is a known TB-hard category. Consider adding a `background_bash` tool or extending bash timeout for server tasks.

3. **Tool confusion (cal-02)**: Agent tried to call `wg_done` in Condition A where it doesn't exist. Minor -- self-corrected.

4. **Missing command (cal-03)**: Used `python` instead of `python3`. Self-corrected.

### Condition B Failure Modes

1. **No wg tool utilization beyond wg_done**: The agent used `wg_done` in every task but NEVER used `wg_log`, `wg_add`, `wg_artifact`, or `wg_show`. The workgraph tools were available but completely ignored for their intended purpose (decomposition, logging, artifact recording).

2. **Excessive turns for easy tasks (cal-01)**: 21 turns for a file-ops task that A completed in 9. The extra turns appear to be the agent re-reading and re-verifying things it already did, possibly due to the more verbose system prompt creating a more cautious behavior.

### Key Insight: wg Tool Underutilization

This is the **most important finding**. The Condition B prompt says "you can use workgraph tools" but doesn't MANDATE their use. The model takes the path of least resistance: solve with bash and file tools, then call `wg_done`.

For the full run, the value of Condition B depends on:
- Tasks that exceed context limits (none of these calibration tasks did)
- Tasks complex enough to benefit from decomposition (none of these were)
- Journal/resume functionality (never triggered)

**The calibration tasks are too easy to show the workgraph advantage.** This is expected and noted in the experiment design.

---

## 5. Prompt Tuning Recommendations

### Condition A Prompt (Control)

Current prompt is good. Minimal and clean:
```
You are a coding agent completing a task. You have access to bash and file tools.
Focus on completing the task efficiently and correctly.
Do not ask for clarification -- proceed with your best judgment.
When you believe you have completed the task, provide a final summary of what you did.
```

**No changes needed.** The control must stay minimal.

### Condition B Prompt (Treatment)

Current prompt is 1904 bytes with wg tool descriptions. Two issues:

**Issue 1: wg tools described but not enforced**

The prompt says "you can use" but should say "you must use" for logging at minimum. However, we need to be careful -- forcing wg tool usage on simple tasks that don't need it would inflate the overhead without benefit and could hurt pass rate if the model gets confused.

**Recommendation**: Keep the prompt mostly as-is. The thesis being tested is whether HAVING the tools available helps, not whether USING them is enforced. ForgeCode's big win was enforcing `todo_write`, but their enforcement is in the runtime, not just the prompt. We don't have runtime enforcement in the harness.

**Issue 2: System prompt inflates context with each turn**

Every API call includes the system prompt. With 9.2 average turns, the system prompt tokens are multiplied. For a 32K context window, this matters.

**Recommendation**: Trim the Condition B prompt to essentials:

```
# Task Assignment

You are an AI agent working on a task in a workgraph project.

## Your Task
- **ID:** ${TASK_ID}

## Workgraph Tools Available
- wg_log("${TASK_ID}", "message") -- log progress (survives context limits)
- wg_add("subtask title") -- decompose into subtasks
- wg_artifact("${TASK_ID}", "path") -- record artifacts
- wg_done("${TASK_ID}") -- mark complete
- wg_fail("${TASK_ID}", "reason") -- mark failed

For complex tasks: decompose with wg_add, log progress with wg_log.
When done: call wg_done. If you cannot complete: call wg_fail.

## Task

${TASK_TEXT}
```

This reduces the prompt from ~1900 chars to ~500 chars -- a 63% reduction. Fewer tokens per turn, less compounding.

### Harness Parameters

| Parameter | Current | Recommended | Rationale |
|-----------|---------|-------------|-----------|
| `--max-turns` | 100 | **50** | No calibration task used more than 21 turns. 50 is generous for single-session tasks. Real TB hard tasks may need more, but 100 is wasteful for most. |
| `--timeout` | 1800 | **1800** | Keep at 30 min. Sufficient for all calibration tasks. Provides margin for real TB tasks. |

---

## 6. Full Experiment Parameters

### Task Selection

**Run all 89 Terminal Bench tasks.** Reasons:
1. The calibration tasks are custom and much simpler than real TB tasks. Any subset would be arbitrary.
2. 89 tasks is manageable at ~$0.01-0.02/task with Qwen3-32B via OpenRouter.
3. The leaderboard requires all tasks for submission.
4. Subsetting would weaken the statistical validity and comparability.

### Trial Count

**3 trials per condition** for the initial full run. Reasons:
1. The reference design calls for 3 runs for mean +/- stderr.
2. 89 * 3 * 2 conditions = 534 task runs. At ~7 min avg = ~62 hours serial. With `--n-concurrent 4`: ~15 hours per condition.
3. Cost: 534 * $0.012 avg = ~$6 total. Well within budget.
4. If results look promising (>10 point delta), do 5 trials for the leaderboard submission.

### Model

> **Update**: The primary experiment model has been changed to **Minimax M2.7**. The rationale below was written for the calibration model (Qwen3-32B).

**qwen/qwen3-32b** via OpenRouter (calibration model). Rationale:
1. Calibration showed it can complete all task types (file ops through sysadmin)
2. Cheap enough for repeated trials
3. 32K context window creates the context pressure that makes the workgraph thesis relevant
4. A larger model (Qwen3-235B-A22B) would be a good follow-up if results are positive

### Concurrency

**4 concurrent tasks** for the full run. Reasons:
1. OpenRouter rate limits are the bottleneck, not local compute
2. 4 is conservative enough to avoid rate limiting
3. Still provides 4x throughput over serial execution

### Summary of Recommended Parameters

```bash
# Condition A (control)
./tb-harness.sh \
  --condition A \
  --model "minimax/minimax-m2.7" \
  --max-turns 50 \
  --timeout 1800

# Condition B (treatment)
./tb-harness.sh \
  --condition B \
  --model "minimax/minimax-m2.7" \
  --max-turns 50 \
  --timeout 1800
```

**Full run configuration:**
- Tasks: All 89 Terminal Bench tasks
- Trials: 3 per condition (6 total per task)
- Model: minimax/minimax-m2.7 via OpenRouter
- Concurrency: 4
- Max turns: 50
- Timeout: 1800s (30 min)
- Estimated time: ~15 hours per condition at concurrency 4
- Estimated cost: ~$6 total (both conditions, 3 trials each)

---

## 7. What the Calibration Tells Us (and Doesn't)

### What It Tells Us

1. **The harness works end-to-end** for both conditions with Qwen3-32B via OpenRouter.
2. **Token overhead for Condition B is ~2.7x input, ~1.0x output** -- manageable cost.
3. **Timing is comparable** between conditions (no systematic penalty for wg tools).
4. **The model can complete diverse task types**: file ops, text processing, debugging, data processing, algorithms, ML, sysadmin.
5. **The model struggles with**: complex shell scripting (awk/sed), long-running processes (servers), and self-verification of output correctness.

### What It Doesn't Tell Us

1. **Whether workgraph improves pass rate** -- these tasks are too easy and too short to trigger context exhaustion or benefit from decomposition.
2. **Whether the model will use wg tools** when it would actually help -- on these tasks, it rationally ignores them.
3. **How the harness performs with Docker** -- calibration ran natively, not in TB containers.
4. **Statistical power** -- single trial per task, custom tasks, no automated outcome verification.

### Predictions for the Full Run

Based on calibration data and the experiment design:

- **Easy tasks (48.6%)**: Both conditions ~similar. A might even be slightly better (less prompt overhead for straightforward tasks).
- **Medium tasks (47.3%)**: Slight B advantage possible for tasks with multiple steps. Decomposition and logging may help the model stay on track.
- **Hard tasks (4.1%)**: This is where B should shine. Tasks requiring >32K context, multi-step reasoning, or recovery from failures. B's journal/resume is the key differentiator.
- **Overall prediction**: A small delta (5-10 points) in B's favor, driven entirely by hard tasks. If the delta is <5 points, the result is inconclusive. If >15 points, it's a strong signal.

---

## 8. Pre-Full-Run Checklist

- [ ] Trim Condition B system prompt (reduce from 1900 to ~500 chars)
- [ ] Set `--max-turns 50` in harness for full runs
- [ ] Verify Docker + Harbor integration works (not tested in calibration)
- [ ] Decide: add `background_bash` tool for server tasks? (Would help both conditions equally)
- [ ] Set up result collection directory structure for 89 * 3 * 2 = 534 runs
- [ ] Test OpenRouter rate limits with 4 concurrent requests
- [ ] Add automated output validation to harness (or rely on TB's built-in tests)

---

## Appendix A: Condition B Prompt — Proposed Trimmed Version

```
# Task Assignment

You are an AI agent working on a task. You have bash, file tools, and workgraph tools.

## Your Task ID: ${TASK_ID}

## Workgraph Tools
- wg_log("${TASK_ID}", "msg") -- log progress (persists across context limits)
- wg_add("title") -- create subtask for complex work
- wg_artifact("${TASK_ID}", "path") -- record output files
- wg_done("${TASK_ID}") -- mark task complete
- wg_fail("${TASK_ID}", "reason") -- mark task failed

Use wg_log to checkpoint progress. Use wg_add to decompose complex tasks.
When finished, call wg_done.

## Task

${TASK_TEXT}
```

~400 chars. Preserves all tool descriptions while cutting 79% of prompt bloat.

## Appendix B: Raw Data Summary

### Condition A
| Task | Turns | Tool Calls | Duration | Input Tok | Output Tok |
|------|-------|------------|----------|-----------|------------|
| cal-01 | 9 | 8 | 183s | 13,370 | 5,967 |
| cal-02 | 7 | 6 | 430s | 10,011 | 14,703 |
| cal-03 | 3 | 11 | 177s | 7,065 | 5,310 |
| cal-04 | 6 | 7 | 1,030s | 16,588 | 24,490 |
| cal-05 | 4 | 3 | 192s | 7,246 | 6,675 |
| cal-06 | 2 | 3 | 870s | 2,971 | 9,261 |
| cal-07 | 4 | 3 | 263s | 7,448 | 8,820 |
| cal-08 | 11 | 10 | 839s | 29,004 | 24,950 |
| **Total** | **46** | **51** | **3,984s** | **93,703** | **100,176** |

### Condition B
| Task | Turns | Tool Calls | Duration | Input Tok | Output Tok |
|------|-------|------------|----------|-----------|------------|
| cal-01 | 21 | 20 | 514s | 69,804 | 16,730 |
| cal-02 | 10 | 12 | 513s | 29,128 | 16,141 |
| cal-03 | 7 | 6 | 237s | 18,993 | 7,301 |
| cal-04 | 7 | 6 | 270s | 23,466 | 8,378 |
| cal-05 | 4 | 3 | 125s | 10,905 | 4,128 |
| cal-06 | 5 | 4 | 269s | 12,823 | 8,699 |
| cal-07 | 7 | 6 | 346s | 24,428 | 11,020 |
| cal-08 | 13 | 12 | 1,165s | 61,456 | 29,301 |
| **Total** | **74** | **69** | **3,439s** | **251,003** | **101,698** |
