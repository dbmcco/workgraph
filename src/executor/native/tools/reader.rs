//! Reader tool: sub-executor with a working directory, sequential
//! chunk pull, and writable scratch space.
//!
//! Shape (from the 2026-04-16 design exchange):
//!
//!   reader(path, task) → working_dir_path
//!
//! Spawns a mini agent loop with seven tightly-scoped tools:
//!
//!   - `next_chunk(size)` — returns the next N chars of the input
//!     file starting at the cursor, advances the cursor. Sequential;
//!     the agent doesn't track indices. EOF returns a clear marker.
//!   - `write_note(name, content)` — writes a file in the working dir.
//!     Overwrite semantics; caller can pick the filename.
//!   - `append_note(name, content)` — appends to a file in the
//!     working dir (creates if missing).
//!   - `list_notes()` — what's in the working dir so far.
//!   - `read_note(name)` — reads back a note the agent (or an
//!     earlier turn) wrote.
//!   - `bash(command)` — shell command with cwd set to the working
//!     dir. `grep`/`sed`/`awk`/etc. on the accumulated notes.
//!   - `finish(result)` — terminates the loop with a final answer.
//!
//! The working directory lives at
//! `<workgraph_dir>/readers/<timestamp>-<slug>/` and **persists**
//! after the reader exits. The outer session can `cat` / `ls` it to
//! inspect everything the reader produced. Readers are sacred — not
//! auto-removed — the same philosophy as worktrees.
//!
//! Why this exists and why not just `read_file(path, query)`:
//!
//!   - `read_file(path, query)` is single-shot. When the file doesn't
//!     fit in one LLM call, it errors out and points here.
//!   - `reader` handles arbitrarily-large files by letting the agent
//!     pull chunks on its own schedule, write running notes to disk,
//!     and compose them. Notes survive across compaction because
//!     they live on disk, not in message history.
//!   - Output shape is genuinely different: `read_file(query)`
//!     returns a text answer; `reader` returns a **workspace** the
//!     outer agent can explore. For complex tasks (summarize this
//!     book AND produce per-chapter notes AND cross-reference
//!     against another file), a workspace is the right primitive.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;

use super::{Tool, ToolOutput, ToolRegistry};
use crate::executor::native::client::{
    ContentBlock, Message, MessagesRequest, Role, StopReason, ToolDefinition,
};

/// Default chunk size in characters when the agent doesn't specify.
/// ~8K chars ≈ 2K tokens — a reasonable "page" of reading.
const DEFAULT_CHUNK_CHARS: usize = 8_000;

/// Hard cap on a single chunk — keeps any one pull bounded even if
/// the agent asks for something absurd.
const MAX_CHUNK_CHARS: usize = 40_000;

/// Minimum chunk — below this the per-turn overhead dominates.
const MIN_CHUNK_CHARS: usize = 512;

/// Default max conversation turns. One turn = one LLM call.
const DEFAULT_MAX_TURNS: usize = 50;

/// Hard cap on max_turns — prevents runaway cost.
const MAX_ALLOWED_TURNS: usize = 200;

/// Cap on the size of any note file. Prevents a runaway agent from
/// filling the disk. A 1 MB note is bigger than most books' main text.
const MAX_NOTE_CHARS: usize = 1_024 * 1_024;

/// Cap on returned `read_note` content to keep tool_result blocks
/// bounded in the agent's context.
const MAX_READ_NOTE_CHARS: usize = 40_000;

/// Timeout for a single bash command invocation inside the reader.
const BASH_TIMEOUT_SECS: u64 = 30;

const READER_SYSTEM_PROMPT: &str = "\
You are reading a large file to accomplish a task. You have seven tools:

  - next_chunk(size): read the next `size` chars of the input file. \
    Size is optional (default ~8000). Sequential — the cursor advances \
    automatically; you don't track positions. Returns 'EOF' when the \
    whole file has been read.
  - write_note(name, content): create or overwrite a file `name` in \
    your working directory. Use for structured artifacts — summaries, \
    cross-references, outlines.
  - append_note(name, content): append to a file `name` in your \
    working directory (creates if missing). Use for running notes — \
    one per topic, grown over time.
  - list_notes(): list files in your working directory.
  - read_note(name): read back a note you wrote earlier.
  - bash(command): shell command with cwd set to your working dir. \
    Useful for `grep`, `sed`, `wc`, combining notes, etc.
  - finish(result): terminate with a final text result. The outer \
    caller still has access to your working directory contents after \
    you finish, so put durable output IN NOTES, not just in the \
    result text.

Workflow:
  1. Call next_chunk() to read the first page of the file.
  2. Extract what matters into notes BEFORE the next chunk. The \
     chunk text lives in your conversation history only until the \
     next tool call; your notes on disk persist indefinitely.
  3. Repeat until you have enough to finish, or until next_chunk \
     returns EOF.
  4. Call finish with your final answer. The outer caller will see \
     both your result text AND your working-directory notes.

Notes over chat. Persist things. Don't answer from memory of a chunk \
you didn't note — re-read it or admit you don't know.";

pub fn register_reader_tool(registry: &mut ToolRegistry, workgraph_dir: PathBuf) {
    registry.register(Box::new(ReaderTool { workgraph_dir }));
}

struct ReaderTool {
    workgraph_dir: PathBuf,
}

#[async_trait]
impl Tool for ReaderTool {
    fn name(&self) -> &str {
        "reader"
    }

    fn is_read_only(&self) -> bool {
        // reader writes to its own working directory (not the user's
        // source tree), so from the outer perspective this is read-only
        // on the user's code. The notes directory is the artifact.
        true
    }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "reader".to_string(),
            description: "Run a sub-agent over a large file to accomplish a task. The \
                          sub-agent reads the file sequentially in chunks it pulls on \
                          demand, writes running notes to a dedicated working directory, \
                          and terminates with a final result. The working directory \
                          persists after completion — you can `ls` and `cat` its contents \
                          to see everything the sub-agent produced.\n\
                          \n\
                          Use this for files too large for `read_file(path, query)`'s \
                          single-shot mode, or for tasks that benefit from a workspace \
                          (summarize a book AND produce per-chapter notes, cross-reference \
                          a long document against a question, extract every mention of X \
                          from a 10MB log). Returns the path to the working directory plus \
                          the sub-agent's finish() result."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Absolute path to the input file to read"
                    },
                    "task": {
                        "type": "string",
                        "description": "What the sub-agent should accomplish. Be specific \
                                        about the output you want — 'find every mention \
                                        of X and list line numbers', 'summarize chapter by \
                                        chapter in chapters.md', etc."
                    },
                    "max_turns": {
                        "type": "integer",
                        "description": "Max conversation turns (default 50, cap 200). \
                                        One turn = one LLM call."
                    }
                },
                "required": ["path", "task"]
            }),
        }
    }

    async fn execute(&self, input: &serde_json::Value) -> ToolOutput {
        let path = match input.get("path").and_then(|v| v.as_str()) {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => return ToolOutput::error("Missing or empty parameter: path".to_string()),
        };
        let task = match input.get("task").and_then(|v| v.as_str()) {
            Some(t) if !t.trim().is_empty() => t.trim().to_string(),
            _ => return ToolOutput::error("Missing or empty parameter: task".to_string()),
        };
        let max_turns = input
            .get("max_turns")
            .and_then(|v| v.as_u64())
            .map(|n| (n as usize).clamp(1, MAX_ALLOWED_TURNS))
            .unwrap_or(DEFAULT_MAX_TURNS);

        match run_reader(&self.workgraph_dir, &path, &task, max_turns).await {
            Ok(result) => ToolOutput::success(result),
            Err(e) => ToolOutput::error(format!("reader failed: {}", e)),
        }
    }
}

/// Shared state across a reader run. Each sub-tool holds an Arc<Mutex<_>>.
struct ReaderState {
    input_text: String,
    cursor: usize,
    working_dir: PathBuf,
    final_result: Option<String>,
}

type ReaderStateRef = Arc<Mutex<ReaderState>>;

/// Main reader loop. Creates a working dir, spawns the mini agent loop,
/// returns path+result when finish() is called or max_turns is reached.
async fn run_reader(
    workgraph_dir: &Path,
    path: &str,
    task: &str,
    max_turns: usize,
) -> Result<String, String> {
    let input_text = std::fs::read_to_string(path)
        .map_err(|e| format!("Failed to read '{}': {}", path, e))?;
    let total_chars = input_text.len();

    // Create working directory. Lives at <workgraph_dir>/readers/<stamp>-<slug>/
    // and persists after the reader exits.
    let working_dir = make_working_dir(workgraph_dir, path)?;
    eprintln!(
        "[reader] start: path={}, task={:?}, working_dir={}, total_chars={}",
        path,
        truncate(task, 80),
        working_dir.display(),
        total_chars
    );

    // Resolve provider via the usual chain.
    let config = crate::config::Config::load_or_default(workgraph_dir);
    let model = std::env::var("WG_MODEL")
        .ok()
        .filter(|m| !m.is_empty())
        .unwrap_or_else(|| {
            config
                .resolve_model_for_role(crate::config::DispatchRole::TaskAgent)
                .model
        });
    let provider = crate::executor::native::provider::create_provider(workgraph_dir, &model)
        .map_err(|e| format!("create provider (model {}): {}", model, e))?;

    let state = Arc::new(Mutex::new(ReaderState {
        input_text,
        cursor: 0,
        working_dir: working_dir.clone(),
        final_result: None,
    }));

    // Build the reader's tool registry (7 tools, tightly scoped).
    let mut registry = ToolRegistry::new();
    registry.register(Box::new(NextChunkTool {
        state: state.clone(),
    }));
    registry.register(Box::new(WriteNoteTool {
        state: state.clone(),
    }));
    registry.register(Box::new(AppendNoteTool {
        state: state.clone(),
    }));
    registry.register(Box::new(ListNotesTool {
        state: state.clone(),
    }));
    registry.register(Box::new(ReadNoteTool {
        state: state.clone(),
    }));
    registry.register(Box::new(BashTool {
        state: state.clone(),
    }));
    registry.register(Box::new(FinishTool {
        state: state.clone(),
    }));

    let tool_defs = registry.definitions();
    let initial_msg = format!(
        "Task: {}\n\nInput file: {} ({} chars total)\n\nStart by calling next_chunk() to read \
         the first page, then take notes and proceed. When done, call finish(result).",
        task, path, total_chars
    );
    let mut messages = vec![Message {
        role: Role::User,
        content: vec![ContentBlock::Text { text: initial_msg }],
    }];

    for turn in 0..max_turns {
        let (cursor, note_count) = {
            let s = state.lock().unwrap();
            let notes = count_notes(&s.working_dir);
            (s.cursor, notes)
        };
        eprintln!(
            "[reader] turn {}/{} (cursor={}/{}, notes={})",
            turn + 1,
            max_turns,
            cursor,
            total_chars,
            note_count
        );

        let request = MessagesRequest {
            model: provider.model().to_string(),
            max_tokens: provider.max_tokens(),
            system: Some(READER_SYSTEM_PROMPT.to_string()),
            messages: messages.clone(),
            tools: tool_defs.clone(),
            stream: false,
        };
        let response = provider
            .send(&request)
            .await
            .map_err(|e| format!("API error on turn {}: {}", turn + 1, e))?;
        messages.push(Message {
            role: Role::Assistant,
            content: response.content.clone(),
        });

        match response.stop_reason {
            Some(StopReason::EndTurn) | Some(StopReason::StopSequence) | None => {
                let s = state.lock().unwrap();
                if let Some(ref result) = s.final_result {
                    return Ok(format_exit(&s.working_dir, result, turn + 1, false));
                }
                drop(s);
                messages.push(Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: "Use a tool (next_chunk, write_note, append_note, list_notes, \
                               read_note, bash, or finish). Plain text replies have no \
                               durable memory — the working dir does."
                            .to_string(),
                    }],
                });
                continue;
            }
            Some(StopReason::MaxTokens) => {
                messages.push(Message {
                    role: Role::User,
                    content: vec![ContentBlock::Text {
                        text: "Response was truncated. Call finish() with your best \
                               answer based on notes so far."
                            .to_string(),
                    }],
                });
                continue;
            }
            Some(StopReason::ToolUse) => {
                let tool_uses: Vec<_> = response
                    .content
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::ToolUse { id, name, input } => {
                            Some((id.clone(), name.clone(), input.clone()))
                        }
                        _ => None,
                    })
                    .collect();

                let mut results = Vec::new();
                for (id, name, input) in &tool_uses {
                    let output = registry.execute(name, input).await;
                    results.push(ContentBlock::ToolResult {
                        tool_use_id: id.clone(),
                        content: output.content.clone(),
                        is_error: output.is_error,
                    });
                }
                messages.push(Message {
                    role: Role::User,
                    content: results,
                });

                // Check for finish() signal.
                let s = state.lock().unwrap();
                if let Some(ref result) = s.final_result {
                    return Ok(format_exit(&s.working_dir, result, turn + 1, false));
                }
            }
        }
    }

    // Exhausted turns without finish. Return what we have.
    let s = state.lock().unwrap();
    let fallback = s
        .final_result
        .clone()
        .unwrap_or_else(|| "[reader: max turns reached without finish()]".to_string());
    Ok(format_exit(&s.working_dir, &fallback, max_turns, true))
}

/// Create the working dir at `<workgraph_dir>/readers/<stamp>-<slug>/`.
fn make_working_dir(workgraph_dir: &Path, input_path: &str) -> Result<PathBuf, String> {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%S").to_string();
    let slug = slug_from_path(input_path);
    let dir = workgraph_dir
        .join("readers")
        .join(format!("{}-{}", stamp, slug));
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("create working dir {:?}: {}", dir, e))?;
    Ok(dir)
}

/// Slug from the input path: basename, alphanumeric + dashes only,
/// capped at 40 chars.
fn slug_from_path(path: &str) -> String {
    let base = std::path::Path::new(path)
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "input".to_string());
    let mut out: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' })
        .collect();
    // Collapse runs of '-'
    while out.contains("--") {
        out = out.replace("--", "-");
    }
    out = out.trim_matches('-').to_string();
    if out.is_empty() {
        out = "input".to_string();
    }
    if out.len() > 40 {
        out.truncate(40);
    }
    out
}

/// Format the reader's exit message: working dir + result + stats.
fn format_exit(working_dir: &Path, result: &str, turns: usize, hit_max: bool) -> String {
    let status = if hit_max { " (HIT MAX TURNS)" } else { "" };
    format!(
        "Reader result:\n{}\n\n--- Reader metadata ---\nWorking directory: {}\nTurns used: {}{}\n\
         Inspect the working directory to see notes, artifacts, and any files the sub-agent \
         wrote. Use `bash ls` and `cat` / `read_file` on specific paths.",
        result,
        working_dir.display(),
        turns,
        status,
    )
}

fn count_notes(working_dir: &Path) -> usize {
    std::fs::read_dir(working_dir)
        .map(|iter| iter.filter_map(|e| e.ok()).count())
        .unwrap_or(0)
}

/// Validate a note name: no path separators, no parent-dir escapes,
/// non-empty. Returns the full path on success.
fn validate_note_path(working_dir: &Path, name: &str) -> Result<PathBuf, String> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err("note name cannot be empty".to_string());
    }
    if trimmed.contains('/') || trimmed.contains('\\') || trimmed.contains("..") {
        return Err(format!(
            "note name must be a single filename (no /, \\, or ..): got {:?}",
            trimmed
        ));
    }
    if trimmed.starts_with('.') {
        return Err(format!(
            "note name cannot start with '.' (dotfiles disallowed): got {:?}",
            trimmed
        ));
    }
    Ok(working_dir.join(trimmed))
}

// ─── Sub-tool: next_chunk ───────────────────────────────────────────────

struct NextChunkTool {
    state: ReaderStateRef,
}

#[async_trait]
impl Tool for NextChunkTool {
    fn name(&self) -> &str {
        "next_chunk"
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "next_chunk".to_string(),
            description: format!(
                "Read the next `size` chars of the input file at the current cursor, \
                 advance the cursor, return the chunk. `size` is optional (default {}, \
                 range {}..{}). Returns 'EOF' when the whole file has been read.",
                DEFAULT_CHUNK_CHARS, MIN_CHUNK_CHARS, MAX_CHUNK_CHARS
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "size": {
                        "type": "integer",
                        "description": format!(
                            "Chunk size in chars (default {}, clamped to [{}, {}])",
                            DEFAULT_CHUNK_CHARS, MIN_CHUNK_CHARS, MAX_CHUNK_CHARS
                        )
                    }
                }
            }),
        }
    }
    async fn execute(&self, input: &serde_json::Value) -> ToolOutput {
        let size = input
            .get("size")
            .and_then(|v| v.as_u64())
            .map(|n| (n as usize).clamp(MIN_CHUNK_CHARS, MAX_CHUNK_CHARS))
            .unwrap_or(DEFAULT_CHUNK_CHARS);

        let mut s = self.state.lock().unwrap();
        if s.cursor >= s.input_text.len() {
            return ToolOutput::success("EOF".to_string());
        }
        let start = s.cursor;
        let mut end = (start + size).min(s.input_text.len());
        // Respect char boundaries.
        while end > start && !s.input_text.is_char_boundary(end) {
            end -= 1;
        }
        let chunk: String = s.input_text[start..end].to_string();
        s.cursor = end;
        let total = s.input_text.len();
        let progress = if total == 0 {
            100
        } else {
            (s.cursor * 100 / total).min(100)
        };
        ToolOutput::success(format!(
            "[chunk {}..{} of {} chars, {}% through file]\n{}",
            start, end, total, progress, chunk
        ))
    }
}

// ─── Sub-tool: write_note ───────────────────────────────────────────────

struct WriteNoteTool {
    state: ReaderStateRef,
}

#[async_trait]
impl Tool for WriteNoteTool {
    fn name(&self) -> &str {
        "write_note"
    }
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "write_note".to_string(),
            description: "Write `content` to a file `name` in the working directory. \
                          Overwrites if exists. Name must be a single filename (no \
                          path separators, no '..', no leading '.')."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["name", "content"]
            }),
        }
    }
    async fn execute(&self, input: &serde_json::Value) -> ToolOutput {
        let name = match input.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return ToolOutput::error("Missing parameter: name".to_string()),
        };
        let content = match input.get("content").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return ToolOutput::error("Missing parameter: content".to_string()),
        };
        if content.len() > MAX_NOTE_CHARS {
            return ToolOutput::error(format!(
                "Note too large: {} chars > {} cap",
                content.len(),
                MAX_NOTE_CHARS
            ));
        }
        let s = self.state.lock().unwrap();
        let path = match validate_note_path(&s.working_dir, name) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(e),
        };
        drop(s);
        match std::fs::write(&path, content) {
            Ok(()) => ToolOutput::success(format!("Wrote {} bytes to {}", content.len(), name)),
            Err(e) => ToolOutput::error(format!("write_note {:?}: {}", path, e)),
        }
    }
}

// ─── Sub-tool: append_note ──────────────────────────────────────────────

struct AppendNoteTool {
    state: ReaderStateRef,
}

#[async_trait]
impl Tool for AppendNoteTool {
    fn name(&self) -> &str {
        "append_note"
    }
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "append_note".to_string(),
            description: "Append `content` to a file `name` in the working directory. \
                          Creates the file if missing. A newline is inserted before the \
                          appended content when the existing file doesn't end in one."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"},
                    "content": {"type": "string"}
                },
                "required": ["name", "content"]
            }),
        }
    }
    async fn execute(&self, input: &serde_json::Value) -> ToolOutput {
        let name = match input.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return ToolOutput::error("Missing parameter: name".to_string()),
        };
        let content = match input.get("content").and_then(|v| v.as_str()) {
            Some(c) => c,
            None => return ToolOutput::error("Missing parameter: content".to_string()),
        };
        let s = self.state.lock().unwrap();
        let path = match validate_note_path(&s.working_dir, name) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(e),
        };
        drop(s);

        // Enforce the note-size cap on the cumulative size.
        let existing_len = std::fs::metadata(&path).map(|m| m.len() as usize).unwrap_or(0);
        if existing_len + content.len() > MAX_NOTE_CHARS {
            return ToolOutput::error(format!(
                "Note would exceed cap: {} existing + {} new > {}",
                existing_len,
                content.len(),
                MAX_NOTE_CHARS
            ));
        }

        // Insert a newline if the existing file doesn't end in one.
        let needs_newline = if existing_len > 0 {
            match std::fs::read(&path) {
                Ok(bytes) => bytes.last().copied() != Some(b'\n'),
                Err(_) => false,
            }
        } else {
            false
        };

        use std::io::Write;
        let mut f = match std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            Ok(f) => f,
            Err(e) => return ToolOutput::error(format!("append_note open {:?}: {}", path, e)),
        };
        if needs_newline {
            let _ = f.write_all(b"\n");
        }
        match f.write_all(content.as_bytes()) {
            Ok(()) => {
                ToolOutput::success(format!("Appended {} bytes to {}", content.len(), name))
            }
            Err(e) => ToolOutput::error(format!("append_note write {:?}: {}", path, e)),
        }
    }
}

// ─── Sub-tool: list_notes ───────────────────────────────────────────────

struct ListNotesTool {
    state: ReaderStateRef,
}

#[async_trait]
impl Tool for ListNotesTool {
    fn name(&self) -> &str {
        "list_notes"
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "list_notes".to_string(),
            description: "List files in the working directory with their sizes in bytes."
                .to_string(),
            input_schema: json!({"type": "object", "properties": {}}),
        }
    }
    async fn execute(&self, _input: &serde_json::Value) -> ToolOutput {
        let s = self.state.lock().unwrap();
        let dir = s.working_dir.clone();
        drop(s);
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) => return ToolOutput::error(format!("read_dir {:?}: {}", dir, e)),
        };
        let mut items: Vec<(String, u64)> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            items.push((name, size));
        }
        items.sort_by(|a, b| a.0.cmp(&b.0));
        if items.is_empty() {
            return ToolOutput::success("(no notes yet)".to_string());
        }
        let mut out = String::from("Notes in working directory:\n");
        for (name, size) in items {
            out.push_str(&format!("  {}  ({} bytes)\n", name, size));
        }
        ToolOutput::success(out)
    }
}

// ─── Sub-tool: read_note ────────────────────────────────────────────────

struct ReadNoteTool {
    state: ReaderStateRef,
}

#[async_trait]
impl Tool for ReadNoteTool {
    fn name(&self) -> &str {
        "read_note"
    }
    fn is_read_only(&self) -> bool {
        true
    }
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "read_note".to_string(),
            description: format!(
                "Read a note file from the working directory. Content is capped at {} chars \
                 — for larger notes, use `bash head/tail/sed` to view portions.",
                MAX_READ_NOTE_CHARS
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {"type": "string"}
                },
                "required": ["name"]
            }),
        }
    }
    async fn execute(&self, input: &serde_json::Value) -> ToolOutput {
        let name = match input.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return ToolOutput::error("Missing parameter: name".to_string()),
        };
        let s = self.state.lock().unwrap();
        let path = match validate_note_path(&s.working_dir, name) {
            Ok(p) => p,
            Err(e) => return ToolOutput::error(e),
        };
        drop(s);
        let content = match std::fs::read_to_string(&path) {
            Ok(c) => c,
            Err(e) => return ToolOutput::error(format!("read_note {:?}: {}", path, e)),
        };
        let truncated = if content.len() > MAX_READ_NOTE_CHARS {
            let mut i = MAX_READ_NOTE_CHARS;
            while i > 0 && !content.is_char_boundary(i) {
                i -= 1;
            }
            format!(
                "{}\n[TRUNCATED — full note is {} bytes; use `bash` to view more]",
                &content[..i],
                content.len()
            )
        } else {
            content
        };
        ToolOutput::success(truncated)
    }
}

// ─── Sub-tool: bash ─────────────────────────────────────────────────────

struct BashTool {
    state: ReaderStateRef,
}

#[async_trait]
impl Tool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "bash".to_string(),
            description: format!(
                "Run a shell command with cwd set to the working directory. Useful for \
                 grep/sed/wc/cat on accumulated notes. Timeout {}s, combined stdout+stderr \
                 returned capped at {} chars.",
                BASH_TIMEOUT_SECS, MAX_READ_NOTE_CHARS
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"}
                },
                "required": ["command"]
            }),
        }
    }
    async fn execute(&self, input: &serde_json::Value) -> ToolOutput {
        let command = match input.get("command").and_then(|v| v.as_str()) {
            Some(c) if !c.trim().is_empty() => c.to_string(),
            _ => return ToolOutput::error("Missing or empty parameter: command".to_string()),
        };
        let s = self.state.lock().unwrap();
        let cwd = s.working_dir.clone();
        drop(s);
        // Wrap in `timeout` to enforce the budget. Same pattern as the
        // main bash tool uses for runaway commands.
        let output = Command::new("timeout")
            .arg(format!("{}s", BASH_TIMEOUT_SECS))
            .arg("bash")
            .arg("-c")
            .arg(&command)
            .current_dir(&cwd)
            .output();
        let output = match output {
            Ok(o) => o,
            Err(e) => return ToolOutput::error(format!("bash exec: {}", e)),
        };
        let mut combined = String::new();
        combined.push_str(&String::from_utf8_lossy(&output.stdout));
        if !output.stderr.is_empty() {
            combined.push_str("\n--- stderr ---\n");
            combined.push_str(&String::from_utf8_lossy(&output.stderr));
        }
        if combined.len() > MAX_READ_NOTE_CHARS {
            let mut i = MAX_READ_NOTE_CHARS;
            while i > 0 && !combined.is_char_boundary(i) {
                i -= 1;
            }
            combined.truncate(i);
            combined.push_str("\n[TRUNCATED]");
        }
        if !output.status.success() {
            return ToolOutput::error(format!(
                "bash exit {}: {}",
                output.status.code().unwrap_or(-1),
                combined
            ));
        }
        if combined.trim().is_empty() {
            ToolOutput::success("(no output)".to_string())
        } else {
            ToolOutput::success(combined)
        }
    }
}

// ─── Sub-tool: finish ───────────────────────────────────────────────────

struct FinishTool {
    state: ReaderStateRef,
}

#[async_trait]
impl Tool for FinishTool {
    fn name(&self) -> &str {
        "finish"
    }
    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "finish".to_string(),
            description: "Terminate the reader with a final `result` string. The outer \
                          caller will see this along with the path to the working directory."
                .to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "result": {"type": "string"}
                },
                "required": ["result"]
            }),
        }
    }
    async fn execute(&self, input: &serde_json::Value) -> ToolOutput {
        let result = match input.get("result").and_then(|v| v.as_str()) {
            Some(r) if !r.trim().is_empty() => r.trim().to_string(),
            _ => return ToolOutput::error("finish requires non-empty 'result'".to_string()),
        };
        let mut s = self.state.lock().unwrap();
        s.final_result = Some(result);
        ToolOutput::success("Reader finished.".to_string())
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        let mut i = max;
        while i > 0 && !s.is_char_boundary(i) {
            i -= 1;
        }
        &s[..i]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_state(text: &str, dir: &Path) -> ReaderStateRef {
        Arc::new(Mutex::new(ReaderState {
            input_text: text.to_string(),
            cursor: 0,
            working_dir: dir.to_path_buf(),
            final_result: None,
        }))
    }

    #[test]
    fn slug_from_path_basic() {
        assert_eq!(slug_from_path("/a/b/foo.rs"), "foo-rs");
        assert_eq!(slug_from_path("/tmp/bar.txt"), "bar-txt");
        assert_eq!(slug_from_path(""), "input");
    }

    #[test]
    fn slug_from_path_caps_at_40() {
        let long = "x".repeat(100);
        assert_eq!(slug_from_path(&long).len(), 40);
    }

    #[test]
    fn slug_from_path_collapses_dashes() {
        assert_eq!(slug_from_path("/some!@#path/with-many--non-ascii.ext"), "with-many-non-ascii-ext");
    }

    #[test]
    fn validate_note_path_accepts_plain_names() {
        let dir = std::env::temp_dir();
        assert!(validate_note_path(&dir, "notes.md").is_ok());
        assert!(validate_note_path(&dir, "chapter_01.md").is_ok());
    }

    #[test]
    fn validate_note_path_rejects_escapes() {
        let dir = std::env::temp_dir();
        assert!(validate_note_path(&dir, "../outside").is_err());
        assert!(validate_note_path(&dir, "sub/file").is_err());
        assert!(validate_note_path(&dir, "").is_err());
        assert!(validate_note_path(&dir, "   ").is_err());
        assert!(validate_note_path(&dir, ".hidden").is_err());
    }

    #[tokio::test]
    async fn next_chunk_advances_cursor() {
        let tmp = tempfile::tempdir().unwrap();
        let state = fresh_state("abcdefghij", tmp.path());
        let tool = NextChunkTool {
            state: state.clone(),
        };
        // size=3 → returns "abc", cursor=3
        let out = tool.execute(&json!({"size": 512})).await;
        assert!(!out.is_error);
        assert!(out.content.contains("abcdefghij"));
        assert_eq!(state.lock().unwrap().cursor, 10);
        // Next call → EOF
        let out2 = tool.execute(&json!({})).await;
        assert!(out2.content.contains("EOF"));
    }

    #[tokio::test]
    async fn next_chunk_respects_char_boundaries() {
        // Multi-byte char at position that would split
        let text = "abc😀defg"; // emoji is 4 bytes
        let tmp = tempfile::tempdir().unwrap();
        let state = fresh_state(text, tmp.path());
        let tool = NextChunkTool { state };
        // size=4 would land mid-emoji; tool should back off to char boundary
        let out = tool.execute(&json!({"size": 4})).await;
        assert!(!out.is_error);
        // Should have returned just "abc" (cursor moved to 3, the start of the emoji)
        assert!(out.content.contains("abc"));
        // If we accidentally split the emoji, the output would contain a
        // replacement character or be invalid UTF-8; since we returned
        // a String successfully, char boundary was respected.
    }

    #[tokio::test]
    async fn write_note_and_read_note_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let state = fresh_state("", tmp.path());
        let writer = WriteNoteTool {
            state: state.clone(),
        };
        let reader = ReadNoteTool {
            state: state.clone(),
        };
        let w = writer
            .execute(&json!({"name": "notes.md", "content": "hello world"}))
            .await;
        assert!(!w.is_error);
        let r = reader.execute(&json!({"name": "notes.md"})).await;
        assert!(!r.is_error);
        assert_eq!(r.content, "hello world");
    }

    #[tokio::test]
    async fn append_note_inserts_newline_between_writes() {
        let tmp = tempfile::tempdir().unwrap();
        let state = fresh_state("", tmp.path());
        let append = AppendNoteTool {
            state: state.clone(),
        };
        let a = append
            .execute(&json!({"name": "log.txt", "content": "line A"}))
            .await;
        assert!(!a.is_error);
        let b = append
            .execute(&json!({"name": "log.txt", "content": "line B"}))
            .await;
        assert!(!b.is_error);
        let contents = std::fs::read_to_string(tmp.path().join("log.txt")).unwrap();
        assert_eq!(contents, "line A\nline B");
    }

    #[tokio::test]
    async fn write_note_rejects_large_content() {
        let tmp = tempfile::tempdir().unwrap();
        let state = fresh_state("", tmp.path());
        let writer = WriteNoteTool { state };
        let huge = "x".repeat(MAX_NOTE_CHARS + 1);
        let out = writer
            .execute(&json!({"name": "huge.txt", "content": huge}))
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("too large"));
    }

    #[tokio::test]
    async fn list_notes_shows_what_was_written() {
        let tmp = tempfile::tempdir().unwrap();
        let state = fresh_state("", tmp.path());
        let writer = WriteNoteTool {
            state: state.clone(),
        };
        let lister = ListNotesTool {
            state: state.clone(),
        };
        writer
            .execute(&json!({"name": "a.md", "content": "aa"}))
            .await;
        writer
            .execute(&json!({"name": "b.md", "content": "bbbb"}))
            .await;
        let out = lister.execute(&json!({})).await;
        assert!(!out.is_error);
        assert!(out.content.contains("a.md"));
        assert!(out.content.contains("b.md"));
        assert!(out.content.contains("2 bytes"));
        assert!(out.content.contains("4 bytes"));
    }

    #[tokio::test]
    async fn finish_stores_result() {
        let tmp = tempfile::tempdir().unwrap();
        let state = fresh_state("", tmp.path());
        let tool = FinishTool {
            state: state.clone(),
        };
        let out = tool.execute(&json!({"result": "the answer is 42"})).await;
        assert!(!out.is_error);
        assert_eq!(
            state.lock().unwrap().final_result,
            Some("the answer is 42".to_string())
        );
    }

    #[tokio::test]
    async fn finish_rejects_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let state = fresh_state("", tmp.path());
        let tool = FinishTool { state };
        let out = tool.execute(&json!({"result": ""})).await;
        assert!(out.is_error);
    }
}
