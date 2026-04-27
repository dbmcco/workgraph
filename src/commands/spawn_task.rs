use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

#[derive(Clone, Debug, PartialEq, Eq)]
enum HandlerSpec {
    Claude {
        chat_ref: String,
        model: Option<String>,
    },
}

fn is_primary_coordinator(task_id: &str) -> bool {
    task_id == ".coordinator-0" || task_id == "coordinator-0"
}

fn resolve_chat_ref(task_id: &str) -> String {
    if let Some(n) = task_id.strip_prefix(".coordinator-") {
        format!("coordinator-{}", n)
    } else {
        task_id.to_string()
    }
}

fn resolve_handler(
    workgraph_dir: &Path,
    task_id: &str,
    role_override: Option<&str>,
) -> Result<HandlerSpec> {
    if !is_primary_coordinator(task_id) {
        anyhow::bail!("this workgraph line supports only the primary coordinator for spawn-task");
    }

    let config = workgraph::config::Config::load_or_default(workgraph_dir);
    let coordinator_cfg = config.coordinator.clone();
    if role_override == Some("coordinator") {
        // Keep current coordinator semantics; no extra mutation needed.
    }

    let executor = coordinator_cfg.effective_executor();
    if executor != "claude" {
        anyhow::bail!(
            "unsupported coordinator executor for this tranche: {} (expected claude)",
            executor
        );
    }

    Ok(HandlerSpec::Claude {
        chat_ref: resolve_chat_ref(task_id),
        model: coordinator_cfg.model.clone(),
    })
}

fn current_wg_binary() -> Result<PathBuf> {
    std::env::current_exe().context("failed to resolve current wg binary")
}

fn dispatch(workgraph_dir: &Path, spec: &HandlerSpec) -> Result<()> {
    let wg = current_wg_binary()?;
    let mut cmd = Command::new(wg);
    cmd.arg("--dir").arg(workgraph_dir);

    match spec {
        HandlerSpec::Claude { chat_ref, model } => {
            cmd.arg("claude-handler").arg("--chat").arg(chat_ref);
            if let Some(model) = model {
                cmd.arg("-m").arg(model);
            }
        }
    }

    let status = cmd.status().context("failed to run handler subprocess")?;
    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("handler subprocess exited with status {}", status)
    }
}

pub fn run(
    workgraph_dir: &Path,
    task_id: &str,
    role_override: Option<&str>,
    dry_run: bool,
) -> Result<()> {
    let spec = resolve_handler(workgraph_dir, task_id, role_override)?;
    if dry_run {
        match spec {
            HandlerSpec::Claude {
                ref chat_ref,
                ref model,
            } => {
                let mut preview = format!("wg claude-handler --chat {}", chat_ref);
                if let Some(model) = model {
                    preview.push_str(&format!(" -m {}", model));
                }
                println!("{}", preview);
            }
        }
        return Ok(());
    }

    dispatch(workgraph_dir, &spec)
}
