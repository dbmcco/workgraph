use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};

use crate::commands::service::{
    COORDINATOR_MODEL_OVERRIDE_ENV, COORDINATOR_RUNTIME_EXECUTOR_ENV,
};

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

fn runtime_executor_override() -> Option<String> {
    std::env::var(COORDINATOR_RUNTIME_EXECUTOR_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn runtime_model_override() -> Option<String> {
    std::env::var(COORDINATOR_MODEL_OVERRIDE_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn resolve_handler(
    workgraph_dir: &Path,
    task_id: &str,
    role_override: Option<&str>,
) -> Result<HandlerSpec> {
    if !is_primary_coordinator(task_id) {
        anyhow::bail!("this workgraph line supports only the primary coordinator for spawn-task");
    }

    match role_override {
        None | Some("coordinator") => {}
        Some(other) => anyhow::bail!(
            "unsupported coordinator role for this tranche: {} (expected coordinator)",
            other
        ),
    }

    let config = workgraph::config::Config::load_or_default(workgraph_dir);
    let coordinator_cfg = config.coordinator.clone();
    let resolved_model = runtime_model_override().or(coordinator_cfg.model.clone());

    let executor = runtime_executor_override().unwrap_or_else(|| {
        let mut runtime_cfg = coordinator_cfg.clone();
        runtime_cfg.model = resolved_model.clone();
        runtime_cfg.effective_executor()
    });
    if executor != "claude" {
        anyhow::bail!(
            "unsupported coordinator executor for this tranche: {} (expected claude)",
            executor
        );
    }

    Ok(HandlerSpec::Claude {
        chat_ref: resolve_chat_ref(task_id),
        model: resolved_model,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;
    use tempfile::TempDir;

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: &str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe { std::env::set_var(key, value) };
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            unsafe { std::env::remove_var(key) };
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.previous.as_deref() {
                Some(value) => unsafe { std::env::set_var(self.key, value) },
                None => unsafe { std::env::remove_var(self.key) },
            }
        }
    }

    fn with_runtime_env<F>(home: &TempDir, executor: Option<&str>, model: Option<&str>, f: F)
    where
        F: FnOnce(),
    {
        let _home = EnvGuard::set("HOME", home.path().to_str().unwrap());
        let _executor = match executor {
            Some(value) => EnvGuard::set(COORDINATOR_RUNTIME_EXECUTOR_ENV, value),
            None => EnvGuard::remove(COORDINATOR_RUNTIME_EXECUTOR_ENV),
        };
        let _model = match model {
            Some(value) => EnvGuard::set(COORDINATOR_MODEL_OVERRIDE_ENV, value),
            None => EnvGuard::remove(COORDINATOR_MODEL_OVERRIDE_ENV),
        };

        f();
    }

    #[test]
    #[serial]
    fn resolve_handler_prefers_runtime_model_override_over_static_config() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[coordinator]\nexecutor = \"claude\"\nmodel = \"claude:baseline\"\n",
        )
        .unwrap();

        with_runtime_env(&home, None, Some("claude:override"), || {
            let spec = resolve_handler(dir.path(), ".coordinator-0", Some("coordinator")).unwrap();
            assert_eq!(
                spec,
                HandlerSpec::Claude {
                    chat_ref: "coordinator-0".to_string(),
                    model: Some("claude:override".to_string()),
                }
            );
        });
    }

    #[test]
    #[serial]
    fn resolve_handler_uses_explicit_runtime_intent_before_static_config() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[coordinator]\nexecutor = \"native\"\nmodel = \"openrouter:minimax/minimax-m1\"\n",
        )
        .unwrap();

        with_runtime_env(&home, Some("claude"), Some("claude:override"), || {
            let spec = resolve_handler(dir.path(), ".coordinator-0", Some("coordinator")).unwrap();
            assert_eq!(
                spec,
                HandlerSpec::Claude {
                    chat_ref: "coordinator-0".to_string(),
                    model: Some("claude:override".to_string()),
                }
            );
        });
    }

    #[test]
    #[serial]
    fn resolve_handler_rejects_unsupported_runtime_executor_override() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[coordinator]\nexecutor = \"claude\"\nmodel = \"claude:haiku\"\n",
        )
        .unwrap();

        with_runtime_env(&home, Some("native"), None, || {
            let err =
                resolve_handler(dir.path(), ".coordinator-0", Some("coordinator")).unwrap_err();
            assert!(
                err.to_string()
                    .contains("unsupported coordinator executor for this tranche: native"),
                "unexpected error: {err:#}"
            );
        });
    }

    #[test]
    #[serial]
    fn resolve_handler_falls_back_to_config_model_when_runtime_model_is_absent() {
        let dir = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[coordinator]\nexecutor = \"claude\"\nmodel = \"claude:haiku\"\n",
        )
        .unwrap();

        with_runtime_env(&home, None, None, || {
            let spec = resolve_handler(dir.path(), ".coordinator-0", Some("coordinator")).unwrap();
            assert_eq!(
                spec,
                HandlerSpec::Claude {
                    chat_ref: "coordinator-0".to_string(),
                    model: Some("claude:haiku".to_string()),
                }
            );
        });
    }
}
