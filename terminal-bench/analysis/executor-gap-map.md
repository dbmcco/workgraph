# Executor Gap Map: TB Failures → Code Paths

**Date:** 2026-04-04
**Task:** research-executor-gaps
**Purpose:** Map each TB experiment failure to specific executor code, creating an implementation roadmap for 5 parallel improvement tracks.

---

## 1. No `--after` Usage (Executor Context Injection)

### Problem
TB agents create subtasks with `wg add` but never use `--after` to express dependencies. This produces flat, unordered task lists instead of dependency graphs.

### Root Code Path

**Where agent context/prompts are built:**

1. **`src/commands/spawn/execution.rs:29-153`** — `spawn_agent_inner()`: The master function. Loads the task, resolves scope, builds `TemplateVars`, constructs `ScopeContext`, and calls `build_prompt()`.

2. **`src/commands/spawn/context.rs:16-82`** — `build_task_context()`: Gathers dependency artifacts and logs from `task.after[]` dependencies. This is what populates `## Context from Dependencies` in the prompt.

3. **`src/commands/spawn/context.rs:88-160`** — `build_scope_context()`: Builds `ScopeContext` with downstream awareness (R1), graph summaries, CLAUDE.md content. The `downstream_info` field (line 106) lists tasks that depend on the current task.

4. **`src/service/executor.rs:362-486`** — `build_prompt()`: Assembles the final prompt from `TemplateVars` + `ScopeContext`. Key sections:
   - Line 411: `## Context from Dependencies` (from `vars.task_context`)
   - Line 438: `## Downstream Consumers` (from `ctx.downstream_info`)
   - Line 449: `REQUIRED_WORKFLOW_SECTION` — contains `wg add` examples
   - Line 453: `AUTOPOIETIC_GUIDANCE` — decomposition guidance

5. **`src/service/executor.rs:173-218`** — `AUTOPOIETIC_GUIDANCE`: The decomposition template. Already includes `--after {{task_id}}` examples, but they're generic patterns, not task-specific.

### What to Change

The prompt already tells agents about `--after`. The issue is that **non-Claude executors** (TB adapter) don't get the same rich prompt. For the native executor path, the prompt is passed verbatim from `settings.prompt_template` (see `execution.rs:856-897`). The TB adapter builds its own system prompt independently.

**Improvement:** Inject decomposition-awareness context into the prompt that's **model-agnostic** — i.e., make the `AUTOPOIETIC_GUIDANCE` section more directive about *always* using `--after` when creating subtasks. Currently it says "encouraged" — it should say "MUST" with concrete examples that show the anti-pattern (flat list) vs. correct pattern (dependency graph).

**Primary file:** `src/service/executor.rs` lines 173-218 (`AUTOPOIETIC_GUIDANCE`)
**Secondary file:** `src/commands/spawn/context.rs` — could inject "your parent task ID is X, always use `--after X` when creating subtasks" into the context.

---

## 2. False-PASS Verification

### Problem
Agents call `wg done` and the task transitions to Done even when verification criteria aren't actually met. The `--verify` gate runs the command but the agent itself doesn't run verification before calling `wg done`.

### Root Code Path

**Where verification runs:**

1. **`src/commands/done.rs:160-163`** — `run()`: Entry point for `wg done`. Calls `run_inner()`.

2. **`src/commands/done.rs:222-350`** — Verify command gate: When `task.verify` is set, `run_verify_command()` executes the shell command. If it fails:
   - Increments `task.verify_failures` (line 298)
   - Logs failure with stdout/stderr (lines 302-321)
   - Circuit breaker: auto-fails after `max_verify_failures` (lines 327-350)
   
3. **`src/commands/done.rs:29-123`** — `run_verify_command()`: Spawns `sh -c <verify_cmd>` in the project root, captures stdout/stderr, 120s timeout.

4. **`src/commands/done.rs:438-481`** — External validation path: When configured, transitions to `PendingValidation` instead of `Done`, requiring explicit approval.

**Current flow:** Agent calls `wg done` → verify command runs in the same process → pass/fail gates the transition. The verify command runs as the **same agent that did the work**, not as an independent reviewer.

### What to Change

The key issue is that verification happens **inside the agent's own `wg done` call**. The agent can (and does) call `wg done` before actually checking if its work satisfies the criteria. Two improvements:

**Improvement A: Separate-agent verification calls.** Instead of running verify in-line inside `wg done`, transition to `PendingValidation` and spawn a **separate agent** (or inline eval process) to run the verification. This is similar to how `.evaluate-*` and `.flip-*` tasks work.

**Primary files:**
- `src/commands/done.rs:222-350` — Change verify flow to always transition to `PendingValidation` when `task.verify` is set, rather than running inline
- `src/commands/service/coordinator.rs:1538-1688` — `build_flip_verification_tasks()`: Model for how to create post-completion verification tasks. The pattern exists already for FLIP scoring.
- `src/commands/service/coordinator.rs:2858-2927` — Inline task spawning for eval/flip/assignment. A similar inline path could be added for verify tasks.

**Improvement B: Pre-`wg done` verification prompt.** In the agent prompt (`REQUIRED_WORKFLOW_SECTION`), strengthen the verification step to explicitly require running the verify command *before* calling `wg done`. Currently the prompt says "Validate your work before marking done" — but doesn't force the agent to actually run the verify command.

**Primary file:** `src/service/executor.rs:20-97` — `REQUIRED_WORKFLOW_SECTION`, specifically the validation step (step 3).

---

## 3. Test Discovery Failure

### Problem
Agents need to know which tests exist for the code they're modifying. Currently, the executor doesn't scan for relevant tests before spawning an agent.

### Root Code Path

**Where the executor sets up a task before spawning:**

1. **`src/commands/spawn/execution.rs:29-153`** — `spawn_agent_inner()`: The setup phase. After loading the task and building context, it:
   - Resolves executor config (line 160-161)
   - Builds `TemplateVars` (line 131)
   - Builds `ScopeContext` (line 122)
   - Writes prompt file (lines 816-819 for claude executor)
   
   **There is no pre-task scanning step.** The executor goes straight from context assembly to command construction.

2. **`src/commands/spawn/context.rs:88-160`** — `build_scope_context()`: Gathers graph-level context. Could be extended to scan for test files.

3. **`src/service/executor.rs:509-600`** — `TemplateVars::from_task()`: Constructs template variables. Could include a `test_files` or `relevant_tests` field populated by scanning the project.

### What to Change

**Insert a pre-spawn scanning step** between context assembly and prompt writing. This step would:
1. Parse the task description for file paths mentioned
2. Scan for test files associated with those paths (e.g., `tests/test_*.rs`, `*_test.py`, etc.)
3. Inject discovered test file paths into the prompt as a new section

**Primary files:**
- `src/commands/spawn/context.rs` — Add a new function `discover_relevant_tests(task, workgraph_dir) -> Vec<String>` that scans for test files
- `src/service/executor.rs:336-353` — Add a `relevant_tests: String` field to `ScopeContext`
- `src/service/executor.rs:362-486` — Add a new section in `build_prompt()` that includes discovered tests
- `src/commands/spawn/execution.rs:119-128` — Call the test discovery function during context assembly

**Additionally, for `--verify` auto-gating:**
- `src/commands/spawn/execution.rs:29-153` — If the task description mentions test files but `task.verify` is None, auto-populate `verify` with a `cargo test <test_name>` command based on discovered tests.

---

## 4. Flat Subtask Lists (Decomposition Intelligence)

### Problem
When agents create subtasks, they create flat lists without dependency edges, even though the prompt tells them about `--after`. The decomposition guidance is generic and doesn't adapt to the task type.

### Root Code Path

**Where decomposition guidance is injected:**

1. **`src/service/executor.rs:173-218`** — `AUTOPOIETIC_GUIDANCE`: Static decomposition guidance. Contains generic patterns for fan-out, pipeline, and bug-fix decomposition. All patterns use `{{task_id}}` placeholder.

2. **`src/service/executor.rs:260-295`** — `PATTERN_KEYWORDS_GLOSSARY`: Additional pattern vocabulary injected when trigger keywords appear in the description. Activated by `description_has_pattern_keywords()` (line 299).

3. **`src/service/executor.rs:449-455`** — In `build_prompt()`, the guidance sections are appended at task+ scope:
   ```
   REQUIRED_WORKFLOW_SECTION
   GIT_HYGIENE_SECTION  
   MESSAGE_POLLING_SECTION
   ETHOS_SECTION
   AUTOPOIETIC_GUIDANCE
   GRAPH_PATTERNS_SECTION
   ```

4. **`src/service/executor.rs:98-131`** — `GRAPH_PATTERNS_SECTION`: Contains the "golden rule: same files = sequential edges" guidance and pattern vocabulary.

### What to Change

**Improvement A: Adaptive decomposition templates.** Instead of one-size-fits-all `AUTOPOIETIC_GUIDANCE`, generate decomposition hints based on:
- Task type (from tags/keywords: implementation vs. research vs. refactor)
- Task scope (number of files mentioned, estimated complexity)
- Graph topology (are sibling tasks already decomposed? what patterns are working?)

**Primary files:**
- `src/service/executor.rs:170-218` — Replace static `AUTOPOIETIC_GUIDANCE` with a function `build_decomposition_guidance(task, graph, config) -> String` that generates task-specific decomposition advice
- `src/commands/spawn/context.rs:88-160` — `build_scope_context()` could analyze sibling task patterns and inject "your siblings used this decomposition pattern" context

**Improvement B: Stronger `--after` mandate.** In `AUTOPOIETIC_GUIDANCE`, change "encouraged" to "MUST" and add an explicit anti-pattern section showing what goes wrong without `--after`.

**Primary file:** `src/service/executor.rs:173-218`

---

## 5. Model Context Gap (Non-Claude Model Support)

### Problem
When the executor routes tasks to non-Claude models (via `requires_native_executor()`), those models don't get CLAUDE.md-equivalent context. The native executor receives the same prompt as Claude, but the model may not understand Claude Code-specific conventions (tool names, wg CLI patterns, etc.).

### Root Code Path

**Where the executor detects the model:**

1. **`src/commands/service/coordinator.rs:2958-2978`** — `requires_native_executor()` detection: When the resolved model is non-Anthropic, the coordinator switches from `claude` to `native` executor. This is the auto-detection point.

2. **`src/commands/service/coordinator.rs:2677-2700`** — `requires_native_executor()` function: Checks provider prefix, model ID format, and registry aliases.

3. **`src/commands/spawn/execution.rs:856-897`** — Native executor command construction: Writes prompt file, passes `--exec-mode`, `--model`, `--provider` flags to `wg native-exec`.

4. **`src/commands/native_exec.rs:28-80`** — Native executor entry point: Reads prompt, resolves bundle for exec_mode, builds tool registry, runs agent loop. The prompt is the **same prompt** built by `build_prompt()` — no model-specific adaptation.

5. **`src/executor/native/bundle.rs`** — Bundle system: Filters tools based on exec_mode. Could be extended to filter/adapt based on model capabilities.

6. **`src/executor/native/agent.rs:46-75`** — `AgentLoop`: The tool-use loop. Has `supports_tools: bool` field — when false, tools are omitted. This is the closest thing to model-capability detection.

**Where CLAUDE.md content enters:**

7. **`src/commands/spawn/context.rs:379-392`** — `read_claude_md()`: Reads CLAUDE.md from project root. Only injected at `Full` scope (line 147-149 of `build_scope_context`).

8. **`src/service/executor.rs:475-481`** — CLAUDE.md section in `build_prompt()`: Wrapped as `## Project Instructions (CLAUDE.md)`.

### What to Change

**Improvement A: Model-aware prompt adaptation.** When `requires_native_executor()` detects a non-Claude model:
- Strip Claude Code-specific tool references from the prompt (Edit, Write, Glob, Grep → generic equivalents)
- Translate tool names to what the native executor actually provides
- Add model-specific preamble explaining the tool interface

**Primary files:**
- `src/service/executor.rs:362-486` — `build_prompt()`: Add a `model_family: Option<ModelFamily>` parameter, and conditionally adapt tool references and workflow sections
- `src/commands/spawn/execution.rs:268-278` — Where `build_prompt()` is called: Pass model info
- `src/executor/native/bundle.rs` — Bundle system: Add model-specific bundles that translate tool names

**Improvement B: Inject model-specific CLAUDE.md equivalent.** For non-Claude models, inject a translated version of project instructions that uses the native executor's tool names and conventions.

**Primary files:**
- `src/commands/spawn/context.rs:379-392` — `read_claude_md()`: Add a `translate_for_model()` step
- `src/service/executor.rs:475-481` — Adapt CLAUDE.md injection based on executor type

---

## File Ownership & Parallel Work Assignment

### Track 1: Executor Context Injection (`--after` awareness)
- **Owns:** `src/service/executor.rs` (lines 170-218: `AUTOPOIETIC_GUIDANCE`)
- **Touches:** `src/commands/spawn/context.rs` (add parent-task-ID injection)
- **Risk:** LOW — surgical text change to static prompt section

### Track 2: Separate-Agent Verification
- **Owns:** `src/commands/done.rs` (verify flow, lines 222-350)
- **Touches:** `src/commands/service/coordinator.rs` (new verify task builder, modeled on `build_flip_verification_tasks` at line 1538)
- **Risk:** MEDIUM — changes task state machine (Done → PendingValidation → verify agent → Done). Existing FLIP pipeline is a proven pattern to follow.

### Track 3: Pre-Task Test Discovery
- **Owns:** `src/commands/spawn/context.rs` (new `discover_relevant_tests()` function)
- **Touches:** `src/service/executor.rs` (new `ScopeContext` field + `build_prompt()` section)
- **Touches:** `src/commands/spawn/execution.rs` (call test discovery during setup)
- **Risk:** LOW — additive. Worst case: extra prompt section with no test files found.

### Track 4: Adaptive Decomposition Templates
- **Owns:** `src/service/executor.rs` (lines 170-295: `AUTOPOIETIC_GUIDANCE` + `PATTERN_KEYWORDS_GLOSSARY`)
- **Risk:** LOW — primarily prompt text changes. Making guidance adaptive requires reading graph state, which `build_prompt()` doesn't currently do (it only receives pre-computed `ScopeContext`).

### Track 5: Non-Claude Model Context
- **Owns:** `src/executor/native/bundle.rs` (model-specific bundles)
- **Touches:** `src/service/executor.rs` (`build_prompt()` model-family parameter)
- **Touches:** `src/commands/spawn/execution.rs` (pass model info to `build_prompt()`)
- **Touches:** `src/commands/spawn/context.rs` (`read_claude_md()` translation)
- **Risk:** MEDIUM — requires understanding which prompt sections are Claude-specific vs. model-agnostic. The native executor already works, but prompt content assumes Claude Code tool names.

---

## File Conflict Matrix

| File | Track 1 | Track 2 | Track 3 | Track 4 | Track 5 |
|------|---------|---------|---------|---------|---------|
| `src/service/executor.rs` | WRITE | read | WRITE | WRITE | WRITE |
| `src/commands/done.rs` | — | WRITE | — | — | — |
| `src/commands/spawn/context.rs` | write | — | WRITE | — | write |
| `src/commands/spawn/execution.rs` | — | — | write | — | write |
| `src/commands/service/coordinator.rs` | — | write | — | — | — |
| `src/executor/native/bundle.rs` | — | — | — | — | WRITE |

**Conflict zones:**
- `src/service/executor.rs` is touched by Tracks 1, 3, 4, and 5. **Tracks 1 and 4** modify the same section (decomposition guidance) — they should be **serialized** (Track 1 → Track 4).
- Tracks 3 and 5 touch different sections of `executor.rs` (`ScopeContext` vs. `build_prompt()` model adaptation) and can run in parallel.
- Track 2 is fully independent of all other tracks.

### Recommended Execution Order
```
Track 2 (verify)     ─────────────────────────────►
Track 3 (test disc.) ─────────────────────────────►
Track 5 (model ctx)  ─────────────────────────────►
Track 1 (--after)    ──────────► Track 4 (decomp.) ──────────►
```

---

## Interface Contracts Between Tracks

### Track 3 → Tracks 1, 4 (ScopeContext extension)
Track 3 adds a `relevant_tests: String` field to `ScopeContext`. Tracks 1 and 4 should be aware that `build_scope_context()` gains a new step but this doesn't affect their work directly.

### Track 5 → Track 4 (model-aware decomposition)
Track 5 adds model-family awareness to `build_prompt()`. Track 4's adaptive decomposition should use this to tailor decomposition advice per model capability. **Contract:** Track 5 exposes a `ModelFamily` enum or parameter on `build_prompt()` that Track 4 can optionally consume.

### Track 2 → Track 3 (verify + test discovery synergy)
Track 3 discovers test files; Track 2 runs verification. **Contract:** Track 3 should populate `task.verify` with auto-generated test commands when the task has no explicit `--verify`. Track 2 should handle both explicit and auto-generated verify commands identically.

---

## Risk Assessment

| Track | Invasiveness | Risk | Mitigation |
|-------|-------------|------|------------|
| Track 1 | **Surgical** | LOW | Text-only change to `AUTOPOIETIC_GUIDANCE`. No behavioral change to executor. |
| Track 2 | **Moderate** | MEDIUM | Changes task state machine. Must preserve backward compat: tasks without `--verify` are unaffected. FLIP pipeline is a proven pattern to follow. |
| Track 3 | **Additive** | LOW | New function + new prompt section. Falls back gracefully when no tests found. |
| Track 4 | **Surgical** | LOW | Prompt text changes. Adaptive version reads graph state but only to inform prompt content, not behavior. |
| Track 5 | **Moderate** | MEDIUM | Must ensure prompt translation doesn't break working Claude executor path. Guard with `if executor == "native"` conditionals. |

### Most Invasive Change
Track 2 (separate-agent verification) touches the deepest infrastructure — the `wg done` state machine. However, the existing FLIP verification pipeline (`build_flip_verification_tasks` at `coordinator.rs:1538`) provides an exact pattern to follow. The change is conceptually: "do what FLIP already does, but for `--verify` commands."

### Safest Starting Point
Track 1 (strengthen `--after` guidance) — pure prompt text change, zero code risk, immediate impact on agent behavior.
