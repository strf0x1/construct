//! The tools this binary adds to every session beyond the engine's own.
//!
//! The design choice: this is the composition root's one place to register
//! them (#314). Core only offers `ToolSource`; which crates plug in is decided
//! here, so a build without Deck's tools is one line out. A source behind a
//! cargo feature (`web`) is the only `cfg` a tool crate costs (#271).

use construct_core::tools::ToolSources;

pub(super) fn tool_sources() -> ToolSources {
    let sources = ToolSources::default();
    #[cfg(feature = "web")]
    let sources = sources.with(construct_tools_web::Web);
    #[cfg(feature = "history")]
    let sources = sources.with(construct_tools_history::History);
    #[cfg(feature = "jobs")]
    let sources = sources.with(construct_tools_jobs::Jobs);
    sources.with(construct_deck::DeckTools)
}
