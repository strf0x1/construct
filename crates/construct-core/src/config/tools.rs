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
    /// The read/edit family: "replace" (old_string/new_string) or "hashline"
    /// (`LINE:HASH` anchors). One key picks a family, never a single tool,
    /// since the pair only works together.
    pub editor: String,
    /// Tool names that must not exist this session, even if the model asks.
    pub disabled: Vec<String>,
    /// The wire name of the content-search tool: only the name changes, never
    /// the schema or the description. A knob for measuring whether models
    /// reach for a tool by that name (docs/adr, issue #10).
    pub search_name: String,
    pub hashline: HashlineConfig,
    pub web: WebConfig,
    pub shell: crate::jobs::ShellConfig,
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
            editor: "replace".into(),
            disabled: vec![],
            search_name: SEARCH_TOOL.into(),
            hashline: HashlineConfig::default(),
            web: WebConfig::default(),
            shell: Default::default(),
        }
    }
}

/// Knobs for `editor = "hashline"`, the ones a bench sweeps.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct HashlineConfig {
    /// Lines of context on each side of a failed anchor in a rejection.
    pub window_radius: usize,
    /// Starting displayed-hash length; colliding hashes grow past it.
    pub min_hash_len: usize,
    /// Lines of context around each edited region in a success.
    pub context_lines: usize,
    /// Most ops accepted in one edit call.
    pub max_ops: usize,
    /// Strip anchors the model echoed into edit text. Every strip is reported.
    pub strip_echoes: bool,
    /// What an anchor is: `LINE:HASH`, or the hash alone.
    pub anchor: AnchorMode,
    /// How identical lines are told apart: their rank, or rank and count.
    pub salt: Salt,
}

/// `"line+hash"` pins where and what, so moved content is a loud mismatch.
/// `"hash"` names content wherever it is now, so an edit follows content
/// that moved: relocation, kept as a knob so a bench can measure it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum AnchorMode {
    #[default]
    #[serde(rename = "line+hash")]
    LineHash,
    #[serde(rename = "hash")]
    Hash,
}

/// `"rank"` salts a duplicate with its order among identical lines, as the
/// TypeScript reference does: a new copy above shifts which line a stale
/// anchor names. `"count"` adds how many copies there are, so adding or
/// removing any copy re-hashes them all and a stale anchor fails loudly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Salt {
    #[default]
    #[serde(rename = "rank")]
    Rank,
    #[serde(rename = "count")]
    Count,
}

impl Default for HashlineConfig {
    fn default() -> Self {
        Self {
            window_radius: 4,
            min_hash_len: 2,
            context_lines: 1,
            max_ops: 256,
            strip_echoes: true,
            anchor: AnchorMode::LineHash,
            salt: Salt::Rank,
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
