use std::path::Path;

use anyhow::Result;
use chrono::Utc;

use workgraph::graph::{Node, Status, Task};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Position {
    Before,
    After,
    Parallel,
}

impl Position {
    fn as_str(self) -> &'static str {
        match self {
            Position::Before => "before",
            Position::After => "after",
            Position::Parallel => "parallel",
        }
    }
}

impl std::str::FromStr for Position {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "before" => Ok(Position::Before),
            "after" => Ok(Position::After),
            "parallel" => Ok(Position::Parallel),
            other => Err(format!(
                "invalid position '{}' — must be one of: before, after, parallel",
                other
            )),
        }
    }
}

#[derive(Debug, Default)]
pub struct InsertOptions {
    pub splice: bool,
    pub replace_edges: bool,
}

pub fn run(
    dir: &Path,
    position: Position,
    target_id: &str,
    title: &str,
    description: Option<&str>,
    new_id: Option<&str>,
    opts: InsertOptions,
) -> Result<String> {
    if title.trim().is_empty() {
        anyhow::bail!("Task title must not be empty");
    }

    let candidate_id = match new_id {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => derive_id_from_title(title),
    };

    let assigned_id = super::mutate_workgraph(dir, |graph| {
        graph.get_task_or_err(target_id)?;

        let final_id = unique_id(graph, &candidate_id);
        let (target_after, target_before) = {
            let target = graph.get_task_or_err(target_id)?;
            (target.after.clone(), target.before.clone())
        };

        let mut new_task = Task {
            id: final_id.clone(),
            title: title.to_string(),
            description: description.map(str::to_string),
            status: Status::Open,
            created_at: Some(Utc::now().to_rfc3339()),
            ..Default::default()
        };

        match position {
            Position::Before => {
                if opts.splice {
                    new_task.after = target_after.clone();
                    new_task.before = vec![target_id.to_string()];

                    for pred_id in &target_after {
                        if let Some(pred) = graph.get_task_mut(pred_id) {
                            if !pred.before.contains(&final_id) {
                                pred.before.push(final_id.clone());
                            }
                            pred.before.retain(|dep| dep != target_id);
                        }
                    }

                    if let Some(target) = graph.get_task_mut(target_id) {
                        target.after = vec![final_id.clone()];
                    }
                } else {
                    new_task.before = vec![target_id.to_string()];
                    if let Some(target) = graph.get_task_mut(target_id)
                        && !target.after.contains(&final_id)
                    {
                        target.after.push(final_id.clone());
                    }
                }
            }
            Position::After => {
                if opts.splice {
                    new_task.after = vec![target_id.to_string()];
                    new_task.before = target_before.clone();

                    for succ_id in &target_before {
                        if let Some(succ) = graph.get_task_mut(succ_id) {
                            if !succ.after.contains(&final_id) {
                                succ.after.push(final_id.clone());
                            }
                            succ.after.retain(|dep| dep != target_id);
                        }
                    }

                    if let Some(target) = graph.get_task_mut(target_id) {
                        target.before = vec![final_id.clone()];
                    }
                } else {
                    new_task.after = vec![target_id.to_string()];
                    if let Some(target) = graph.get_task_mut(target_id)
                        && !target.before.contains(&final_id)
                    {
                        target.before.push(final_id.clone());
                    }
                }
            }
            Position::Parallel => {
                new_task.after = target_after.clone();
                new_task.before = target_before.clone();

                for pred_id in &target_after {
                    if let Some(pred) = graph.get_task_mut(pred_id)
                        && !pred.before.contains(&final_id)
                    {
                        pred.before.push(final_id.clone());
                    }
                }

                for succ_id in &target_before {
                    if let Some(succ) = graph.get_task_mut(succ_id) {
                        if !succ.after.contains(&final_id) {
                            succ.after.push(final_id.clone());
                        }
                        if opts.replace_edges {
                            succ.after.retain(|dep| dep != target_id);
                        }
                    }
                }

                if opts.replace_edges
                    && let Some(target) = graph.get_task_mut(target_id)
                {
                    target.before.clear();
                }
            }
        }

        graph.add_node(Node::Task(new_task));
        Ok(final_id)
    })?;

    super::notify_graph_changed(dir);

    let config = workgraph::config::Config::load_or_default(dir);
    let actor = std::env::var("WG_ACTOR")
        .ok()
        .or_else(|| std::env::var("WG_AGENT_ID").ok());
    let _ = workgraph::provenance::record(
        dir,
        "insert",
        Some(&assigned_id),
        actor.as_deref(),
        serde_json::json!({
            "target": target_id,
            "position": position.as_str(),
            "splice": opts.splice,
            "replace_edges": opts.replace_edges,
        }),
        config.log.rotation_threshold,
    );

    Ok(assigned_id)
}

fn derive_id_from_title(title: &str) -> String {
    let mut out = String::with_capacity(title.len());
    let mut prev_dash = false;
    for c in title.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.len() > 40 {
        trimmed[..40].trim_end_matches('-').to_string()
    } else {
        trimmed
    }
}

fn unique_id(graph: &workgraph::graph::WorkGraph, candidate: &str) -> String {
    if graph.get_task(candidate).is_none() {
        return candidate.to_string();
    }
    for n in 2..=999u32 {
        let tried = format!("{}-{}", candidate, n);
        if graph.get_task(&tried).is_none() {
            return tried;
        }
    }
    format!("{}-{}", candidate, Utc::now().timestamp_millis())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use workgraph::parser::load_graph;
    use workgraph::test_helpers::{make_task_with_status, setup_workgraph};

    fn make(id: &str) -> Task {
        make_task_with_status(id, id, Status::Open)
    }

    #[test]
    fn before_additive_keeps_old_predecessor_edge() {
        let dir = tempdir().unwrap();
        let mut p = make("p");
        let mut t = make("t");
        p.before = vec!["t".into()];
        t.after = vec!["p".into()];
        setup_workgraph(dir.path(), vec![p, t]);

        let new_id = run(
            dir.path(),
            Position::Before,
            "t",
            "new prereq",
            None,
            Some("n"),
            InsertOptions::default(),
        )
        .unwrap();
        assert_eq!(new_id, "n");

        let g = load_graph(&crate::commands::graph_path(dir.path())).unwrap();
        let t2 = g.get_task("t").unwrap();
        assert!(t2.after.contains(&"p".to_string()));
        assert!(t2.after.contains(&"n".to_string()));
        let n = g.get_task("n").unwrap();
        assert_eq!(n.before, vec!["t".to_string()]);
        assert!(n.after.is_empty());
    }

    #[test]
    fn before_splice_redirects_old_predecessor_through_new_node() {
        let dir = tempdir().unwrap();
        let mut p = make("p");
        let mut t = make("t");
        p.before = vec!["t".into()];
        t.after = vec!["p".into()];
        setup_workgraph(dir.path(), vec![p, t]);

        run(
            dir.path(),
            Position::Before,
            "t",
            "n",
            None,
            Some("n"),
            InsertOptions {
                splice: true,
                ..Default::default()
            },
        )
        .unwrap();

        let g = load_graph(&crate::commands::graph_path(dir.path())).unwrap();
        let t2 = g.get_task("t").unwrap();
        assert_eq!(t2.after, vec!["n".to_string()]);
        let n = g.get_task("n").unwrap();
        assert_eq!(n.after, vec!["p".to_string()]);
        let p2 = g.get_task("p").unwrap();
        assert!(p2.before.contains(&"n".to_string()));
        assert!(!p2.before.contains(&"t".to_string()));
    }

    #[test]
    fn after_additive_appends_successor() {
        let dir = tempdir().unwrap();
        let mut t = make("t");
        let mut s = make("s");
        t.before = vec!["s".into()];
        s.after = vec!["t".into()];
        setup_workgraph(dir.path(), vec![t, s]);

        run(
            dir.path(),
            Position::After,
            "t",
            "follow",
            None,
            Some("f"),
            InsertOptions::default(),
        )
        .unwrap();

        let g = load_graph(&crate::commands::graph_path(dir.path())).unwrap();
        let t2 = g.get_task("t").unwrap();
        assert!(t2.before.contains(&"s".to_string()));
        assert!(t2.before.contains(&"f".to_string()));
        let f = g.get_task("f").unwrap();
        assert_eq!(f.after, vec!["t".to_string()]);
        assert!(f.before.is_empty());
    }

    #[test]
    fn after_splice_redirects_old_successor() {
        let dir = tempdir().unwrap();
        let mut t = make("t");
        let mut s = make("s");
        t.before = vec!["s".into()];
        s.after = vec!["t".into()];
        setup_workgraph(dir.path(), vec![t, s]);

        run(
            dir.path(),
            Position::After,
            "t",
            "f",
            None,
            Some("f"),
            InsertOptions {
                splice: true,
                ..Default::default()
            },
        )
        .unwrap();

        let g = load_graph(&crate::commands::graph_path(dir.path())).unwrap();
        let t2 = g.get_task("t").unwrap();
        assert_eq!(t2.before, vec!["f".to_string()]);
        let f = g.get_task("f").unwrap();
        assert_eq!(f.before, vec!["s".to_string()]);
        let s2 = g.get_task("s").unwrap();
        assert!(s2.after.contains(&"f".to_string()));
        assert!(!s2.after.contains(&"t".to_string()));
    }

    #[test]
    fn parallel_additive_inherits_both_edge_sets_leaves_target_intact() {
        let dir = tempdir().unwrap();
        let mut p = make("p");
        let mut t = make("t");
        let mut s = make("s");
        p.before = vec!["t".into()];
        t.after = vec!["p".into()];
        t.before = vec!["s".into()];
        s.after = vec!["t".into()];
        setup_workgraph(dir.path(), vec![p, t, s]);

        run(
            dir.path(),
            Position::Parallel,
            "t",
            "alt",
            None,
            Some("alt"),
            InsertOptions::default(),
        )
        .unwrap();

        let g = load_graph(&crate::commands::graph_path(dir.path())).unwrap();
        let n = g.get_task("alt").unwrap();
        assert_eq!(n.after, vec!["p".to_string()]);
        assert_eq!(n.before, vec!["s".to_string()]);
        let t2 = g.get_task("t").unwrap();
        assert_eq!(t2.after, vec!["p".to_string()]);
        assert_eq!(t2.before, vec!["s".to_string()]);
        let s2 = g.get_task("s").unwrap();
        assert!(s2.after.contains(&"t".to_string()));
        assert!(s2.after.contains(&"alt".to_string()));
    }

    #[test]
    fn parallel_replace_edges_routes_successors_to_new_only() {
        let dir = tempdir().unwrap();
        let mut p = make("p");
        let mut t = make("t");
        let mut s = make("s");
        p.before = vec!["t".into()];
        t.after = vec!["p".into()];
        t.before = vec!["s".into()];
        s.after = vec!["t".into()];
        setup_workgraph(dir.path(), vec![p, t, s]);

        run(
            dir.path(),
            Position::Parallel,
            "t",
            "rescue",
            None,
            Some("rescue"),
            InsertOptions {
                replace_edges: true,
                ..Default::default()
            },
        )
        .unwrap();

        let g = load_graph(&crate::commands::graph_path(dir.path())).unwrap();
        let rescue = g.get_task("rescue").unwrap();
        assert_eq!(rescue.after, vec!["p".to_string()]);
        assert_eq!(rescue.before, vec!["s".to_string()]);
        let s2 = g.get_task("s").unwrap();
        assert_eq!(s2.after, vec!["rescue".to_string()]);
        let t2 = g.get_task("t").unwrap();
        assert!(t2.before.is_empty());
        assert_eq!(t2.after, vec!["p".to_string()]);
    }

    #[test]
    fn nonexistent_target_does_not_mutate_graph() {
        let dir = tempdir().unwrap();
        let t = make("t");
        setup_workgraph(dir.path(), vec![t]);

        let result = run(
            dir.path(),
            Position::Before,
            "missing",
            "n",
            None,
            Some("n"),
            InsertOptions::default(),
        );
        assert!(result.is_err());

        let g = load_graph(&crate::commands::graph_path(dir.path())).unwrap();
        assert!(g.get_task("n").is_none());
        assert!(g.get_task("t").is_some());
    }
}
