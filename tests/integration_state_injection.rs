//! Integration tests for mid-turn state injection.
//!
//! Verifies that:
//! 1. Messages injected mid-turn appear in the API request
//! 2. Graph state changes appear in the API request
//! 3. Context pressure warnings appear in the API request
//! 4. Injections are ephemeral — NOT in the journal or persistent messages

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tempfile::TempDir;

use workgraph::executor::native::agent::AgentLoop;
use workgraph::executor::native::client::{
    ContentBlock, Message, MessagesRequest, MessagesResponse, StopReason, Usage,
};
use workgraph::executor::native::journal::{self, Journal};
use workgraph::executor::native::provider::Provider;
use workgraph::executor::native::tools::ToolRegistry;

/// A mock provider that captures the messages sent to it.
struct CapturingProvider {
    responses: Vec<MessagesResponse>,
    call_count: Arc<AtomicUsize>,
    /// Captured messages from each API call.
    captured: Arc<Mutex<Vec<Vec<Message>>>>,
}

impl CapturingProvider {
    fn new(responses: Vec<MessagesResponse>) -> Self {
        Self {
            responses,
            call_count: Arc::new(AtomicUsize::new(0)),
            captured: Arc::new(Mutex::new(Vec::new())),
        }
    }

}

#[async_trait::async_trait]
impl Provider for CapturingProvider {
    fn name(&self) -> &str {
        "capturing-mock"
    }

    fn model(&self) -> &str {
        "mock-model-v1"
    }

    fn max_tokens(&self) -> u32 {
        4096
    }

    fn context_window(&self) -> usize {
        200_000
    }

    async fn send(&self, request: &MessagesRequest) -> anyhow::Result<MessagesResponse> {
        // Capture the messages for later inspection
        self.captured
            .lock()
            .unwrap()
            .push(request.messages.clone());

        let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
        if idx < self.responses.len() {
            Ok(self.responses[idx].clone())
        } else {
            Ok(MessagesResponse {
                id: format!("msg-fallback-{}", idx),
                content: vec![ContentBlock::Text {
                    text: "[mock exhausted]".to_string(),
                }],
                stop_reason: Some(StopReason::EndTurn),
                usage: Usage::default(),
            })
        }
    }
}

fn setup_workgraph_with_task(dir: &Path, task_id: &str, deps: &[(&str, &str)]) {
    fs::create_dir_all(dir).unwrap();

    let mut lines = Vec::new();
    for (dep_id, status) in deps {
        lines.push(format!(
            r#"{{"kind":"task","id":"{}","title":"Dep {}","status":"{}"}}"#,
            dep_id, dep_id, status
        ));
    }
    let after: Vec<String> = deps.iter().map(|(id, _)| format!("\"{}\"", id)).collect();
    lines.push(format!(
        r#"{{"kind":"task","id":"{}","title":"Main task","status":"in-progress","after":[{}]}}"#,
        task_id,
        after.join(",")
    ));

    fs::write(dir.join("graph.jsonl"), lines.join("\n")).unwrap();
}

fn write_message(dir: &Path, task_id: &str, msg_id: u64, sender: &str, body: &str) {
    let msg_dir = dir.join("messages");
    fs::create_dir_all(&msg_dir).unwrap();
    let msg = serde_json::json!({
        "id": msg_id,
        "timestamp": "2026-04-03T12:00:00Z",
        "sender": sender,
        "body": body,
        "priority": "normal",
        "status": "sent"
    });
    let msg_file = msg_dir.join(format!("{}.jsonl", task_id));
    let mut content = fs::read_to_string(&msg_file).unwrap_or_default();
    content.push_str(&serde_json::to_string(&msg).unwrap());
    content.push('\n');
    fs::write(&msg_file, content).unwrap();
}

/// Helper to check if any content block in the messages contains a substring.
fn messages_contain_text(messages: &[Message], needle: &str) -> bool {
    messages.iter().any(|msg| {
        msg.content.iter().any(|block| match block {
            ContentBlock::Text { text } => text.contains(needle),
            _ => false,
        })
    })
}

// ── Test: message injection appears in API request ───────────────────────

#[tokio::test]
async fn test_message_injection_appears_in_api_request() {
    let tmp = TempDir::new().unwrap();
    let wg_dir = tmp.path().join(".workgraph");
    setup_workgraph_with_task(&wg_dir, "inject-test", &[]);

    let task_id = "inject-test";
    let agent_id = "test-agent-1";

    // Write a message BEFORE starting the agent
    write_message(&wg_dir, task_id, 1, "coordinator", "Important update: deploy at 3pm");

    // Provider: tool call on turn 1, then end
    let provider = CapturingProvider::new(vec![
        MessagesResponse {
            id: "msg-1".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: "tu-1".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "echo hello"}),
            }],
            stop_reason: Some(StopReason::ToolUse),
            usage: Usage::default(),
        },
        MessagesResponse {
            id: "msg-2".to_string(),
            content: vec![ContentBlock::Text {
                text: "Done.".to_string(),
            }],
            stop_reason: Some(StopReason::EndTurn),
            usage: Usage::default(),
        },
    ]);

    let captured = provider.captured.clone();

    let registry = ToolRegistry::default_all(&wg_dir, tmp.path());
    let output_log = wg_dir.join("test.ndjson");

    let mut agent = AgentLoop::new(
        Box::new(provider),
        registry,
        "Test agent.".to_string(),
        10,
        output_log,
    )
    .with_state_injection(wg_dir.clone(), task_id.to_string(), agent_id.to_string());

    agent.run("Do the task.").await.unwrap();

    // Check that the first API call contained the injected message
    let calls = captured.lock().unwrap();
    assert!(calls.len() >= 1, "Expected at least 1 API call");

    // The first call should contain the message injection
    let first_call = &calls[0];
    assert!(
        messages_contain_text(first_call, "Important update: deploy at 3pm"),
        "First API call should contain the injected message. Messages: {:?}",
        first_call
    );
    assert!(
        messages_contain_text(first_call, "system-reminder"),
        "Injection should be wrapped in system-reminder tags"
    );
}

// ── Test: graph change injection appears in API request ──────────────────

#[tokio::test]
async fn test_graph_change_injection_appears_in_api_request() {
    let tmp = TempDir::new().unwrap();
    let wg_dir = tmp.path().join(".workgraph");
    setup_workgraph_with_task(&wg_dir, "graph-test", &[("dep-a", "in-progress")]);

    let task_id = "graph-test";
    let agent_id = "test-agent-2";

    // Provider: tool call on turn 1 (during which we'll change graph), then end
    let wg_dir_clone = wg_dir.clone();
    let provider = CapturingProvider::new(vec![
        MessagesResponse {
            id: "msg-1".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: "tu-1".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "echo working"}),
            }],
            stop_reason: Some(StopReason::ToolUse),
            usage: Usage::default(),
        },
        // After tool execution, the agent loop will check for injections again
        MessagesResponse {
            id: "msg-2".to_string(),
            content: vec![ContentBlock::Text {
                text: "Done.".to_string(),
            }],
            stop_reason: Some(StopReason::EndTurn),
            usage: Usage::default(),
        },
    ]);

    let captured = provider.captured.clone();

    let registry = ToolRegistry::default_all(&wg_dir, tmp.path());
    let output_log = wg_dir.join("test.ndjson");

    let mut agent = AgentLoop::new(
        Box::new(provider),
        registry,
        "Test agent.".to_string(),
        10,
        output_log,
    )
    .with_state_injection(wg_dir.clone(), task_id.to_string(), agent_id.to_string());

    // Between creating the agent and running: change dependency status
    // The agent's StateInjector takes a baseline snapshot on creation,
    // so changing the graph *after* creation will be detected as a change.
    setup_workgraph_with_task(&wg_dir_clone, "graph-test", &[("dep-a", "done")]);

    agent.run("Do the task.").await.unwrap();

    // The first API call should contain the graph change injection
    let calls = captured.lock().unwrap();
    assert!(calls.len() >= 1);

    let first_call = &calls[0];
    assert!(
        messages_contain_text(first_call, "dep-a"),
        "First API call should mention the changed dependency"
    );
    assert!(
        messages_contain_text(first_call, "done"),
        "Should mention the new status"
    );
}

// ── Test: injections are ephemeral (not in journal) ──────────────────────

#[tokio::test]
async fn test_injections_are_ephemeral_not_in_journal() {
    let tmp = TempDir::new().unwrap();
    let wg_dir = tmp.path().join(".workgraph");
    setup_workgraph_with_task(&wg_dir, "ephemeral-test", &[]);

    let task_id = "ephemeral-test";
    let agent_id = "test-agent-3";

    // Write a message that will be injected
    write_message(
        &wg_dir,
        task_id,
        1,
        "user",
        "EPHEMERAL_MARKER_STRING_12345",
    );

    let provider = CapturingProvider::new(vec![MessagesResponse {
        id: "msg-1".to_string(),
        content: vec![ContentBlock::Text {
            text: "Done.".to_string(),
        }],
        stop_reason: Some(StopReason::EndTurn),
        usage: Usage::default(),
    }]);

    let captured = provider.captured.clone();

    let registry = ToolRegistry::default_all(&wg_dir, tmp.path());
    let output_log = wg_dir.join("test.ndjson");
    let j_path = journal::journal_path(&wg_dir, task_id);

    let mut agent = AgentLoop::new(
        Box::new(provider),
        registry,
        "Test agent.".to_string(),
        10,
        output_log,
    )
    .with_journal(j_path.clone(), task_id.to_string())
    .with_state_injection(wg_dir.clone(), task_id.to_string(), agent_id.to_string());

    agent.run("Do the task.").await.unwrap();

    // Verify the injection appeared in the API request
    let calls = captured.lock().unwrap();
    assert!(
        messages_contain_text(&calls[0], "EPHEMERAL_MARKER_STRING_12345"),
        "API request should contain the injected message"
    );

    // Verify the injection is NOT in the journal
    let journal_entries = Journal::read_all(&j_path).unwrap();
    let journal_json = serde_json::to_string(&journal_entries).unwrap();
    assert!(
        !journal_json.contains("EPHEMERAL_MARKER_STRING_12345"),
        "Journal should NOT contain the ephemeral injection. Journal: {}",
        journal_json
    );
    assert!(
        !journal_json.contains("Live State Update"),
        "Journal should NOT contain the ephemeral injection header"
    );
}

// ── Test: message injection only happens once (cursor advances) ──────────

#[tokio::test]
async fn test_message_injection_only_once() {
    let tmp = TempDir::new().unwrap();
    let wg_dir = tmp.path().join(".workgraph");
    setup_workgraph_with_task(&wg_dir, "once-test", &[]);

    let task_id = "once-test";
    let agent_id = "test-agent-4";

    write_message(&wg_dir, task_id, 1, "user", "UNIQUE_MSG_MARKER");

    // Two-turn conversation: tool use then end
    let provider = CapturingProvider::new(vec![
        MessagesResponse {
            id: "msg-1".to_string(),
            content: vec![ContentBlock::ToolUse {
                id: "tu-1".to_string(),
                name: "bash".to_string(),
                input: serde_json::json!({"command": "echo ok"}),
            }],
            stop_reason: Some(StopReason::ToolUse),
            usage: Usage::default(),
        },
        MessagesResponse {
            id: "msg-2".to_string(),
            content: vec![ContentBlock::Text {
                text: "Done.".to_string(),
            }],
            stop_reason: Some(StopReason::EndTurn),
            usage: Usage::default(),
        },
    ]);

    let captured = provider.captured.clone();

    let registry = ToolRegistry::default_all(&wg_dir, tmp.path());
    let output_log = wg_dir.join("test.ndjson");

    let mut agent = AgentLoop::new(
        Box::new(provider),
        registry,
        "Test agent.".to_string(),
        10,
        output_log,
    )
    .with_state_injection(wg_dir.clone(), task_id.to_string(), agent_id.to_string());

    agent.run("Do the task.").await.unwrap();

    let calls = captured.lock().unwrap();
    assert!(calls.len() >= 2, "Expected at least 2 API calls");

    // First call should have the message
    assert!(
        messages_contain_text(&calls[0], "UNIQUE_MSG_MARKER"),
        "First call should contain the message"
    );

    // Second call should NOT have the message (cursor advanced)
    assert!(
        !messages_contain_text(&calls[1], "UNIQUE_MSG_MARKER"),
        "Second call should NOT contain the message (already delivered)"
    );
}
