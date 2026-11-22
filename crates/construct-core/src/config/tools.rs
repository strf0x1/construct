//! Which tools exist this session, and the knobs of the ones that have any.
//! Lives in `config` so a tool reads its settings from here rather than
//! `config` re-exporting a type from the module it configures (the same shape
//! `[learn]` had). The one wire name kept here is the search tool's default,
//! since `search_name` defaults to it; `tools::grep::Grep::NAME` is this.

use serde::{Deserialize, Serialize};

/// The content-search tool's wire name unless `[tools] search_name` says
/// otherwise (ADR 0024).
pub const SEARCH_TOOL: &str = "grep";

/// Which tools exist this session. A profile for an experiment is just a
/// config file with a different `[tools]` table.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct ToolsConfig {
    /// Tool names that must not exist this session, even if the model asks.
    pub disabled: Vec<String>,
    /// The wire name of the content-search tool: only the name changes, never
    /// the schema or the description. A knob for measuring whether models
    /// reach for a tool by that name (docs/adr, issue #10).
    pub search_name: String,
    pub web: WebConfig,
    pub shell: super::ShellConfig,
    /// Tool groups on from the first request instead of when routed or
    /// asked for (`web`, `memory`, `history`, `skills`, `developer`, `jobs`, ...).
    /// Every group switched on later changes the `tools` array, which comes
    /// first in a provider's cached prefix, so the next request misses the
    /// cache entirely (#187). Empty by default: the first request stays small.
    pub initial: Vec<String>,
}

impl ToolsConfig {
    /// The name the system prompt should call the content-search tool, or
    /// `None` when the session has no such tool.
    pub fn search_tool(&self) -> Option<&str> {
        (!self.disabled.iter().any(|d| d == SEARCH_TOOL)).then_some(self.search_name.as_str())
    }
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self {
            disabled: vec![],
            search_name: SEARCH_TOOL.into(),
            web: WebConfig::default(),
            shell: Default::default(),
            initial: vec![],
        }
    }
}

/// Knobs for the web tools.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct WebConfig {
    /// Which backend answers `web_search`: "auto" (the keyed backends in
    /// order Exa, Brave, Tavily, else DuckDuckGo) or one of "exa", "brave",
    /// "tavily", "ddg". A named backend is the only one tried.
    pub search: String,
}

impl Default for WebConfig {
    fn default() -> Self {
        Self { search: "auto".into() }
    }
}
