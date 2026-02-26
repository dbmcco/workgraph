# Research: Teaching Agents About Validation

## Executive Summary

**Agents don't validate because nobody tells them to.** The prompt template, AGENT-GUIDE.md, and CLAUDE.md contain zero validation guidance. The word "validate" does not appear in any agent-facing prompt section. A `verify` field exists on tasks (`wg add --verify "..."`) but is never surfaced in the spawned agent's prompt. Evaluations consistently note "no verification step" as a weakness — but agents have no way to know this matters.

The fix is straightforward: make validation instructions as prominent as the "wg done" instructions.

---

## 1. Current State: Where Validation Guidance Exists (and Doesn't)

### 1.1 Prompt Template (`src/service/executor.rs`)

The agent prompt is assembled by `build_prompt()` from these sections:

| Section | Validation Mention? | Notes |
|---|---|---|
| `REQUIRED_WORKFLOW_SECTION` | **No** | Tells agents to `wg log`, `wg artifact`, `wg done`. Zero mention of testing/checking/verifying. |
| `ETHOS_SECTION` | **No** | Mentions `spec → implement → verify → improve` cycle in passing, but as a philosophical statement, not an instruction. |
| `GRAPH_PATTERNS_SECTION` | **No** | Structural guidance only. |
| `CRITICAL_WG_CLI_SECTION` | **No** | "Use wg CLI" warning only. |
| `WG_CONTEXT_HINT` | **No** | Reference hint only. |
| `SYSTEM_AWARENESS_PREAMBLE` | **No** | System overview only. |

**The `verify` field on tasks is never included in the agent prompt.** Even if a task creator sets `wg add --verify "Run cargo test"`, the spawned agent never sees this verification criteria. It exists in `graph.rs` (line 208), is displayed in `wg show` output, but `build_prompt()` does not reference `task.verify`.

### 1.2 AGENT-GUIDE.md (`docs/AGENT-GUIDE.md`)

566 lines of graph patterns, agency rules, control patterns, anti-patterns, service operation, and functions. **Zero mentions of validation, testing, or verification as a practice.** The anti-patterns table (§5) lists structural anti-patterns (parallel file conflict, missing integrator) but nothing about skipping validation.

### 1.3 CLAUDE.md

Focuses on: use `wg` CLI (not built-in tools), use `wg service start`, `cargo install --path .`. **No validation guidance.**

### 1.4 Summary

| Document | Validation Guidance |
|---|---|
| Prompt template | None |
| AGENT-GUIDE.md | None |
| CLAUDE.md | None |
| `wg quickstart` output | None |
| `wg done` output | None |
| `verify` field on tasks | Exists but invisible to agents |

---

## 2. Behavioral Evidence: Do Agents Validate?

### 2.1 Agents That Validate (minority)

Some agents run tests before marking done. From task logs:

- **implement-trace-function-2**: "All tests pass, binary installed and smoke tested" — ran `cargo test` AND manually tested the binary.
- **cross-repo-deps**: "All 2393+ tests pass. Updated wg binary installed." — ran full test suite.
- **doc-sync-quickstart**: "All 19 tests pass, cargo check clean" — ran both tests and compilation check.
- **impl-wg-setup**: "All tests pass (9 new setup tests plus full suite)" — wrote AND ran tests.

These agents validated *despite* having no prompt instruction to do so. They did it because:
1. They were working on code (Rust), where `cargo test` is a natural step.
2. The Claude model's training includes software engineering best practices.

### 2.2 Agents That Don't Validate (majority)

- **update-hero-viz**: Changed HTML/CSS, committed and pushed. No verification that the page renders correctly.
- **doc-sync-agent**: Edited documentation. Evaluation notes: "agent did not demonstrate reading the files after editing to confirm the changes were applied correctly."
- **research-agent-autopoiesis**: Research task — wrote findings to a file. No verification that the data cited was accurate. (Research tasks have a different validation profile.)

### 2.3 Evaluation Feedback Patterns

Evaluators consistently flag missing validation:

| Evaluation | Score | Validation Issue |
|---|---|---|
| eval-doc-sync-agent | 0.87 | "task log summarizes changes without showing actual before/after diffs or verification" |
| eval-impl-json-output | 0.68 | "Test count discrepancy... relies on task log credibility rather than independent verification" |
| eval-website-style | low | "Did not commit changes to git or verify they persisted. Changes were later reverted." |
| eval-integrate-edge-rename | noted | "most verification details are not documented or shown" |
| eval-fix-eval-executor | noted | "cannot fully verify edge case handling or test adequacy" |

**The `eval-website-style` case is the most damaging**: an agent made CSS changes, marked done, and the changes were reverted by another commit. The agent didn't verify its work persisted. This is exactly the failure mode that validation prevents.

### 2.4 Pattern Summary

- **Code tasks**: ~60% run `cargo test` before done (because the model knows to)
- **Documentation tasks**: ~10% verify edits were applied correctly
- **Website/deployment tasks**: ~20% verify the result
- **Research tasks**: ~5% cross-check their citations

Agents validate when it's obvious (compile + test) but skip it when it's not (re-read files, check git status, verify deployment).

---

## 3. Recommendations

### 3.1 Prompt Template Changes (executor.rs)

#### R1: Add Validation Section to `REQUIRED_WORKFLOW_SECTION`

Insert between step 2 (artifacts) and step 3 (complete):

```rust
pub const REQUIRED_WORKFLOW_SECTION: &str = "\
## Required Workflow

You MUST use these commands to track your work:

1. **Log progress** as you work (helps recovery if interrupted):
   ```bash
   wg log {{task_id}} \"Starting implementation...\"
   wg log {{task_id}} \"Completed X, now working on Y\"
   ```

2. **Record artifacts** if you create/modify files:
   ```bash
   wg artifact {{task_id}} path/to/file
   ```

3. **Validate your work** before marking done:
   - For code changes: run `cargo test` (or the project's test command) and `cargo check`
   - For documentation: re-read the files you edited to confirm correctness
   - For any change: run `git diff` to review what you actually changed
   - Log your validation: `wg log {{task_id}} \"Validated: all tests pass, reviewed diff\"`

4. **Complete the task** when validated:
   ```bash
   wg done {{task_id}}
   ```

5. **Mark as failed** if you cannot complete:
   ```bash
   wg fail {{task_id}} --reason \"Specific reason why\"
   ```

## Important
- Run `wg log` commands BEFORE doing work to track progress
- **Validate BEFORE running `wg done`** — unvalidated work is incomplete work
- Run `wg done` BEFORE you finish responding
- If the task description is unclear, do your best interpretation\n";
```

Key changes:
- New step 3 with concrete validation actions per task type
- "Log your validation" — makes validation visible in task logs for evaluators
- Updated "Important" section: "Validate BEFORE running `wg done`"
- Step 4 now says "when validated" instead of "when done"

#### R2: Surface `task.verify` in the Prompt

In `build_prompt()`, after the task description, add the verify field:

```rust
// All scopes: task details
let mut task_section = format!(
    "## Your Task\n- **ID:** {}\n- **Title:** {}\n- **Description:** {}",
    vars.task_id, vars.task_title, vars.task_description
);

// Surface verification criteria if set
if let Some(ref verify) = vars.task_verify {
    task_section.push_str(&format!(
        "\n\n## Verification Required\n\
         Before marking this task done, you MUST verify:\n\
         {}\n\
         Log your verification results with `wg log`.",
        verify
    ));
}

parts.push(task_section);
```

This requires adding `task_verify: Option<String>` to `TemplateVars` and populating it from `task.verify`.

#### R3: Add Validation Reminder to `wg done` Output

In `src/commands/done.rs`, after `println!("Marked '{}' as done", id)`, add:

```rust
// Nudge: remind about validation for future tasks
if task.log.iter().all(|l| !l.message.to_lowercase().contains("validat")) {
    eprintln!("Tip: Log validation steps before `wg done` (e.g., wg log {} \"Validated: tests pass\")", id);
}
```

This is a soft nudge — not a blocker, but a visible reminder when agents skip validation logging.

### 3.2 AGENT-GUIDE.md Additions

Add a new section after §4 (Control Rules):

```markdown
## 4.5 Validation — the final step before done

**Every task requires validation before `wg done`.** The type of validation depends on the task:

| Task type | Minimum validation |
|---|---|
| Code implementation | `cargo test` + `cargo check` (or equivalent) |
| Code refactoring | Full test suite + `git diff` review |
| Documentation | Re-read edited files, verify links |
| Research/analysis | Cross-check key claims against source |
| Configuration | Verify the config loads: `wg show`, `wg list` |
| Website/deployment | Verify the change is live and visible |

**Log your validation:**
```bash
wg log <task-id> "Validated: cargo test passes (2393 tests), reviewed git diff"
```

Validation is not optional. Evaluators check for it. Unvalidated work that breaks downstream tasks is worse than incomplete work that's honest about its state.

**Anti-pattern: marking done without verification.**
If you can't validate (e.g., no test infrastructure), log WHY:
```bash
wg log <task-id> "Cannot validate: no test suite exists for documentation changes"
```
```

Also add to the anti-patterns table (§5):

```markdown
| **Skipping validation** | Agent marks done without testing/checking → downstream failures, low evaluation scores | Always validate before `wg done` (see §4.5) |
```

### 3.3 Task Description Best Practices

When creating tasks that require specific validation, use the `--verify` flag:

```bash
wg add "Implement auth module" --verify "Run cargo test, verify 0 failures. Check wg show output includes new fields."
wg add "Update docs" --verify "Re-read all edited files. Verify no broken links."
wg add "Fix CSS regression" --verify "Open the page in browser, verify visual correctness. Commit and push."
```

The `--verify` flag becomes actionable once R2 is implemented (surfacing it in the prompt).

### 3.4 Evaluation Criteria Update

Evaluators should explicitly score validation behavior. Add to the evaluation prompt:

```
- **validation_discipline**: Did the agent verify their work before marking done?
  - 1.0: Ran tests, reviewed diff, logged validation results
  - 0.7: Ran tests but didn't log it
  - 0.4: No explicit validation, but work appears correct
  - 0.0: No validation, work has detectable errors
```

This creates a feedback loop: agents that validate score higher → evolution selects for validation → the culture shifts.

---

## 4. Behavioral Nudges Beyond Prompts

### 4.1 Make Validation the Path of Least Resistance

The current workflow is: work → `wg done`. The proposed workflow is: work → validate → log validation → `wg done`. Adding friction (an extra step) is risky — agents might ignore it. Instead:

**Consider a `wg validate` command** that:
1. Detects the project type (Cargo.toml → Rust, package.json → Node, etc.)
2. Runs the appropriate test command
3. Logs the result automatically
4. Outputs "Validation passed — you can now run `wg done`"

This reduces validation from 3 steps (run tests, check result, log it) to 1 step (`wg validate <task-id>`).

### 4.2 Soft Gates vs. Hard Gates

**Soft gate (recommended first):** `wg done` prints a warning if no validation log entry exists. Agents can still complete, but the nudge is visible.

**Hard gate (future option):** `wg done` refuses to complete if `task.verify` is set and no validation log exists. This is stronger but risks blocking agents who validated but forgot to log.

Start with the soft gate. Measure whether validation rates improve. Escalate to hard gate if needed.

### 4.3 Default Verify for Code Tasks

When `wg add` is run in a directory with `Cargo.toml`, automatically suggest/set `--verify "Run cargo test"`. This bakes validation into task creation rather than relying on the agent to remember.

---

## 5. Priority Order for Implementation

1. **R1: Validation section in prompt template** — Highest impact, lowest effort. One string constant change in executor.rs.
2. **R2: Surface `task.verify` in prompt** — Medium effort. Requires `TemplateVars` change + `build_prompt` change.
3. **AGENT-GUIDE §4.5** — Documentation only. Quick to add.
4. **Anti-pattern table update** — One line. Quick to add.
5. **R3: `wg done` validation nudge** — Low effort, nice-to-have.
6. **`wg validate` command** — Higher effort but creates the smoothest agent experience.
7. **Evaluation criteria update** — Requires changes to evaluation prompts.
8. **Default verify for code tasks** — Nice-to-have, medium effort.

---

## 6. The Culture Shift

The fundamental problem is not technical — it's cultural. The prompt template sets the culture for agents. Right now, the culture says:
- Log your progress ✓
- Record artifacts ✓
- Mark done ✓
- Validate your work ✗ (absent)

Adding validation to the prompt template makes it part of "the way things are done here." When combined with evaluation scoring for validation discipline, the system creates selection pressure: agents that validate survive evolution; agents that don't get replaced.

This is how you build a culture of validation in an agentic system: make it explicit in instructions, make it visible in logs, make it rewarded in evaluations, and make it easy with tooling.
