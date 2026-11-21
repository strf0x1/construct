//! The tools this binary adds to every session beyond the engine's own.
//!
//! The design choice: this is the composition root's one place to register
//! them (#314). Core only offers `ToolSource`; which crates plug in is decided
//! here, so a build without Deck's tools is one line out.

use construct_core::tools::ToolSources;

pub(super) fn tool_sources() -> ToolSources {
    ToolSources::default().with(construct_deck::DeckTools)
}
