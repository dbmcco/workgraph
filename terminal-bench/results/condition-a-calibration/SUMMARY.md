# Condition A Calibration Summary

**Date**: 2026-04-03
**Model**: qwen/qwen3-32b via OpenRouter
**Condition**: A (bare agent, no workgraph)
**Harness**: tb-harness.sh (native executor, Condition A bundle)
**Tasks**: 8 diverse tasks spanning 3 difficulty levels and 8 categories

> **Note**: This calibration was performed with Qwen3-32B. The primary experiment model was subsequently changed to **Minimax M2.7**.

## Results

| # | Task ID | Category | Difficulty | Result | Duration | Turns | Input Tok | Output Tok | Notes |
|---|---------|----------|------------|--------|----------|-------|-----------|------------|-------|
| 1 | cal-01-file-ops | file-ops | easy | **PASS** | 183s | 9 | 13,370 | 5,967 | Created dir structure, ran pytest, verified. Had to create venv for pytest. |
| 2 | cal-02-text-processing | text-processing | easy | **PASS** | 430s | 7 | 10,011 | 14,703 | Word frequency script correct. Tried wg_done (not available in Cond A). |
| 3 | cal-03-debugging | debugging | medium | **PASS** | 177s | 3 | 7,065 | 5,310 | All 3 bugs found and fixed. Used `python` initially (not found), recovered with `python3`. All 6 tests pass. |
| 4 | cal-04-shell-scripting | shell-scripting | medium | **FAIL** | 1030s | 6 | 16,588 | 24,490 | Output has bugs: "Total requests: 9" (should be 10), URL parsing shows "10 000". Multiple rewrites didn't fix core issues. |
| 5 | cal-05-data-processing | data-processing | medium | **PASS** | 192s | 4 | 7,246 | 6,675 | JSON generation + CSV aggregation correct. Had one tool arg parse error, recovered. |
| 6 | cal-06-algorithm | algorithm | hard | **PASS** | 870s | 2 | 2,971 | 9,261 | KV store with nested transactions: output matches expected exactly. Stream interruptions caused slow wall time. |
| 7 | cal-07-ml | ml | hard | **PASS** | 263s | 4 | 7,448 | 8,820 | K-means from scratch: centroids within 0.1 of true centers, 50 pts/cluster, inertia 71.55 < 100. |
| 8 | cal-08-sysadmin | sysadmin | hard | **FAIL** | 839s | 11 | 29,004 | 24,950 | HTTP server with rate limiting: multiple timeouts, port binding conflicts, bash syntax errors. Agent claimed success but evidence shows failures. |

## Aggregate Statistics

- **Pass rate**: 6/8 = **75%**
- **By difficulty**:
  - Easy: 2/2 (100%)
  - Medium: 2/3 (67%)
  - Hard: 2/3 (67%)
- **Total duration**: 3,984s (~66 min)
- **Mean duration**: 498s (~8.3 min) per task
- **Total tokens**: 93,703 input + 100,176 output = 193,879 total
- **Mean tokens/task**: ~24,235 total

## Key Findings

### 1. Harness works across task diversity
tb-harness.sh successfully ran all 8 tasks to completion (exit_code=0 for all). The native executor + OpenRouter pipeline is functional.

### 2. Categories where the agent struggles
- **Long-running processes (sysadmin)**: Tasks requiring background server + client testing are problematic. The agent struggles with: (a) running servers in background from bash tool, (b) port conflicts on retry, (c) command timeout limits. This is a known difficulty category in Terminal Bench.
- **Complex shell scripting**: The agent had trouble with awk/sed parsing and escaping in shell scripts, leading to incorrect results even after multiple attempts.

### 3. Baseline pass rate
**75% on this calibration set** (6/8). However, this set was designed to be achievable, not matched to Terminal Bench difficulty distribution. Real TB tasks involve Docker containers, pre-installed environments, and outcome-based tests — expect lower pass rate on the real benchmark.

### 4. Timing & token observations
- qwen/qwen3-32b via OpenRouter is **very slow**: generates many thinking tokens (up to 24K output tokens per task)
- OpenRouter stream interruptions add significant latency (task 6: 870s for only 2 turns)
- The model's thinking-heavy approach means wall-clock time doesn't correlate well with task difficulty
- **Recommended timeout**: 1200s (20 min) for TB runs to accommodate slow model + stream retries
- **Recommended max-turns**: 30 for easy/medium, 40 for hard tasks

### 5. Error patterns
- **Tool arg parse failures**: One instance of JSON parse error in tool arguments (task 5). The native executor recovered by returning error to model.
- **Missing command**: Model used `python` instead of `python3` (task 3). Self-corrected.
- **Tool confusion**: Model tried to call `wg_done` in Condition A where it's not available (task 2).
- **Bash timeout**: 120s default bash timeout is too short for server startup tasks. Consider configurable per-task.

### 6. Token efficiency
| Difficulty | Avg Input Tokens | Avg Output Tokens | Avg Total |
|------------|-----------------|-------------------|-----------|
| Easy | 11,691 | 10,335 | 22,026 |
| Medium | 10,300 | 12,158 | 22,458 |
| Hard | 13,141 | 14,344 | 27,485 |

Output tokens are high across all difficulties due to qwen3-32b's thinking mode. This inflates cost but is inherent to the model, not the scaffold.

## Recommendations for Full Run

1. **Increase timeout to 1200s** (20 min) to handle OpenRouter stream issues
2. **Increase bash tool timeout** for tasks involving servers (or add a `background_bash` tool)
3. **Consider retry logic** at the task level for OpenRouter stream interruptions
4. **The model works well** for code generation, debugging, and algorithm tasks
5. **Sysadmin/server tasks** may need special handling (dedicated background process tool)
6. **Shell scripting** accuracy is a weakness — test with more regex/parsing tasks
