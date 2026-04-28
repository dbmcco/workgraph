# Research: Native Executor compact_messages Pattern

**Task:** research-native-executor-compact-messages-pattern  
**Date:** 2026-04-09  
**Status:** Complete

---

## Overview

The native executor implements a journal-based compaction pattern to manage context window pressure. When the conversation history grows too large, older messages are compacted into a summary, preserving the most recent context while dramatically reducing token count. The pattern is implemented across three files: `journal.rs`, `resume.rs`, and `agent.rs`.

---

## Core Components

### 1. Journal Entry Types (`journal.rs`, lines 19–94)

The journal records every conversation event as an append-only JSONL entry:

```rust
pub enum JournalEntryKind {
    Init { model, provider, system_prompt, tools, task_id },
    Message { role, content, usage, response_id, stop_reason },
    ToolExecution { tool_use_id, name, input, output, is_error, duration_ms },
    Compaction { compacted_through_seq, summary, original_message_count, original_token_count },
    End { reason, total_usage, turns },
}
```

**Key insight:** The `Compaction` variant is a **marker entry** — it records that a prior compaction happened but does NOT itself carry the summary. The summary is reconstructed by the summarizer (`summarize_messages`).

### 2. Compaction Logic (`resume.rs`, lines 184–223)

The `compact_messages` function performs journal-based compaction:

```rust
fn compact_messages(messages: Vec<Message>, _budget_tokens: usize) -> Vec<Message> {
    if messages.len() <= KEEP_RECENT_MESSAGES + 1 {
        return messages;  // Too few messages to compact
    }

    let split_point = messages.len().saturating_sub(KEEP_RECENT_MESSAGES);
    let older = &messages[..split_point];
    let summary = summarize_messages(older);  // Generate summary

    let mut compacted = Vec::with_capacity(KEEP_RECENT_MESSAGES + 1);

    // Inject summary as a USER message (API requires alternating roles)
    compacted.push(Message {
        role: Role::User,
        content: vec![ContentBlock::Text {
            text: format!(
                "[Resume: This conversation is being resumed from a journal. \
                 The first {} messages were compacted into this summary:]\n\n{}",
                split_point, summary
            ),
        }],
    });

    // Keep the recent messages verbatim
    compacted.extend_from_slice(&messages[split_point..]);

    ensure_valid_alternation(&mut compacted);
    compacted
}
```

**Constants:**
- `KEEP_RECENT_MESSAGES = 6` — number of recent message pairs to preserve verbatim
- `CHARS_PER_TOKEN = 4` — rough heuristic for token estimation

### 3. Summary Generation (`resume.rs`, lines 230–316)

`summarize_messages` is a **local, synchronous summarizer** — no LLM call. It extracts:

- **Tool calls:** `"Tools called: read_file(path/to/foo.rs), write_file(...)"`
- **Errors:** `"Tool error: <preview>"` for failed tool executions
- **Key texts:** First 4 and last 4 text messages (truncated to 200 chars)

```rust
fn summarize_messages(messages: &[Message]) -> String {
    // For each message:
    // - Text blocks: keep short ones, truncate long (>200 chars)
    // - ToolUse blocks: summarize as "name(input_summary)"
    // - ToolResult blocks: capture errors, skip successful results
    // - Thinking blocks: skip entirely (internal reasoning)
}
```

### 4. Emergency Compaction (`resume.rs`, lines 796–844)

Used during agent loop when context hits 90% capacity:

```rust
pub fn emergency_compact(messages: Vec<Message>, keep_recent: usize) -> Vec<Message> {
    // DIFFERENT from compact_messages: strips tool results, doesn't summarize
    // Replaces large tool results (>200 bytes) with "[Tool result removed. Size: N bytes. Preview: ...]"
    // Keeps recent messages verbatim
}
```

**Two compaction strategies:**

| Scenario | Trigger | Strategy | Location |
|----------|---------|----------|----------|
| `compact_messages` | Resume from journal (budget exceeded) | LLM-free summarization, keeps KEEP_RECENT verbatim | resume.rs:188 |
| `emergency_compact` | Runtime 90% threshold | Strip large tool results, keep verbatim recent | resume.rs:798 |

### 5. Context Budget Tracking (`resume.rs`, lines 714–783)

`ContextBudget` monitors context pressure with three thresholds:

```rust
pub struct ContextBudget {
    pub window_size: usize,        // From provider config
    pub warning_threshold: f64,     // 0.80 → inject warning message
    pub compact_threshold: f64,     // 0.90 → emergency compaction
    pub hard_limit: f64,            // 0.95 → clean exit
}
```

After each turn, `check_pressure()` returns `ContextPressureAction::Warning | EmergencyCompaction | CleanExit`.

### 6. Journal Replay on Resume (`resume.rs`, lines 132–161)

When resuming, `reconstruct_messages` replays the journal:

```rust
fn reconstruct_messages(entries: &[JournalEntry]) -> Vec<Message> {
    for entry in entries {
        match &entry.kind {
            JournalEntryKind::Message { role, content, .. } => {
                messages.push(Message { role, content });
            }
            JournalEntryKind::Compaction { summary, .. } => {
                // Re-inject prior compaction summary as user message
                messages.push(Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: format!(
                            "[Resume: Prior conversation was compacted. Summary of earlier work:]\n{}",
                            summary
                        ),
                    }],
                });
            }
            // Init, ToolExecution, End — skip
        }
    }
}
```

**Key insight:** Compaction entries in the journal become user messages in the reconstructed history, preserving the alternating role pattern.

### 7. Agent Loop Integration (`agent.rs`)

**Two compaction call sites:**

1. **Line 517–576:** On API `context_too_long` error — emergency compact + retry once
   ```rust
   if super::openai_client::is_context_too_long(&e) {
       messages = ContextBudget::emergency_compact(messages, 5);
       // retry with compacted messages
   }
   ```

2. **Line 1020–1041:** At 90% capacity threshold — emergency compact + journal the event
   ```rust
   ContextPressureAction::EmergencyCompaction => {
       messages = ContextBudget::emergency_compact(messages, 5);
       if let Some(ref mut j) = journal {
           j.append(JournalEntryKind::Compaction { ... });
       }
   }
   ```

---

## Pattern to Mirror

The key architectural pattern is **journal-based compaction with marker entries**:

1. **Append-only journal** records all events (including `Compaction` markers)
2. **Compaction is idempotent:** replaying the journal reconstructs the compacted state
3. **Two-tier strategy:** `compact_messages` (summarization-based) for resume, `emergency_compact` (tool-result stripping) for runtime pressure
4. **Summary is local/LLM-free:** uses heuristics rather than an LLM call
5. **API compliance:** ensures alternating user/assistant roles by injecting summaries as user messages

---

## Key File References

| File | Lines | Purpose |
|------|-------|---------|
| `src/executor/native/journal.rs` | 19–94 | `JournalEntry` and `JournalEntryKind` type definitions |
| `src/executor/native/journal.rs` | 114–179 | `Journal::open`, `Journal::append`, `Journal::read_all` |
| `src/executor/native/resume.rs` | 28 | `KEEP_RECENT_MESSAGES = 6` |
| `src/executor/native/resume.rs` | 69–125 | `load_resume_data` — loads journal, reconstructs, compacts |
| `src/executor/native/resume.rs` | 132–161 | `reconstruct_messages` — replays journal to messages |
| `src/executor/native/resume.rs` | 184–223 | `compact_messages` — summarization-based compaction |
| `src/executor/native/resume.rs` | 230–316 | `summarize_messages` — local LLM-free summarizer |
| `src/executor/native/resume.rs` | 322–351 | `ensure_valid_alternation` — fixes role sequence |
| `src/executor/native/resume.rs` | 714–783 | `ContextBudget` — threshold monitoring |
| `src/executor/native/resume.rs` | 796–844 | `emergency_compact` — runtime tool-result stripping |
| `src/executor/native/agent.rs` | 517–576 | Error-triggered emergency compaction + retry |
| `src/executor/native/agent.rs` | 1020–1041 | Threshold-triggered compaction + journal entry |

---

## Validation

- All 1536 unit tests pass (`cargo test --lib`)
- Compaction pattern has dedicated tests in `resume.rs:992–1045`
- Journal entry serialization tested in `journal.rs:621–656`
