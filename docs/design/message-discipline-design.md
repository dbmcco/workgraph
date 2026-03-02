# Message Discipline: Unread Messages Block Task Completion

## Problem

When an agent calls `wg done`, unread messages are silently ignored. This means:

- A user or coordinator sends an updated requirement mid-task — the agent never sees it and marks the task done with stale work.
- A sibling agent sends context ("I changed the API shape") — the agent completes without adapting.
- The coordinator sends a correction ("Don't modify file X") — the agent already modified it and marked done.

Messages are the coordination primitive between agents. If agents can complete without reading them, the message system is decorative rather than structural.

## Design

### 1. `wg done` blocks on unread messages

When `wg done <task-id>` is called, before any status transition, check for unread messages for the current agent on the current task.

**Algorithm:**

```
1. Determine agent_id from task.assigned or $WG_AGENT_ID env var
2. If agent_id is known:
   a. Call messages::poll_messages(dir, task_id, agent_id)
   b. If unread messages exist → error with message count and instructions
3. If agent_id is unknown (manual `wg done` by a human):
   a. Call messages::list_messages(dir, task_id)
   b. Compare against any known cursors — or skip the check entirely
   c. Decision: skip the check. Humans calling `wg done` manually are
      already outside the agent workflow. Blocking them helps no one.
```

**Error message when blocked:**

```
Error: Cannot mark 'my-task' as done: 3 unread messages.

Read them with:  wg msg read my-task --agent agent-1234

After reading and acting on messages, retry: wg done my-task
Use --force to bypass this check (emergency only).
```

**Where in the code:** `src/commands/done.rs`, inserted after the blocker check (line ~60 in current code) and before the status mutation (line ~138).

**Implementation detail — resolving agent_id:**

```rust
// In done.rs::run(), after loading the graph:
let agent_id: Option<String> = graph.get_task(id)
    .and_then(|t| t.assigned.clone())
    .or_else(|| std::env::var("WG_AGENT_ID").ok());

if let Some(ref agent_id) = agent_id {
    let unread = messages::poll_messages(dir, id, agent_id)?;
    if !unread.is_empty() && !force {
        let urgent_count = unread.iter()
            .filter(|m| m.priority == "urgent")
            .count();
        let urgent_note = if urgent_count > 0 {
            format!(" ({} urgent)", urgent_count)
        } else {
            String::new()
        };
        anyhow::bail!(
            "Cannot mark '{}' as done: {} unread message{}{}\n\n\
             Read them with:  wg msg read {} --agent {}\n\n\
             After reading and acting on messages, retry: wg done {}\n\
             Use --force to bypass this check (emergency only).",
            id, unread.len(),
            if unread.len() == 1 { "" } else { "s" },
            urgent_note, id, agent_id, id
        );
    }
}
```

Note: we use `poll_messages` (not `read_unread`) so that checking doesn't advance the cursor. The agent must explicitly `wg msg read` to acknowledge messages.

### 2. `--force` flag on `wg done`

Add `--force` flag to `wg done` that bypasses the message check.

```
wg done my-task --force
```

When forced:
- Log a warning: `"Task completed with N unread messages (--force)"`
- Print to stderr: `"Warning: Completing with N unread messages"`
- Still complete the task

**CLI addition** (`src/cli.rs`):

```rust
Done {
    #[arg(value_name = "TASK")]
    id: String,

    #[arg(long)]
    converged: bool,

    /// Bypass unread message check (emergency only)
    #[arg(long)]
    force: bool,
},
```

### 3. `wg fail` warns but does not block

When `wg fail <task-id>` is called with unread messages:
- Print a warning to stderr: `"Warning: Failing with N unread messages. Check wg msg read <task> --agent <agent> for context that may help debug."`
- Do NOT block — a failing agent is already in trouble, and preventing failure would leave the task stuck.

**Where in the code:** `src/commands/fail.rs`, after the status mutation and before the save, print the warning.

**Implementation:**

```rust
// In fail.rs::run(), after setting task.status = Status::Failed:
let agent_id: Option<String> = task.assigned.clone()
    .or_else(|| std::env::var("WG_AGENT_ID").ok());
if let Some(ref agent_id) = agent_id {
    if let Ok(unread) = messages::poll_messages(dir, id, agent_id) {
        if !unread.is_empty() {
            eprintln!(
                "Warning: Failing with {} unread message{}. \
                 These may contain context that explains the failure.\n\
                 Read them with: wg msg read {} --agent {}",
                unread.len(),
                if unread.len() == 1 { "" } else { "s" },
                id, agent_id
            );
        }
    }
}
```

### 4. Convenience helper: `messages::count_unread`

Add a lightweight function to `src/messages.rs` that returns the unread count without loading all message bodies:

```rust
/// Count unread messages for an agent on a task without loading bodies.
///
/// This is cheaper than poll_messages() when you only need the count.
pub fn count_unread(workgraph_dir: &Path, task_id: &str, agent_id: &str) -> Result<usize> {
    let cursor = read_cursor(workgraph_dir, agent_id, task_id)?;
    let all = list_messages(workgraph_dir, task_id)?;
    Ok(all.into_iter().filter(|m| m.id > cursor).count())
}
```

In practice this isn't much cheaper than `poll_messages` (we still parse the JSONL), but it avoids returning the full `Vec<Message>` and communicates intent clearly. The `done` command should use `poll_messages` anyway since it reports the urgent count — but `count_unread` is useful for the soft validation tip and other call sites.

### 5. Skill/quickstart update

Add message discipline instructions to the agent prompt. This goes in two places:

**a) SKILL.md — agent workflow section:**

Add after the existing `wg msg read/poll` section:

```markdown
### Message discipline

Messages are a coordination primitive — not optional notifications.

- **Check messages periodically** during long tasks: `wg msg read <task-id> --agent $WG_AGENT_ID`
- **Check messages before completing**: `wg done` will reject completion if you have unread messages
- **Act on messages**: Reading isn't enough — if a message changes requirements, adapt your work
- **Don't --force unless truly stuck**: The `--force` flag exists for emergencies, not convenience
```

**b) Prompt template — required workflow section:**

The spawned agent prompt already includes a "Messages" section telling agents to check `wg msg read`. Strengthen it:

```markdown
## Messages

Check for new messages periodically during long-running tasks:
```bash
wg msg read <task-id> --agent $WG_AGENT_ID
```
Messages may contain updated requirements, context from other agents,
or instructions from the user. Check at natural breakpoints in your work.

**IMPORTANT: `wg done` will fail if you have unread messages.**
Read and address all messages before completing your task.
```

### 6. Configuration: `message_discipline` setting

Add a config option to disable the check for projects that don't use messaging:

```toml
[agent]
# Whether wg done blocks on unread messages (default: true)
message_discipline = true
```

**Where in the code:** `src/config.rs`, add to `AgentConfig`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    // ... existing fields ...

    /// Whether wg done blocks on unread messages (default: true)
    #[serde(default = "default_true")]
    pub message_discipline: bool,
}
```

When `message_discipline = false`, `wg done` skips the unread check entirely.

**Per-task override is NOT proposed.** The task-level equivalent is `--force`, and a per-task config would add schema complexity for a rare use case. If someone needs it later, it can be added as a `task.tags` convention (e.g., tag `no-msg-check`).

### 7. Interaction with existing `wg done` flow

The current `wg done` flow:

```
1. Load graph
2. Check task exists, not already done
3. Check unresolved blockers (cycle-aware)
4. Check --converged validity
5. Mutate status to Done
6. Set completed_at, log entry
7. Extract token usage
8. Evaluate cycle iteration
9. Save graph
10. Record provenance
11. Archive agent
12. Capture task output
13. Soft validation tip
```

The message check inserts at step **3.5** — after blocker check, before converged check:

```
3. Check unresolved blockers (cycle-aware)
3.5. CHECK UNREAD MESSAGES (new)
4. Check --converged validity
5. Mutate status to Done
...
```

This ordering means:
- If blockers exist, the blocker error fires first (more fundamental)
- If blockers are clear but messages unread, the message error fires
- If both are clear, proceed to converged check and completion

### 8. Cross-executor behavior

All executors call `wg done` the same way (it's a CLI command), so the message check works identically for all:

| Executor | How agent calls done | Message check works? |
|----------|---------------------|---------------------|
| `claude` | `wg done <id>` via bash tool | Yes — agent gets error, reads messages, retries |
| `amplifier` | `wg done <id>` via shell | Yes — same mechanism |
| `shell` | `wg done <id>` in script | Yes — script sees exit code 1 |
| `bare` | `wg done <id>` in script | Yes — same mechanism |
| Manual (human) | `wg done <id>` | Skipped — no agent_id resolved |

The wrapper script (`run.sh`) calls `wg show --json` after the agent exits to check task status. If the agent failed to call `wg done` because of unread messages but then exited, the wrapper will see the task is still in-progress and mark it as failed. This is correct behavior — the agent should handle the error within its session.

### 9. Coordinator agent behavior

The coordinator agent (the one the user interacts with) should also check messages, but it doesn't call `wg done` for its own tasks in the same way. The coordinator:

1. Doesn't have "its own task" in the graph — it's the orchestrator
2. Processes messages via `wg msg read` when explicitly told to
3. Uses `wg watch` to monitor events

No special coordinator handling is needed for this feature. The coordinator benefits indirectly because the agents it spawns will now be forced to read messages before completing, which means coordinator-sent messages (requirement updates, context additions) actually get processed.

### 10. Edge cases

**Agent reads messages but doesn't act on them:**
Not enforceable at the system level. The check ensures messages are *read* (cursor advances via `wg msg read`). Whether the agent *acts* on them is a prompt instruction and evaluation concern, not a gate.

**Messages arrive between the check and the status save:**
Race condition window. The check happens, finds 0 unread. A message arrives. The task is marked done. The message is now unread on a done task. This is acceptable:
- The window is milliseconds (check → save is a few lines of code)
- The alternative (lock the message queue during completion) adds contention
- Post-completion messages already work fine (they accumulate, can be read later)

**Agent has no `assigned` field and no `$WG_AGENT_ID`:**
Skip the check. This happens when a human runs `wg done` manually. We cannot determine which messages are "unread" without knowing the reader's identity.

**Task has never received any messages:**
`poll_messages` returns empty vec → check passes instantly. No message file means no unread messages. Zero overhead for tasks that don't use messaging.

**`--force --converged` combined:**
Both flags work independently. `--force` bypasses the message check. `--converged` signals loop convergence. They can be combined.

**Agent reads messages, more arrive, agent calls done:**
The agent read messages (cursor advanced to, say, message 5). Two new messages arrive (6, 7). Agent calls `wg done`. The check finds 2 unread messages and blocks. Agent must read again. This is correct — late-arriving messages should be seen.

## Implementation plan

### Phase 1: Core done-blocking (minimal)

1. **`src/messages.rs`**: Add `count_unread()` helper function
2. **`src/cli.rs`**: Add `--force` flag to `Done` variant
3. **`src/commands/done.rs`**: Add unread message check after blocker check, before status mutation. Respect `--force` flag and `config.agent.message_discipline`.
4. **`src/main.rs`**: Pass `force` through to `done::run()`
5. **`src/config.rs`**: Add `message_discipline` field to `AgentConfig` (default true)

### Phase 2: Fail warning

6. **`src/commands/fail.rs`**: Add unread message warning (non-blocking)

### Phase 3: Prompt updates

7. **SKILL.md**: Add message discipline section to agent instructions
8. **`src/commands/spawn/execution.rs`** or prompt template: Strengthen the message-checking instructions in the agent prompt

### Phase 4: Tests

9. **`src/commands/done.rs` tests**: Test done-with-unread-blocks, done-with-force-bypasses, done-without-messages-succeeds, done-without-agent-id-skips-check
10. **`src/commands/fail.rs` tests**: Test fail-with-unread-warns (stderr output)
11. **`src/messages.rs` tests**: Test count_unread

### Estimated scope

- ~50 lines in `done.rs` (check + error formatting)
- ~15 lines in `fail.rs` (warning)
- ~10 lines in `messages.rs` (count_unread)
- ~5 lines in `cli.rs` (--force flag)
- ~5 lines in `config.rs` (message_discipline field)
- ~3 lines in `main.rs` (pass-through)
- ~100 lines in tests
- ~20 lines in SKILL.md / prompt updates

Total: ~210 lines of code changes across 7 files.
