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

## Follow-Up Compatibility Work

- OpenCode now has a Workgraph executor path: `opencode`, `zai`, and `z-ai` model prefixes resolve to the `opencode` handler, and `spawn-task` can dispatch `wg opencode-handler`.
- Workgraph now has central Z.AI GLM route IDs for fast/standard/premium usage; the runtime path converts `zai:glm-5.1` and `z-ai:glm-5.1` into the `opencode` CLI's `zai/glm-5.1` model form.
- If Speedrift wants Codexd as a distinct daemonized surface rather than the current Codex CLI handler, Workgraph also needs an explicit `codexd` provider/executor mapping and spawn adapter.
- Remaining OpenCode/Z.AI work is integration polish: setup/profile templates and Speedrift/driftdriver policy for when to select the Z.AI route.
