use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;
use workgraph::graph::{Node, Status, Task, WorkGraph};
use workgraph::parser::save_graph;

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

fn init_git_repo(path: &Path) {
    Command::new("git")
        .args(["init"])
        .arg(path)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.email", "test@test.com"])
        .current_dir(path)
        .output()
        .unwrap();
    Command::new("git")
        .args(["config", "user.name", "Test"])
        .current_dir(path)
        .output()
        .unwrap();
    fs::write(path.join("tracked.txt"), "seed").unwrap();
    Command::new("git")
        .args(["add", "."])
        .current_dir(path)
        .output()
        .unwrap();
    Command::new("git")
        .args(["commit", "-m", "init"])
        .current_dir(path)
        .output()
        .unwrap();
}

fn setup_repo() -> (TempDir, PathBuf) {
    let dir = TempDir::new().unwrap();
    init_git_repo(dir.path());

    let wg_dir = dir.path().join(".workgraph");
    fs::create_dir_all(&wg_dir).unwrap();
    fs::write(
        wg_dir.join("config.toml"),
        r#"
[coordinator]
worktree_isolation = true
"#,
    )
    .unwrap();

    let graph_path = wg_dir.join("graph.jsonl");
    let mut graph = WorkGraph::new();
    graph.add_node(Node::Task(Task {
        id: "t1".to_string(),
        title: "Write in worktree".to_string(),
        status: Status::Open,
        exec: Some("pwd > isolated_pwd.txt".to_string()),
        ..Task::default()
    }));
    save_graph(&graph, &graph_path).unwrap();

    (dir, wg_dir)
}

fn wait_for(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("Timed out waiting for {:?}", path);
}

fn wait_for_absent(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if !path.exists() {
            return;
        }
        thread::sleep(Duration::from_millis(50));
    }
    panic!("Timed out waiting for {:?} to be removed", path);
}

#[test]
fn spawn_uses_isolated_worktree_when_enabled() {
    let (dir, wg_dir) = setup_repo();

    wg_ok(&wg_dir, &["spawn", "t1", "--executor", "shell"]);

    let agent_dir = wg_dir.join("agents").join("agent-1");
    let metadata_path = agent_dir.join("metadata.json");
    wait_for(&metadata_path);

    let metadata: Value =
        serde_json::from_str(&fs::read_to_string(&metadata_path).unwrap()).unwrap();
    let worktree_path = PathBuf::from(
        metadata["worktree_path"]
            .as_str()
            .expect("worktree_path missing from metadata"),
    );
    assert_eq!(metadata["worktree_branch"].as_str(), Some("wg/agent-1/t1"));

    let output_file = worktree_path.join("isolated_pwd.txt");
    wait_for(&output_file);
    let recorded_pwd = fs::read_to_string(&output_file).unwrap();
    assert_eq!(recorded_pwd.trim(), worktree_path.to_string_lossy());
    assert!(worktree_path.join(".workgraph").exists());
    assert!(!dir.path().join("isolated_pwd.txt").exists());
}

#[test]
fn service_tick_reaps_marked_isolated_worktree_after_agent_exit() {
    let (dir, wg_dir) = setup_repo();

    wg_ok(&wg_dir, &["spawn", "t1", "--executor", "shell"]);

    let agent_dir = wg_dir.join("agents").join("agent-1");
    let metadata_path = agent_dir.join("metadata.json");
    wait_for(&metadata_path);

    let metadata: Value =
        serde_json::from_str(&fs::read_to_string(&metadata_path).unwrap()).unwrap();
    let worktree_path = PathBuf::from(
        metadata["worktree_path"]
            .as_str()
            .expect("worktree_path missing from metadata"),
    );
    let worktree_branch = metadata["worktree_branch"]
        .as_str()
        .expect("worktree_branch missing from metadata")
        .to_string();

    let output_file = worktree_path.join("isolated_pwd.txt");
    wait_for(&output_file);

    let cleanup_marker = worktree_path.join(".wg-cleanup-pending");
    wait_for(&cleanup_marker);

    wg_ok(&wg_dir, &["service", "tick"]);

    wait_for_absent(&worktree_path);

    let branch_check = Command::new("git")
        .args(["branch", "--list", &worktree_branch])
        .current_dir(dir.path())
        .output()
        .unwrap();
    assert!(
        branch_check.status.success(),
        "git branch --list failed: {}",
        String::from_utf8_lossy(&branch_check.stderr)
    );
    assert!(
        String::from_utf8_lossy(&branch_check.stdout)
            .trim()
            .is_empty(),
        "worktree branch should be deleted after cleanup"
    );
}
