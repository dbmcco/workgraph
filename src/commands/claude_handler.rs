use std::path::Path;

use anyhow::{Result, bail};

pub fn run(_workgraph_dir: &Path, _chat: &str, _model: Option<&str>) -> Result<()> {
    bail!("claude-handler not implemented yet")
}
