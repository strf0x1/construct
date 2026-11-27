//! What the composition root registers (#314, #315): Deck's tools in one
//! `ToolSource`.
//!
//! The design choice: the tools are pushed in the order they had inside core
//! (`developer`, the three boundary tools, then `deck`), after the built-ins,
//! so request order is unchanged. Each exists only when its Deck surface
//! does: `developer` always, the boundary tools with a boundary endpoint, the
//! `deck` tool for a coordinator.

use crate::boundary::{Boundary, Status, Wait, Withdraw};
use crate::control::Control;
use crate::developer::{self, Developer};
use construct_core::config::ToolsConfig;
use construct_core::tools::boundary::BoundaryHost;
use construct_core::tools::{Deck, Tool, ToolSource};
use std::path::Path;
use std::sync::Arc;

pub struct DeckTools;

impl ToolSource for DeckTools {
    fn tools(
        &self,
        _cfg: &ToolsConfig,
        deck: &Deck,
        _jobs: &construct_core::jobs::Jobs,
    ) -> anyhow::Result<Vec<Arc<dyn Tool>>> {
        let mut tools: Vec<Arc<dyn Tool>> = vec![Arc::new(Developer::new(deck.github.clone()))];
        if let Some(boundary) = deck.boundary.clone() {
            tools.push(Arc::new(Wait::new(boundary.clone())));
            tools.push(Arc::new(Status::new(boundary.clone())));
            tools.push(Arc::new(Withdraw::new(boundary)));
        }
        tools.extend(deck.control.iter().map(|url| Arc::new(Control::new(url)) as Arc<dyn Tool>));
        Ok(tools)
    }

    fn boundary(&self, base_url: &str) -> Option<Arc<dyn BoundaryHost>> {
        Some(Arc::new(Boundary::new(base_url)))
    }

    fn worktree_note(&self, git_dir: &Path) -> Option<String> {
        Some(developer::worktree_note(git_dir))
    }
}
