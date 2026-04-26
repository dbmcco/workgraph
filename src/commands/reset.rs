use std::collections::HashSet;
use std::path::Path;

use anyhow::Result;
use chrono::Utc;

use workgraph::graph::{LogEntry, Status, WorkGraph};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Forward,
    Backward,
    Both,
}

impl std::str::FromStr for Direction {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        match s.to_ascii_lowercase().as_str() {
            "forward" | "down" | "downstream" => Ok(Direction::Forward),
            "backward" | "up" | "upstream" => Ok(Direction::Backward),
            "both" => Ok(Direction::Both),
            other => Err(format!(
                "invalid direction '{}' — must be one of: forward, backward, both",
                other
            )),
        }
    }
}

pub struct ResetOptions {
    pub direction: Direction,
    pub also_strip_meta: bool,
    pub dry_run: bool,
    pub yes: bool,
}

#[allow(dead_code)]
#[derive(Debug, Default)]
pub struct ResetReport {
    pub closure: Vec<String>,
    pub meta_to_strip: Vec<String>,
    pub was_dry_run: bool,
    pub reset_count: usize,
    pub stripped_count: usize,
}

pub fn run(dir: &Path, seeds: &[String], opts: ResetOptions) -> Result<ResetReport> {
    if seeds.is_empty() {
        anyhow::bail!("reset requires at least one seed task id");
    }

    let (graph, _) = super::load_workgraph(dir)?;
    let missing: Vec<String> = seeds
        .iter()
        .filter(|seed| graph.get_task(seed).is_none())
        .cloned()
        .collect();
    if !missing.is_empty() {
        anyhow::bail!("seed task(s) not found: {}", missing.join(", "));
    }

    let closure = compute_closure(&graph, seeds, opts.direction);
    let meta_to_strip = if opts.also_strip_meta {
        find_meta_attached_to_closure(&graph, &closure)
    } else {
        HashSet::new()
    };

    let mut closure_sorted: Vec<String> = closure.iter().cloned().collect();
    closure_sorted.sort();
    let mut meta_sorted: Vec<String> = meta_to_strip.iter().cloned().collect();
    meta_sorted.sort();

    eprintln!(
        "\x1b[1mwg reset\x1b[0m — seeds={}, direction={:?}, closure={} task(s), meta-to-strip={}",
        seeds.join(","),
        opts.direction,
        closure_sorted.len(),
        meta_sorted.len(),
    );
    for id in &closure_sorted {
        eprintln!("  • {}", id);
    }
    if opts.also_strip_meta && !meta_sorted.is_empty() {
        eprintln!("  meta tasks that would be stripped:");
        for id in &meta_sorted {
            eprintln!("    - {}", id);
        }
    }

    if opts.dry_run {
        eprintln!("\x1b[2m(dry run — no changes applied; drop --dry-run to execute)\x1b[0m");
        return Ok(ResetReport {
            closure: closure_sorted,
            meta_to_strip: meta_sorted,
            was_dry_run: true,
            reset_count: 0,
            stripped_count: 0,
        });
    }

    let destructive_count = closure_sorted.len() + meta_sorted.len();
    if destructive_count > 1 && !opts.yes {
        anyhow::bail!(
            "refusing to reset {} tasks without --yes (use --dry-run first to preview, then re-run with --yes)",
            destructive_count
        );
    }

    let closure_set: HashSet<String> = closure_sorted.iter().cloned().collect();
    let meta_set: HashSet<String> = meta_sorted.iter().cloned().collect();
    let seed_summary = seeds.join(",");

    let (reset_count, stripped_count) = super::mutate_workgraph(dir, |graph| {
        let mut reset_count = 0usize;
        let mut stripped_count = 0usize;
        let now = Utc::now().to_rfc3339();

        for id in &closure_set {
            if let Some(task) = graph.get_task_mut(id) {
                let prev = task.status;
                task.status = Status::Open;
                task.assigned = None;
                task.started_at = None;
                task.completed_at = None;
                task.failure_reason = None;
                task.retry_count = 0;
                task.ready_after = None;
                task.wait_condition = None;
                task.checkpoint = None;
                task.paused = false;
                task.log.push(LogEntry {
                    timestamp: now.clone(),
                    actor: Some("reset".to_string()),
                    message: format!("reset via `wg reset {}`; was {:?}", seed_summary, prev),
                });
                reset_count += 1;
            }
        }

        for id in &meta_set {
            if graph.remove_node(id).is_some() {
                stripped_count += 1;
            }
        }

        Ok((reset_count, stripped_count))
    })?;

    let config = workgraph::config::Config::load_or_default(dir);
    let _ = workgraph::provenance::record(
        dir,
        "reset",
        None,
        Some("reset"),
        serde_json::json!({
            "seeds": seeds,
            "direction": format!("{:?}", opts.direction),
            "closure": closure_sorted,
            "meta_stripped": meta_sorted,
            "reset_count": reset_count,
            "stripped_count": stripped_count,
        }),
        config.log.rotation_threshold,
    );

    super::notify_graph_changed(dir);

    eprintln!(
        "\x1b[32m✓\x1b[0m reset {} task(s), stripped {} meta task(s)",
        reset_count, stripped_count
    );

    Ok(ResetReport {
        closure: closure_sorted,
        meta_to_strip: meta_sorted,
        was_dry_run: false,
        reset_count,
        stripped_count,
    })
}

fn compute_closure(graph: &WorkGraph, seeds: &[String], direction: Direction) -> HashSet<String> {
    let mut visited = HashSet::new();
    let mut stack: Vec<String> = seeds
        .iter()
        .filter(|seed| !workgraph::graph::is_system_task(seed))
        .cloned()
        .collect();

    while let Some(id) = stack.pop() {
        if !visited.insert(id.clone()) {
            continue;
        }
        let Some(task) = graph.get_task(&id) else {
            continue;
        };
        let next_ids = match direction {
            Direction::Forward => task.before.clone(),
            Direction::Backward => task.after.clone(),
            Direction::Both => {
                let mut ids = task.before.clone();
                ids.extend(task.after.iter().cloned());
                ids
            }
        };
        for next_id in next_ids {
            if !visited.contains(&next_id) && !workgraph::graph::is_system_task(&next_id) {
                stack.push(next_id);
            }
        }
    }

    visited
}

fn find_meta_attached_to_closure(graph: &WorkGraph, closure: &HashSet<String>) -> HashSet<String> {
    graph
        .tasks()
        .filter(|task| workgraph::graph::is_system_task(&task.id))
        .filter(|task| {
            task.after.iter().any(|dep| closure.contains(dep))
                || task.before.iter().any(|dep| closure.contains(dep))
        })
        .map(|task| task.id.clone())
        .collect()
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

    fn write_chain(dir: &Path) {
        let mut p = make("p", Status::Done);
        let mut t = make("t", Status::Failed);
        let mut s = make("s", Status::Open);
        p.before = vec!["t".into()];
        t.after = vec!["p".into()];
        t.before = vec!["s".into()];
        s.after = vec!["t".into()];

        let mut flip = make(".flip-t", Status::Done);
        flip.after = vec!["t".into()];
        let mut eval = make(".evaluate-t", Status::Open);
        eval.after = vec!["t".into()];

        setup_workgraph(dir, vec![p, t, s, flip, eval]);
    }

    #[test]
    fn forward_closure_includes_seeds_and_downstream() {
        let dir = tempdir().unwrap();
        write_chain(dir.path());
        let (g, _) = crate::commands::load_workgraph(dir.path()).unwrap();
        let c = compute_closure(&g, &["t".to_string()], Direction::Forward);
        let mut ids: Vec<String> = c.into_iter().collect();
        ids.sort();
        assert_eq!(ids, vec!["s".to_string(), "t".to_string()]);
    }

    #[test]
    fn backward_closure_includes_seeds_and_upstream() {
        let dir = tempdir().unwrap();
        write_chain(dir.path());
        let (g, _) = crate::commands::load_workgraph(dir.path()).unwrap();
        let c = compute_closure(&g, &["t".to_string()], Direction::Backward);
        let mut ids: Vec<String> = c.into_iter().collect();
        ids.sort();
        assert_eq!(ids, vec!["p".to_string(), "t".to_string()]);
    }

    #[test]
    fn both_closure_includes_everything_reachable() {
        let dir = tempdir().unwrap();
        write_chain(dir.path());
        let (g, _) = crate::commands::load_workgraph(dir.path()).unwrap();
        let c = compute_closure(&g, &["t".to_string()], Direction::Both);
        let mut ids: Vec<String> = c.into_iter().collect();
        ids.sort();
        assert_eq!(ids, vec!["p".to_string(), "s".to_string(), "t".to_string()]);
    }

    #[test]
    fn closure_skips_system_tasks() {
        let dir = tempdir().unwrap();
        write_chain(dir.path());
        let (g, _) = crate::commands::load_workgraph(dir.path()).unwrap();
        let c = compute_closure(&g, &["t".to_string()], Direction::Both);
        assert!(!c.contains(".flip-t"));
        assert!(!c.contains(".evaluate-t"));
    }

    #[test]
    fn meta_attached_to_closure_is_found() {
        let dir = tempdir().unwrap();
        write_chain(dir.path());
        let (g, _) = crate::commands::load_workgraph(dir.path()).unwrap();
        let closure: HashSet<String> = ["t", "s"].iter().map(|s| s.to_string()).collect();
        let meta = find_meta_attached_to_closure(&g, &closure);
        let mut ids: Vec<String> = meta.into_iter().collect();
        ids.sort();
        assert_eq!(ids, vec![".evaluate-t".to_string(), ".flip-t".to_string()]);
    }

    #[test]
    fn dry_run_mutates_nothing() {
        let dir = tempdir().unwrap();
        write_chain(dir.path());

        run(
            dir.path(),
            &["t".to_string()],
            ResetOptions {
                direction: Direction::Forward,
                also_strip_meta: true,
                dry_run: true,
                yes: true,
            },
        )
        .unwrap();

        let g = load_graph(&super::super::graph_path(dir.path())).unwrap();
        assert_eq!(g.get_task("t").unwrap().status, Status::Failed);
        assert!(g.get_task(".flip-t").is_some());
        assert!(g.get_task(".evaluate-t").is_some());
    }

    #[test]
    fn full_reset_clears_statuses_and_strips_meta() {
        let dir = tempdir().unwrap();
        write_chain(dir.path());

        let report = run(
            dir.path(),
            &["t".to_string()],
            ResetOptions {
                direction: Direction::Forward,
                also_strip_meta: true,
                dry_run: false,
                yes: true,
            },
        )
        .unwrap();

        assert_eq!(report.reset_count, 2);
        assert_eq!(report.stripped_count, 2);

        let g = load_graph(&super::super::graph_path(dir.path())).unwrap();
        let t = g.get_task("t").unwrap();
        assert_eq!(t.status, Status::Open);
        assert!(t.failure_reason.is_none());
        assert!(t.log.iter().any(|e| e.message.contains("reset via")));
        let s = g.get_task("s").unwrap();
        assert_eq!(s.status, Status::Open);
        assert!(g.get_task(".flip-t").is_none());
        assert!(g.get_task(".evaluate-t").is_none());
        assert_eq!(g.get_task("p").unwrap().status, Status::Done);
    }

    #[test]
    fn refuses_multi_task_reset_without_yes() {
        let dir = tempdir().unwrap();
        write_chain(dir.path());

        let result = run(
            dir.path(),
            &["t".to_string()],
            ResetOptions {
                direction: Direction::Forward,
                also_strip_meta: false,
                dry_run: false,
                yes: false,
            },
        );
        assert!(result.is_err());
        assert!(format!("{}", result.err().unwrap()).contains("--yes"));
    }

    #[test]
    fn unknown_seed_errors_cleanly_without_mutation() {
        let dir = tempdir().unwrap();
        write_chain(dir.path());

        let result = run(
            dir.path(),
            &["nonexistent".to_string()],
            ResetOptions {
                direction: Direction::Forward,
                also_strip_meta: false,
                dry_run: true,
                yes: false,
            },
        );
        assert!(result.is_err());

        let g = load_graph(&super::super::graph_path(dir.path())).unwrap();
        assert_eq!(g.get_task("t").unwrap().status, Status::Failed);
    }
}
