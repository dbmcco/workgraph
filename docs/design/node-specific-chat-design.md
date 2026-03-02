# Node-Specific Chats and Per-Task Messaging in TUI

## Status: Design (March 2026)

## Overview

The TUI currently has a single Chat tab that talks to the coordinator agent via `chat/inbox.jsonl` and `chat/outbox.jsonl`. This design extends the TUI to surface **per-task message threads** — the existing `messages/{task-id}.jsonl` queues — alongside the coordinator chat, creating a contextual communication layer where selecting a task switches the chat panel to show that task's messages.

## 1. Core Concept: Contextual Chat Panel

The Chat tab in the right panel becomes **context-sensitive**:

| Selection state | Chat panel shows | Sends messages to |
|---|---|---|
| No task selected | Coordinator chat (current behavior) | Coordinator (via `chat/inbox.jsonl`) |
| Task selected | Task's message thread | Task's message queue (via `messages/{task-id}.jsonl`) |

The same physical panel, same keybindings — but the context changes based on selection.

### Why not a separate tab?

Adding a fourth "Messages" tab fragments attention. The whole point is that messaging should feel natural: you select a task and immediately see its conversation. The contextual approach means:

- No new keybinding to learn
- No mode-switching confusion
- The panel header tells you what you're looking at

## 2. UX Mockups

### 2.1 No task selected — Coordinator Chat (unchanged)

```
┌─ Chat ──────────────────────┐
│  Chat with Coordinator      │
│                             │
│  you: plan the auth system  │
│  coordinator: I'll create   │
│    tasks for authentication │
│    with the following deps… │
│                             │
│  ▸ _                        │
└─────────────────────────────┘
```

### 2.2 Task selected — Task Message Thread

```
┌─ Messages: impl-auth ───────┐
│  ── impl-auth ──────────    │
│                             │
│  [10:05] user:              │
│    Also handle empty input  │
│  [10:06] coordinator:       │
│    Forwarded to agent       │
│  [10:12] agent-a3f2:        │
│    Acknowledged, adding     │
│    empty-input edge case    │
│                             │
│  ▸ _                        │
└─────────────────────────────┘
```

### 2.3 Completed task — Read-only Message History

```
┌─ Messages: impl-auth (done) ┐
│  ── impl-auth ──────────    │
│                             │
│  [10:05] user:              │
│    Also handle empty input  │
│  [10:12] agent-a3f2:        │
│    Done. Added edge case.   │
│                             │
│  (task completed — history  │
│   retained as artifact)     │
│                             │
│  Press 'c' to leave review  │
│  feedback on this task.     │
└─────────────────────────────┘
```

### 2.4 Tab Header Indicator

The tab bar updates to show context:

```
No selection:    1:Detail │ 2:Chat   │ 3:Agents
Task selected:   1:Detail │ 2:Msgs•3 │ 3:Agents
                                 ↑ unread count
```

When a task is selected, the "Chat" label changes to "Msgs" (or "Msgs•N" if unread messages exist). This visual cue makes the contextual switch obvious.

## 3. Interaction Model

### 3.1 Selecting a Task Switches Context

When the user navigates to a different task (arrow keys, search, click):

1. The chat panel header updates to show the task ID
2. The message list loads from `messages/{task-id}.jsonl`
3. The input prompt changes from "chat>" to the task ID
4. Previous coordinator chat state is preserved (not cleared)

When the user deselects (Esc clears selection):

1. The chat panel reverts to coordinator chat
2. Coordinator chat history is still there (stored in `ChatState`)

### 3.2 Sending Messages

**To a selected task (task message thread):**
- Press `c` or `:` while a task is selected → enters chat input mode
- Type message → press Enter
- Runs `wg msg send {task-id} "{text}"` in background
- Message appears in the thread immediately (optimistic display)
- The task's assigned agent receives the message via the existing `messages/{task-id}.jsonl` → notification file pipeline

**To the coordinator (no task selected):**
- Same as current behavior — sends via `wg chat "{text}"` IPC → `chat/inbox.jsonl`

### 3.3 Key Bindings (no changes needed)

| Key | No task selected | Task selected |
|---|---|---|
| `c` or `:` | Open coordinator chat input | Open task message input |
| `Enter` (in input) | Send to coordinator | Send to task |
| `Esc` (in input) | Cancel input | Cancel input |
| `Up`/`Down` (in chat focus) | Scroll coordinator chat | Scroll task messages |
| `Alt+Up`/`Alt+Down` (in input) | Scroll while typing | Scroll while typing |

### 3.4 The 'm' Key Shortcut (Existing)

Currently `m` toggles mouse capture. The existing `TextPromptAction::SendMessage` text prompt (an overlay popup) can still be triggered — but with the contextual chat panel, the primary way to message a task is simply `c` while the task is selected. The overlay prompt becomes a fallback for quick one-off messages without switching to the chat tab.

**No change to 'm'** — it stays as mouse toggle. The contextual chat panel is the more natural UX for messaging.

## 4. Relationship to Coordinator Chat

### 4.1 Two Separate Systems, One Panel

The design keeps the two messaging systems distinct:

| Aspect | Coordinator Chat | Task Messages |
|---|---|---|
| Storage | `chat/inbox.jsonl`, `chat/outbox.jsonl` | `messages/{task-id}.jsonl` |
| Protocol | IPC `UserChat` → daemon → coordinator agent | `wg msg send` → JSONL append |
| Response model | Request-response with `request_id` | Fire-and-forget (agent polls) |
| Bidirectional? | Yes (inbox + outbox) | Yes (any sender writes to same queue) |
| Cursor model | Outbox cursor for TUI polling | Per-agent cursors in `.cursors/` |

The TUI chat panel simply renders from the appropriate source based on whether a task is selected.

### 4.2 Coordinator Visibility

**Does the coordinator see task-level messages?** Not automatically. This is intentional:

- Task messages are between the user and the task's agent (or future agents picking up the task)
- The coordinator manages the graph, not individual task conversations
- If the user wants the coordinator involved, they send a coordinator chat message like "tell impl-auth to also handle empty input"

**Exception: routing through the coordinator.** A future enhancement could add `wg msg send --via-coordinator {task-id} "{text}"` which:
1. Sends the message to the coordinator chat
2. The coordinator interprets it and forwards to the task's queue
3. Provides coordinator awareness of the instruction

This is out of scope for v1 but the architecture supports it — it's just an inbox message that the coordinator processes with additional context.

### 4.3 Cross-Agent Coordination

For agent A to message agent B's task:
```bash
wg msg send task-b "I changed the shared API — see artifact api-spec.md"
```

This already works via the existing message system. The TUI surfaces it: when viewing task-b's messages, the user sees agent-a's message in the thread. No new plumbing needed.

## 5. Storage Model

### 5.1 No New Storage — Existing Infrastructure

The design uses only existing storage mechanisms:

- **Coordinator chat**: `chat/inbox.jsonl`, `chat/outbox.jsonl` (from `src/chat.rs`)
- **Task messages**: `messages/{task-id}.jsonl` (from `src/messages.rs`)
- **Read cursors**: `messages/.cursors/{agent-id}.{task-id}` (from `src/messages.rs`)

### 5.2 TUI Cursor for Task Messages

The TUI needs to track which task messages it has displayed (to avoid re-rendering on every poll). Options:

**Option A: In-memory cursor per task (recommended)**

```rust
pub struct TaskMessageState {
    /// Task ID this state is for.
    pub task_id: String,
    /// Loaded messages for display.
    pub messages: Vec<TaskMessageEntry>,
    /// Last-seen message ID (for incremental polling).
    pub cursor: u64,
}
```

The TUI maintains this in memory. When switching tasks, it loads the full history from `messages/{task-id}.jsonl`. On poll ticks, it reads only new messages (id > cursor). State is discarded when the TUI exits — no persistent cursor file needed for the TUI viewer.

**Why not a persistent file cursor?** The TUI is a viewer, not an agent. It doesn't "consume" messages. The file-based cursors in `.cursors/` are for agents tracking what they've processed. The TUI just displays everything.

**Option B: Use a TUI-specific agent ID** (e.g., `tui-viewer`) for `.cursors/tui-viewer.{task-id}` files. Rejected because it would advance cursors and affect the agent's unread count, which would break message discipline.

### 5.3 Message Format (existing)

```rust
pub struct Message {
    pub id: u64,
    pub timestamp: String,
    pub sender: String,   // "user", "coordinator", agent-id, or task-id
    pub body: String,
    pub priority: String, // "normal" or "urgent"
}
```

The TUI renders `sender` as the role label in the thread. Color coding:

| Sender | Color | Label shown |
|---|---|---|
| `"user"` | Yellow | `you` |
| `"coordinator"` | Cyan | `coordinator` |
| Agent ID (`agent-*`) | Green | Agent ID (truncated) |
| Task ID | Magenta | Task ID |

### 5.4 Completed Tasks Retain History

**Yes.** Message files (`messages/{task-id}.jsonl`) persist after task completion. They are append-only JSONL — no cleanup on `wg done`. The TUI can display them as read-only threads on completed tasks.

This serves multiple purposes:
- **Audit trail** — what did the user tell the agent mid-task?
- **Review feedback** — post-completion messages become review notes
- **Re-assignment context** — if a task is retried, the new agent sees the full history via `format_queued_messages()`

Garbage collection: message files could be cleaned up by `wg gc` (future), but this design doesn't add automatic cleanup.

## 6. TUI Panel Behavior

### 6.1 State Changes in `ChatState` → `ChatPanelState`

Rename and extend the existing `ChatState` to handle both contexts:

```rust
/// Which context the chat panel is showing.
#[derive(Clone, PartialEq, Eq)]
pub enum ChatContext {
    /// Global coordinator chat.
    Coordinator,
    /// Per-task message thread.
    Task(String), // task_id
}

pub struct ChatPanelState {
    /// Current context.
    pub context: ChatContext,

    // ── Coordinator chat state (existing) ──
    pub coordinator_messages: Vec<ChatDisplayMessage>,
    pub coordinator_input: String,
    pub coordinator_scroll: usize,
    pub coordinator_awaiting: bool,
    pub coordinator_outbox_cursor: u64,
    pub coordinator_last_request_id: Option<String>,

    // ── Task message state ──
    pub task_messages: Vec<ChatDisplayMessage>,
    pub task_input: String,
    pub task_scroll: usize,
    pub task_cursor: u64,  // last-seen message ID for current task
}

pub struct ChatDisplayMessage {
    pub role: ChatRole,
    pub sender: String,  // raw sender string for task messages
    pub text: String,
    pub timestamp: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ChatRole {
    User,
    Coordinator,
    Agent,
    System,
}
```

### 6.2 Context Switching Logic

In `VizApp`, when selection changes (in the method that updates `selected_task_idx`):

```rust
fn update_chat_context(&mut self) {
    let new_context = match self.selected_task_id() {
        Some(task_id) => ChatContext::Task(task_id.to_string()),
        None => ChatContext::Coordinator,
    };

    if self.chat.context != new_context {
        // Save current context's input buffer
        match &self.chat.context {
            ChatContext::Coordinator => {
                // coordinator state persists in its dedicated fields
            }
            ChatContext::Task(_) => {
                // discard task input buffer on switch
                self.chat.task_input.clear();
            }
        }

        // Load new context
        match &new_context {
            ChatContext::Coordinator => {
                // coordinator state is already loaded — just switch view
            }
            ChatContext::Task(task_id) => {
                self.load_task_messages(task_id);
            }
        }

        self.chat.context = new_context;
    }
}

fn load_task_messages(&mut self, task_id: &str) {
    let messages = match workgraph::messages::list_messages(
        &self.workgraph_dir, task_id
    ) {
        Ok(msgs) => msgs,
        Err(_) => vec![],
    };

    self.chat.task_messages.clear();
    for msg in &messages {
        let role = match msg.sender.as_str() {
            "user" => ChatRole::User,
            "coordinator" => ChatRole::Coordinator,
            s if s.starts_with("agent") => ChatRole::Agent,
            _ => ChatRole::System,
        };
        self.chat.task_messages.push(ChatDisplayMessage {
            role,
            sender: msg.sender.clone(),
            text: msg.body.clone(),
            timestamp: Some(msg.timestamp.clone()),
        });
    }
    self.chat.task_cursor = messages.last().map(|m| m.id).unwrap_or(0);
    self.chat.task_scroll = 0;
}
```

### 6.3 Polling for New Messages

In the existing `poll_chat_messages()` method (called on refresh ticks), add task message polling:

```rust
pub fn poll_chat_messages(&mut self) {
    // Existing coordinator outbox polling (unchanged)
    // ...

    // NEW: poll task messages if viewing a task
    if let ChatContext::Task(ref task_id) = self.chat.context {
        let new_msgs = match workgraph::messages::list_messages(
            &self.workgraph_dir, task_id
        ) {
            Ok(msgs) => msgs.into_iter()
                .filter(|m| m.id > self.chat.task_cursor)
                .collect::<Vec<_>>(),
            Err(_) => vec![],
        };

        for msg in &new_msgs {
            let role = match msg.sender.as_str() {
                "user" => ChatRole::User,
                "coordinator" => ChatRole::Coordinator,
                s if s.starts_with("agent") => ChatRole::Agent,
                _ => ChatRole::System,
            };
            self.chat.task_messages.push(ChatDisplayMessage {
                role,
                sender: msg.sender.clone(),
                text: msg.body.clone(),
                timestamp: Some(msg.timestamp.clone()),
            });
        }

        if let Some(last) = new_msgs.last() {
            self.chat.task_cursor = last.id;
        }
    }
}
```

### 6.4 Sending Task Messages

When in task context and the user submits a message:

```rust
pub fn send_task_message(&mut self, task_id: &str, text: String) {
    // Optimistic display
    self.chat.task_messages.push(ChatDisplayMessage {
        role: ChatRole::User,
        sender: "user".to_string(),
        text: text.clone(),
        timestamp: None, // will be filled on next poll
    });
    self.chat.task_scroll = 0;

    // Send via wg msg send in background
    self.exec_command(
        vec![
            "msg".to_string(),
            "send".to_string(),
            task_id.to_string(),
            text,
        ],
        CommandEffect::Notify(format!("Message sent to '{}'", task_id)),
    );
}
```

### 6.5 Render Changes

The `draw_chat_tab()` function checks `app.chat.context`:

```rust
fn draw_chat_tab(frame: &mut Frame, app: &VizApp, area: Rect) {
    match &app.chat.context {
        ChatContext::Coordinator => draw_coordinator_chat(frame, app, area),
        ChatContext::Task(task_id) => draw_task_messages(frame, app, task_id, area),
    }
}
```

`draw_coordinator_chat()` is the existing `draw_chat_tab()` code, unchanged.

`draw_task_messages()` renders from `app.chat.task_messages` with timestamps and sender labels.

### 6.6 Tab Label Update

```rust
fn draw_tab_bar(frame: &mut Frame, active: RightPanelTab, chat_context: &ChatContext, area: Rect) {
    let chat_label = match chat_context {
        ChatContext::Coordinator => "2:Chat".to_string(),
        ChatContext::Task(id) => {
            let short_id = if id.len() > 12 { &id[..12] } else { id };
            format!("2:{}", short_id)
        }
    };
    let tab_labels = vec!["1:Detail".to_string(), chat_label, "3:Agents".to_string()];
    // ... render tabs ...
}
```

## 7. Unread Message Indicators

### 7.1 Per-Task Unread Badge

The TUI can show unread message counts in the graph visualization. When rendering a task node, if `messages/{task-id}.jsonl` has messages newer than what the TUI has displayed, show a badge:

```
  [ ] impl-auth (in_progress) 💬2
```

Or in the detail panel:

```
Messages: 5 total (2 since last view)
```

Implementation: the TUI tracks `{task_id: last_seen_id}` in a `HashMap<String, u64>` in memory. On each refresh, compare the latest message ID in each task's file against the tracked value.

### 7.2 Performance Consideration

Checking every task's message file on each refresh is expensive for large graphs. Optimize:

1. **Only check visible tasks** — tasks rendered in the current viewport
2. **Stat the file** — check mtime of `messages/{task-id}.jsonl` before parsing; skip if unchanged since last check
3. **Batch check** — on a slower interval (every 5s), scan all message files and cache unread counts

For v1, checking only the currently-selected task is sufficient. Unread badges on all visible tasks can be a follow-up.

## 8. Review Feedback on Completed Tasks

### 8.1 Leaving Review Notes

When a task is completed, the user can still send messages to it. These serve as review feedback:

```
you> The error handling looks good but the retry logic should use exponential backoff
```

This writes to `messages/{task-id}.jsonl` with sender "user". If the task is later retried, the new agent sees this feedback in `format_queued_messages()`.

### 8.2 Connecting to Evaluation

The evaluation system (if present) could read task messages tagged as review feedback. This is a natural extension but out of scope for this design — the message queue is already the right storage layer.

## 9. Implementation Plan

### Phase 1: Contextual Chat Panel (core)

**Files modified:**

| File | Changes |
|---|---|
| `src/tui/viz_viewer/state.rs` | Extend `ChatState` with `ChatContext`, task message fields, `ChatDisplayMessage` struct, context switching methods |
| `src/tui/viz_viewer/render.rs` | Split `draw_chat_tab` into coordinator vs task rendering, update tab label, add timestamp display for task messages |
| `src/tui/viz_viewer/event.rs` | Update `handle_chat_input` to dispatch to either coordinator or task message send based on context |

**Estimated scope:** ~200 lines changed across 3 files.

### Phase 2: Polling and Live Updates

**Files modified:**

| File | Changes |
|---|---|
| `src/tui/viz_viewer/state.rs` | Add `poll_task_messages()`, integrate into `maybe_refresh()` cycle |

**Estimated scope:** ~40 lines.

### Phase 3: Unread Indicators

**Files modified:**

| File | Changes |
|---|---|
| `src/tui/viz_viewer/state.rs` | Add `unread_counts: HashMap<String, u64>`, check on refresh |
| `src/tui/viz_viewer/render.rs` | Show unread badge on tab label and optionally in graph |

**Estimated scope:** ~60 lines.

### Phase 4: Review Feedback on Completed Tasks

**Files modified:**

| File | Changes |
|---|---|
| `src/tui/viz_viewer/render.rs` | Show "leave review" hint on completed task message view |
| `src/tui/viz_viewer/event.rs` | Allow message sending to completed tasks (no change needed — already supported by `wg msg send`) |

**Estimated scope:** ~20 lines.

## 10. Design Decisions

| Decision | Choice | Rationale |
|---|---|---|
| Same panel vs new tab | Same panel, contextual | Reduces cognitive load; messaging is tied to what you're looking at |
| Tab label when task selected | Show task ID | Clear indication of context; prevents confusion about where messages go |
| Store TUI cursor for task messages | In-memory only | TUI is a viewer, not a consumer; persisting would break agent cursor semantics |
| Coordinator sees task messages? | No (by default) | Separation of concerns; coordinator manages graph, not task conversations |
| Review feedback mechanism | Same message queue | No new storage; messages on done tasks naturally become review notes |
| Unread badge scope | Selected task only (v1) | Performance; checking all tasks is expensive; follow-up for visible-task badges |
| 'm' key rebinding | No | Keep mouse toggle; contextual `c` in chat panel is the primary messaging UX |

## 11. Relationship to Existing Work

| Component | How this design uses it |
|---|---|
| `src/messages.rs` | Reads `list_messages()` for task threads; sends via `send_message()` |
| `src/chat.rs` | Unchanged — coordinator chat continues to use inbox/outbox |
| `src/commands/msg.rs` | `wg msg send` used for task message dispatch from TUI |
| `src/commands/chat.rs` | `wg chat` used for coordinator messages from TUI (unchanged) |
| TUI Chat tab (`render.rs`, `state.rs`, `event.rs`) | Extended with context switching and dual rendering |
| Message discipline (`design-message-discipline`) | Compatible — messages sent via TUI appear in agent's unread queue, blocking `wg done` |
| Coordinator chat protocol (`coordinator-chat-protocol`) | Unchanged — coordinator chat path is orthogonal to task messages |

## 12. Future Extensions (Out of Scope)

- **Threaded replies** within a task's message queue (would need a `reply_to` field on `Message`)
- **Message types** (instruction, review, question) for structured interaction
- **Coordinator routing** — `wg msg send --via-coordinator` for coordinator-aware task messaging
- **Agent-to-agent** — dedicated channels between agents (currently agents message each other's tasks)
- **Message search** — search across all task message queues from the TUI
- **Notification overlay** — popup when a message arrives on a non-selected task
