# GPT-OSS-120B Condition A Analysis

**Run:** gpt-oss-120b-condition-A  
**Model:** openrouter:openai/gpt-oss-120b:free  
**Date:** 2026-04-13 03:36–04:04 UTC  
**Tasks:** 89 | **Passed:** 5 (5.6%) | **Failed:** 81 | **Errors:** 3  
**Concurrency:** 5 trials simultaneously  

## 1. Executive Summary

**The raw 5.6% pass rate dramatically understates the model's actual capability.** The majority of failures are attributable to infrastructure problems, not model limitations:

| Category | Count | % of 89 | Root Cause |
|----------|-------|---------|------------|
| SSL certificate failures (zero-token) | 16 | 18.0% | Docker containers missing `/usr/lib/ssl/certs` — agent can't reach OpenRouter |
| Docker/infra errors | 3 | 3.4% | Container startup failure (install-windows), wg binary not functional (qemu-*) |
| wg add failure | 1 | 1.1% | Agent setup error (pytorch-model-recovery) — "wg add failed: None" |
| "Explain-and-bail" (1-turn, no tools) | 14 | 15.7% | Model refuses to attempt task, outputs text saying "I can't do this" |
| Rate-limit-truncated (429 killed mid-task) | ~8 | 9.0% | Model was actively working but 429 errors caused fatal termination |
| Genuine model failures (attempted but wrong) | ~42 | 47.2% | Model attempted work across multiple turns but didn't produce correct output |
| **Passed** | **5** | **5.6%** | Model completed task correctly |

**Estimated real pass rate if infrastructure were perfect: ~7–10%.** Removing the 20 pure infrastructure failures (SSL + Docker + wg-add) gives 5/69 = 7.2%. If we also account for rate-limit-truncated tasks where the model was making progress, the ceiling might be 9–12%.

**The model's biggest genuine weakness: "explain-and-bail."** 14 tasks (16%) saw the model immediately refuse to attempt the work, outputting a text explanation of why the task is too hard instead of trying tool calls. This is a model-level behavioral problem, not infrastructure.

## 2. Passing Task Profiles

All 5 passing tasks share common traits: moderate complexity, 3–5 turns, and the model used tools effectively without getting stuck.

### 2.1 constraints-scheduling (5 turns, 27.8K input / 1.3K output, 117s)
- **Task:** Schedule a meeting across three people's calendars (ICS format)
- **Strategy:** Read Alice, Bob, Carol calendars → reason about free slots → write meeting ICS file
- **Complexity:** Simple — parse structured data, find overlap, write structured output
- **Notable:** Even this passing task hit 25 rate-limit (429) errors. The model wrote the correct output file in turn 5, then the agent was killed by 429 errors on the next API call. It passed only because the verifier ran on the already-written file.
- **Termination:** `wg_fail` (rate-limited to death after writing correct answer)

### 2.2 log-summary-date-ranges (3 turns, 13.5K input / 613 output, 48s)
- **Strategy:** Single bash call with inline Python script to parse log files and write CSV summary
- **Complexity:** Simple — read log files, count by severity and date range, write CSV
- **Notable:** Efficient — wrote the entire solution in one Python script in turn 1
- **Termination:** `natural_stop`

### 2.3 modernize-scientific-stack (3 turns, 13.3K input / 680 output, 46s)
- **Strategy:** Wrote a modernized Python analysis script and requirements.txt in 2 turns
- **Complexity:** Simple — rewrite a legacy script to use modern libraries
- **Termination:** `natural_stop`

### 2.4 multi-source-data-merger (4 turns, 19.6K input / 1.4K output, 50s)
- **Strategy:** `ls -R` → write merge script → run it → stop
- **Complexity:** Moderate — read multiple data files, merge with conflict resolution
- **Notable:** Zero 429 errors — this was one of the luckier tasks in timing
- **Termination:** `natural_stop`

### 2.5 openssl-selfsigned-cert (4 turns, 18.3K input / 899 output, 27s)
- **Strategy:** Single bash command chaining openssl commands → verify → write check script
- **Complexity:** Simple — well-known CLI operations
- **Notable:** Zero 429 errors, fastest passing task at 27s
- **Termination:** `natural_stop`

**Pattern:** All 5 passing tasks are simple-to-moderate, well-known programming/sysadmin tasks that the model could solve in ≤5 efficient turns with standard tool calls (bash, read_file, write_file).

## 3. Failure Categorization

### 3.1 SSL Certificate Failures — 16 tasks (ALL zero-token, ALL infra)

**Root cause:** The Docker containers for these 16 tasks have a broken OpenSSL configuration where `/usr/lib/ssl/certs` is missing or inaccessible. When the `wg` binary inside the container attempts to connect to `openrouter.ai` via HTTPS, the TLS handshake fails with:

```
error:0A000086:SSL routines:tls_post_process_server_certificate:certificate verify failed
(unable to get local issuer certificate)
```

The agent binary never makes a single API call. Zero tokens consumed, zero turns executed. All 16 show identical error chains ending with `error sending request for url (https://openrouter.ai/api/v1/chat/completions)`.

**Affected tasks:**
adaptive-rejection-sampler, compile-compcert, dna-assembly, dna-insert, extract-moves-from-video, financial-document-processor, merge-diff-arc-agi-task, overfull-hbox, polyglot-c-py, polyglot-rust-c, regex-log, sparql-university, sqlite-with-gcov, torch-pipeline-parallelism, torch-tensor-parallelism, write-compressor

**Timing distribution:** These are evenly scattered throughout the run (batches 1 through 18), confirming this is a per-container SSL configuration issue, not a temporal rate-limit effect.

**Fix:** Ensure `ca-certificates` is installed and `/usr/lib/ssl/certs` is populated in all task Docker images. Alternatively, mount the host's CA bundle into containers.

### 3.2 Docker/Binary Infrastructure Errors — 4 tasks

| Task | Error | Category |
|------|-------|----------|
| install-windows-3.11 | Docker compose container exited immediately (code 2) | Container build failure |
| qemu-alpine-ssh | "wg binary not functional inside container: None" | Binary compat issue |
| qemu-startup | "wg binary not functional inside container: None" | Binary compat issue |
| pytorch-model-recovery | "wg add failed: None" (0 turns, `llm_error`) | Agent setup failure |

These are purely infrastructure failures — the model was never given a chance to attempt the task.

### 3.3 "Explain-and-Bail" One-Turn Failures — 14 tasks

These are the model's most distinctive failure mode. The model outputs text explaining why it can't do the task, without attempting a single tool call (bash, write_file, etc.). The model says variations of:

- *"I'm unable to complete this task in the current environment"*
- *"The task requires [X] which cannot be performed within the constraints"*
- *"I'm unable to complete the required implementation within the given constraints"*

**Analyzed examples:**

| Task | Model's Excuse | Could It Have Tried? |
|------|----------------|---------------------|
| build-cython-ext | "Missing build tools, system-wide Python" | Yes — build tools were available in the container |
| build-pov-ray | [empty output — only 51 tokens, 48 in reasoning] | Yes — should have at least explored the filesystem |
| caffe-cifar-10 | "Insufficient environment to install Caffe" | Maybe — Caffe is legitimately hard to build |
| circuit-fibsqrt | "Can't generate 32,000-line logic-gate program" | Debatable — this is a hard task |
| configure-git-webserver | Wrote a plan but no tool calls | Yes — had a plan, never executed it |
| extract-elf | "Needs detailed understanding of binary" | Yes — standard reverse-engineering task |
| feal-linear-cryptanalysis | "Non-trivial cryptanalysis implementation" | Should have tried |
| git-multibranch | "Requires full Git server with SSH access" | Yes — the container likely had git |
| make-doom-for-mips | "Need MIPS cross-compiler" | Could have checked if one was available |
| make-mips-interpreter | "Substantial low-level emulation" | Should have tried |
| model-extraction-relu-logits | "Reverse-engineering a neural network" | Should have tried |
| regex-chess | "Can't implement chess move generator in regex" | Debatable — very hard task |
| llm-inference-batching-scheduler | "Can't meet performance thresholds" | Should have tried |
| protein-assembly | "Missing required external data" | Could have verified |

**Two sub-patterns:**
1. **Legitimate difficulty** (2-3 tasks): circuit-fibsqrt, regex-chess — these are genuinely very hard tasks where refusing is somewhat reasonable
2. **Premature surrender** (11-12 tasks): The model assumes the environment is insufficient without checking. This is a behavioral/training issue — the model was likely trained to be cautious and doesn't explore before giving up.

**Notable:** `sam-cell-seg` had 1 turn with 1,979 output tokens — it wrote code (a conversion script) but without running it, so the output wasn't functional. `winning-avg-corewars` similarly claimed completion in text but never executed anything.

### 3.4 Rate-Limit-Truncated Tasks — ~8 tasks

These tasks show the model actively working (multiple tool calls, making progress) but then getting killed by cascading 429 errors. The native executor retries 3 times per request, then surfaces the error to the model up to 3 more times, and after 4 consecutive 429 failures, gives up.

**Clear examples:**
- **constraints-scheduling** (passed anyway): 25× 429 errors, killed after turn 5 but had already written correct answer
- **vulnerable-secret** (14 turns): Was actively investigating a binary exploit, hit 23× 429 errors, died
- **reshard-c4-data** (14 turns): 35× 429 errors, was writing data processing scripts
- **portfolio-optimization** (11 turns): 32× 429 errors

**The rate limit pattern:** `free-models-per-min` was the dominant limiter (427 total 429 errors across all tasks, with 367 being per-minute limits). With 5 concurrent trials, each making API calls, the aggregate request rate easily exceeded the free tier's per-minute limit.

### 3.5 Genuine Multi-Turn Model Failures — ~42 tasks

These tasks show the model attempting work across 2–23 turns but failing to produce correct output. Sub-patterns:

#### 3.5.1 Low output-per-turn (model struggling)

Many multi-turn failures show extremely low output tokens per turn (40–80), suggesting the model is making small, ineffective actions:
- **fix-code-vulnerability** (7 turns, 40 tokens/turn): Poking around with bash but not fixing anything
- **code-from-image** (10 turns, 45 tokens/turn): Repeated small attempts
- **feal-differential-cryptanalysis** (8 turns, 46 tokens/turn): Small bash commands, never writing real code

Compare to passing tasks which average 204–362 output tokens per turn — the model produces substantive code/scripts per turn when it knows what to do.

#### 3.5.2 Wrong strategy / stuck in loops

- **db-wal-recovery** (22 turns): The model ran many sqlite3 commands (`.dump`, `.recover`, `PRAGMA`, `.backup`) but never identified the actual recovery steps needed. It kept trying variations of the same approach.
- **large-scale-text-editing** (17 turns): The model repeatedly wrote and re-wrote a Vim macro file, ran it, checked output, and rewrote — classic trial-and-error loop without converging.
- **largest-eigenval** (23 turns): Longest task — edited eigen.py, ran it, got wrong answer, edited again. 56× 429 errors also slowed it down.
- **build-pmars** (11 turns): Tried apt-get, source modifications, Makefile edits — was actually making reasonable progress fixing a build but ran out of runway.

#### 3.5.3 2-3 turn failures (ran out of steam quickly)

17 tasks had 2-3 turns. These show the model making an initial attempt then stopping:
- **cobol-modernization** (3 turns, 1,209 output): Wrote code but didn't verify/fix it
- **gpt2-codegolf** (3 turns, 1,917 output): Produced substantial code but it didn't pass verification
- **schemelike-metacircular-eval** (3 turns, 1,593 output): Wrote an evaluator but it wasn't correct
- **chess-best-move** (2 turns, 103 output): Barely attempted the task

## 4. Rate Limit Timeline

### 4.1 Aggregate 429 Errors

| Metric | Value |
|--------|-------|
| Total 429 errors across all 89 tasks | 427 |
| Tasks experiencing at least one 429 | 31 (35%) |
| Tasks with >10 429 errors | 14 |
| `free-models-per-min` errors | 367 (86%) |
| `free-models-per-day-high-balance` errors | 6 (1.4%) |

### 4.2 Temporal Pattern

The 429 errors are **not** clustered later in the run — they occur throughout, because 5 concurrent trials all share the same API key and rate limit. Key observations:

- **Batch 1** (03:36): `reshard-c4-data` already hitting 35× 429 errors — rate limits were active from the very start
- **Batch 3** (03:37): `largest-eigenval` hitting 56× 429 — this was the worst-affected task
- **Batch 17-18** (04:01+): `constraints-scheduling` hit 25× 429, `vulnerable-secret` hit 23× 429 — rate limits persisted to the end

The `free-models-per-day-high-balance` errors (6 total) only appeared in the last 3 minutes of the run (constraints-scheduling, vulnerable-secret), suggesting the daily quota was nearly exhausted by end of run.

### 4.3 Effective Rate

- Run duration: ~28 minutes (03:36 to 04:04)
- Total successful turns across all tasks: ~464 (sum of all task turns)
- Approximate actual API calls: ~464 successful + ~427 rejected = ~891 attempts
- Effective rate: ~32 requests/minute attempted
- With 5 concurrent trials: ~6.4 requests/minute per trial

The free tier limit for OpenRouter is typically 20 requests/minute. With 5 concurrent tasks, the aggregate rate of ~32 req/min was well above this, explaining the persistent rate limiting.

### 4.4 Zero-Token Tasks Are NOT Rate-Limited

The 16 zero-token tasks are definitively SSL certificate failures, not rate limit issues. They have zero 429 errors and identical OpenSSL error traces. Their timing is evenly distributed (not clustered late in the run).

## 5. Systematic Patterns

### 5.1 Task Type Pattern

| Task Type | Pass | Fail | Pass Rate |
|-----------|------|------|-----------|
| Data processing / scripting | 3 | 8 | 27% |
| Sysadmin / infrastructure | 1 | 12 | 8% |
| Build/compile tasks | 0 | 8 | 0% |
| Security / reverse-engineering | 0 | 7 | 0% |
| Scientific computing | 1 | 8 | 11% |
| ML/AI tasks | 0 | 9 | 0% |
| Formal methods / math | 0 | 4 | 0% |
| Other/creative coding | 0 | 10 | 0% |

**Simple data processing and scripting tasks pass most often.** Tasks requiring compilation, security analysis, ML infrastructure, or formal proofs never pass.

### 5.2 Complexity Pattern

| Turn Count | Tasks | Pass | Pass Rate |
|------------|-------|------|-----------|
| 0 (infra) | 20 | 0 | N/A |
| 1 | 16 | 0 | 0% |
| 2–3 | 17 | 2 | 12% |
| 4–5 | 10 | 3 | 30% |
| 6–11 | 17 | 0 | 0% |
| 12+ | 9 | 0 | 0% |

**Sweet spot is 3–5 turns.** All 5 passing tasks completed in 3–5 turns. Tasks requiring 6+ turns all failed — the model either can't sustain a coherent multi-step strategy or gets rate-limited before finishing.

### 5.3 Output Efficiency Pattern

Passing tasks produce 204–362 output tokens per turn on average. Failing multi-turn tasks with <100 tokens/turn are typically thrashing (running small bash commands without real progress). Failing tasks with >300 tokens/turn (headless-terminal at 763, rstan-to-pystan at 489) are writing substantial code that just doesn't pass verification.

## 6. Recommendations for Next Run

### 6.1 Infrastructure Fixes (Critical)

1. **Fix SSL certificates in Docker images.** Install `ca-certificates` and ensure `/usr/lib/ssl/certs` exists. This alone recovers 16 tasks (18% of the run).

2. **Fix wg binary compatibility.** The qemu-* tasks fail because the wg binary doesn't work inside their containers. Either static-link it or match the container's libc.

3. **Fix container startup.** install-windows-3.11 exits immediately — investigate the Docker image.

### 6.2 Rate Limit Mitigation (Critical)

4. **Reduce concurrency to 2–3.** With 5 concurrent trials sharing one free-tier API key, the aggregate rate (~32 req/min) far exceeds the ~20 req/min limit. Reducing to 2–3 concurrent trials would stay under the limit.

5. **Use a paid API tier.** The free tier is clearly insufficient for batch evaluation. A paid tier with higher rate limits would eliminate 429 errors entirely.

6. **Add exponential backoff with longer waits.** The current retry strategy (3 retries with 1–4s waits) is too aggressive. Consider 3–10s base wait with jitter.

### 6.3 Model-Level Improvements

7. **Address explain-and-bail behavior.** 14 tasks (16%) saw the model refuse to try. If using a system prompt, add instructions like "Always attempt the task using available tools before concluding it's impossible. Explore the environment first."

8. **Consider max_turns budget.** Currently set to 50 but no task uses more than 23 turns. The model naturally stops early. This isn't a limiting factor.

### 6.4 Adjusted Scoring

For accurate model capability assessment:

| Scenario | Denominator | Numerator | Pass Rate |
|----------|-------------|-----------|-----------|
| Raw | 89 | 5 | 5.6% |
| Excluding infra failures (SSL + Docker + wg-add) | 69 | 5 | 7.2% |
| Excluding infra + explain-and-bail | 55 | 5 | 9.1% |
| Best case (infra-free + recoverable rate-limit tasks) | 69 | ~7–8 | 10–12% |

**Estimated real pass rate with perfect infrastructure: 7–10%.** The model genuinely solves simple scripting, data processing, and sysadmin tasks in 3–5 turns. It struggles with compilation, security, ML, and formal methods. The explain-and-bail behavior is the biggest model-level issue, costing it opportunities on tasks it might have partially solved.
