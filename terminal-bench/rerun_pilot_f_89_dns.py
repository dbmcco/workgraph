#!/usr/bin/env python3
"""
Re-run the 29 DNS-failed trials from pilot-f-89.

Reads summary.json, identifies the 29 failed trials, re-runs them using the
same infrastructure as run_pilot_f_89.py, then merges results back into
summary.json preserving the original 61 passed trials.
"""

import asyncio
import json
import os
import shutil
import sys
import tempfile
import time
from datetime import datetime, timezone
from pathlib import Path

# Import the trial runner and task definitions from the original script
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from run_pilot_f_89 import (
    ALL_TASKS,
    RESULTS_DIR,
    MODEL,
    REPLICAS,
    MAX_ITERATIONS,
    CYCLE_DELAY,
    run_trial,
    write_summary,
)

SCRIPT_DIR = os.path.dirname(os.path.abspath(__file__))


def load_original_summary():
    """Load the original summary.json."""
    summary_path = os.path.join(RESULTS_DIR, "summary.json")
    with open(summary_path) as f:
        return json.load(f)


def identify_failed_trials(summary: dict) -> list[dict]:
    """Return list of failed trial entries from summary."""
    failed = []
    for trial in summary["trials"]:
        if trial["status"] != "done":
            failed.append(trial)
    return failed


def get_task_def(task_id: str) -> dict:
    """Look up task definition by ID."""
    for t in ALL_TASKS:
        if t["id"] == task_id:
            return t
    raise ValueError(f"Unknown task: {task_id}")


async def main():
    # Load original summary
    original = load_original_summary()
    failed_trials = identify_failed_trials(original)

    print(f"Pilot F-89 DNS Rerun")
    print(f"  Original: {original['passed']}/{original['total_trials']} passed")
    print(f"  Failed trials to re-run: {len(failed_trials)}")
    print(f"  Model: {MODEL}")
    print()

    # List the failed trials
    print("  Failed trials:")
    for t in failed_trials:
        print(f"    {t['trial_id']}  task={t['task']}  replica={t['replica']}")
    print()

    if len(failed_trials) != 29:
        print(f"  WARNING: Expected 29 failed trials, found {len(failed_trials)}")

    # Build list of (task_def, replica) pairs to re-run
    rerun_pairs = []
    for trial in failed_trials:
        task_def = get_task_def(trial["task"])
        rerun_pairs.append((task_def, trial["replica"]))

    # Back up original summary
    backup_path = os.path.join(RESULTS_DIR, "summary-pre-rerun.json")
    with open(backup_path, "w") as f:
        json.dump(original, f, indent=2)
    print(f"  Backed up original summary to {backup_path}")

    # Run the failed trials sequentially (same tmp paths, no concurrency)
    run_trial._counter = 0
    rerun_results = []
    overall_start = time.monotonic()

    for task_def, replica in rerun_pairs:
        r = await run_trial(task_def, replica)
        # Mark as re-run
        r["is_rerun"] = True
        r["rerun_reason"] = "dns_network_failure"
        rerun_results.append(r)

        # Progress
        done_so_far = sum(1 for x in rerun_results if x["status"] == "done")
        print(f"\n  === RERUN PROGRESS: {len(rerun_results)}/{len(failed_trials)}, "
              f"{done_so_far} passed ===\n", flush=True)

    total_wall = time.monotonic() - overall_start

    # Print rerun results
    print(f"\n{'='*60}")
    print(f"  RERUN RESULTS")
    print(f"{'='*60}")
    rerun_passed = sum(1 for r in rerun_results if r["status"] == "done")
    print(f"  Re-run: {rerun_passed}/{len(rerun_results)} passed")
    print(f"  Wall clock: {total_wall:.1f}s ({total_wall/60:.1f}min)")
    print()

    # Merge: keep the 61 original passed trials, replace 29 failed with rerun results
    failed_trial_ids = {t["trial_id"] for t in failed_trials}
    merged_trials_raw = []

    # Keep original passed trials (in their original format from the trial list)
    for trial in original["trials"]:
        if trial["trial_id"] not in failed_trial_ids:
            merged_trials_raw.append(trial)

    # Add rerun results (convert to trial format)
    for r in rerun_results:
        trial_entry = {
            "trial_id": r["trial_id"],
            "task": r["task"],
            "difficulty": r["difficulty"],
            "replica": r["replica"],
            "status": r["status"],
            "elapsed_s": r["elapsed_s"],
            "model_verified": r["model_verified"],
            "surveillance_iterations": r["surveillance"]["iterations"],
            "surveillance_converged": r["surveillance"]["converged"],
            "surveillance_issues": r["surveillance"]["issues_caught"],
            "metrics": r["metrics"],
            "verify_output": r["verify_output"],
            "error": r["error"],
            "is_rerun": True,
            "rerun_reason": "dns_network_failure",
        }
        merged_trials_raw.append(trial_entry)

    # Sort by trial_id to maintain consistent order
    # Build sort key: task order first, then replica
    task_order = {t["id"]: i for i, t in enumerate(ALL_TASKS)}
    merged_trials_raw.sort(
        key=lambda t: (task_order.get(t["task"], 999), t["replica"])
    )

    # Recompute summary stats
    total_trials = len(merged_trials_raw)
    passed_count = sum(1 for t in merged_trials_raw if t["status"] == "done")
    failed_count = total_trials - passed_count

    times = [t["elapsed_s"] for t in merged_trials_raw if t["elapsed_s"] > 0]

    # Token stats
    total_input = sum((t.get("metrics") or {}).get("total_input_tokens", 0) for t in merged_trials_raw)
    total_output = sum((t.get("metrics") or {}).get("total_output_tokens", 0) for t in merged_trials_raw)
    total_cost = sum((t.get("metrics") or {}).get("total_cost_usd", 0.0) for t in merged_trials_raw)
    total_turns = sum((t.get("metrics") or {}).get("total_turns", 0) for t in merged_trials_raw)
    total_agents = sum((t.get("metrics") or {}).get("num_agents_spawned", 0) for t in merged_trials_raw)
    model_verified = sum(1 for t in merged_trials_raw if t.get("model_verified"))

    # Surveillance stats
    surv_iterations_total = sum(t.get("surveillance_iterations", 0) for t in merged_trials_raw)
    surv_converged_first = sum(
        1 for t in merged_trials_raw
        if t.get("surveillance_converged") and t.get("surveillance_iterations", 0) <= 1
    )
    surv_needed_retry = sum(1 for t in merged_trials_raw if t.get("surveillance_iterations", 0) > 1)
    all_issues = []
    for t in merged_trials_raw:
        for issue in t.get("surveillance_issues", []):
            all_issues.append({"trial": t["trial_id"], "issue": issue})

    # Per-difficulty stats
    difficulty_stats = {}
    for diff in ("easy", "medium", "hard"):
        diff_trials = [t for t in merged_trials_raw if t.get("difficulty") == diff]
        if diff_trials:
            d_passed = sum(1 for t in diff_trials if t["status"] == "done")
            d_times = [t["elapsed_s"] for t in diff_trials if t["elapsed_s"] > 0]
            difficulty_stats[diff] = {
                "total": len(diff_trials),
                "passed": d_passed,
                "pass_rate": d_passed / len(diff_trials),
                "mean_time_s": round(sum(d_times) / len(d_times), 2) if d_times else 0,
            }

    # Per-task stats
    task_stats = {}
    for task_def in ALL_TASKS:
        task_trials = [t for t in merged_trials_raw if t.get("task") == task_def["id"]]
        if task_trials:
            t_passed = sum(1 for t in task_trials if t["status"] == "done")
            t_times = [t["elapsed_s"] for t in task_trials if t["elapsed_s"] > 0]
            t_surv_iters = sum(t.get("surveillance_iterations", 0) for t in task_trials)
            task_stats[task_def["id"]] = {
                "total": len(task_trials),
                "passed": t_passed,
                "pass_rate": t_passed / len(task_trials),
                "mean_time_s": round(sum(t_times) / len(t_times), 2) if t_times else 0,
                "total_surveillance_iterations": t_surv_iters,
            }

    # Count reruns
    rerun_count = sum(1 for t in merged_trials_raw if t.get("is_rerun"))
    rerun_passed = sum(1 for t in merged_trials_raw if t.get("is_rerun") and t["status"] == "done")

    merged_summary = {
        "run_id": "pilot-f-89",
        "condition": "F",
        "description": "Condition F at scale: wg-native with surveillance loops, 18 tasks x 5 replicas (29 DNS-failed trials re-run)",
        "timestamp": datetime.now(timezone.utc).isoformat(),
        "original_timestamp": original["timestamp"],
        "model": MODEL,
        "replicas": REPLICAS,
        "unique_tasks": len(ALL_TASKS),
        "max_iterations": MAX_ITERATIONS,
        "cycle_delay": CYCLE_DELAY,
        "total_trials": total_trials,
        "passed": passed_count,
        "failed": failed_count,
        "pass_rate": passed_count / total_trials if total_trials else 0,
        "mean_time_s": round(sum(times) / len(times), 2) if times else 0,
        "total_wall_clock_s": round(sum(times), 2) if times else 0,
        "model_verified_count": model_verified,
        "claude_fallback_detected": model_verified < total_trials,
        "wg_context_available": True,
        "rerun_info": {
            "rerun_count": rerun_count,
            "rerun_passed": rerun_passed,
            "rerun_failed": rerun_count - rerun_passed,
            "rerun_reason": "dns_network_failure",
            "original_passed": original["passed"],
            "original_failed": original["failed"],
            "rerun_wall_clock_s": round(total_wall, 2),
        },
        "token_stats": {
            "total_input_tokens": total_input,
            "total_output_tokens": total_output,
            "total_tokens": total_input + total_output,
            "total_cost_usd": round(total_cost, 4),
            "total_turns": total_turns,
            "total_agents_spawned": total_agents,
            "mean_tokens_per_trial": round(
                (total_input + total_output) / total_trials, 0
            ) if total_trials else 0,
        },
        "surveillance_loop_stats": {
            "loops_created": 90,  # All trials had surveillance
            "cycle_edges_created": 90,
            "total_iterations_across_trials": surv_iterations_total,
            "trials_converged_first_try": surv_converged_first,
            "trials_needed_retry": surv_needed_retry,
            "issues_detected": all_issues,
            "issues_detected_count": len(all_issues),
        },
        "difficulty_stats": difficulty_stats,
        "task_stats": task_stats,
        "trials": merged_trials_raw,
    }

    # Write merged summary
    summary_path = os.path.join(RESULTS_DIR, "summary.json")
    with open(summary_path, "w") as f:
        json.dump(merged_summary, f, indent=2)

    print(f"\n{'='*60}")
    print(f"  MERGED RESULTS (original 61 passed + {rerun_passed}/{rerun_count} rerun passed)")
    print(f"{'='*60}")
    print(f"  Total: {passed_count}/{total_trials} ({passed_count/total_trials:.1%})")
    print(f"  Original passed: {original['passed']}")
    print(f"  Rerun passed: {rerun_passed}/{rerun_count}")
    print(f"  Rerun wall clock: {total_wall:.1f}s ({total_wall/60:.1f}min)")
    print()
    print(f"  Per-difficulty:")
    for diff in ("easy", "medium", "hard"):
        d = difficulty_stats.get(diff, {})
        if d:
            print(f"    {diff}: {d['passed']}/{d['total']} ({d['pass_rate']:.0%})")
    print()
    print(f"  Per-task (re-run tasks only):")
    for task_id in ["cobol-modernization", "build-cython-ext", "fix-code-vulnerability",
                     "constraints-scheduling", "multi-module-type-migration", "iterative-test-fix"]:
        ts = task_stats.get(task_id, {})
        if ts:
            print(f"    {task_id}: {ts['passed']}/{ts['total']} ({ts['pass_rate']:.0%})")

    print(f"\n  Results: {summary_path}")
    print(f"  Backup: {backup_path}")
    print(f"{'='*60}")

    return 0


if __name__ == "__main__":
    sys.exit(asyncio.run(main()))
