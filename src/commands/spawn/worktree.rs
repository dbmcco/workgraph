//! Git worktree isolation for spawned agents.
//!
//! This is the minimal substrate needed for later lifecycle cleanup and
//! recovery. When enabled, a code-writing agent can execute from its own git
//! worktree under `.wg-worktrees/<agent-id>/`, with `.workgraph/` symlinked in
//! so the existing `wg` CLI keeps working.

use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub branch: String,
    pub project_root: PathBuf,
}

pub fn create_worktree(
    project_root: &Path,
    workgraph_dir: &Path,
    agent_id: &str,
    task_id: &str,
) -> Result<WorktreeInfo> {
    let branch = format!("wg/{}/{}", agent_id, task_id);
    let worktree_dir = project_root.join(".wg-worktrees").join(agent_id);

    if worktree_dir.exists() {
        anyhow::bail!(
            "Worktree already exists at {:?}. Archive it explicitly before reusing {}.",
            worktree_dir,
            agent_id
        );
    }

    let output = Command::new("git")
        .args(["worktree", "add"])
        .arg(&worktree_dir)
        .args(["-b", &branch, "HEAD"])
        .current_dir(project_root)
        .output()
        .context("Failed to run git worktree add")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git worktree add failed: {}", stderr.trim());
    }

    let symlink_target = workgraph_dir
        .canonicalize()
        .context("Failed to canonicalize .workgraph path")?;
    let symlink_path = worktree_dir.join(".workgraph");
    create_workgraph_symlink(&symlink_target, &symlink_path)?;

    let setup_script = workgraph_dir.join("worktree-setup.sh");
    if setup_script.exists() {
        let _ = Command::new("bash")
            .arg(&setup_script)
            .arg(&worktree_dir)
            .arg(project_root)
            .current_dir(&worktree_dir)
            .output();
    }

    Ok(WorktreeInfo {
        path: worktree_dir,
        branch,
        project_root: project_root.to_path_buf(),
    })
}

#[cfg(unix)]
fn create_workgraph_symlink(target: &Path, link: &Path) -> Result<()> {
    std::os::unix::fs::symlink(target, link)
        .context("Failed to symlink .workgraph into worktree")?;
    Ok(())
}

#[cfg(windows)]
fn create_workgraph_symlink(target: &Path, link: &Path) -> Result<()> {
    std::os::windows::fs::symlink_dir(target, link)
        .context("Failed to symlink .workgraph into worktree")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn init_git_repo(path: &Path) {
        Command::new("git").args(["init"]).arg(path).output().unwrap();
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
        std::fs::write(path.join("file.txt"), "hello").unwrap();
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

    #[test]
    fn create_worktree_creates_isolated_checkout_and_symlink() {
        let temp = TempDir::new().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        init_git_repo(&project);

        let wg_dir = project.join(".workgraph");
        std::fs::create_dir_all(&wg_dir).unwrap();

        let info = create_worktree(&project, &wg_dir, "agent-1", "task-foo").unwrap();
        assert!(info.path.exists());
        assert_eq!(info.branch, "wg/agent-1/task-foo");
        assert!(info.path.join(".workgraph").exists());
        assert!(info.path.join("file.txt").exists());
    }

    #[test]
    fn create_worktree_refuses_to_overwrite_existing_directory() {
        let temp = TempDir::new().unwrap();
        let project = temp.path().join("project");
        std::fs::create_dir_all(&project).unwrap();
        init_git_repo(&project);

        let wg_dir = project.join(".workgraph");
        std::fs::create_dir_all(&wg_dir).unwrap();

        let _ = create_worktree(&project, &wg_dir, "agent-collide", "task-one").unwrap();
        let err = create_worktree(&project, &wg_dir, "agent-collide", "task-two").unwrap_err();
        assert!(format!("{:#}", err).contains("already exists"));
    }
}
