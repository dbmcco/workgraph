//! `wg worktree` subcommands — list, archive, and inspect agent worktrees.

use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result};

/// List all worktrees under `.wg-worktrees/`.
pub fn list(workgraph_dir: &Path) -> Result<()> {
    let project_root = workgraph_dir
        .parent()
        .context("Cannot determine project root from workgraph dir")?;
    let worktrees_dir = project_root.join(".wg-worktrees");

    if !worktrees_dir.exists() {
        println!("No worktrees directory found.");
        return Ok(());
    }

    let mut entries: Vec<_> = std::fs::read_dir(&worktrees_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .collect();
    entries.sort_by_key(|e| e.file_name());

    if entries.is_empty() {
        println!("No worktrees found.");
        return Ok(());
    }

    println!("Agent worktrees ({}):", entries.len());
    for entry in &entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let path = entry.path();
        let size = dir_size_human(&path);
        let mtime = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| {
                let elapsed = t.elapsed().ok()?;
                Some(humanize_duration(elapsed))
            })
            .unwrap_or_else(|| "unknown".to_string());

        // Check if there are uncommitted changes
        let has_changes = has_uncommitted_changes(&path);
        let status = if has_changes {
            " [uncommitted changes]"
        } else {
            ""
        };

        println!("  {} — {} — modified {}{}", name, size, mtime, status);
    }

    Ok(())
}

/// Archive a specific agent's worktree: commit uncommitted work,
/// then optionally remove the directory.
pub fn archive(workgraph_dir: &Path, agent_id: &str, remove: bool) -> Result<()> {
    let project_root = workgraph_dir
        .parent()
        .context("Cannot determine project root from workgraph dir")?;
    let worktrees_dir = project_root.join(".wg-worktrees");
    let wt_path = worktrees_dir.join(agent_id);

    if !wt_path.exists() {
        anyhow::bail!(
            "Worktree for '{}' not found at {}",
            agent_id,
            wt_path.display()
        );
    }

    // Check for uncommitted changes and auto-commit them
    if has_uncommitted_changes(&wt_path) {
        eprintln!(
            "[worktree] Committing uncommitted changes in {} ...",
            agent_id
        );

        // Stage all changes
        let add = Command::new("git")
            .args(["add", "-A"])
            .current_dir(&wt_path)
            .output()
            .context("Failed to run git add")?;

        if !add.status.success() {
            let stderr = String::from_utf8_lossy(&add.stderr);
            anyhow::bail!("git add failed: {}", stderr.trim());
        }

        // Commit with archive message
        let msg = format!(
            "archive: {} work snapshot\n\nAuto-committed by `wg worktree archive` to preserve\nuncommitted agent work before archival.",
            agent_id
        );
        let commit = Command::new("git")
            .args(["commit", "-m", &msg])
            .current_dir(&wt_path)
            .output()
            .context("Failed to run git commit")?;

        if !commit.status.success() {
            let stderr = String::from_utf8_lossy(&commit.stderr);
            // "nothing to commit" is OK
            if !stderr.contains("nothing to commit") {
                anyhow::bail!("git commit failed: {}", stderr.trim());
            }
        } else {
            eprintln!(
                "[worktree] Committed: {}",
                String::from_utf8_lossy(&commit.stdout).trim()
            );
        }
    } else {
        eprintln!("[worktree] No uncommitted changes in {}", agent_id);
    }

    if remove {
        eprintln!(
            "[worktree] Removing worktree directory {} ...",
            wt_path.display()
        );

        // First try git worktree remove (clean git integration)
        let wt_remove = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&wt_path)
            .current_dir(project_root)
            .output();

        match wt_remove {
            Ok(output) if output.status.success() => {
                eprintln!("[worktree] Removed via git worktree remove");
            }
            _ => {
                // Fallback: manual removal (not a real git worktree,
                // just a directory)
                std::fs::remove_dir_all(&wt_path).context("Failed to remove worktree directory")?;
                eprintln!("[worktree] Removed directory manually");
            }
        }

        eprintln!("[worktree] Archived and removed: {}", agent_id);
    } else {
        eprintln!("[worktree] Archived (preserved on disk): {}", agent_id);
        eprintln!("  To remove: wg worktree archive {} --remove", agent_id);
    }

    Ok(())
}

fn has_uncommitted_changes(wt_path: &Path) -> bool {
    Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(wt_path)
        .output()
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false)
}

fn dir_size_human(path: &Path) -> String {
    let output = Command::new("du").args(["-sh"]).arg(path).output().ok();
    output
        .and_then(|o| {
            String::from_utf8(o.stdout)
                .ok()
                .map(|s| s.split_whitespace().next().unwrap_or("?").to_string())
        })
        .unwrap_or_else(|| "?".to_string())
}

fn humanize_duration(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}
