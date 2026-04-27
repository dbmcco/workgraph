use std::path::Path;

use anyhow::{Result, bail};

pub fn run(
    _workgraph_dir: &Path,
    _task_id: &str,
    _role_override: Option<&str>,
    _dry_run: bool,
) -> Result<()> {
    bail!("spawn-task not implemented yet")
}
