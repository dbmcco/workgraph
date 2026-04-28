# Terminal-Bench 2.0 Leaderboard Submissions

Submission data for the [Terminal-Bench 2.0 leaderboard](https://huggingface.co/datasets/harborframework/terminal-bench-2-leaderboard).

## Conditions

| Directory | Agent | Model | Tasks | Trials | Pass Rate | Data Format |
|-----------|-------|-------|-------|--------|-----------|-------------|
| `condition-a/` | Bare agent (no wg context) | Minimax M2.7 | 89 | 1/task | 41.6% | Harbor native |
| `condition-f/` | Full wg context + surveillance | Minimax M2.7 | 18 | 5/task | 98.9% | wg runner |

## Data Provenance

### Condition A (`pilot-a-89`)
- **Source**: `results/pilot-a-89/` — run via Harbor framework
- **Format**: Harbor-native `result.json` files in each trial directory
- **Config**: `ConditionAAgent` with `timeout_multiplier: 1.0`, no overrides
- **Model verification**: `all_agents_used_m2_7: true`, `no_claude_fallback: true`

### Condition F (`pilot-f-89`)
- **Source**: `results/pilot-f-89/` — run via wg runner (not Harbor)
- **Format**: `summary.json` + per-trial `workgraph_state/` directories
- **Config**: `ConditionFAgent` with full wg context injection + surveillance loops
- **Model verification**: `model_verified: true` on all 90 trials

## Submission Readiness

**NOT YET SUBMITTABLE.** The leaderboard requires:
- 89 tasks × 5 trials per condition (minimum)
- Harbor-native `result.json` in every trial directory

Current gaps:
- **Condition A**: Has all 89 tasks but only 1 trial each (need 5)
- **Condition F**: Has 5 trials but only 18 tasks (need 89)
- **Condition F**: Data in wg runner format; needs re-run through Harbor for leaderboard compatibility

## Next Steps

1. Run full 89-task × 5-trial experiments through Harbor for both conditions
2. Audit for API keys in agent logs before packaging
3. Fork `harborframework/terminal-bench-2-leaderboard` on HuggingFace
4. Copy condition directories into `submissions/terminal-bench/2.0/`
5. Open PR for bot validation

See `terminal-bench/docs/HOWTO-submit-to-leaderboard.md` for the full workflow.
