# Pilot Task Selection: 10 Tasks at ~50% Pass Rate

**Source:** Condition A re-run (`terminal-bench/results/rerun-condition-a/summary.json`)
**Date:** 2026-04-04

## Selection Criteria

- Pass rate 33–66% in Condition A (1/3 or 2/3 passes)
- All 3 trials valid (no errors — timeout, env build failure, or cancellation)
- Diverse task characteristics (building, debugging, config, reasoning, data processing, etc.)

## Candidate Pool

From 89 tasks total:
- **9 tasks** at 33.3% (1/3 pass) — all with 0 errors
- **14 tasks** at 66.7% (2/3 pass) — all with 0 errors
- **23 total candidates** meeting criteria

Tasks excluded: those with errors (timeout/env build), 0% pass rate (too hard), or 100% pass rate (too easy).

## Selected Tasks (5 × 33.3% + 5 × 66.7%)

### 33.3% Pass Rate (1/3)

| # | Task ID | Category | Description |
|---|---------|----------|-------------|
| 1 | `build-cython-ext` | Building | Build Cython extension for pyknotid; verify numpy, repo clone, import |
| 2 | `cancel-async-tasks` | Async programming | Implement async task runner with cancellation + max-concurrency |
| 3 | `nginx-request-logging` | Server config | Install/configure nginx with request logging and custom index |
| 4 | `overfull-hbox` | Debugging (LaTeX) | Fix overfull hbox warnings without modifying synonym definitions |
| 5 | `regex-log` | Text processing | Parse log files with regex to extract structured data |

### 66.7% Pass Rate (2/3)

| # | Task ID | Category | Description |
|---|---------|----------|-------------|
| 6 | `count-dataset-tokens` | Data processing | Count tokens in dataset, produce specific output format |
| 7 | `custom-memory-heap-crash` | Debugging (C/memory) | Debug heap crash without modifying protected files; compile debug+release |
| 8 | `merge-diff-arc-agi-task` | Reasoning/AI | Init git repo, fetch ARC-AGI bundles, write merge-diff solver |
| 9 | `qemu-startup` | System/emulation | Set up and boot a QEMU VM; version check verification |
| 10 | `sparql-university` | Query language | Write SPARQL queries against university dataset |

## Diversity Analysis

The 10 tasks span these dimensions:
- **Build from source:** `build-cython-ext` (Python/C), `qemu-startup` (VM)
- **Debugging:** `overfull-hbox` (LaTeX), `custom-memory-heap-crash` (C heap)
- **Multi-step implementation:** `cancel-async-tasks`, `merge-diff-arc-agi-task`
- **Configuration:** `nginx-request-logging`
- **Data/text processing:** `regex-log`, `count-dataset-tokens`
- **Domain-specific querying:** `sparql-university`

## Why These Tasks

These borderline tasks are where improved agent behavior is most likely to show an effect:

1. **Verification loops** should help tasks where the agent got close but didn't validate its output (e.g., `regex-log`, `count-dataset-tokens`, `sparql-university`)
2. **Decomposition** should help multi-step tasks where the agent needs to break work into phases (e.g., `merge-diff-arc-agi-task`, `cancel-async-tasks`, `nginx-request-logging`)
3. **Organization generation** should help complex tasks requiring structured approaches (e.g., `custom-memory-heap-crash`, `build-cython-ext`, `qemu-startup`)

## Not Selected (remaining 13 candidates)

33.3%: `mailman`, `sqlite-with-gcov`, `tune-mjcf`, `winning-avg-corewars`
66.7%: `bn-fit-modify`, `break-filter-js-from-html`, `build-pmars`, `build-pov-ray`, `fix-git`, `largest-eigenval`, `log-summary-date-ranges`, `multi-source-data-merger`, `vulnerable-secret`

These were excluded to maximize diversity. They could serve as a secondary validation set if the pilot shows promising results.
