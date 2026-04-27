use std::path::Path;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use chrono::Utc;

use workgraph::chat;
use workgraph::session_lock::{HandlerKind, SessionLock};

use crate::commands::service::DaemonLogger;
use crate::commands::service::coordinator_agent::{
    COORDINATOR_TURN_TIMEOUT_SECS, ClaudeSession, CollectedTurn, HANDLER_IDLE_POLL_MS,
    build_coordinator_context,
};

pub fn run(workgraph_dir: &Path, chat_ref: &str, model: Option<&str>) -> Result<()> {
    if chat_ref != "coordinator-0" {
        bail!(
            "claude-handler currently supports only --chat coordinator-0 (got {})",
            chat_ref
        );
    }

    let chat_dir = coordinator_chat_dir(workgraph_dir, chat_ref);
    let _session_lock = SessionLock::acquire(&chat_dir, HandlerKind::Adapter)
        .with_context(|| format!("claude-handler[{chat_ref}] failed to acquire session lock"))?;

    let logger = DaemonLogger::open(workgraph_dir)
        .context("Failed to open daemon log for claude-handler")?;
    logger.info(&format!("claude-handler[{}]: starting", chat_ref));

    let model_override = std::env::var("WG_COORDINATOR_MODEL_OVERRIDE").ok();
    let effective_model = model.or(model_override.as_deref());
    let mut session = ClaudeSession::start(workgraph_dir, effective_model, &logger)?;
    logger.info(&format!(
        "claude-handler[{}]: Claude CLI started (PID {})",
        chat_ref,
        session.pid()
    ));

    let mut last_interaction = Utc::now().to_rfc3339();

    loop {
        if let Some(status) = session
            .try_wait()
            .context("Failed to poll claude subprocess from handler")?
        {
            bail!(
                "claude-handler observed idle claude subprocess exit: {:?}",
                status
            );
        }

        let cursor = chat::read_coordinator_cursor(workgraph_dir)
            .context("Failed to read coordinator cursor in claude-handler")?;
        let new_messages = chat::read_inbox_since(workgraph_dir, cursor)
            .context("Failed to read coordinator inbox in claude-handler")?;

        if new_messages.is_empty() {
            thread::sleep(Duration::from_millis(HANDLER_IDLE_POLL_MS));
            continue;
        }

        for message in new_messages {
            process_message(
                workgraph_dir,
                &logger,
                &mut session,
                &message,
                &last_interaction,
            )?;
            last_interaction = Utc::now().to_rfc3339();
        }
    }
}

fn coordinator_chat_dir(workgraph_dir: &Path, chat_ref: &str) -> std::path::PathBuf {
    debug_assert_eq!(chat_ref, "coordinator-0");
    workgraph_dir.join("chat")
}

fn process_message(
    workgraph_dir: &Path,
    logger: &DaemonLogger,
    session: &mut ClaudeSession,
    message: &chat::ChatMessage,
    last_interaction: &str,
) -> Result<()> {
    logger.info(&format!(
        "claude-handler[coordinator-0]: processing request_id={}",
        message.request_id
    ));

    let context = match build_coordinator_context(workgraph_dir, last_interaction, None) {
        Ok(ctx) => ctx,
        Err(e) => {
            logger.warn(&format!(
                "claude-handler[coordinator-0]: failed to build coordinator context: {}",
                e
            ));
            String::new()
        }
    };

    let full_content = if context.is_empty() {
        format!("User message:\n{}", message.content)
    } else {
        format!("{}\n\n---\n\nUser message:\n{}", context, message.content)
    };

    if let Err(e) = session.send_user_turn(&full_content) {
        chat::clear_streaming(workgraph_dir);
        append_request_error_and_advance(
            workgraph_dir,
            message,
            &format!(
                "The coordinator agent crashed while accepting your message.\n\nError:\n{:#}",
                e
            ),
            logger,
        );
        return Err(e.context("claude-handler failed to send user turn"));
    }

    let collected = session.collect_response_streaming(
        logger,
        Duration::from_secs(COORDINATOR_TURN_TIMEOUT_SECS),
        workgraph_dir,
    );
    chat::clear_streaming(workgraph_dir);

    match collected {
        CollectedTurn::Response(resp) if !resp.summary.is_empty() => {
            logger.info(&format!(
                "claude-handler[coordinator-0]: got response ({} chars{}) for request_id={}",
                resp.summary.len(),
                if resp.full_text.is_some() {
                    ", with tool calls"
                } else {
                    ""
                },
                message.request_id
            ));
            chat::append_outbox_full(
                workgraph_dir,
                &resp.summary,
                resp.full_text,
                &message.request_id,
            )
            .with_context(|| {
                format!(
                    "claude-handler failed to write outbox for request_id={}",
                    message.request_id
                )
            })?;
            chat::write_coordinator_cursor(workgraph_dir, message.id).with_context(|| {
                format!(
                    "claude-handler failed to advance coordinator cursor to {}",
                    message.id
                )
            })?;
            Ok(())
        }
        CollectedTurn::Response(_) => {
            if let Some(status) = wait_for_session_exit_status(session, Duration::from_millis(500))
                .context("Failed while waiting for claude subprocess exit after empty response")?
            {
                append_request_error_and_advance(
                    workgraph_dir,
                    message,
                    &format!(
                        "The coordinator agent crashed while processing your message.\n\nProcess status: {:?}",
                        status
                    ),
                    logger,
                );
                bail!(
                    "claude-handler observed claude subprocess exit while processing {}: {:?}",
                    message.request_id,
                    status
                );
            }

            append_request_error_and_advance(
                workgraph_dir,
                message,
                "The coordinator processed your message but produced no response text.",
                logger,
            );
            Ok(())
        }
        CollectedTurn::StreamEnded => {
            logger.warn(&format!(
                "claude-handler[coordinator-0]: claude stream ended for request_id={}, exiting handler",
                message.request_id
            ));
            append_request_error_and_advance(
                workgraph_dir,
                message,
                "The coordinator agent crashed while processing your message.\n\nProcess status: stream ended",
                logger,
            );
            logger.warn(&format!(
                "claude-handler[coordinator-0]: system-error recorded for request_id={}, returning failure to supervisor",
                message.request_id
            ));
            bail!(
                "claude-handler observed stream end while processing {}",
                message.request_id
            );
        }
        CollectedTurn::Timeout => {
            let exit_status = wait_for_session_exit_status(session, Duration::from_millis(500))
                .context("Failed while waiting for claude subprocess exit after empty response")?;
            let error_text = if let Some(status) = exit_status {
                logger.warn(&format!(
                    "claude-handler[coordinator-0]: claude subprocess exited after timeout for request_id={}: {:?}",
                    message.request_id, status
                ));
                format!(
                    "The coordinator agent crashed while processing your message.\n\nProcess status: {:?}",
                    status
                )
            } else {
                "The coordinator agent timed out processing your message. It may be performing a long-running operation."
                    .to_string()
            };
            append_request_error_and_advance(workgraph_dir, message, &error_text, logger);
            if let Some(status) = exit_status {
                bail!(
                    "claude-handler observed claude subprocess exit while processing {}: {:?}",
                    message.request_id,
                    status
                );
            }
            Ok(())
        }
    }
}

fn wait_for_session_exit_status(
    session: &mut ClaudeSession,
    timeout: Duration,
) -> Result<Option<std::process::ExitStatus>> {
    let start = std::time::Instant::now();
    loop {
        if let Some(status) = session.try_wait()? {
            return Ok(Some(status));
        }
        if start.elapsed() >= timeout {
            return Ok(None);
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn append_request_error_and_advance(
    workgraph_dir: &Path,
    message: &chat::ChatMessage,
    error_text: &str,
    logger: &DaemonLogger,
) {
    logger.warn(&format!(
        "claude-handler[coordinator-0]: appending system-error and advancing cursor for request_id={}",
        message.request_id
    ));
    if let Err(e) = chat::append_error(workgraph_dir, error_text, &message.request_id) {
        logger.error(&format!(
            "claude-handler[coordinator-0]: failed to append system-error for request_id={}: {}",
            message.request_id, e
        ));
        return;
    }
    logger.warn(&format!(
        "claude-handler[coordinator-0]: system-error appended for request_id={}",
        message.request_id
    ));

    if let Err(e) = chat::write_coordinator_cursor(workgraph_dir, message.id) {
        logger.error(&format!(
            "claude-handler[coordinator-0]: failed to advance coordinator cursor after request_id={}: {}",
            message.request_id, e
        ));
        return;
    }
    logger.warn(&format!(
        "claude-handler[coordinator-0]: coordinator cursor advanced to {} for request_id={}",
        message.id, message.request_id
    ));
}
