//! Launch-time resume selection. Bare `--resume` delegates to the same TUI
//! browser; `--continue` deliberately retains newest-started selection.

use crate::config::Paths;
use crate::journal;
use anyhow::{Result, ensure};

pub(super) fn resolve_resume(
    resume: &Option<Option<String>>,
    cont: bool,
    paths: &Paths,
    interactive: bool,
) -> Result<Option<String>> {
    if let Some(id) = resume {
        ensure!(
            id.is_some() || interactive,
            "--resume without an ID requires an interactive terminal; use --resume <id>"
        );
        return Ok(id.clone());
    }
    if cont {
        let cwd = std::env::current_dir()?;
        let file = journal::path(paths, &cwd);
        if !file.exists() {
            return Ok(None);
        }
        return journal::Journal::read_only(&file)?.latest(&cwd.display().to_string());
    }
    Ok(None)
}
