use std::path::Path;

use anyhow::{Context, Result};
use chrono::Utc;

use crate::commands::insert::{self, InsertOptions, Position};
use workgraph::graph::{LogEntry, Status};

pub fn run(
    dir: &Path,
    target_id: &str,
    description: &str,
    title: Option<&str>,
    new_id: Option<&str>,
    from_eval: Option<&str>,
    actor: Option<&str>,
) -> Result<String> {
    if description.trim().is_empty() {
        anyhow::bail!(
            "Rescue description is required — it becomes the next agent's assignment. Be specific about what needs to change."
        );
    }

    {
        let (graph, _) = crate::commands::load_workgraph(dir)?;
        let target = graph.get_task(target_id).ok_or_else(|| {
            anyhow::anyhow!("rescue target task '{}' not found in graph", target_id)
        })?;
        if target.status == Status::Done {
            eprintln!(
                "\x1b[33m[rescue] warning: target '{}' is already Done. Proceeding — new task will route successors around it.\x1b[0m",
                target_id
            );
        }
    }

    let rescue_title = title
        .map(str::to_string)
        .unwrap_or_else(|| format!("Rescue: {}", target_id));
    let stamped_description = format!(
        "## Rescue for `{target}`\n\
         \n\
         This task supersedes `{target}`, which failed evaluation. Your job is to complete the work correctly.\n\
         \n\
         **Source task:** `{target}`  \n\
         **Eval task that spawned this rescue:** {eval}  \n\
         **Rescue attempt created:** {when}\n\
         \n\
         ---\n\
         \n\
         ## What to fix (from the evaluator)\n\
         \n\
         {body}",
        target = target_id,
        eval = from_eval.unwrap_or("(none — invoked directly)"),
        when = Utc::now().to_rfc3339(),
        body = description.trim(),
    );

    let new_task_id = insert::run(
        dir,
        Position::Parallel,
        target_id,
        &rescue_title,
        Some(&stamped_description),
        new_id,
        InsertOptions {
            replace_edges: true,
            ..Default::default()
        },
    )
    .context("insert::run for rescue failed")?;

    let actor_str = actor.unwrap_or("rescue").to_string();
    let target_id_s = target_id.to_string();
    let new_id_s = new_task_id.clone();
    let eval_ref = from_eval.unwrap_or("(direct)").to_string();

    super::mutate_workgraph(dir, |graph| {
        if let Some(target) = graph.get_task_mut(&target_id_s) {
            target.log.push(LogEntry {
                timestamp: Utc::now().to_rfc3339(),
                actor: Some(actor_str.clone()),
                message: format!(
                    "superseded by rescue task '{}' (from eval {})",
                    new_id_s, eval_ref
                ),
            });
        }

        if let Some(rescue) = graph.get_task_mut(&new_id_s) {
            rescue.log.push(LogEntry {
                timestamp: Utc::now().to_rfc3339(),
                actor: Some(actor_str.clone()),
                message: format!(
                    "supersedes '{}'; created from eval {}",
                    target_id_s, eval_ref
                ),
            });
            if !rescue.tags.iter().any(|tag| tag == "rescue") {
                rescue.tags.push("rescue".to_string());
            }
        }

        Ok(())
    })?;

    let config = workgraph::config::Config::load_or_default(dir);
    let _ = workgraph::provenance::record(
        dir,
        "rescue",
        Some(&new_task_id),
        Some(&actor_str),
        serde_json::json!({
            "target": target_id,
            "from_eval": from_eval,
            "title": rescue_title,
        }),
        config.log.rotation_threshold,
    );

    eprintln!(
        "\x1b[2m[rescue] created '{}' superseding '{}'{}\x1b[0m",
        new_task_id,
        target_id,
        from_eval
            .map(|eval| format!(" (from eval {})", eval))
            .unwrap_or_default()
    );

    Ok(new_task_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use workgraph::graph::Task;
    use workgraph::parser::load_graph;
    use workgraph::test_helpers::{make_task_with_status, setup_workgraph};

    fn make(id: &str, status: Status) -> Task {
        make_task_with_status(id, id, status)
    }

    fn setup_classic_fan(dir: &Path) {
        let mut p = make("p", Status::Done);
        let mut t = make("t", Status::Failed);
        let mut s = make("s", Status::Open);
        p.before = vec!["t".into()];
        t.after = vec!["p".into()];
        t.before = vec!["s".into()];
        s.after = vec!["t".into()];
        setup_workgraph(dir, vec![p, t, s]);
    }

    #[test]
    fn rescue_creates_first_class_task_with_rescue_tag() {
        let dir = tempdir().unwrap();
        setup_classic_fan(dir.path());

        let new_id = run(
            dir.path(),
            "t",
            "Implement the feature correctly — the previous attempt wrote to /tmp, use cwd.",
            None,
            Some("rescue-t"),
            Some(".evaluate-t"),
            Some("evaluator"),
        )
        .unwrap();

        let g = load_graph(&crate::commands::graph_path(dir.path())).unwrap();
        let r = g.get_task(&new_id).unwrap();
        assert!(!r.id.starts_with('.'));
        assert!(r.tags.iter().any(|tag| tag == "rescue"));
        let desc = r.description.as_deref().unwrap_or("");
        assert!(desc.contains("Rescue for `t`"));
        assert!(desc.contains(".evaluate-t"));
        assert!(desc.contains("What to fix"));
        assert!(desc.contains("wrote to /tmp"));
    }

    #[test]
    fn rescue_reroutes_successors_and_preserves_target() {
        let dir = tempdir().unwrap();
        setup_classic_fan(dir.path());

        let new_id = run(
            dir.path(),
            "t",
            "fix the thing",
            None,
            Some("rescue-t"),
            Some(".evaluate-t"),
            Some("evaluator"),
        )
        .unwrap();

        let g = load_graph(&crate::commands::graph_path(dir.path())).unwrap();
        let r = g.get_task(&new_id).unwrap();
        assert_eq!(r.after, vec!["p".to_string()]);
        assert_eq!(r.before, vec!["s".to_string()]);

        let s = g.get_task("s").unwrap();
        assert_eq!(s.after, vec![new_id.clone()]);

        let t = g.get_task("t").unwrap();
        assert_eq!(t.status, Status::Failed);
        assert!(t.before.is_empty());
        assert!(
            t.log
                .iter()
                .any(|entry| entry.message.contains("superseded by rescue task"))
        );
    }

    #[test]
    fn rescue_rejects_empty_description() {
        let dir = tempdir().unwrap();
        setup_classic_fan(dir.path());

        let result = run(
            dir.path(),
            "t",
            "   ",
            None,
            None,
            Some(".evaluate-t"),
            Some("evaluator"),
        );
        assert!(result.is_err());
        assert!(format!("{}", result.err().unwrap()).contains("description"));
    }

    #[test]
    fn rescue_errors_on_nonexistent_target() {
        let dir = tempdir().unwrap();
        setup_classic_fan(dir.path());

        let result = run(
            dir.path(),
            "nonexistent",
            "do a thing",
            None,
            None,
            None,
            Some("evaluator"),
        );
        assert!(result.is_err());
        assert!(format!("{}", result.err().unwrap()).contains("not found"));
    }

    #[test]
    fn rescue_writes_operations_log_entry() {
        let dir = tempdir().unwrap();
        setup_classic_fan(dir.path());

        let new_id = run(
            dir.path(),
            "t",
            "fix",
            None,
            Some("rescue-t"),
            Some(".evaluate-t"),
            Some("evaluator"),
        )
        .unwrap();

        let ops_path = workgraph::provenance::operations_path(dir.path());
        assert!(ops_path.exists());
        let content = std::fs::read_to_string(&ops_path).unwrap();
        assert!(content.contains(r#""op":"rescue""#));
        assert!(content.contains(&new_id));
        assert!(content.contains("\"target\":\"t\""));
        assert!(content.contains(".evaluate-t"));
    }
}
