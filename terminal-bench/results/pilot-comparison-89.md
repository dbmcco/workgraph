# Pilot Comparison: Condition A vs Condition F — 89-Task Scale

**Date:** 2026-04-07
**Model:** openrouter:minimax/minimax-m2.7 (MiniMax M2.7)
**Scale:** A = 89 unique tasks × 1 trial; F = 18 unique tasks × 5 replicas (90 trials)

## Executive Summary

| Metric | Condition A | Condition F | Ratio / Difference |
|--------|-------------|-------------|-------------------|
| **Pass rate** | 37/89 (41.6%) | 89/90 (98.9%) | +57.3 pp |
| **95% CI** | [31.9%, 52.0%] | [94.0%, 99.8%] | Non-overlapping |
| **Mean time (all)** | 289.7s | 304.4s | 1.05x |
| **Median time (all)** | 246.2s | 162.1s | 0.66x |
| **Total tokens** | 18.2M | 63.9M | 3.5x |
| **Tokens/trial** | 203,953 | 709,753 | 3.5x |
| **Agents/trial** | 1.0 | 2.5 | 2.5x |
| **Turns/trial** | 18.0 | 29.8 | 1.7x |
| **Wall clock** | 7.2h | 7.6h | 1.06x |
| **Surveillance loops fired** | N/A | 0/90 | — |
| **Issues caught by surveillance** | N/A | 0 | — |

**Bottom line:** F's pass rate (98.9%) dramatically exceeds A's (41.6%), but the two conditions ran **different task sets**. The raw pass-rate gap is not a valid treatment-effect estimate. On the 8 overlapping tasks, A scored 50% vs F's 100% — a strong signal that wg context helps, but confounded by F's 5-replica design giving it more chances. Surveillance loops added zero value: every passing trial converged on the first attempt with no issues detected.

## Condition Definitions

| Condition | Executor | WG Context | Surveillance Loop | Task Set | Replicas |
|-----------|----------|------------|-------------------|----------|----------|
| **A** | docker_agent_loop | None (clean scope) | No | 89 TB 2.0 standard tasks | 1 |
| **F** | wg-native | Graph scope + WG Quick Guide | Yes (max 3 iter, 1m delay) | 18 tasks (8 calibration + 10 hard) | 5 |

## Critical Design Caveat

**The task sets are not matched.** Condition A ran all 89 Terminal Bench 2.0 tasks (1 trial each). Condition F ran 18 selected tasks (5 replicas each). Only **8 tasks overlap** by name. The remaining 81 A-only tasks and 10 F-only tasks cannot be directly compared.

This means:
1. The headline pass-rate difference (57.3 pp) conflates the treatment effect with task-selection bias
2. F's 10 unique tasks (file-ops, text-processing, debugging, etc.) are calibration-level — likely easier than A's full TB 2.0 suite
3. F's 5-replica design measures reliability; A's single-shot design measures breadth

**The valid comparison surface is the 8 overlapping hard tasks** (40 F trials vs 8 A trials).

## Model Verification

| Condition | Requested Model | Verified All M2.7 | Claude Fallback |
|-----------|-----------------|--------------------|-----------------|
| A | openrouter:minimax/minimax-m2.7 | Yes (89/89) | None detected |
| F | openrouter:minimax/minimax-m2.7 | Yes (90/90) | None detected |

Both conditions confirmed M2.7 on every trial. No model routing errors.

## 1. Pass/Fail Rates by Difficulty Tier

### Condition F Difficulty Breakdown

| Difficulty | Trials | Passed | Pass Rate | 95% CI | Mean Time |
|-----------|--------|--------|-----------|--------|-----------|
| Easy | 10 | 10 | 100.0% | [72.2%, 100%] | 128.1s |
| Medium | 15 | 15 | 100.0% | [79.6%, 100%] | 84.3s |
| Hard | 65 | 64 | 98.5% | [91.8%, 99.7%] | 382.4s |

F's only failure was `iterative-test-fix-r1` (hard tier) — a timeout after 1,805s and 137 turns. This is a genuine model-capability failure, not infrastructure.

### Condition A (no difficulty labels available)

A passed 37/89 (41.6%). The 52 failures span diverse tasks: cryptanalysis, system builds, video processing, language-specific tooling, and more. A's broader task set includes many problems that are likely harder than anything in F's 18-task subset.

### Matched-Task Comparison (8 overlapping tasks, all hard tier)

| Task | A (1 trial) | F (5 trials) | Verdict |
|------|-------------|-------------|---------|
| build-cython-ext | **PASS** (248.8s) | 5/5 pass (mean 124.3s) | Both pass; F faster |
| cobol-modernization | **PASS** (414.5s) | 5/5 pass (mean 826.5s) | Both pass; A faster |
| configure-git-webserver | **FAIL** (350.0s) | 5/5 pass (mean 149.0s) | F wins |
| constraints-scheduling | **FAIL** (77.2s) | 5/5 pass (mean 220.5s) | F wins |
| financial-document-processor | **FAIL** (239.1s) | 5/5 pass (mean 674.1s) | F wins |
| fix-code-vulnerability | **FAIL** (261.7s) | 5/5 pass (mean 167.0s) | F wins |
| mailman | **PASS** (246.2s) | 5/5 pass (mean 400.3s) | Both pass; A faster |
| multi-source-data-merger | **PASS** (70.4s) | 5/5 pass (mean 243.2s) | Both pass; F slower |

**Matched results:** A passed 4/8, F passed 40/40 (100% across 5 replicas per task).

On the 4 tasks where A failed, F passed all 20 trials (5 replicas × 4 tasks). This is the strongest evidence that wg context improves outcomes — but note that F also used 5 attempts per task, and even a single retry could explain some of this gap.

On the 4 tasks where both passed, F was faster on 1 (build-cython-ext: 0.50x), slower on 3 (cobol-modernization: 2.0x, mailman: 1.6x, multi-source-data-merger: 3.5x). The wg context overhead adds latency on problems the model could already solve.

## 2. Timing Comparison

### Aggregate Timing

| Metric | Condition A | Condition F |
|--------|-------------|-------------|
| Mean | 289.7s | 304.4s |
| Median | 246.2s | 162.1s |
| Std Dev | 211.8s | 381.4s |
| p5 | 66.0s | 51.8s |
| p25 | 140.8s | 77.0s |
| p50 | 246.2s | 162.1s |
| p75 | 350.0s | 382.7s |
| p90 | 574.1s | 838.4s |
| p95 | 798.1s | 1,234.2s |
| IQR | 209s | 306s |

F has a **bimodal** distribution: many fast trials (easy/medium tasks complete quickly) and a long tail from hard tasks (cobol-modernization, financial-document-processor, iterative-test-fix). F's median is lower than A's (162s vs 246s) because F's calibration tasks are fast, but the p95 is much higher (1,234s vs 798s) because F's hard tasks with wg overhead take longer.

### Passed-Only Timing

| Metric | A (n=37) | F (n=89) |
|--------|----------|----------|
| Mean | 213.0s | 287.6s |
| Median | 218.7s | 162.0s |
| Std Dev | 120.3s | 348.1s |
| p95 | 414.5s | 1,184.1s |

### Per-Task Timing (Matched Tasks)

| Task | A Time | F Median | F Min–Max | F/A Ratio |
|------|--------|----------|-----------|-----------|
| build-cython-ext | 248.8s | 112.3s | 102–172s | **0.45x** |
| cobol-modernization | 414.5s | 823.3s | 398–1,244s | **1.99x** |
| mailman | 246.2s | 442.6s | 177–657s | **1.80x** |
| multi-source-data-merger | 70.4s | 172.1s | 132–563s | **2.44x** |

For the 4 tasks both conditions passed, F is slower 3 out of 4 times. The wg context injection + surveillance agent add ~1.5–2.5x time overhead on average when the model can already solve the task.

## 3. Quality Assessment

**No quality differentiation is possible.** Both conditions used binary pass/fail verification (the same `--verify` commands). When both pass, the solutions are functionally equivalent by definition — there is no code-quality metric collected beyond the verify gate.

For the 4 tasks where A failed but F passed, F produced working solutions that A could not. This is a meaningful quality signal: **wg context enabled the model to solve 4 additional hard tasks** (configure-git-webserver, constraints-scheduling, financial-document-processor, fix-code-vulnerability).

However, this could also be attributed to F's 5 replicas — the model might have succeeded on some of these tasks in A with multiple attempts. A's single-trial design cannot distinguish "hard but solvable with retries" from "requires wg context."

## 4. Surveillance Value

### Quantified Impact: Zero

| Metric | Value |
|--------|-------|
| Surveillance loops created | 90/90 |
| Cycle edges created | 90/90 |
| Total surveillance iterations across all trials | **0** |
| Trials converged first try | 86/90 (96%) |
| Trials needing retry | **0/90** |
| Issues detected by surveillance | **0** |
| Issues detected count | **0** |

The surveillance loop infrastructure worked correctly (loops and edges created for every trial), but **never activated**. Every trial either passed on the first attempt or failed outright (the 1 iterative-test-fix timeout). No trial entered a fix-and-retry cycle.

### What Surveillance Cost

The surveillance agent adds overhead without contributing to outcomes:
- **2.5 agents spawned per trial** (vs 1.0 in A) — the second agent is the surveillance watcher
- **3.5x token overhead** per trial (709,753 vs 203,953)
- **1.7x more turns** per trial (29.8 vs 18.0)

Estimated surveillance-specific overhead (context injection + surveillance agent):
- ~500,000 extra input tokens per trial × 90 trials ≈ **45M tokens** of pure overhead
- At typical API pricing, this would be significant (though M2.7 via OpenRouter reports $0.00)

### Why Surveillance Added No Value

1. **M2.7 was reliable enough.** On F's 18-task set, the model produced correct solutions on the first attempt 89/90 times. There were no cases where a first attempt failed but a surveillance-triggered retry succeeded.
2. **Binary verify gate is sufficient.** The surveillance agent essentially re-runs the same verify command that `--verify` already checks. Without deeper inspection logic (code review, edge case testing), it's redundant.
3. **The one failure was a timeout.** iterative-test-fix-r1 ran for 1,805s and 137 turns before timing out — surveillance cannot fix a model that runs out of budget.

### Surveillance value is latent, not disproven

This result does **not** prove surveillance is useless in general. It proves it was not needed for this model-task combination. Surveillance may show value when:
- The model fails ~10–30% of the time (creating retriable failures)
- The surveillance agent applies deeper checks than `--verify`
- Tasks have subtle correctness issues that pass basic verification

## 5. Cost Comparison

### Token Costs

| Metric | Condition A | Condition F | F/A Ratio |
|--------|-------------|-------------|-----------|
| Input tokens | 17,634,354 | 62,778,390 | 3.6x |
| Output tokens | 517,433 | 1,099,411 | 2.1x |
| **Total tokens** | **18,151,787** | **63,877,801** | **3.5x** |
| Reported USD | $0.00 | $0.00 | N/A |

Both conditions report $0.00 cost (OpenRouter's M2.7 pricing). **Token count is the only available cost proxy.**

### Per-Trial Cost

| Metric | Condition A | Condition F | F/A Ratio |
|--------|-------------|-------------|-----------|
| Tokens/trial | 203,953 | 709,753 | **3.5x** |
| Turns/trial | 18.0 | 29.8 | 1.7x |
| Agents/trial | 1.0 | 2.5 | 2.5x |

### Cost-Effectiveness

| Metric | Condition A | Condition F |
|--------|-------------|-------------|
| Tokens per pass | 490,859 | 717,726 |
| Tokens per pass (matched 8 tasks only) | 725,822 (4 passes) | 201,919 (40 passes) |
| Wall clock | 7.2h | 7.6h |

At the aggregate level, A is more token-efficient per pass because its 37 passes cost 18.2M tokens (490K/pass) while F's 89 passes cost 63.9M tokens (718K/pass). But this comparison is skewed by different task sets.

On the **matched 8 tasks**, F is more cost-effective: A spent 2.9M tokens for 4 passes (725K/pass) while F spent 8.1M tokens for 40 passes (202K/pass). F's wg context actually **reduced** per-pass token cost on shared tasks — likely because the context helps the model solve tasks more efficiently, avoiding wasted tokens on failed approaches.

### Does F's Overhead Justify Its Benefits?

On these 18 tasks: **No.** Surveillance added 3.5x token cost and caught nothing. The pass-rate improvement comes from wg context injection, not from surveillance loops.

Recommendation: **wg context without surveillance** (a hypothetical "Condition G") would likely capture most of F's benefit at lower cost. The 5-task pilot comparison already noted this, and the 89-task data confirms it.

## 6. Model Verification

| Check | Condition A | Condition F |
|-------|-------------|-------------|
| All trials used M2.7 | ✅ 89/89 | ✅ 90/90 |
| No Claude fallback | ✅ | ✅ |
| Executor type | docker_agent_loop | wg-native |
| WG context leaked | No | N/A (intentional) |

Both conditions cleanly routed all trials to M2.7 via OpenRouter. No model contamination.

## F Rerun Context

F's original run (2026-04-07 ~00:00–08:40 UTC) suffered a DNS/network outage starting around trial 61, causing 29 consecutive failures. These were **operational failures** (DNS resolution, connection resets), not model failures. All 29 were re-run after network recovery:

| Metric | Value |
|--------|-------|
| Original passed | 61/90 (67.8%) |
| Original DNS failures | 29/90 |
| Re-run passed | 28/29 (96.6%) |
| Re-run genuine failure | 1 (iterative-test-fix-r2) |
| **Merged total** | **89/90 (98.9%)** |

The single genuine failure confirms the merged result is a valid model-capability measurement.

## Threats to Validity

1. **Different task sets (primary confound).** A's 89-task set and F's 18-task set overlap on only 8 tasks. The pass-rate gap cannot be attributed to the treatment alone.
2. **Single trial vs 5 replicas.** A ran each task once; F ran each task 5 times. A's pass rate might be higher with retries.
3. **Cost reported as $0.00.** No dollar-cost comparison is possible. Token count is an imperfect proxy.
4. **Surveillance tested at lowest-value mode.** The surveillance agent re-ran verify commands. A more sophisticated surveillance agent (code review, edge case testing) might have shown value.
5. **DNS outage in F.** 29/90 trials were re-run after a network failure. The re-runs occurred hours later under potentially different network/API conditions. We flag this but assess impact as minimal (28/29 passed, matching the original 100% pass rate on non-DNS-affected trials).
6. **No difficulty labels for A.** We cannot stratify A's 89 tasks by difficulty, making cross-condition difficulty matching impossible beyond the 8 overlapping tasks.

## Go/No-Go Recommendation

### Recommendation: **GO for matched-set experiment, NO-GO for publication of current comparison**

**Why not publish this comparison:**
- The task-set mismatch is a fatal confound. The headline "98.9% vs 41.6%" compares apples to oranges.
- Surveillance showed zero value, which is publishable only as a null result (interesting but not compelling).

**What to do next:**

1. **Run A on F's 18 tasks** (18 tasks × 5 replicas = 90 trials, condition A). This creates a proper matched comparison: same tasks, same replicas, different treatment. This is the minimum viable experiment for a treatment-effect claim.

2. **Run F without surveillance** (condition G: wg context only, no loop). This isolates whether the benefit comes from context injection or the surveillance loop. Based on this data (surveillance fired 0 times), the prediction is that G ≈ F in pass rate at lower token cost.

3. **If both experiments confirm F > A on matched tasks**, the result is publishable with proper statistical power (5 replicas × 18 tasks = 90 paired comparisons per condition).

4. **Consider broader task coverage.** F's 18-task set may be too easy for M2.7 (98.9% pass rate leaves little room for improvement). A more interesting experiment would select tasks where A's pass rate is 30–70% — the zone where treatment effects are most detectable.

---

*Generated by pilot-compare-a-vs-f-89 agent. Source data: terminal-bench/results/pilot-a-89/summary.json, terminal-bench/results/pilot-f-89/summary.json*
