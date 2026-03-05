# Agent Activity & Token Reporting Protocol

## Status

**Draft** — 2026-03-05

---

## 1. Problem Statement

Two related bugs stem from incomplete executor abstraction:

### Problem 1: Opaque tool activity display

The coordinator agent shows `[Running: Bash...]` (line 1054 of `coordinator_agent.rs`) with no detail about *what* command is running or *what* came back. Meanwhile, the TUI's `update_agent_streams()` (`state.rs:4408-4560`) extracts rich detail from Claude CLI JSONL — Bash commands, file paths, grep patterns — but this is Claude-CLI-specific parsing that doesn't use the unified `StreamEvent` protocol at all.

### Problem 2: Token counting is wrong

In `coordinator_agent.rs:862`, input tokens are *assigned* (`total_input_tokens = input`) rather than *accumulated* (`+=`), so they always reflect only the last turn's usage. Output tokens are correctly accumulated with `+=`. This causes the "input=1, output=high" display bug.

Additionally, there are three independent token-tracking paths that may disagree:

1. **`coordinator_agent.rs:852-866`** — reads Claude CLI's `message.usage` in the coordinator's own stdout reader (the bug above)
2. **`graph.rs:382-509`** — `parse_token_usage()` / `parse_token_usage_live()` — reads `output.log` or `raw_stream.jsonl` for completed or in-progress worker agents
3. **`stream_event.rs:376-422`** — `AgentStreamState::ingest()` — reads unified `stream.jsonl` events

### Root cause

Both problems exist because each executor has its own event format and the translation to `StreamEvent` is incomplete. The `ToolStart`/`ToolEnd` events only carry the tool name, not the detail needed for useful display. The `graph.rs` token parsers read Claude-CLI-specific JSONL directly instead of going through `StreamEvent`.

---

## 2. Design: Enriched StreamEvent Protocol

### 2.1 Extend `ToolStart` and `ToolEnd` with detail

The core change: add a `detail` field to `ToolStart` and an `output_summary` field to `ToolEnd`. These fields carry executor-agnostic, human-readable descriptions.

```rust
// src/stream_event.rs

pub enum StreamEvent {
    // ... existing variants unchanged ...

    /// Tool execution started.
    ToolStart {
        name: String,
        /// Human-readable detail about what the tool is doing.
        /// Examples: "$ cargo test", "src/main.rs", "pattern: foo.*bar"
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
        timestamp_ms: i64,
    },

    /// Tool execution completed.
    ToolEnd {
        name: String,
        is_error: bool,
        duration_ms: u64,
        /// Brief summary of the output (not the raw output).
        /// Examples: "exit 0, 42 lines", "matched 3 files", "wrote 128 bytes"
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_summary: Option<String>,
        timestamp_ms: i64,
    },

    // ... rest unchanged ...
}
```

**Why `Option<String>` rather than structured data:**
- Different tools have fundamentally different "details" (a command vs. a file path vs. a pattern)
- The consumer only needs a display string
- Adding `Option` keeps backward compatibility with existing `stream.jsonl` files
- Avoids an ever-growing enum of tool-specific detail types

### 2.2 Detail extraction: a single generic function

Instead of each display path (TUI, coordinator, CLI) parsing tool inputs differently, define one function that extracts detail from a tool_use block:

```rust
// src/stream_event.rs (or a new src/tool_detail.rs)

/// Extract a human-readable detail string from a tool_use content block.
///
/// This is the ONE place that knows how to summarize tool inputs.
/// All display paths call this instead of doing their own parsing.
pub fn extract_tool_detail(name: &str, input: &serde_json::Value) -> Option<String> {
    match name {
        "Bash" | "bash" => {
            let cmd = input.get("command")?.as_str()?;
            let cmd = cmd.trim();
            if cmd.len() > 100 {
                Some(format!("$ {}...", &cmd[..cmd.floor_char_boundary(100)]))
            } else {
                Some(format!("$ {cmd}"))
            }
        }
        "Read" | "Write" | "Edit" => {
            let path = input.get("file_path")?.as_str()?;
            Some(format!("{path}"))
        }
        "Grep" => {
            let pattern = input.get("pattern")?.as_str()?;
            let path = input.get("path").and_then(|v| v.as_str());
            match path {
                Some(p) => Some(format!("/{pattern}/ in {p}")),
                None => Some(format!("/{pattern}/")),
            }
        }
        "Glob" => {
            let pattern = input.get("pattern")?.as_str()?;
            Some(format!("{pattern}"))
        }
        "TodoWrite" => Some("updating todos".to_string()),
        "WebSearch" | "web_search" => {
            input.get("query")?.as_str().map(|q| format!("\"{q}\""))
        }
        "WebFetch" | "web_fetch" => {
            input.get("url")?.as_str().map(|u| u.to_string())
        }
        // wg tools
        _ if name.starts_with("wg_") || name.starts_with("mcp__") => {
            // For wg tools, show the first string argument
            input.as_object().and_then(|m| {
                m.values()
                    .find_map(|v| v.as_str())
                    .map(|s| {
                        if s.len() > 80 {
                            format!("{}...", &s[..s.floor_char_boundary(80)])
                        } else {
                            s.to_string()
                        }
                    })
            })
        }
        _ => None,
    }
}

/// Extract a brief output summary from tool results.
///
/// Produces compact summaries like "exit 0, 42 lines" or "3 matches".
pub fn summarize_tool_output(name: &str, output: &str, is_error: bool) -> String {
    let line_count = output.lines().count();
    let byte_count = output.len();

    if is_error {
        let first_line = output.lines().next().unwrap_or("error");
        let truncated = if first_line.len() > 80 {
            format!("{}...", &first_line[..first_line.floor_char_boundary(80)])
        } else {
            first_line.to_string()
        };
        return format!("ERROR: {truncated}");
    }

    match name {
        "Bash" | "bash" => {
            if line_count == 0 {
                "exit 0 (no output)".to_string()
            } else if line_count <= 3 {
                let preview: String = output.lines().take(3).collect::<Vec<_>>().join("; ");
                if preview.len() > 100 {
                    format!("{}...", &preview[..preview.floor_char_boundary(100)])
                } else {
                    preview
                }
            } else {
                format!("{line_count} lines")
            }
        }
        "Grep" => {
            format!("{line_count} matches")
        }
        "Read" => {
            format!("{line_count} lines")
        }
        "Write" | "Edit" => {
            format!("{byte_count} bytes written")
        }
        "Glob" => {
            format!("{line_count} files")
        }
        _ => {
            if line_count <= 1 {
                let preview = output.lines().next().unwrap_or("");
                if preview.len() > 80 {
                    format!("{}...", &preview[..preview.floor_char_boundary(80)])
                } else {
                    preview.to_string()
                }
            } else {
                format!("{line_count} lines")
            }
        }
    }
}
```

### 2.3 How each executor populates detail

**Native executor** (`src/executor/native/agent.rs`):

The native executor has direct access to tool inputs/outputs. When emitting `ToolStart`, call `extract_tool_detail(name, input)` to populate `detail`. When emitting `ToolEnd`, call `summarize_tool_output()` to populate `output_summary`.

```rust
// In agent.rs tool execution loop (around line 234):
if let Some(ref sw) = self.stream_writer {
    sw.write_tool_start_with_detail(
        name,
        extract_tool_detail(name, input),
    );
}
// ... execute tool ...
if let Some(ref sw) = self.stream_writer {
    sw.write_tool_end_with_summary(
        name,
        output.is_error,
        duration_ms,
        Some(summarize_tool_output(name, &output.content, output.is_error)),
    );
}
```

**Claude CLI executor** (`translate_claude_event` in `stream_event.rs`):

The Claude CLI doesn't emit per-tool events in its JSONL stream. Tool details are embedded in `assistant` message content blocks (type=tool_use). The translation layer should extract `ToolStart` events from these blocks:

```rust
// In translate_claude_event(), within the "assistant" arm:
// After extracting tools_used, also emit ToolStart events for each tool_use block.
// Return a Vec<StreamEvent> instead of Option<StreamEvent>.

// New signature:
pub fn translate_claude_event(line: &str) -> Vec<StreamEvent> { ... }
```

For each `tool_use` content block in an `assistant` message, emit a `ToolStart` with detail extracted via `extract_tool_detail(name, &input)`. The `ToolEnd` can be synthesized when the next `assistant` or `result` event arrives (since Claude CLI doesn't emit explicit tool-result events at the stream level).

**Amplifier/Shell executors**:

These don't report tool-level activity. Their `stream.jsonl` files only have Init+Result bookends. This is acceptable — the protocol is additive. Consumers display whatever detail is available.

### 2.4 Update `AgentStreamState` to track detail

```rust
// src/stream_event.rs

pub struct AgentStreamState {
    // ... existing fields ...

    /// Current tool being executed (if any).
    pub current_tool: Option<String>,
    /// Detail about the current tool operation.
    pub current_tool_detail: Option<String>,
    /// Summary of the last completed tool operation.
    pub last_tool_summary: Option<String>,
}
```

In `ingest()`:

```rust
StreamEvent::ToolStart { name, detail, .. } => {
    self.current_tool = Some(name.clone());
    self.current_tool_detail = detail.clone();
    self.last_tool_summary = None;
}
StreamEvent::ToolEnd { output_summary, .. } => {
    self.last_tool_summary = output_summary.clone();
    self.current_tool = None;
    self.current_tool_detail = None;
}
```

---

## 3. Token Accumulation Strategy

### 3.1 Fix the coordinator agent bug

In `coordinator_agent.rs:862`, change:

```rust
// BEFORE (bug):
total_input_tokens = input;     // Overwrites — always shows last turn
total_output_tokens += output;  // Accumulates — correct

// AFTER (fix):
total_input_tokens += input;    // Accumulate like output tokens
total_output_tokens += output;
```

This is a simple bug fix that should be done immediately.

### 3.2 Canonical token source: `stream.jsonl`

There are currently three independent token-tracking paths. The canonical source should be `stream.jsonl` via `AgentStreamState`. The others should be deprecated or become fallbacks.

**Current paths:**

| Path | Source | Used by | Correctness |
|---|---|---|---|
| `AgentStreamState::ingest()` | `stream.jsonl` | Coordinator liveness detection | Correct (accumulates per-turn, overwritten by Result) |
| `parse_token_usage()` / `parse_token_usage_live()` | `output.log` (Claude CLI raw JSONL) | Task completion, `wg show` | Correct for completed runs; in-progress sums per-turn |
| `stdout_reader` token tracking | Claude CLI stdout pipe | Coordinator agent TUI display | **Buggy** (input_tokens assignment) |

**Target state:**

| Path | Source | Used by | Status |
|---|---|---|---|
| `AgentStreamState::ingest()` | `stream.jsonl` | All live monitoring (coordinator, TUI, CLI) | Primary source |
| `parse_token_usage()` | `output.log` → `stream.jsonl` | Task completion storage | Migrate to read `stream.jsonl` Result event |
| `stdout_reader` tracking | Removed | Replaced by `AgentStreamState` for coordinator | Deprecated |

### 3.3 Per-executor token semantics

All executors should produce `Turn` events with token usage. The semantics are:

| Executor | Token source | Input token semantics | Notes |
|---|---|---|---|
| Claude CLI | `message.usage` in JSONL | **Per-turn** (not cumulative) — includes all context tokens for that API call | Must accumulate across turns |
| Native | API response `usage` field | Per-turn (same as Claude API) | Already correct |
| OpenRouter/OpenAI-compat | `usage` in chat completion response | Per-turn (same convention) | Already correct via OpenAiClient |
| Amplifier | No per-turn data | N/A | Only Init+Result bookends; token tracking unavailable |
| Shell | N/A | N/A | No LLM involved |

**Important: Claude CLI reports per-turn input tokens, NOT cumulative.** Each turn's `input_tokens` includes the full context sent to the API for that turn. Summing them across turns gives the total tokens billed, which is what users care about (cost = sum of per-turn usage). This is the same semantics as the Anthropic API.

### 3.4 Migrate `parse_token_usage` to read `stream.jsonl`

Currently `parse_token_usage()` reads Claude-CLI-specific `output.log` format. It should be updated to prefer `stream.jsonl`:

```rust
pub fn parse_token_usage_from_stream(agent_dir: &Path) -> Option<TokenUsage> {
    let stream_path = agent_dir.join("stream.jsonl");
    if stream_path.exists() {
        let (events, _) = read_stream_events(&stream_path, 0).ok()?;
        // Find the Result event (last one)
        for event in events.iter().rev() {
            if let StreamEvent::Result { usage, .. } = event {
                return Some(TokenUsage {
                    cost_usd: usage.cost_usd.unwrap_or(0.0),
                    input_tokens: usage.input_tokens,
                    output_tokens: usage.output_tokens,
                    cache_read_input_tokens: usage.cache_read_input_tokens.unwrap_or(0),
                    cache_creation_input_tokens: usage.cache_creation_input_tokens.unwrap_or(0),
                });
            }
        }
        // No Result yet — sum Turn events (in-progress)
        let mut total = TokenUsage::default();
        let mut found_any = false;
        for event in &events {
            if let StreamEvent::Turn { usage: Some(u), .. } = event {
                found_any = true;
                total.input_tokens += u.input_tokens;
                total.output_tokens += u.output_tokens;
                total.cache_read_input_tokens += u.cache_read_input_tokens.unwrap_or(0);
                total.cache_creation_input_tokens += u.cache_creation_input_tokens.unwrap_or(0);
            }
        }
        if found_any {
            return Some(total);
        }
    }
    // Fallback: try legacy output.log parsing
    parse_token_usage_live(agent_dir.join("output.log").as_path())
}
```

---

## 4. Display Consumer Changes

### 4.1 Coordinator agent streaming display

**Current** (`coordinator_agent.rs:1054`):
```rust
streaming_text.push_str(&format!("\n[Running: {}...]\n", name));
```

**Proposed** — use `extract_tool_detail` on the tool input:
```rust
Ok(ResponseEvent::ToolUse { name, input }) => {
    has_tool_calls = true;
    let detail = serde_json::from_str::<serde_json::Value>(&input)
        .ok()
        .and_then(|v| extract_tool_detail(&name, &v));
    let display = match detail {
        Some(d) => format!("\n[{name}: {d}]\n"),
        None => format!("\n[{name}...]\n"),
    };
    streaming_text.push_str(&display);
    let _ = chat::write_streaming(dir, &streaming_text);
    parts.push(ResponsePart::ToolUse { name, input });
}
```

This gives the coordinator the same rich display as the TUI without any Claude-CLI-specific parsing in the display path.

### 4.2 TUI `update_agent_streams()` — migrate to unified protocol

**Current:** TUI reads `output.log` (Claude CLI JSONL) directly, with Claude-CLI-specific parsing of `assistant` messages → `tool_use` content blocks → per-tool detail extraction.

**Proposed:** TUI reads `stream.jsonl` via `AgentStreamState` or a lightweight reader. Since `stream.jsonl` now contains `ToolStart { detail }` and `ToolEnd { output_summary }`, the TUI can use these directly:

```rust
// Replace the Claude-CLI-specific parsing in update_agent_streams()
// with reading stream.jsonl:

let stream_path = agents_dir.join(agent_id).join("stream.jsonl");
let (events, new_offset) = read_stream_events(&stream_path, info.file_offset)?;
info.file_offset = new_offset;

for event in &events {
    match event {
        StreamEvent::Turn { .. } => {
            info.message_count += 1;
        }
        StreamEvent::ToolStart { name, detail, .. } => {
            info.latest_snippet = Some(match detail {
                Some(d) => format!("{name}: {d}"),
                None => name.clone(),
            });
            info.latest_is_tool = true;
        }
        StreamEvent::ToolEnd { output_summary, .. } => {
            if let Some(summary) = output_summary {
                info.latest_snippet = Some(summary.clone());
                info.latest_is_tool = true;
            }
        }
        // Text snippets could come from a new StreamEvent variant
        // or from Turn events if we add a text_snippet field
        _ => {}
    }
}
```

**Gap:** The TUI currently also shows assistant text snippets. `StreamEvent` doesn't have a variant for assistant text (it only has `Turn` with tool names). Two options:

1. **Add a `Text` variant** to `StreamEvent` for assistant text snippets (simple, but adds volume to stream.jsonl)
2. **Keep reading output.log for text snippets** while using stream.jsonl for tool details (hybrid, pragmatic)

**Recommendation:** Option 2 for now. Tool activity is the primary display concern. Text snippets are secondary and already work for Claude CLI agents. Adding a Text event can be a follow-up if native executor display needs it.

### 4.3 `wg agents` CLI display

Currently shows no live activity info. After this change, it could read `stream.jsonl` for each active agent and display the last tool activity:

```
ID            TASK                    EXECUTOR  PID    UPTIME  ACTIVITY               STATUS
agent-1234    implement-auth          claude    45678  5m32s   Bash: $ cargo test     working
agent-1235    fix-parser              native    45679  2m10s   Read: src/parser.rs    working
```

This is a follow-up enhancement, not required for the initial protocol change.

---

## 5. Migration Plan

### Phase 1: Fix token bug (immediate, ~10 lines)

1. Fix `coordinator_agent.rs:862`: change `total_input_tokens = input` to `total_input_tokens += input`
2. Verify with a test that tokens accumulate correctly

**Files changed:** `src/commands/service/coordinator_agent.rs`

### Phase 2: Enrich StreamEvent (small, ~80 lines)

1. Add `detail: Option<String>` to `ToolStart`
2. Add `output_summary: Option<String>` to `ToolEnd`
3. Add `extract_tool_detail()` and `summarize_tool_output()` functions to `stream_event.rs`
4. Update `StreamWriter` with `write_tool_start_with_detail()` and `write_tool_end_with_summary()` convenience methods (keeping the old methods as wrappers that pass `None`)
5. Update `AgentStreamState` to track `current_tool_detail` and `last_tool_summary`
6. Update serde tests for backward compatibility

**Files changed:** `src/stream_event.rs`

### Phase 3: Native executor emits detail (small, ~20 lines)

1. Update `agent.rs:234-249` to call `extract_tool_detail` and `summarize_tool_output`
2. Use the new `write_tool_start_with_detail()` / `write_tool_end_with_summary()` methods

**Files changed:** `src/executor/native/agent.rs`

### Phase 4: Claude CLI translation emits detail (medium, ~60 lines)

1. Change `translate_claude_event()` to return `Vec<StreamEvent>` (or add a new function `translate_claude_event_detailed()`)
2. For `assistant` events with `tool_use` content blocks, emit `ToolStart` with detail extracted via `extract_tool_detail`
3. For `tool_result` events (if present in Claude CLI output), synthesize `ToolEnd` with output summary
4. Update `translate_claude_stream()` to use the new function
5. Update callers

**Files changed:** `src/stream_event.rs`, `src/commands/spawn/execution.rs` (if Claude wrapper needs changes)

### Phase 5: Coordinator uses enriched events (small, ~15 lines)

1. Update the `ResponseEvent::ToolUse` handler in `collect_response_with_timeout()` to call `extract_tool_detail` on the input
2. Show `[Bash: $ cargo test]` instead of `[Running: Bash...]`

**Files changed:** `src/commands/service/coordinator_agent.rs`

### Phase 6: TUI migrates to stream.jsonl (medium, ~80 lines)

1. Update `update_agent_streams()` to prefer reading `stream.jsonl` over `output.log`
2. Use `ToolStart.detail` for activity display instead of Claude-CLI-specific parsing
3. Keep `output.log` as fallback for text snippets and agents that only have raw JSONL

**Files changed:** `src/tui/viz_viewer/state.rs`

### Phase 7: Unify token parsing (small, ~40 lines)

1. Add `parse_token_usage_from_stream()` that reads `stream.jsonl`
2. Update callers of `parse_token_usage()` / `parse_token_usage_live()` to try the stream-based path first
3. Keep the old functions as fallbacks for backward compatibility

**Files changed:** `src/graph.rs`

---

## 6. Interaction with Existing StreamEvent System

**This design extends, not replaces, the existing system.**

- `StreamEvent` enum gains optional fields on two existing variants — fully backward compatible
- `AgentStreamState` gains two optional fields — default to `None`, no breaking change
- `translate_claude_event()` signature change (Option → Vec) is the only breaking API change, requiring caller updates
- All existing `stream.jsonl` files remain readable (new fields default via `#[serde(default)]`)
- No new file formats or file paths introduced
- `StreamWriter` keeps existing methods, adds new convenience methods

The `extract_tool_detail()` and `summarize_tool_output()` functions are pure utility functions with no state or side effects. They can be used by any display path without coupling.

---

## 7. Summary of Changes by File

| File | Changes | Phase |
|---|---|---|
| `src/stream_event.rs` | Add fields to ToolStart/ToolEnd, add extract/summarize functions, update AgentStreamState, update translate_claude_event | 2, 4 |
| `src/executor/native/agent.rs` | Use enriched ToolStart/ToolEnd writes | 3 |
| `src/commands/service/coordinator_agent.rs` | Fix token bug (line 862), use extract_tool_detail for display | 1, 5 |
| `src/tui/viz_viewer/state.rs` | Migrate update_agent_streams to read stream.jsonl | 6 |
| `src/graph.rs` | Add parse_token_usage_from_stream, migrate callers | 7 |

Total estimated diff: ~300 lines across 5 files, in 7 incremental phases.

---

## 8. Validation Against Known Executor Output Formats

### Claude CLI JSONL format

```jsonl
{"type":"system","session_id":"abc","model":"claude-sonnet-4-20250514"}
{"type":"assistant","message":{"content":[{"type":"text","text":"Let me check..."},{"type":"tool_use","id":"tu1","name":"Bash","input":{"command":"ls -la"}}],"usage":{"input_tokens":1500,"output_tokens":200,"cache_read_input_tokens":1000}}}
{"type":"tool_result","tool_use_id":"tu1","content":"total 42\ndrwx..."}
{"type":"assistant","message":{"content":[{"type":"text","text":"I see the files."}],"usage":{"input_tokens":2000,"output_tokens":100}}}
{"type":"result","total_cost_usd":0.05,"usage":{"input_tokens":3500,"output_tokens":300,"cache_read_input_tokens":2000}}
```

**Token accumulation:** Per-turn input_tokens are per-API-call context sizes. Sum across turns = total billed tokens. The `result` event has the authoritative totals.

**Tool detail extraction:** `tool_use` blocks in `assistant` messages have `input` objects. `extract_tool_detail("Bash", {"command":"ls -la"})` → `"$ ls -la"`. Works.

### Native executor stream.jsonl format

```jsonl
{"type":"init","executor_type":"native","model":"claude-sonnet-4-20250514","timestamp_ms":1709600000000}
{"type":"turn","turn_number":1,"tools_used":["Bash"],"usage":{"input_tokens":500,"output_tokens":200},"timestamp_ms":1709600001000}
{"type":"tool_start","name":"Bash","detail":"$ cargo test","timestamp_ms":1709600001500}
{"type":"tool_end","name":"Bash","is_error":false,"duration_ms":3000,"output_summary":"exit 0, 42 lines","timestamp_ms":1709600004500}
{"type":"result","success":true,"usage":{"input_tokens":500,"output_tokens":200,"cost_usd":0.01},"timestamp_ms":1709600005000}
```

**Token accumulation:** Same per-turn semantics. Native executor already emits correct Turn events with usage.

**Tool detail:** Native executor has direct access to tool inputs — straightforward.

### Amplifier executor stream.jsonl format

```jsonl
{"type":"init","executor_type":"amplifier","timestamp_ms":1709600000000}
{"type":"result","success":true,"usage":{"input_tokens":0,"output_tokens":0},"timestamp_ms":1709600060000}
```

**Token accumulation:** No per-turn data available. Tokens are 0. This is a known limitation.

**Tool detail:** No tool events. Display shows no activity. Acceptable — the amplifier manages its own tool execution internally.

### OpenRouter (via native executor with OpenAiClient)

Uses the same native executor stream format. The `OpenAiClient` translates OpenAI-format responses to the internal `MessagesResponse` type, which includes `usage`. The agent loop emits Turn events with this usage. No special handling needed.

---

## 9. Open Questions

1. **Should `ToolStart` detail for Claude CLI be synthesized from the `assistant` event or from the separate `tool_use` event?** Claude CLI emits both — the `assistant` event has content blocks with tool_use, and sometimes a top-level `tool_use` event follows. Recommend: extract from `assistant` content blocks since they're always present.

2. **Should the TUI show ToolEnd summaries ephemerally or persist them?** Current design: `last_tool_summary` persists until the next `ToolStart`. This means a completed tool's summary stays visible until the next tool starts, which gives a natural "what just happened" display.

3. **Should `translate_claude_event` emit `ToolEnd` for `tool_result` events?** Claude CLI's `tool_result` events don't include duration, so `duration_ms` would be 0 or estimated from timestamps. Recommend: yes, emit with `duration_ms: 0` — the output summary is more valuable than the duration.
