# Sessions as identity — the unified handler/session model

**Status:** design. See `sessions-as-identity-rollout.md` for staged work plan.

## The principle

> The coordinator isn't a process. The coordinator is a *session*.
> The process is an ephemeral *handler* that embodies the session
> while it's running.

Every LLM-backed task in workgraph — coordinator, agent, evaluator,
interactive nex — is (a) a persistent *session* on disk, plus (b) a
currently-active *handler* process. The session is the identity; the
handler is the body. Bodies can die and be reborn; the session persists.

This is already true for our native `wg nex` runtime — `--resume`
restores conversation from the journal, sessions live at
`.workgraph/chat/<uuid>/`, aliases map human-readable names to UUIDs.
What's missing is the *enforcement*: a single-writer lock to guarantee
at-most-one handler per session, and the lifecycle conventions that
follow from it.

This document specifies the full model. The sibling
`sessions-as-identity-rollout.md` specifies how we get there.

## What collapses

Today workgraph has three parallel mental models for "thing an LLM is
doing":

1. **Coordinator.** Spawned by the daemon, identified by numeric id
   (`coordinator-0`), uses a hand-rolled loop in
   `commands/service/coordinator_agent.rs`, talks to the TUI via chat
   files.

2. **Task agent.** Spawned by the daemon or `wg claim`, identified by
   task id, uses `AgentLoop::run` (soon: `run_interactive` with
   `--autonomous`), streams to `.workgraph/chat/task-<id>/streaming`.

3. **Interactive nex.** Spawned by the user at a terminal, identified
   by a random UUID alias, uses `AgentLoop::run_interactive` with
   either `TerminalSurface` or `ChatSurfaceState`.

These are the same thing in different clothes. Unified model:

- Every LLM activity is a **task** in the graph
- Every task that has a handler has a **session** at `.workgraph/chat/<uuid>/`
- The task id IS the session alias (coordinator-0 → the alias for
  some UUID, soon replaced by direct UUID-referencing task ids)
- The **handler** is whichever process currently owns the session
  (enforced by a lock file)

## Data model

### Session

Lives at `.workgraph/chat/<uuid>/`.

```
<uuid>/
  conversation.jsonl     # full journal (replayable)
  session-summary.md     # compaction artefact for --resume
  inbox.jsonl            # user → handler queue
  outbox.jsonl           # handler → user finalized turns
  .streaming             # in-flight assistant text (dotfile, ephemeral)
  stream.jsonl           # structured per-turn telemetry (tool_start/end, etc.)
  trace.ndjson           # low-level event log
  .handler.pid           # NEW: currently-active handler, see §Lock below
```

The session registry at `.workgraph/chat/sessions.json` maps UUID →
`SessionMeta { aliases, kind, created, forked_from }`. Aliases like
`coordinator-0` or `task-foo` are symlinks in the chat dir pointing at
the canonical UUID dir.

### Task

A graph task with `executor_type` set to something that runs an LLM
(native, claude, codex, gemini, amplifier). The task's `chat_ref` (new
field, or derivable from task_id) resolves to a session UUID. When the
task transitions to `in-progress`, it means a handler has claimed the
session.

### Handler

The process currently inhabiting a session. Could be:
- `wg nex --chat <uuid> --resume` (native handler)
- `claude --resume <their-session>` (claude handler, via adapter)
- `codex --session <id>` (codex handler, via adapter)
- Any other CLI the executor map knows how to launch

The handler's PID is written to `<uuid>/.handler.pid` on startup and
removed on clean exit (§Lock).

## The lock

File: `.workgraph/chat/<uuid>/.handler.pid`. Contents:

```
<pid>\n<exec-start-iso8601>\n<handler-kind>\n
```

### Acquire

A new handler process that wants to own the session:

1. Open `.handler.pid` with `O_CREAT | O_EXCL | O_WRONLY`. If it
   succeeds, we own it. Write PID + start time + kind, fsync, close.
2. If `EEXIST`, read the existing file:
   - **PID exists in `/proc` (Linux) / `kill -0 pid == 0` (Unix):**
     the lock is live. Either refuse (First-handler-wins), signal the
     existing handler to exit (TUI-takeover), or fall back to observer
     (read-only tail).
   - **PID is dead:** lock is stale. Delete the file and retry the
     O_EXCL create.
3. On clean handler exit, remove the file.
4. On crash, the file stays. Next acquire sees a dead PID and recovers.

### Release

On `SIGTERM` / `SIGINT` / clean `/quit` / EndTurn-in-autonomous-mode,
the handler:
1. Removes `.handler.pid` before exiting.
2. (Optional) Writes a final `end` entry to `conversation.jsonl`.

`atexit` / `Drop` hooks cover clean exits. Crashes are recovered via
stale-PID detection on next acquire.

### Takeover

Handler A holds the lock. Handler B wants it. Flow:
1. B reads A's PID.
2. B sends `SIGTERM` to A.
3. B polls the lock file every 100ms for up to 5s.
4. A catches SIGTERM, finishes its current turn (the conversation
   loop's turn-boundary is the safe point), removes the lock, exits.
5. B acquires via O_EXCL.
6. If A doesn't release within 5s, B escalates to `SIGKILL`, waits
   another 1s, then removes the stale file and acquires.

## Handoff policy

When the TUI opens and wants to own session X but X already has a
handler:

**Decision:** *TUI always wins.* The TUI issues a takeover. The
existing handler (likely a daemon-spawned coordinator) exits cleanly
at its next turn boundary, the TUI's PTY-backed handler picks up. The
journal ensures continuity — the new handler resumes from where the
old one stopped.

Rationale: the everyday UX is "open TUI, talk to my agent". Anything
else (refuse, degrade to read-only, modal "take over? y/n") is a
paper-cut for the common case. Autonomous-daemon users who *don't*
want takeover can `wg nex --chat X --no-takeover` in their terminal,
which stays put and refuses to be replaced.

## The TUI view

**One right-panel tab collapses five.** Chat, Log, Messages, Firehose,
and Output all become a single PTY view of the focused task's handler.

- **Focus a task in the graph →** right pane shows
  `wg spawn-task <task-id>` in a PTY. If the task has no handler yet,
  `spawn-task` spawns one and acquires the lock. If a handler already
  runs, takeover.
- **Input in the pane →** PTY stdin → handler. Native nex sees rustyline;
  claude sees its own input box; each handler behaves per its own UX.
- **The TUI renders bytes.** It doesn't interpret handler-specific
  structure. vt100 emulation + tui-term gives faithful rendering of
  whatever the CLI draws.

The "current coordinator" concept dissolves. There's no active
coordinator — there are N tasks, you focus whichever one.

## Heterogeneous executors

Different LLM-backed tasks use different CLIs. We unify identity and
lifecycle; we preserve handler-specific UI.

| Layer | Unified across executors | Heterogeneous |
|---|---|---|
| Session identity (UUID, alias, dir) | ✓ | |
| Handler-PID lock | ✓ | |
| Task lifecycle (ready → in-progress → done) | ✓ | |
| Working dir / worktree | ✓ | |
| PTY transport to the TUI | ✓ | |
| Terminal UI inside the PTY | | ✓ |
| Resume mechanics | | ✓ |
| Native tool surface | | ✓ |

### The `wg spawn-task` abstraction

A single CLI entry point that resolves a task id to its handler
command:

```
wg spawn-task <task-id>       # blocks, owns the lock, PTY-friendly
wg spawn-task <task-id> --no-lock   # tail-mode (no acquire)
```

Dispatches per executor:

- **native:** `wg nex --chat <uuid> --resume --role <task-role>`
- **claude:** `claude --resume <claude-session-id>` (session-id stored
  in task metadata; first run creates it)
- **codex:** `codex --session <id>` or equivalent
- **gemini:** TBD (check current CLI flags)
- **amplifier:** `wg amplifier-run <uuid>`

The TUI PTYs `wg spawn-task`, never the underlying CLI. When a CLI
vendor changes flags or adds resume support, we change one adapter in
`commands/spawn_task.rs`, not the TUI.

### Tool parity via MCP

`wg nex` has wg_add, wg_done, wg_fail, etc. built in. Claude/Codex
don't. An MCP server (`wg-mcp`) ships the same tool surface over MCP.
Handlers that support MCP (claude does, codex does, gemini: verify)
get tool parity.

For CLIs without MCP, a wrapper process proxies tool calls back to
`wg` via IPC. Ugly but functional fallback.

### Resume fallback

Not every CLI has session resume. For those, `wg spawn-task` prepends
a context preamble (the journal's session-summary) as the first user
message on re-invocation. Same trick `wg nex --resume` uses when there's
no journal but a summary exists. Lossy but workable.

## Daemon role

Daemon stops being the coordinator-owner. New role:

- **Supervisor.** Watch sessions with `in-progress` tasks and no live
  handler (stale or crashed). Respawn the handler.
- **Scheduler.** Continue walking ready tasks, spawning handlers for
  them up to `max_agents`.
- **Event sink.** Notification backends (Matrix, Telegram) still hook
  into lifecycle events emitted by handlers.

Daemon is opt-in. Running workgraph without a daemon works: the TUI
(or `wg spawn-task` at a terminal) owns any handler you care about.
Daemon is for users who want autonomous-when-I'm-away behavior.

## Open questions (unresolved in design, answered in rollout)

1. **Finished task focus.** Focus a `done` task. PTY shows
   `wg nex --chat X --resume` which replays journal and offers to
   continue the conversation. Should it be read-only by default with
   an explicit "resume and extend" command? Or hot, ready-to-type?

2. **Never-started task focus.** Focus a task with no journal, status
   `open`. Does focus auto-spawn its handler (effectively claiming
   the task)? Or require explicit start (e.g., `s` keybind)?

3. **Remote worktree sessions.** Agent running on a git worktree.
   Does its chat dir live in the main `.workgraph/chat/` (shared) or
   in the worktree's `.workgraph/chat/`? PTY embedding assumes
   shared; worktree-local chat requires cross-worktree session
   resolution.

4. **Task-id migration.** `.coordinator-N` exists in today's graphs.
   Options: auto-migrate on startup to `.chat-<uuid>`, keep
   `.coordinator-N` as a permanent alias, or provide a one-shot
   migration tool.

5. **TUI takeover semantics for active conversations.** If the daemon
   is mid-turn when the TUI takes over, do we wait for the turn to
   complete (journal consistency) or interrupt immediately? Lean
   toward "wait up to 5s, then SIGKILL" per §Lock §Takeover.

## Non-goals

- **Homogenizing CLI UIs.** Claude's box-drawing won't look like nex's
  box-drawing. That's fine. Users see "claude's UI when on a claude
  task" — honest and simple.
- **Single-binary consolidation.** We're not rewriting claude/codex
  as Rust crates or linking them in-process. PTY keeps them
  subprocess-boundary clean.
- **Live multi-user per session.** One handler at a time. Two users
  want to watch? Second user gets tail-mode. Collaborative editing of
  the same session is out of scope.

## Related prior art in this repo

- `nex-as-coordinator.md` — introduced the ConversationSurface trait
  as the plug point for nex serving every role. Landed as
  commits 737d223f / f92c7b8a / d7f8b5cb / ecdc1252 (3a).
- `chat_sessions.rs` — session UUID registry, aliasing,
  `register_coordinator_session` helper.
- `commands/tui_pty.rs` — PTY-embed `wg nex` in ratatui, commit
  b5642aea (3b). The infrastructure the TUI Chat tab will build on.
