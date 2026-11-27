//! Built-in core tools: shell, file reading/writing/editing, glob, grep, and
//! MCP tool adapters. Each is a leaf over the core tool contract (`Tool`, `Ctx`).
//!
//! The design choice: grouping built-in tool implementations here keeps the
//! core contract (`Ctx`, `Tool`, `ToolSource`, `Catalog`) cleanly layered and
//! lets `Catalog` assemble them without any tool depending back on `Catalog`.

pub mod edit;
mod fs;
pub mod glob;
pub mod grep;
pub mod mcp;
pub mod read;
pub mod shell;
pub mod write;

use super::{Ctx, Output, Tool, ToolError, ToolSources};
use crate::config::ToolsConfig;
use anyhow::{Result, bail};
use std::sync::Arc;

/// Old wire names and the tool each now is (ADR 0046). An alias is accepted
/// wherever a user or an old journal names a tool, never offered to the model.
const RENAMED: &[(&[&str], &str)] = &[(shell::Shell::ALIASES, shell::Shell::NAME)];

/// The current name for `name`: itself, unless it is a renamed tool's old name.
/// Callers look `name` up as given first, so a tool that really has an old
/// name (a custom `search_name`, say) still wins.
pub fn canonical(name: &str) -> &str {
    RENAMED.iter().find(|(old, _)| old.contains(&name)).map_or(name, |(_, new)| new)
}

/// Construct core's built-in tools in definition order for `cfg`, `sources`, and `jobs`.
pub(super) fn core_tools(
    cfg: &ToolsConfig,
    sources: &ToolSources,
    jobs: &crate::jobs::Jobs,
) -> Result<Vec<Arc<dyn Tool>>> {
    if cfg.search_name.is_empty() || !cfg.search_name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        bail!("tools.search_name: {:?} is not a usable tool name", cfg.search_name);
    }
    Ok(vec![
        Arc::new(shell::Shell::new(&cfg.shell).with_sources(sources.clone()).with_jobs(jobs.clone())),
        Arc::new(read::Read),
        Arc::new(write::Write),
        Arc::new(edit::Edit),
        Arc::new(grep::Grep::named(&cfg.search_name)),
        Arc::new(glob::Glob),
    ])
}

/// Filesystem walks are synchronous; keep them off the async workers.
fn blocking(ctx: &Ctx, f: impl FnOnce() -> Result<String, ToolError>) -> Result<Output, ToolError> {
    // The walk borrows ctx, so use block_in_place rather than spawn_blocking.
    let r = tokio::task::block_in_place(f)?;
    Ok(Output::plain(ctx.cap_output("out", &r)))
}
