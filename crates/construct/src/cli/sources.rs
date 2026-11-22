//! The tools this binary adds to every session beyond the engine's own.
//!
//! The design choice: this is the composition root's one place to register
//! them (#314). Core only offers `ToolSource`; which crates plug in is decided
//! here. Each source sits behind a cargo feature of the binary (ADR 0064), and
//! that `cfg` is the only one a tool crate costs.

use construct_core::tools::ToolSources;

/// In the order the tools were pushed when they lived in core, so the
/// manifest's list reads as it did.
pub(super) fn tool_sources() -> ToolSources {
    let sources = ToolSources::default();
    #[cfg(feature = "web")]
    let sources = sources.with(construct_tools_web::Web);
    #[cfg(feature = "memory")]
    let sources = sources.with(construct_tools_memory::MemoryTools);
    #[cfg(feature = "skills")]
    let sources = sources.with(construct_tools_skills::Skills);
    #[cfg(feature = "history")]
    let sources = sources.with(construct_tools_history::History);
    #[cfg(feature = "jobs")]
    let sources = sources.with(construct_tools_jobs::Jobs);
    #[cfg(feature = "deck")]
    let sources = sources.with(construct_deck::DeckTools);
    sources
}
