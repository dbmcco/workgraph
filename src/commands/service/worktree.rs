//! Minimal service-side worktree lifecycle cleanup for isolated agent spawns.
//!
//! The spawn wrapper writes `.wg-cleanup-pending` inside an isolated worktree
//! after it exits. Coordinator ticks can then safely reap that worktree once
//! the owning agent is no longer live and the task has reached a terminal
//! state.

use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use workgraph::parser::load_graph;
use workgraph::service::registry::AgentRegistry;

use crate::commands::{graph_path, is_process_alive};

pub const WORKTREES_DIR: &str = ".wg-worktrees";
pub const CLEANUP_PENDING_MARKER: &str = ".wg-cleanup-pending";

pub fn sweep_cleanup_pending_worktrees(dir: &Path) -> Result<usize> {
    let project_root = dir
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Cannot determine project root from {:?}", dir))?;
    let worktrees_root = project_root.join(WORKTREES_DIR);
    if !worktrees_root.exists() {
        return Ok(0);
    }

    let registry = AgentRegistry::load(dir)?;
    let graph = load_graph(&graph_path(dir))?;
    let mut removed = 0usize;

    for entry in fs::read_dir(&worktrees_root)
        .with_context(|| format!("Failed to read worktree directory at {:?}", worktrees_root))?
    {
        let entry = entry?;
        let worktree_path = entry.path();
        let marker = worktree_path.join(CLEANUP_PENDING_MARKER);
        if !marker.exists() {
            continue;
        }

        let agent_id = entry.file_name().to_string_lossy().to_string();
        let Some(agent) = registry.get_agent(&agent_id) else {
            continue;
        };
        if agent.is_alive() && is_process_alive(agent.pid) {
            continue;
        }
        let Some(task) = graph.get_task(&agent.task_id) else {
            continue;
        };
        if !task.status.is_terminal() {
            continue;
        }
        let Some(branch) = find_branch_for_worktree(project_root, &worktree_path)? else {
            eprintln!(
                "[worktree] Skipping cleanup for {:?}: could not determine branch",
                worktree_path
            );
            continue;
        };

        if let Err(err) = remove_worktree(project_root, &worktree_path, &branch) {
            eprintln!(
                "[worktree] Failed to remove {:?} (branch {}): {}",
                worktree_path, branch, err
            );
            continue;
        }

        removed += 1;
    }

    Ok(removed)
}

fn find_branch_for_worktree(project_root: &Path, worktree_path: &Path) -> Result<Option<String>> {
    let output = Command::new("git")
        .args(["worktree", "list", "--porcelain"])
        .current_dir(project_root)
        .output()
        .context("Failed to run git worktree list --porcelain")?;

    if !output.status.success() {
        anyhow::bail!(
            "git worktree list --porcelain failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let mut current_worktree: Option<PathBuf> = None;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            current_worktree = Some(PathBuf::from(path));
            continue;
        }

        if let Some(branch) = line.strip_prefix("branch refs/heads/")
            && current_worktree.as_deref() == Some(worktree_path)
        {
            return Ok(Some(branch.to_string()));
        }

        if line.is_empty() {
            current_worktree = None;
        }
    }

    Ok(None)
}

fn remove_worktree(project_root: &Path, worktree_path: &Path, branch: &str) -> Result<()> {
    let symlink_path = worktree_path.join(".workgraph");
    if symlink_path.exists() {
        fs::remove_file(&symlink_path)
            .with_context(|| format!("Failed to remove {:?}", symlink_path))?;
    }

    let target_dir = worktree_path.join("target");
    if target_dir.exists() {
        fs::remove_dir_all(&target_dir)
            .with_context(|| format!("Failed to remove {:?}", target_dir))?;
    }

    let output = Command::new("git")
        .args(["worktree", "remove", "--force"])
        .arg(worktree_path)
        .current_dir(project_root)
        .output()
        .context("Failed to run git worktree remove --force")?;
    if !output.status.success() {
        anyhow::bail!(
            "git worktree remove failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    let output = Command::new("git")
        .args(["branch", "-D", branch])
        .current_dir(project_root)
        .output()
        .context("Failed to run git branch -D")?;
    if !output.status.success() {
        anyhow::bail!(
            "git branch -D {} failed: {}",
            branch,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }

    Ok(())
}
