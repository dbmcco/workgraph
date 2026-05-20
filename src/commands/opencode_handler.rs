//! `wg opencode-handler` - OpenCode CLI bridge for chat sessions.
//!
//! Dispatched by `wg spawn-task` when the resolved executor is `opencode`.
//! OpenCode is invoked one turn at a time with `opencode run`; provider
//! authentication is owned by OpenCode's own provider configuration.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result};

use workgraph::chat;
use workgraph::session_lock::{HandlerKind, SessionLock};

const INBOX_POLL: Duration = Duration::from_millis(200);

pub fn run(
    workgraph_dir: &Path,
    chat_ref: &str,
    resume: bool,
    role: Option<&str>,
    model: Option<&str>,
) -> Result<()> {
    let _ = resume;
    let chat_dir = chat::chat_dir_for_ref(workgraph_dir, chat_ref);
    std::fs::create_dir_all(&chat_dir)
        .with_context(|| format!("create chat dir {:?}", chat_dir))?;

    let mut _lock = SessionLock::acquire(&chat_dir, HandlerKind::Adapter).with_context(|| {
        format!(
            "acquire session lock for chat session {:?} - another handler is running",
            chat_ref
        )
    })?;

    let handler_log = chat_dir.join("handler.log");
    let logger = HandlerLogger::open(&handler_log)?;
    logger.info(&format!(
        "opencode-handler starting: chat_ref={}, role={:?}, model={:?}",
        chat_ref, role, model
    ));

    let system_prompt = build_handler_system_prompt(workgraph_dir, chat_ref, role);
    let coordinator_id = parse_coordinator_id(chat_ref);
    let mut inbox_cursor = last_answered_inbox_id(workgraph_dir, chat_ref);
    logger.info(&format!(
        "opencode-handler ready: inbox_cursor={}, coordinator_id={:?}",
        inbox_cursor, coordinator_id
    ));

    loop {
        let new_msgs = match chat::read_inbox_since_ref(workgraph_dir, chat_ref, inbox_cursor) {
            Ok(msgs) => msgs,
            Err(e) => {
                logger.warn(&format!("inbox read error: {}", e));
                thread::sleep(INBOX_POLL);
                continue;
            }
        };

        if new_msgs.is_empty() {
            thread::sleep(INBOX_POLL);
            continue;
        }

        for msg in new_msgs {
            inbox_cursor = msg.id.max(inbox_cursor);
            let request_id = if msg.request_id.is_empty() {
                format!("req-{}", msg.id)
            } else {
                msg.request_id.clone()
            };
            let prompt =
                assemble_turn_prompt(workgraph_dir, coordinator_id, &system_prompt, &msg.content);
            let streaming_path = chat::streaming_path_ref(workgraph_dir, chat_ref);

            let reply = match run_opencode_turn(
                &prompt,
                model,
                workgraph_dir,
                &streaming_path,
                &logger,
            ) {
                Ok(reply) => reply,
                Err(e) => {
                    logger.error(&format!("opencode turn failed: {}", e));
                    format!(
                        "The coordinator encountered an error running opencode: {}. Please retry.",
                        e
                    )
                }
            };

            if let Err(e) = chat::append_outbox_ref(workgraph_dir, chat_ref, &reply, &request_id) {
                logger.error(&format!("outbox write failed: {}", e));
            } else {
                logger.info(&format!(
                    "opencode-handler: response written ({} chars) for {}",
                    reply.len(),
                    request_id
                ));
            }
            chat::clear_streaming_ref(workgraph_dir, chat_ref);
        }
    }
}

fn parse_coordinator_id(chat_ref: &str) -> Option<u32> {
    chat_ref
        .strip_prefix("coordinator-")
        .and_then(|s| s.parse::<u32>().ok())
}

fn last_answered_inbox_id(workgraph_dir: &Path, chat_ref: &str) -> u64 {
    let inbox = match chat::read_inbox_since_ref(workgraph_dir, chat_ref, 0) {
        Ok(m) => m,
        Err(_) => return 0,
    };
    let outbox = match chat::read_outbox_since_ref(workgraph_dir, chat_ref, 0) {
        Ok(m) => m,
        Err(_) => return 0,
    };
    let answered_request_ids: std::collections::HashSet<String> =
        outbox.iter().map(|m| m.request_id.clone()).collect();
    inbox
        .iter()
        .filter(|m| answered_request_ids.contains(&m.request_id))
        .map(|m| m.id)
        .max()
        .unwrap_or(0)
}

fn build_handler_system_prompt(workgraph_dir: &Path, chat_ref: &str, role: Option<&str>) -> String {
    if chat_ref.starts_with("coordinator-") || role == Some("coordinator") {
        crate::commands::service::coordinator_agent::build_system_prompt(workgraph_dir)
    } else if let Some(r) = role {
        format!("You are acting in the role of: {}.", r)
    } else {
        String::from("You are a WG task agent.")
    }
}

fn assemble_turn_prompt(
    workgraph_dir: &Path,
    coordinator_id: Option<u32>,
    system_prompt: &str,
    latest_user_msg: &str,
) -> String {
    let mut out = String::new();
    out.push_str("# System\n");
    out.push_str(system_prompt);
    out.push_str("\n\n");

    if let Some(cid) = coordinator_id
        && let Ok(ctx) = crate::commands::service::coordinator_agent::build_coordinator_context(
            workgraph_dir,
            "1970-01-01T00:00:00Z",
            None,
            cid,
        )
        && !ctx.is_empty()
    {
        out.push_str(&ctx);
        out.push_str("\n\n");
    }

    out.push_str("# User\n");
    out.push_str(latest_user_msg);
    out
}

fn run_opencode_turn(
    prompt: &str,
    model: Option<&str>,
    workgraph_dir: &Path,
    streaming_path: &Path,
    logger: &HandlerLogger,
) -> Result<String> {
    let mut cmd = Command::new("opencode");
    cmd.arg("run");
    if let Some(m) = model {
        cmd.arg("--model").arg(opencode_model_arg(m));
    }
    cmd.arg("--dir")
        .arg(workgraph_dir.parent().unwrap_or(workgraph_dir))
        .arg(prompt)
        .current_dir(workgraph_dir.parent().unwrap_or(workgraph_dir))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());

    logger.info(&format!(
        "opencode-handler: spawning `opencode run` (model={}, cwd={:?})",
        model.unwrap_or("default"),
        workgraph_dir.parent().unwrap_or(workgraph_dir)
    ));

    let output = cmd.output().context("run opencode")?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    if !output.status.success() {
        let stderr_trimmed = stderr.trim();
        if stderr_trimmed.is_empty() {
            anyhow::bail!("opencode run exited {}", output.status);
        }
        anyhow::bail!("opencode run exited {}: {}", output.status, stderr_trimmed);
    }

    let reply = stdout.trim().to_string();
    if reply.is_empty() {
        anyhow::bail!("opencode run produced no stdout");
    }
    if let Some(parent) = streaming_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(streaming_path, format!("{}\n", reply));
    Ok(reply)
}

fn opencode_model_arg(model: &str) -> String {
    let spec = workgraph::config::parse_model_spec(model);
    match spec.provider.as_deref() {
        Some("opencode") => spec.model_id,
        Some("zai") | Some("z-ai") => format!("zai/{}", spec.model_id),
        Some(_) | None => spec.model_id,
    }
}

#[derive(Clone)]
struct HandlerLogger {
    inner: std::sync::Arc<std::sync::Mutex<HandlerLoggerInner>>,
}

struct HandlerLoggerInner {
    file: std::fs::File,
}

impl HandlerLogger {
    fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .with_context(|| format!("open handler log {:?}", path))?;
        Ok(Self {
            inner: std::sync::Arc::new(std::sync::Mutex::new(HandlerLoggerInner { file })),
        })
    }

    fn log(&self, level: &str, msg: &str) {
        let ts = chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S%.3fZ");
        let line = format!("{} [{}] {}\n", ts, level, msg);
        eprint!("{}", line);
        if let Ok(mut inner) = self.inner.lock() {
            let _ = inner.file.write_all(line.as_bytes());
            let _ = inner.file.flush();
        }
    }

    fn info(&self, msg: &str) {
        self.log("INFO", msg);
    }

    fn warn(&self, msg: &str) {
        self.log("WARN", msg);
    }

    fn error(&self, msg: &str) {
        self.log("ERROR", msg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opencode_model_arg_translates_zai_prefixes() {
        assert_eq!(opencode_model_arg("zai:glm-5.1"), "zai/glm-5.1");
        assert_eq!(opencode_model_arg("z-ai:glm-5.1"), "zai/glm-5.1");
        assert_eq!(opencode_model_arg("opencode:zai/glm-5.1"), "zai/glm-5.1");
    }
}
