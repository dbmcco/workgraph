# Terminal Bench Calibration Report: Condition A vs Condition B

**Date**: 2026-04-03
**Model**: qwen/qwen3-32b via OpenRouter
**Tasks**: 8 diverse (2 easy, 3 medium, 3 hard)
**Trials**: 1 per task per condition (calibration)
**Harness**: tb-harness.sh (native executor, no Docker)

> **Note**: This calibration was performed with Qwen3-32B. The primary experiment model was subsequently changed to **Minimax M2.7**.

## Results Summary

| Task | Category | Difficulty | A Exit | B Exit | A Turns | B Turns | A Duration | B Duration | A Tokens (in/out) | B Tokens (in/out) |
|------|----------|-----------|--------|--------|---------|---------|------------|------------|-------------------|-------------------|
| cal-01-file-ops | file-ops | easy | 0 | 0 | 9 | 21 | 183s | 514s | 13.4k/6.0k | 69.8k/16.7k |
| cal-02-text-processing | text-processing | easy | 0 | 0 | 7 | 10 | 430s | 513s | 10.0k/14.7k | 29.1k/16.1k |
| cal-03-debugging | debugging | medium | 0 | 0 | 3 | 7 | 177s | 237s | 7.1k/5.3k | 19.0k/7.3k |
| cal-04-shell-scripting | shell-scripting | medium | 0 | 0 | 6 | 7 | 1030s | 270s | 16.6k/24.5k | 23.5k/8.4k |
| cal-05-data-processing | data-processing | medium | 0 | 0 | 4 | 4 | 192s | 125s | 7.2k/6.7k | 10.9k/4.1k |
| cal-06-algorithm | algorithm | hard | 0 | 0 | 2 | 5 | 870s | 269s | 3.0k/9.3k | 12.8k/8.7k |
| cal-07-ml | ml | hard | 0 | 0 | 4 | 7 | 263s | 346s | 7.4k/8.8k | 24.4k/11.0k |
| cal-08-sysadmin | sysadmin | hard | 0 | 0 | 11 | 13 | 839s | 1165s | 29.0k/24.9k | 61.5k/29.3k |

## Aggregate Metrics

| Metric | Condition A | Condition B | Delta |
|--------|-------------|-------------|-------|
| Pass Rate | 8/8 (100%) | 8/8 (100%) | 0% |
| Avg Turns | 5.8 | 9.2 | +3.5 |
| Avg Duration | 498s | 430s | -68s |
| Total Duration | 3984s | 3439s | -545s |
| Total Input Tokens | 93.7k | 251.0k | +157.3k (2.7x) |
| Total Output Tokens | 100.2k | 101.7k | +1.5k |

## By Difficulty

**EASY (2 tasks)**: A=2/2 pass (avg 8 turns, 306s) | B=2/2 pass (avg 16 turns, 514s)
**MEDIUM (3 tasks)**: A=3/3 pass (avg 4 turns, 466s) | B=3/3 pass (avg 6 turns, 211s)
**HARD (3 tasks)**: A=3/3 pass (avg 6 turns, 657s) | B=3/3 pass (avg 8 turns, 593s)

## Workgraph Tool Usage (Condition B)

| Task | wg_done | wg_log | wg_add | wg_artifact | wg_show | bash | write_file | edit_file |
|------|---------|--------|--------|-------------|---------|------|------------|-----------|
| cal-01-file-ops | 1 | 0 | 0 | 0 | 0 | 11 | 7 | 1 |
| cal-02-text-processing | 1 | 0 | 0 | 0 | 0 | 3 | 6 | 2 |
| cal-03-debugging | 1 | 0 | 0 | 0 | 0 | 1 | 1 | 3 |
| cal-04-shell-scripting | 1 | 0 | 0 | 0 | 0 | 1 | 4 | 0 |
| cal-05-data-processing | 1 | 0 | 0 | 0 | 0 | 1 | 1 | 0 |
| cal-06-algorithm | 1 | 0 | 0 | 0 | 0 | 1 | 2 | 0 |
| cal-07-ml | 1 | 0 | 0 | 0 | 0 | 2 | 2 | 1 |
| cal-08-sysadmin | 1 | 0 | 0 | 0 | 0 | 1 | 3 | 7 |

**Critical Finding**: The agent used `wg_done` in every task (to mark completion) but NEVER used `wg_log`, `wg_add`, `wg_artifact`, or `wg_show`. The workgraph tools were available but not leveraged for decomposition, progress logging, or artifact recording.

## Key Observations

1. **Pass rate identical (100% both)**: These calibration tasks were solvable in a single session without context exhaustion. The true test is harder, longer tasks.
2. **Condition B uses ~2.7x more input tokens**: The larger system prompt with wg instructions adds significant token overhead even when wg tools aren't used.
3. **Duration is similar overall**: A averaged 498s, B averaged 430s. Condition B was actually faster for medium/hard tasks despite more turns.
4. **Workgraph tools underutilized**: The model solved all tasks directly with bash/file tools. The system prompt needs stronger enforcement of wg tool usage, similar to how ForgeCode enforces todo_write planning.
5. **No decomposition observed**: Even the 'hard' tasks were solved monolithically. Tasks need to be complex enough that decomposition provides real value.

## Recommendations for Full Run

1. **Enforce wg_log**: Require progress logging at minimum. The system prompt should mandate `wg_log` calls before each tool action.
2. **Add truly hard tasks**: Tasks requiring >32K context to solve will be where Condition B's journal/resume capability matters.
3. **Consider mandated decomposition**: For tasks above a complexity threshold, the prompt should require `wg_add` subtask creation before starting work.
4. **Reduce system prompt overhead**: Trim the Condition B prompt to reduce input token cost while keeping wg tool instructions.
5. **Use actual Terminal Bench tasks**: These calibration tasks are simpler than real TB tasks. Docker is needed for the real benchmark.
