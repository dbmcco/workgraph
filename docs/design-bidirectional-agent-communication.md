# Design: Bidirectional Agent Communication via JSONL Streaming

## Status
Research complete. Ready for implementation.

## Context

Currently agents are spawned with `stdin/stdout/stderr` all set to `Stdio::null()` (see `src/commands/spawn/execution.rs:229-231`). We pass `--output-format stream-json` to claude but never read the output. The wrapper script redirects all output to `output.log`. Agents are fire-and-forget.

---

## Research Findings

### 1. JSONL Stream Format (`--output-format stream-json`)

When using `--output-format stream-json --verbose`, Claude CLI emits **newline-delimited JSON objects**. Each line is a self-contained JSON event. The key event types are:

| Event Type | Description | Fields |
|---|---|---|
| `system` (subtype `init`) | First event, contains session metadata | `session_id`, `tools`, `model` |
| `assistant` | Complete assistant message after a turn | `message` (with `content` blocks), `session_id` |
| `result` | Final event when agent completes | `result` (text), `session_id`, `cost_usd`, `usage` |

With `--include-partial-messages`, additional **`stream_event`** objects are emitted containing raw Anthropic API streaming events:

| Stream Event Type | Description |
|---|---|
| `message_start` | Start of a new API message |
| `content_block_start` | Start of text or tool_use content block |
| `content_block_delta` | Incremental text (`text_delta`) or tool input (`input_json_delta`) |
| `content_block_stop` | End of a content block |
| `message_delta` | Message-level updates (stop_reason, usage) |
| `message_stop` | End of the API message |

**Key insight**: Even without `--include-partial-messages`, the stream emits `assistant` messages after each agent turn (each tool use + response cycle). This is sufficient for liveness detection — we don't need token-level streaming.

### 2. Stdin Handling and Message Injection

**CLI stdin**: The `claude -p` command reads the prompt from stdin (or from the `-p` argument). Once the prompt is consumed, stdin is not read again during execution. **There is no mechanism to inject messages into a running `claude -p` process via stdin.**

**`--input-format stream-json`**: The CLI supports `--input-format stream-json` which accepts streaming JSON input. However, documentation is sparse and this appears to be primarily used by the Agent SDK's streaming input mode, not for ad-hoc message injection into a running session.

**Agent SDK streaming input**: The Python/TypeScript Agent SDK supports an `AsyncGenerator` pattern where you yield messages over time. The agent processes them sequentially. This is the **only supported mechanism for mid-flight message injection**, but it requires using the SDK programmatically — not the CLI.

**Conclusion**: **No stdin-based message injection is possible with `claude -p`.** The only paths are:
1. Use `--resume <session-id>` after the agent exits (or between turns if using the SDK)
2. Use the Agent SDK's streaming input mode (Python/TypeScript, not CLI)

### 3. --continue / --resume

- `--continue` (`-c`): Continues the **most recent** conversation in the current directory
- `--resume <session-id>` (`-r`): Resumes a **specific** session by ID
- `--fork-session`: When resuming, creates a new session branch instead of appending

These only work **after** the previous `claude -p` process has exited. You cannot resume a session that's currently in use by a running process.

**Session IDs**: Captured from the first `system` event (subtype `init`) in the stream output. The `session_id` field is present on every event.

### 4. Liveness Detection

**With basic stream-json output** (no `--include-partial-messages`):
- Each agent turn emits an `assistant` message after the model responds and before tool execution
- Silence between events = agent is executing a tool (could be long-running bash command)
- A `result` event signals completion
- No explicit heartbeat events exist

**With `--include-partial-messages`**:
- `content_block_delta` events provide token-level progress during model inference
- `content_block_start` with `type: "tool_use"` signals tool execution starting
- Much more granular liveness signal, but higher volume

**Practical liveness approach**:
- Parse the JSONL stream for any event — any event means the process is alive and working
- Track time since last event. If no event for N seconds, check if PID is still running
- `content_block_start` with tool_use type = "agent is about to run a tool"
- `assistant` message = "agent completed a turn"
- Absence of events + PID alive = agent is executing a tool (normal)
- Absence of events + PID dead = agent crashed

### 5. Architecture for Stream Reading

**Current architecture**:
```
coordinator → spawn wrapper.sh → wrapper runs `claude -p` → output >> output.log
                                 wrapper checks exit code → wg done/fail
```

**Problem with keeping stdout open**: The agent is detached via `setsid()` into its own session so it survives coordinator restarts. If the coordinator holds the stdout pipe, the coordinator becomes tied to the agent's lifetime (or vice versa).

---

## Design Recommendation

### Phase 1: Stream Capture (Low effort, high value)

**Change**: Modify the wrapper script to **tee** the JSONL stream instead of just redirecting to a file.

```bash
# Current:
${timed_command} >> "$OUTPUT_FILE" 2>&1

# New:
${timed_command} 2>> "$OUTPUT_FILE.stderr" | tee -a "$OUTPUT_FILE" > "$OUTPUT_DIR/stream.jsonl"
```

Or simpler — just ensure the output goes to a known JSONL file:
```bash
${timed_command} > "$OUTPUT_DIR/stream.jsonl" 2>> "$OUTPUT_FILE.stderr"
# Also create a combined log:
cat "$OUTPUT_DIR/stream.jsonl" >> "$OUTPUT_FILE"
```

**This preserves the detached process model** — no pipes between coordinator and agent. The coordinator (or a watcher) reads the file.

### Phase 2: Liveness Watcher (Moderate effort)

**Architecture**: A **file-based watcher** in the coordinator poll loop.

```
coordinator poll loop:
  for each in-progress agent:
    1. Check PID alive (existing)
    2. Stat stream.jsonl → last modified time
    3. If stale for > threshold AND pid alive → log warning "agent may be stuck"
    4. Parse last N lines of stream.jsonl for progress info
    5. Optionally extract session_id from init event for future --resume
```

**Why coordinator, not a separate watcher**: The coordinator already polls agents for dead-PID detection. Adding file-stat checks is minimal overhead. A separate watcher process adds operational complexity (another daemon to manage).

**What to extract from the stream**:
- `session_id` (from `init` event) — store in agent registry for later `--resume`
- Last event timestamp — for staleness detection
- Tool names being called — for progress reporting (`wg agents` output)
- Token/cost usage from `result` event — for budget tracking

### Phase 3: Mid-flight Communication (Higher effort, requires Agent SDK)

For true bidirectional communication (sending messages to a running agent), the options are:

**Option A: File-based message passing (works today, no SDK needed)**

This is what `wg msg send` already does. The agent checks for messages via `wg msg read` at natural breakpoints. This is cooperative — the agent must poll. But it works and is already implemented.

**Enhancement**: Add a `--watch` flag to the wrapper script that monitors for new messages and logs them to a file the agent can check, reducing polling frequency.

**Option B: Replace CLI with Agent SDK**

Switch from `claude -p` (CLI subprocess) to the Python/TypeScript Agent SDK:

```python
async def run_agent(task_id, prompt):
    async def message_stream():
        yield {"type": "user", "message": {"role": "user", "content": prompt}}
        # Wait for and yield injected messages
        while True:
            msg = await check_for_new_messages(task_id)
            if msg:
                yield {"type": "user", "message": {"role": "user", "content": msg}}
            await asyncio.sleep(5)

    async for event in query(prompt=message_stream(), options=options):
        handle_event(event)
```

This gives true streaming input but requires:
1. A Python/TypeScript runtime alongside the Rust coordinator
2. A new executor type (e.g., `executor_type = "sdk"`)
3. Managing the SDK process lifecycle

**Option C: `--resume` for follow-up messages**

After an agent completes a turn (detected via stream), use `--resume <session-id>` to send a follow-up:

```bash
# Agent exits or pauses
claude -p "New instructions from coordinator" --resume $SESSION_ID
```

Limitations:
- Only works after the agent exits (not mid-flight)
- Session must have been persisted (conflicts with `--no-session-persistence`)
- The agent loses its ephemeral state (tool results in working memory)

### Recommendation

**Start with Phase 1 + Phase 2. Defer Phase 3.**

Rationale:
1. **Phase 1** (stream capture) is a ~20-line change to the wrapper script template and gives us the raw data
2. **Phase 2** (liveness watcher) is moderate effort in the existing coordinator loop and provides real observability
3. **Phase 3** (bidirectional) has high complexity and the file-based `wg msg` system already provides cooperative message passing. True mid-flight injection is a nice-to-have but not blocking for most workflows

For Phase 3, if/when needed, **Option B (Agent SDK)** is the best long-term path because it's the officially supported mechanism. Option A (file-based) is already working. Option C (--resume) is fragile.

---

## Implementation Plan

### Phase 1: Stream Capture
**Files to modify**: `src/commands/spawn/execution.rs` (wrapper script template)

1. Change the wrapper script to write JSONL to `stream.jsonl` in the agent output dir
2. Keep stderr separate in `stderr.log`
3. Maintain the combined `output.log` for backward compat
4. Ensure `--include-partial-messages` is NOT added (too noisy for file-based capture; basic `assistant` events suffice)

### Phase 2: Liveness + Progress
**Files to modify**: `src/service/mod.rs` (coordinator poll loop), `src/service/registry.rs` (agent metadata)

1. Add `session_id: Option<String>` to agent registry
2. In coordinator poll, parse `stream.jsonl` for:
   - Session ID (from init event)
   - Last event timestamp (for staleness)
   - Current tool (for progress display)
3. Add staleness warning to `wg agents` output
4. Store session_id for potential future `--resume` use

### Phase 3: Agent SDK Executor (Future)
**New files**: `src/service/sdk_executor.rs` or similar

1. New executor type that wraps the Python/TypeScript Agent SDK
2. True streaming input via AsyncGenerator pattern
3. Full bidirectional communication
4. Requires deciding on Python vs TypeScript runtime dependency

---

## Trade-offs Summary

| Approach | Complexity | Bidirectional | Detached Process | Session Survives Restart |
|---|---|---|---|---|
| Current (fire-and-forget) | None | No | Yes | Yes |
| Phase 1 (stream capture) | Low | Read-only | Yes | Yes |
| Phase 2 (liveness watcher) | Medium | Read-only | Yes | Yes |
| Phase 3A (file-based msg) | Low | Cooperative poll | Yes | Yes |
| Phase 3B (Agent SDK) | High | Full streaming | Needs new model | Depends |
| Phase 3C (--resume) | Medium | Between sessions | Yes | Needs persistence |

---

## Open Questions

1. **Should we add `--include-partial-messages`?** More granular liveness but ~100x more output volume. Recommendation: No for Phase 1-2. Revisit if we need token-level progress.
2. **Session persistence**: Currently we use `--no-session-persistence`. If we want `--resume` capability, we need to remove this flag (or make it configurable per task).
3. **Budget tracking**: The `result` event includes `cost_usd` and `usage` — should we surface this in `wg agents` and task logs?
