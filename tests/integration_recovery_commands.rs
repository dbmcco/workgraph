use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use tempfile::TempDir;
use workgraph::graph::{Node, Status, Task, WorkGraph};
use workgraph::parser::{load_graph, save_graph};
use workgraph::provenance;

fn wg_binary() -> PathBuf {
    let mut path = std::env::current_exe().expect("could not get current exe path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    path.push("wg");
    assert!(
        path.exists(),
        "wg binary not found at {:?}. Run `cargo build` first.",
        path
    );
    path
}

fn wg_cmd(wg_dir: &Path, args: &[&str]) -> std::process::Output {
    Command::new(wg_binary())
        .arg("--dir")
        .arg(wg_dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap_or_else(|e| panic!("Failed to run wg {:?}: {}", args, e))
}

fn wg_ok(wg_dir: &Path, args: &[&str]) -> String {
    let output = wg_cmd(wg_dir, args);
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    assert!(
        output.status.success(),
        "wg {:?} failed.\nstdout: {}\nstderr: {}",
        args,
        stdout,
        stderr
    );
    stdout
}

fn make_task(id: &str, title: &str, status: Status) -> Task {
    Task {
        id: id.to_string(),
        title: title.to_string(),
        status,
        ..Task::default()
    }
}

fn setup_wg(tasks: Vec<Task>) -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    let wg_dir = dir.path().join(".workgraph");
    fs::create_dir_all(&wg_dir).unwrap();
    let graph_path = wg_dir.join("graph.jsonl");
    let mut graph = WorkGraph::new();
    for task in tasks {
        graph.add_node(Node::Task(task));
    }
    save_graph(&graph, &graph_path).unwrap();
    (dir, wg_dir)
}

#[test]
fn recovery_insert_parallel_replace_edges_rewires_successors_and_records_operation() {
    let mut p = make_task("p", "Predecessor", Status::Done);
    let mut t = make_task("t", "Target", Status::Failed);
    let mut s = make_task("s", "Successor", Status::Open);
    p.before = vec!["t".into()];
    t.after = vec!["p".into()];
    t.before = vec!["s".into()];
    s.after = vec!["t".into()];
    let (_dir, wg_dir) = setup_wg(vec![p, t, s]);

    wg_ok(
        &wg_dir,
        &[
            "insert",
            "parallel",
            "t",
            "--title",
            "Rescue path",
            "--id",
            "rescue-path",
            "--replace-edges",
        ],
    );

    let graph = load_graph(wg_dir.join("graph.jsonl")).unwrap();
    let inserted = graph.get_task("rescue-path").unwrap();
    assert_eq!(inserted.after, vec!["p".to_string()]);
    assert_eq!(inserted.before, vec!["s".to_string()]);
    assert_eq!(
        graph.get_task("s").unwrap().after,
        vec!["rescue-path".to_string()]
    );
    assert!(graph.get_task("t").unwrap().before.is_empty());

    let ops = provenance::read_all_operations(&wg_dir).unwrap();
    let insert_op = ops.iter().find(|entry| entry.op == "insert").unwrap();
    assert_eq!(insert_op.task_id.as_deref(), Some("rescue-path"));
    assert_eq!(
        insert_op.detail.get("target").and_then(|v| v.as_str()),
        Some("t")
    );
    assert_eq!(
        insert_op.detail.get("position").and_then(|v| v.as_str()),
        Some("parallel")
    );
}

#[test]
fn recovery_rescue_creates_first_class_replacement_and_records_operation() {
    let mut p = make_task("p", "Predecessor", Status::Done);
    let mut t = make_task("t", "Target", Status::Failed);
    let mut s = make_task("s", "Successor", Status::Open);
    p.before = vec!["t".into()];
    t.after = vec!["p".into()];
    t.before = vec!["s".into()];
    s.after = vec!["t".into()];
    let (_dir, wg_dir) = setup_wg(vec![p, t, s]);

    wg_ok(
        &wg_dir,
        &[
            "rescue",
            "t",
            "--description",
            "Fix the failed implementation and keep writes in the repo root.",
            "--id",
            "rescue-t",
            "--from-eval",
            ".evaluate-t",
        ],
    );

    let graph = load_graph(wg_dir.join("graph.jsonl")).unwrap();
    let rescue = graph.get_task("rescue-t").unwrap();
    assert!(!rescue.id.starts_with('.'));
    assert!(rescue.tags.iter().any(|tag| tag == "rescue"));
    assert!(
        rescue
            .description
            .as_deref()
            .unwrap_or("")
            .contains(".evaluate-t")
    );
    assert_eq!(
        graph.get_task("s").unwrap().after,
        vec!["rescue-t".to_string()]
    );
    assert!(
        graph
            .get_task("t")
            .unwrap()
            .log
            .iter()
            .any(|entry| entry.message.contains("superseded by rescue task"))
    );

    let ops = provenance::read_all_operations(&wg_dir).unwrap();
    let rescue_op = ops.iter().find(|entry| entry.op == "rescue").unwrap();
    assert_eq!(rescue_op.task_id.as_deref(), Some("rescue-t"));
    assert_eq!(
        rescue_op.detail.get("target").and_then(|v| v.as_str()),
        Some("t")
    );
}

#[test]
fn recovery_reset_strips_meta_resets_closure_and_records_operation() {
    let mut p = make_task("p", "Predecessor", Status::Done);
    let mut t = make_task("t", "Target", Status::Failed);
    let mut s = make_task("s", "Successor", Status::Blocked);
    p.before = vec!["t".into()];
    t.after = vec!["p".into()];
    t.before = vec!["s".into()];
    t.assigned = Some("agent-1".to_string());
    t.failure_reason = Some("broken".to_string());
    t.retry_count = 2;
    s.after = vec!["t".into()];
    s.paused = true;

    let mut flip = make_task(".flip-t", "Meta", Status::Done);
    flip.after = vec!["t".into()];
    let mut eval = make_task(".evaluate-t", "Meta eval", Status::Open);
    eval.after = vec!["t".into()];
    let (_dir, wg_dir) = setup_wg(vec![p, t, s, flip, eval]);

    wg_ok(
        &wg_dir,
        &[
            "reset",
            "t",
            "--direction",
            "forward",
            "--also-strip-meta",
            "--yes",
        ],
    );

    let graph = load_graph(wg_dir.join("graph.jsonl")).unwrap();
    let target = graph.get_task("t").unwrap();
    assert_eq!(target.status, Status::Open);
    assert!(target.assigned.is_none());
    assert!(target.failure_reason.is_none());
    assert_eq!(target.retry_count, 0);
    assert!(
        target
            .log
            .iter()
            .any(|entry| entry.message.contains("reset via"))
    );
    assert_eq!(graph.get_task("s").unwrap().status, Status::Open);
    assert!(!graph.get_task("s").unwrap().paused);
    assert!(graph.get_task(".flip-t").is_none());
    assert!(graph.get_task(".evaluate-t").is_none());
    assert_eq!(graph.get_task("p").unwrap().status, Status::Done);

    let ops = provenance::read_all_operations(&wg_dir).unwrap();
    let reset_op = ops.iter().find(|entry| entry.op == "reset").unwrap();
    assert_eq!(reset_op.actor.as_deref(), Some("reset"));
    assert_eq!(
        reset_op.detail.get("reset_count").and_then(|v| v.as_u64()),
        Some(2)
    );
}
