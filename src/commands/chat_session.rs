//! Handlers for `wg session ...` — chat-session management CLI.
//!
//! Every `wg nex` invocation — interactive CLI, coordinator,
//! task-agent — registers itself in `chat/sessions.json`. These
//! commands are the human-facing UX around that registry: list
//! sessions, attach to one (tail its outbox + `.streaming`), mint
//! new aliases, remove stale ones.

use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};

use workgraph::chat_sessions::SessionKind;

use crate::cli::{SessionAliasCommands, SessionCommands};

pub fn run(workgraph_dir: &Path, cmd: SessionCommands) -> Result<()> {
    match cmd {
        SessionCommands::List { json } => run_list(workgraph_dir, json),
        SessionCommands::Attach { session } => run_attach(workgraph_dir, &session),
        SessionCommands::New { alias, label } => run_new(workgraph_dir, &alias, label),
        SessionCommands::Fork { source, alias } => run_fork(workgraph_dir, &source, alias),
        SessionCommands::Alias { command } => match command {
            SessionAliasCommands::Add { session, alias } => {
                workgraph::chat_sessions::add_alias(workgraph_dir, &session, &alias)?;
                eprintln!(
                    "\x1b[32m[wg session]\x1b[0m added alias {:?} → {}",
                    alias, session
                );
                Ok(())
            }
            SessionAliasCommands::Rm { alias } => {
                workgraph::chat_sessions::remove_alias(workgraph_dir, &alias)?;
                eprintln!("\x1b[32m[wg session]\x1b[0m removed alias {:?}", alias);
                Ok(())
            }
        },
        SessionCommands::Rm { session } => {
            let uuid = workgraph::chat_sessions::resolve_ref(workgraph_dir, &session)?;
            workgraph::chat_sessions::delete_session(workgraph_dir, &session)?;
            eprintln!("\x1b[32m[wg session]\x1b[0m removed session {}", uuid);
            Ok(())
        }
    }
}

fn run_list(workgraph_dir: &Path, json: bool) -> Result<()> {
    let sessions = workgraph::chat_sessions::list(workgraph_dir)?;
    if json {
        let value: Vec<_> = sessions
            .iter()
            .map(|(uuid, meta)| {
                serde_json::json!({
                    "uuid": uuid,
                    "kind": format!("{:?}", meta.kind).to_lowercase(),
                    "created": meta.created,
                    "aliases": meta.aliases,
                    "label": meta.label,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }
    if sessions.is_empty() {
        eprintln!("\x1b[2m[wg session]\x1b[0m no sessions registered");
        return Ok(());
    }
    // Plain table: UUID (short), kind, aliases, label.
    println!("{:<12} {:<12} {:<40} LABEL", "UUID", "KIND", "ALIASES");
    for (uuid, meta) in sessions {
        let short = &uuid[..std::cmp::min(uuid.len(), 8)];
        let kind = format!("{:?}", meta.kind).to_lowercase();
        let aliases = if meta.aliases.is_empty() {
            "-".to_string()
        } else {
            meta.aliases.join(",")
        };
        let label = meta.label.clone().unwrap_or_default();
        println!("{:<12} {:<12} {:<40} {}", short, kind, aliases, label);
    }
    Ok(())
}

fn run_fork(workgraph_dir: &Path, source: &str, alias: Option<String>) -> Result<()> {
    let fork_uuid = workgraph::chat_sessions::fork_session(workgraph_dir, source, alias.clone())?;
    let reg = workgraph::chat_sessions::load(workgraph_dir)?;
    let meta = reg
        .sessions
        .get(&fork_uuid)
        .ok_or_else(|| anyhow::anyhow!("fork not in registry"))?;
    let handle = meta
        .aliases
        .first()
        .cloned()
        .unwrap_or_else(|| fork_uuid.clone());
    eprintln!(
        "\x1b[32m[wg session]\x1b[0m forked {} → {} (alias: {})",
        source, fork_uuid, handle
    );
    eprintln!("\x1b[2m  Resume it with: \x1b[0mwg nex --chat {}", handle);
    println!("{}", fork_uuid);
    Ok(())
}

fn run_new(workgraph_dir: &Path, alias: &str, label: Option<String>) -> Result<()> {
    let uuid = workgraph::chat_sessions::create_session(
        workgraph_dir,
        SessionKind::Other,
        &[alias.to_string()],
        label,
    )?;
    eprintln!(
        "\x1b[32m[wg session]\x1b[0m created session {} alias={:?}",
        uuid, alias
    );
    println!("{}", uuid);
    Ok(())
}

/// Tail a session's `.streaming` + `outbox.jsonl` to stderr so the
/// human can watch the session's output as it's produced.
///
/// This is read-only. Sending input to the session is a different
/// operation (`wg chat send`, or direct `wg nex --chat <ref>`).
/// Eventually a flag like `--bidir` would make this the full
/// interactive attach.
fn run_attach(workgraph_dir: &Path, session_ref: &str) -> Result<()> {
    use notify::{RecursiveMode, Watcher};
    use std::sync::mpsc::{RecvTimeoutError, channel};

    let uuid = workgraph::chat_sessions::resolve_ref(workgraph_dir, session_ref)
        .with_context(|| format!("no session matching {:?}", session_ref))?;
    eprintln!(
        "\x1b[1;32m[wg session attach]\x1b[0m {} (uuid {})",
        session_ref, uuid
    );
    let chat_dir = workgraph_dir.join("chat").join(session_ref);
    let streaming = chat_dir.join(".streaming");
    let outbox = chat_dir.join("outbox.jsonl");

    // Print whatever's already in .streaming so the user sees the
    // current in-flight turn on attach.
    if let Ok(txt) = std::fs::read_to_string(&streaming)
        && !txt.is_empty()
    {
        eprintln!("\x1b[2m[in-flight turn]\x1b[0m");
        eprint!("{}", txt);
    }

    // Tail outbox.jsonl line-by-line. We start from EOF (new turns
    // only) rather than replaying the whole history.
    let mut outbox_pos: u64 = if let Ok(meta) = std::fs::metadata(&outbox) {
        meta.len()
    } else {
        0
    };

    // Set up an inotify (or FSEvents on macOS) watcher on the chat
    // dir so we wake sub-millisecond when anything changes, instead
    // of polling at human-eyeblink granularity. A 2s timeout on the
    // recv is the safety-net floor — if an event gets dropped we
    // still re-scan within that window.
    let (tx, rx) = channel();
    let mut watcher = notify::recommended_watcher(move |res| {
        let _ = tx.send(res);
    })
    .context("create filesystem watcher for attach")?;
    watcher
        .watch(&chat_dir, RecursiveMode::NonRecursive)
        .with_context(|| format!("watch {:?}", chat_dir))?;

    eprintln!("\x1b[2m[attached — Ctrl-C to detach]\x1b[0m");
    let idle_timeout = Duration::from_secs(2);
    let mut last_streaming = String::new();
    loop {
        // Wait for a filesystem event OR the idle timeout, whichever
        // comes first. Drain any burst so we don't rerun the scan N
        // times for N coalesced events.
        match rx.recv_timeout(idle_timeout) {
            Ok(_) => while rx.try_recv().is_ok() {},
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        // Streaming: print the diff since last seen.
        if let Ok(current) = std::fs::read_to_string(&streaming)
            && current != last_streaming
        {
            if current.starts_with(&last_streaming) {
                eprint!("{}", &current[last_streaming.len()..]);
            } else {
                // Streaming got cleared (turn finished) or overwritten.
                eprintln!();
                eprint!("{}", current);
            }
            last_streaming = current;
        }
        // Outbox: read any new bytes and print each new turn.
        if let Ok(mut f) = std::fs::File::open(&outbox) {
            let len = f.metadata().ok().map(|m| m.len()).unwrap_or(0);
            if len > outbox_pos {
                let _ = f.seek(SeekFrom::Start(outbox_pos));
                let reader = BufReader::new(f);
                for line in reader.lines().map_while(Result::ok) {
                    if let Ok(msg) = serde_json::from_str::<workgraph::chat::ChatMessage>(&line) {
                        eprintln!("\x1b[1;36m↳ {}\x1b[0m {}", msg.request_id, msg.content);
                        last_streaming.clear();
                    }
                }
                outbox_pos = len;
            }
        }
    }
    Ok(())
}
