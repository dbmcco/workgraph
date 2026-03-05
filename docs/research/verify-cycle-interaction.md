# Research: How Should --verify Interact with Cycles and the Evaluation Pipeline?

**Date:** 2026-03-05
**Task:** research-how-should
**Status:** Research complete

---

## Executive Summary

`--verify` (hard gate on `wg done`) and `--max-iterations` (cycle system) are in tension: verify blocks completion, but cycles need tasks to complete so the next iteration can fire. This document analyzes the interaction from four perspectives — reliability engineer, systems architect, pragmatist, and user — and recommends a **context-sensitive verify** approach where verify always blocks, but verify *failure* in a cycle triggers a well-defined recovery path rather than a deadlock.

---

## 1. Should --verify Block in Cycle Context?

### Current Behavior

`wg done` runs the verify command *before* setting status to Done. If verify fails, the task stays InProgress. The cycle's `reactivate_cycle()` requires all members to be Done before firing the next iteration. **Result: a failing verify permanently stalls the cycle.**

### Viewpoint A: Always Block (Reliability Engineer)

**Position:** Verify should always be a hard gate. If the work doesn't pass verification, it isn't done — period. The cycle should iterate on *actually completed* work, not on half-finished garbage.

**Pros:**
- Strong correctness guarantee: every "Done" task has machine-verified output
- Prevents the exact problem that motivated `--verify`: agents lying about completion
- Simple mental model — verify means verify, no exceptions

**Cons:**
- A struggling agent can loop forever: verify fails → can't mark done → cycle never iterates → agent retries the same approach
- No fresh perspective: the same agent keeps retrying with the same context, likely hitting the same wall
- The agent's session may time out or exhaust tokens before verify passes, leaving the task InProgress with no recovery

**Failure modes:**
1. **Stuck agent loop:** Agent tries to fix verify failure, makes it worse, tries again. Never escapes.
2. **Token exhaustion:** Agent burns through context trying to fix a fundamental issue that needs a different approach.
3. **Timeout deadlock:** Agent session ends with task still InProgress. Coordinator sees a dead agent, marks task Failed. If `restart_on_failure` is true, the cycle restarts — but from scratch, losing all progress.

### Viewpoint B: Advisory in Cycles (Pragmatist)

**Position:** In cycles, verify should log its result but not block. Let the task complete, let the cycle iterate, and let the next agent see the verify failure and fix it.

**Pros:**
- Cycles never stall: every iteration completes, even if imperfectly
- Fresh agent on each iteration gets a clean start with knowledge of what failed
- Natural convergence: each iteration sees the previous verify output and can target the specific failures

**Cons:**
- Undermines the core promise of `--verify`: "Done" no longer means "verified"
- Agents could ignore verify failures and claim convergence
- The verify output from one iteration must be preserved and surfaced to the next iteration's agent
- Two definitions of "Done" (verified-done vs cycle-done) creates confusion

**Failure modes:**
1. **False convergence:** Agent marks `--converged` despite verify failing. The cycle stops with broken output.
2. **Noise accumulation:** Each iteration makes partial changes, verify keeps failing on different things. No monotonic progress.
3. **Semantic confusion:** Downstream tasks see a "Done" task whose verify actually failed. They proceed on a broken foundation.

### Viewpoint C: Hybrid — Block on Last Iteration Only (Systems Architect)

**Position:** Intermediate iterations are exploratory — verify is advisory. The final iteration (when `loop_iteration + 1 >= max_iterations` or `--converged`) enforces verify as a hard gate.

**Pros:**
- Cycles can iterate freely during exploration
- The final output has full verification guarantees
- Clear semantics: intermediate = "best effort", final = "must pass"

**Cons:**
- "Last iteration" isn't always knowable in advance (agent decides `--converged`)
- If verify blocks on the last iteration and the agent can't fix it, the task is stuck at max_iterations with no recovery — worse than either A or B
- Complex logic: `wg done` needs to know the iteration count, max_iterations, and whether `--converged` was passed, to decide whether to enforce verify

**Failure modes:**
1. **Last-iteration deadlock:** Agent on the final iteration can't pass verify. No more iterations available. Task is stuck.
2. **Convergence trap:** Agent tries `--converged` but verify blocks. Agent can't mark done without converging, can't converge without passing verify. Must iterate again — but thought work was done.
3. **Off-by-one:** Agent doesn't know it's on the last iteration and doesn't try hard enough.

### Viewpoint D: Always Block, But Verify Failure Triggers `wg fail` (Recommended)

**Position:** Verify always blocks `wg done`. If the agent can't fix the verify failure, it should call `wg fail` with the verify output. The cycle's `restart_on_failure` mechanism then kicks in, re-opening the cycle for a fresh attempt (possibly with a different agent).

**Pros:**
- Verify is always a hard gate — "Done" always means "verified"
- Failed tasks trigger the existing recovery infrastructure (restart_on_failure, max_failure_restarts)
- Fresh agent on restart gets the failure context (verify output in logs) and can try a different approach
- No new concepts needed: uses existing fail + restart_on_failure + max_failure_restarts
- The cycle has a natural stopping point: `max_failure_restarts` (default 3) prevents infinite failure loops

**Cons:**
- Agent must be smart enough to call `wg fail` instead of looping on verify
- Each failure restart is a full iteration restart (all cycle members re-open), which is more expensive than just retrying the one failing task
- Requires agent prompt to explain: "if verify fails and you can't fix it, call wg fail"

**Failure modes:**
1. **Agent doesn't call fail:** Agent keeps retrying verify in a loop. Mitigated by: session timeout → dead agent detection → triage marks as Failed → restart_on_failure fires.
2. **Restart exhaustion:** All `max_failure_restarts` used up, verify still failing. Task is stuck. **This is correct behavior** — if 3+ fresh agents all can't pass verify, the task genuinely needs human attention.

### Recommendation

**Viewpoint D: Always block, failure triggers fail + restart.**

This is the simplest approach that preserves verify's guarantee while leveraging the existing cycle recovery system. The key insight is that verify failure in a cycle shouldn't be special — it's just a failure, and the cycle already has a failure recovery mechanism.

**Required changes:**
- Agent prompts for cycle tasks with `--verify` should include: *"If the verify command fails and you cannot fix it after a reasonable attempt, run `wg fail <id> --reason 'verify failed: <output>'` so the cycle can restart with a fresh agent."*
- No code changes needed — the existing `restart_on_failure` + `max_failure_restarts` infrastructure handles this.

---

## 2. What Should Trigger a Retry vs a New Iteration?

### Terminology

- **Retry:** Same iteration, task re-attempted (possibly by a different agent). Triggered by `wg fail` + `restart_on_failure`.
- **New iteration:** Next iteration of the cycle. All members re-open. Triggered by all members being Done + cycle guards passing.

### Current System

| Event | What happens | Mechanism |
|-------|-------------|-----------|
| Agent calls `wg done` | Task → Done. If all cycle members Done, cycle evaluates next iteration. | `evaluate_cycle_iteration()` |
| Agent calls `wg fail` | Task → Failed. If `restart_on_failure`, all cycle members re-open at same iteration. | `evaluate_cycle_failure_restart()` |
| Agent dies (timeout/crash) | Triage detects dead PID → marks Failed → same as above. | `triage.rs` |
| Verify fails | `wg done` returns error. Task stays InProgress. | `done.rs:170-173` |

### The Gap

Verify failure currently leaves the task in InProgress limbo. It's not Failed (so `restart_on_failure` doesn't fire) and it's not Done (so the cycle doesn't iterate). The agent is expected to fix the issue and retry `wg done`, but this may not succeed.

### Viewpoint A: Verify Failure = Agent Should Fix, Then Done (Current Implicit Design)

The agent has a chance to fix the issue. It sees the verify output, modifies code, and retries `wg done`.

**When this works:** Simple issues — a typo, a missed test, an off-by-one error. The agent fixes it in one or two attempts.

**When this fails:** Fundamental design issues, wrong approach, missing dependency. The agent spins.

### Viewpoint B: Verify Failure = Auto-Fail After N Retries

Track the number of verify attempts per task. After N failures (e.g., 3), automatically transition to Failed, triggering `restart_on_failure`.

**Pros:** Prevents infinite spinning. Gives the agent a chance to self-correct before escalating.

**Cons:** Requires new state (verify_attempts counter). The right N is context-dependent. Adds complexity.

### Viewpoint C: Verify Failure = Agent Decides (Recommended)

The agent is told: "If verify fails and you can't fix it, call `wg fail`." This is the simplest approach and keeps the agent in control. The existing dead-agent detection serves as a backstop if the agent spins without calling fail.

**Recovery chain:**
1. Agent tries to fix → retries `wg done` → verify passes → cycle iterates
2. Agent can't fix → calls `wg fail` → `restart_on_failure` fires → fresh agent
3. Agent spins → session timeout → dead agent → triage marks Failed → restart_on_failure fires
4. Too many failures → `max_failure_restarts` exceeded → cycle stops → human attention needed

This is a complete recovery chain with no new mechanisms needed.

### Recommendation

**Viewpoint C.** Let agents decide whether to retry or fail. The existing infrastructure provides all the backstops needed. The only change is a prompt improvement: teach agents about the verify-fail-restart pattern in cycle context.

---

## 3. How Should --verify Relate to Evaluation?

### Current Pipeline

```
Agent works → wg done → verify gate → [blocks or passes] → status=Done
                                                              ↓
                                              capture_task_output()
                                                              ↓
                                    coordinator creates evaluate-{task} task
                                                              ↓
                                              evaluator agent runs
                                                              ↓
                                          eval score + FLIP score recorded
                                                              ↓
                                    (FLIP < 0.7 → Opus escalation → can fail task)
```

**Key observation:** Verify runs *before* task completion. Evaluation runs *after*. They occupy different positions in the pipeline.

### Viewpoint A: Verify and Eval Are Orthogonal (Current Design)

Verify = "does the output meet machine-checkable criteria?" (tests pass, file exists, lint clean).
Eval = "how well did the agent perform?" (quality, adherence to spec, style).

**Pros:**
- Clean separation of concerns
- Verify is fast and deterministic; eval is slow and probabilistic
- Either can exist without the other

**Cons:**
- A task can pass verify but get a terrible eval score (code works but is awful)
- A task can fail verify but would have gotten a great eval score (approach is right, minor bug)
- No unified quality signal

### Viewpoint B: Eval Can Override Verify (Escalation Model)

If verify passes but eval score is below threshold, the evaluation can retroactively fail the task. This already partially exists: FLIP < 0.7 triggers Opus verification which *can* fail the task.

**Pros:** Catches "technically correct but actually bad" work.
**Cons:** Retroactive failure after a task is "Done" is confusing. Downstream tasks may have already started.

### Viewpoint C: Unified Quality Gate

Combine verify + eval into a single quality gate. Task isn't "Done" until both pass.

**Pros:** Single definition of "done." No retroactive failures.
**Cons:** Eval is slow (requires LLM call). Blocking `wg done` on eval would add significant latency. Eval requires the work to be "done" to evaluate it (chicken-and-egg).

### Viewpoint D: Verify Gates Done, Eval Gates Cycle Iteration (Recommended)

- Verify runs synchronously in `wg done` — fast, deterministic, hard gate.
- Eval runs asynchronously after completion — slow, probabilistic, advisory for non-cycle tasks.
- **In cycle context:** Eval score feeds into a `ScoreAbove` loop guard. The cycle doesn't iterate until eval completes and the score meets the threshold.

This is the architecture proposed in `docs/research/validation-cycles.md` (section 6) and it remains the right long-term direction.

**Pipeline in cycle context:**
```
Agent works → wg done → verify passes → status=Done
                                          ↓
                              evaluate-{task} runs
                                          ↓
                              score recorded on task
                                          ↓
                     coordinator checks cycle: all Done + eval complete?
                                          ↓
                          ScoreAbove guard: score >= threshold?
                                          ↓
                      Yes → converge    No → re-activate cycle
```

**Important:** This requires moving cycle re-activation from `wg done` (synchronous) to the coordinator tick (deferred). This is a significant but clean architectural change: `wg done` sets status to Done and exits; the coordinator detects the completed cycle and decides whether to iterate based on eval results.

### Recommendation

**Short-term (no code changes):** Keep verify and eval orthogonal. Verify gates `wg done`, eval runs after.

**Medium-term:** Move cycle re-activation to the coordinator tick. This decouples completion from iteration and opens the door for eval-gated cycles.

**Long-term:** Add `ScoreAbove` loop guard. Cycle iterates only when eval score < threshold. Verify ensures baseline correctness (tests pass); eval ensures quality (good code).

---

## 4. The Convergence Problem

### The Problem Statement

`wg done --converged` signals "this cycle's work is complete, stop iterating." But agents can falsely claim convergence. The observed failure: 4 agents claimed tests pass when they didn't. `--verify` was designed to catch exactly this.

### Should Verify Gate Convergence?

**Yes, and it already does.** Looking at `done.rs:159-176`, the verify check runs *before* the converged logic (lines 184-252). If verify fails, `wg done` returns an error — the task never reaches Done status, and the converged tag is never applied. So:

```
wg done task --converged
  → verify runs → FAILS → error returned → task stays InProgress
  → converged tag never applied
  → cycle never stops
```

This is exactly correct. An agent that claims convergence but can't pass verify is caught.

### Failure Modes

**False convergence WITH verify:** Impossible. Verify blocks the Done transition, so convergence can't happen without passing verification. This is the whole point.

**False convergence WITHOUT verify:** If a task has no `--verify` command, the agent can claim convergence freely. The system trusts the agent. This is the gap that `--verify` was designed to fill.

**Verify passes but convergence is premature:** The verify command tests specific criteria (e.g., `cargo test`), but the cycle's goal may be broader. Tests pass but the code is ugly, undocumented, or fragile. This is where eval adds value (see section 3).

### Should `--converged` + `--verify` Interact Differently Than `--converged` Alone?

No. The current behavior is correct:

1. Verify gates Done. This happens regardless of `--converged`.
2. If verify passes, Done is set, and the converged tag is applied (if no guard overrides it).
3. The cycle sees the converged tag and stops.

This means: **an agent can only stop a cycle by passing verification.** That's the right guarantee.

### Recommendation

**No changes needed.** The current ordering (verify → done → converged) is correct. Verify already gates convergence implicitly by gating the Done transition.

**One documentation improvement:** Make it explicit in the agent prompt that `--converged` does not bypass `--verify`. Agents should know: "to converge a cycle, your work must pass verification."

---

## 5. Tasks Without --verify

### Current State

Many tasks have no `--verify` command. For these:
- `wg done` transitions to Done unconditionally (modulo blocker checks)
- The only quality gates are eval + FLIP (asynchronous, after completion)
- In cycles: the agent's `--converged` claim is trusted at face value

### Viewpoint A: Rely on Eval/FLIP (Current)

Eval and FLIP run after completion. FLIP < 0.7 can trigger Opus escalation that retroactively fails the task.

**Pros:** No extra work at task creation time. Catches quality issues after the fact.
**Cons:** Retroactive. The cycle may have already iterated or converged before eval completes.

### Viewpoint B: Auto-Generate Verify from Validation Criteria

Many task descriptions include a `## Validation` section with checkboxes. These could be parsed into verify commands.

**Example:**
```markdown
## Validation
- [ ] cargo test test_auth passes
- [ ] endpoint returns 401 for bad tokens
```

Auto-generated verify: `cargo test test_auth && curl -s localhost:8080/auth | grep -q '401'`

**Pros:** Verify everywhere without manual effort.
**Cons:** Parsing natural language into shell commands is error-prone. The validation section often contains subjective criteria ("code is clean") that can't be machine-checked.

### Viewpoint C: Encourage --verify, Don't Require It (Recommended)

The CLAUDE.md task template already includes `--verify`. The system should:
1. Warn when creating a cycle task without `--verify` (`wg check` warning)
2. Include the verify command in the task template prominently
3. Document best practices for writing verify commands
4. Never auto-generate verify from natural language — too fragile

**For cycles without verify:** Eval/FLIP is the backstop. The convergence problem is mitigated by FLIP's ability to retroactively fail tasks. It's not as fast as verify but it's better than nothing.

### Recommendation

**Encourage, don't require.** Add a `wg check` warning for cycle tasks without `--verify`. Keep eval/FLIP as the fallback for unverified tasks. Do not auto-generate verify commands.

---

## 6. Full Pipeline Specification

### Non-Cycle Tasks

```
Agent works on task
  ↓
Agent calls: wg done <id>
  ↓
Verify gate (if task.verify is set):
  ├─ PASS → continue
  └─ FAIL → error returned, task stays InProgress
              Agent should fix and retry wg done
              If agent can't fix: wg fail → restart_on_failure (if configured)
              If agent dies: triage → Failed → restart_on_failure
  ↓
Task status → Done
  ↓
Output captured (git diff, artifacts, logs)
  ↓
Coordinator creates evaluate-{task} task
  ↓
Evaluator runs (async): eval score + FLIP
  ├─ Score recorded
  ├─ FLIP >= 0.7 → task stays Done
  └─ FLIP < 0.7 → Opus escalation → may fail task retroactively
```

### Cycle Tasks (Current + Recommended)

```
Agent works on cycle task (iteration N)
  ↓
Agent calls: wg done <id> [--converged]
  ↓
Verify gate (if task.verify is set):
  ├─ PASS → continue
  └─ FAIL → error returned, task stays InProgress
              Agent should attempt fix, retry wg done
              If agent can't fix: wg fail <id> --reason "verify failed: ..."
                → restart_on_failure fires
                → all cycle members re-open at iteration N (retry)
                → fresh agent assigned
              If agent dies: triage → Failed → same restart path
  ↓
Task status → Done
  ├─ If --converged (and no guard override): "converged" tag applied
  └─ If not converged: no tag
  ↓
evaluate_cycle_iteration() checks:
  1. All cycle members Done?
  2. Converged tag present (if no guard)?  → STOP cycle
  3. loop_iteration + 1 >= max_iterations? → STOP cycle
  4. Guard condition passes?
  5. All checks pass → re-activate cycle (all members → Open, iteration++)
  ↓
If cycle continues: next iteration begins, agents dispatched
If cycle stops: output captured, eval runs as normal
```

### Recommended Future State (Deferred Cycle Re-activation)

```
Agent works on cycle task (iteration N)
  ↓
Agent calls: wg done <id> [--converged]
  ↓
Verify gate: same as above
  ↓
Task status → Done (verify passed)
  ↓
wg done exits (no cycle evaluation in wg done)
  ↓
Coordinator tick detects: all cycle members Done
  ↓
Coordinator creates + runs evaluate-{task} for each member
  ↓
All evaluations complete
  ↓
Coordinator checks cycle iteration conditions:
  1. Converged tag?  → STOP
  2. max_iterations? → STOP
  3. Guard (including ScoreAbove): eval score >= threshold? → STOP
  4. All pass → re-activate cycle
```

This deferred model requires:
- Moving `evaluate_cycle_iteration()` call out of `done.rs` into the coordinator
- The coordinator must track "cycle members all Done but eval pending" as a state
- `ScoreAbove` loop guard variant

This is a medium-term architectural change. The short-term recommendation (section 1, Viewpoint D) works today with no code changes.

---

## 7. Summary of Recommendations

| # | Question | Recommendation | Changes Needed |
|---|----------|---------------|----------------|
| 1 | Should verify block in cycles? | **Yes, always block.** Failure → agent calls `wg fail` → `restart_on_failure` fires. | Prompt changes only (teach agents the pattern) |
| 2 | Retry vs new iteration? | Agent decides. Fix-and-retry or `wg fail` for fresh start. Dead agent detection as backstop. | None |
| 3 | Verify vs eval? | Verify gates Done (sync). Eval gates iteration (async, future). Keep orthogonal short-term. | Medium-term: deferred cycle re-activation |
| 4 | Convergence gating? | Verify already gates convergence (blocks Done). No change needed. | Documentation only |
| 5 | Tasks without verify? | Encourage `--verify` on all cycle tasks. `wg check` warning. Eval/FLIP as fallback. | Add `wg check` warning |

### Design Principles

1. **Verify is always a hard gate.** No exceptions, no advisory mode, no cycle-special-casing. "Done" means "verified."
2. **Failure is a feature.** In cycles, `wg fail` is the correct response to an unfixable verify failure. It triggers recovery, not deadlock.
3. **Evaluation is complementary, not competing.** Verify checks correctness (tests pass). Eval checks quality (code is good). They run at different times and serve different purposes.
4. **Cycles already handle failure.** `restart_on_failure` + `max_failure_restarts` provide a complete recovery mechanism. No new concepts needed for verify-in-cycles.
5. **Don't overengineer the common case.** Most verify failures are fixable by the same agent. The escalation path (fail → restart → new agent → human) handles the uncommon case.

### Concrete Next Steps

1. **Now:** Update agent prompt templates to teach the verify-fail-restart pattern in cycle context
2. **Now:** Add `wg check` warning for cycle tasks without `--verify`
3. **Soon:** Move cycle re-activation from `wg done` to coordinator tick (enables eval-gated cycles)
4. **Later:** Add `ScoreAbove` loop guard for evaluation-driven convergence
5. **Later:** Add `Exec` loop guard for test-driven convergence without `--verify`
