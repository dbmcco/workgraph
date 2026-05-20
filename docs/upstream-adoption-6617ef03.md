# Upstream adoption through 6617ef03

Date: 2026-05-20

## Adopted

- Merged `graphwork/workgraph` `origin/main` at `6617ef03` into local `main`.
- Kept upstream provider/model routing, named profiles, codex init/runtime fixes, TUI/chat/native executor changes, terminology sync, smoke manifest expansion, and new HTML/publish/secret/profile surfaces.
- Preserved local central registry integration: Workgraph route defaults still resolve through `paia-agent-runtime/config/cognition-presets.toml` via `src/model_routes.rs`.
- Preserved Speedrift's Codex-first posture: no-config defaults now use the central `workgraph.codex_cli_premium` route through the codex handler, not the Claude handler.
- Codexd is not modeled as a separate executor in Workgraph; the default route currently lands on the existing `codex` CLI handler.

## Local Compatibility Fixes

- Fixed upstream compile fallout where TUI config tests constructed `EndpointConfig` without `api_key_ref`.
- Updated default-route tests and docs from Claude-first to Codex-first.
- Updated the central Workgraph Codex route entries to the current Codex CLI tier set:
  - fast: `gpt-5.4-mini`
  - standard: `gpt-5.4`
  - premium: `gpt-5.5`

## Deferred

- Broad upstream whitespace/doc churn is adopted as-is. `git diff --check` still reports upstream trailing whitespace in archived docs and CSV fixtures; these are not runtime blockers and should be cleaned in a separate formatting pass if desired.
- `wg setup` still preserves an existing explicit route and OpenRouter key detection. Only the no-config/no-route fallback is Codex-first.

## Conflicts Or Gaps

- OpenCode is not a real Workgraph handler yet. The code accepts `"opencode"` in some config/IPC tests, but `ExecutorKind`, `handler_for_model`, `spawn-task`, and `provider_to_executor` do not define an OpenCode executor path.
- Z.AI GLM is present in the central registry for other PAIA surfaces, but Workgraph has no `zai:`/`z-ai:` provider prefix, no route IDs for Workgraph GLM usage, and no OpenCode-backed execution adapter.
- If Speedrift wants Codexd as a distinct daemonized surface rather than the current Codex CLI handler, Workgraph also needs an explicit `codexd` provider/executor mapping and spawn adapter.
- Therefore OpenCode + Z.AI GLM support needs both sides:
  - Workgraph upstream/local handler support: provider prefix, executor kind or adapter mapping, spawn dispatch, config/profile templates, tests.
  - Speedrift/driftdriver wrapper support: central route IDs, credential assignment, and runtime wiring for any `opencode`/Z.AI invocation policy.
