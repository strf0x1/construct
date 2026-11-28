//! Paths and settings. Everything lives under one home directory
//! ($CONSTRUCT_HOME, default ~/.construct) so a fresh home is a fresh harness.

pub use crate::endpoint::{CacheMode, Dialect, ProviderConfig, Reasoning};
pub use crate::home::{Paths, write_atomic};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

mod baked;
pub mod context;
pub mod deck;
mod jobs;
pub mod learn;
mod llm;
mod mcp;
mod sandbox;
pub mod stamp;
mod tools;
pub use jobs::{JobsConfig, ShellConfig};
pub use llm::{Llm, LlmGateway};
pub use sandbox::SandboxConfig;
pub use tools::{SEARCH_TOOL, ToolsConfig, WebConfig};
pub use {context::ContextConfig, deck::DeckConfig, learn::LearnConfig, mcp::McpServer};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Any OpenAI-compatible endpoint, up to and including `/v1`.
    pub base_url: String,
    pub model: String,
    /// Name of the secret holding the endpoint key (resolved through secretspec).
    pub api_key_secret: String,
    /// Where a peer's model calls go: `local` or `hub` (`config/llm.rs`).
    #[serde(default, skip_serializing_if = "Llm::is_local")]
    pub llm: Llm,
    /// The hub gateway `construct peer` runs for `llm = "hub"`; never in a file.
    #[serde(skip)]
    pub llm_gateway: Option<LlmGateway>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    /// Tool-call rounds per user turn before the agent must stop.
    pub max_steps: u32,
    /// Identical tool-call rounds in a row before the model is told it is
    /// looping; one more ends the turn. 0 turns the guard off.
    pub max_repeats: u32,
    /// Tokens the model can take; compaction plans against this less the output reserve.
    pub context_window: u32,
    /// Whether the config file set `context_window` (the endpoint's list then supplies none).
    #[serde(skip)]
    pub window_pinned: bool,
    pub compaction: CompactionConfig,
    pub context: ContextConfig,
    pub memory: MemoryConfig,
    pub learn: LearnConfig,
    pub router: RouterConfig,
    pub sandbox: SandboxConfig,
    pub tools: ToolsConfig,
    /// Deck's boundary status endpoint, set by the VM runtime (`DECK_BOUNDARY_URL`).
    /// Absent outside a Deck task, which keeps the boundary tools out of the
    /// catalog; skipped when absent so an ordinary session's config hash does not change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deck: Option<DeckConfig>,
    /// Deck's GitHub gateway (`DECK_GITHUB_URL`); see `config/deck.rs`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_gateway: Option<String>,
    pub jobs: JobsConfig,
    pub mcp: BTreeMap<String, McpServer>,
    /// Extra headers sent with every model request.
    pub headers: BTreeMap<String, String>,
    pub provider: ProviderConfig,
    /// Named endpoints besides the top-level one (`providers.rs`, ADR 0027).
    pub providers: BTreeMap<String, crate::providers::Provider>,
    /// The model catalog (`models.rs`): what an id means here. The top-level
    /// `context_window`, `max_tokens`, endpoint and provider are the defaults
    /// for an id with no entry.
    pub models: BTreeMap<String, crate::models::ModelEntry>,
    /// The top-level values as they were before an entry covered them.
    #[serde(skip)]
    pub defaults: Option<Box<crate::models::Defaults>>,
    /// Entries read from the endpoint's own list (`llm::overlay::overlay`).
    /// Never written back, and `models` wins an id in both.
    #[serde(skip)]
    pub discovered: BTreeMap<String, crate::models::ModelEntry>,
    /// Which layer set each key (baked/stamped/file/env), for `construct config`.
    #[serde(skip)]
    pub provenance: baked::Sources,
}

/// When and how history is cut to fit the context window (see `compact`).
/// Fractions are of the budget: the window less the output reserve.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct CompactionConfig {
    /// Estimated prompt size, as a fraction of the budget, that triggers a cut.
    pub threshold: f32,
    /// What a cut must bring the estimate under.
    pub target: f32,
    /// Most recent rounds (an assistant message and its tool results) whose
    /// results are never elided.
    pub keep_results: usize,
    /// Most recent rounds kept verbatim when older history is summarized.
    pub keep_rounds: usize,
    /// Elide a result once it is this many rounds old, whatever the size.
    /// 0 turns it off: every age cut changes the cached prefix.
    pub age_rounds: usize,
    /// Model that writes summaries; empty means the run's model.
    pub model: String,
}

impl Default for CompactionConfig {
    fn default() -> Self {
        Self { threshold: 0.7, target: 0.5, keep_results: 2, keep_rounds: 4, age_rounds: 0, model: String::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    pub memory_chars: usize,
    pub user_chars: usize,
    /// Cap on the current repo's notes (`$CONSTRUCT_HOME/projects/<id>/NOTES.md`).
    pub project_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct RouterConfig {
    /// "auto" uses Jev when TYPESAFE_API_KEY resolves, otherwise the local
    /// lexical router. "jev", "local", or "off" force one.
    pub mode: String,
    pub jev_model: String,
    pub jev_url: String,
    /// Noul probability above which a tool group is switched on.
    pub threshold: f32,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            base_url: "https://openrouter.ai/api/v1".into(),
            model: "openrouter/free".into(),
            api_key_secret: "OPENAI_API_KEY".into(),
            max_tokens: None,
            temperature: None,
            max_steps: 200,
            max_repeats: 5,
            context_window: 131_072,
            window_pinned: false,
            compaction: CompactionConfig::default(),
            context: ContextConfig::default(),
            memory: MemoryConfig::default(),
            learn: LearnConfig::default(),
            router: RouterConfig::default(),
            sandbox: SandboxConfig::default(),
            tools: ToolsConfig::default(),
            deck: None,
            github_gateway: None,
            llm: Llm::Local,
            llm_gateway: None,
            jobs: Default::default(),
            mcp: BTreeMap::new(),
            headers: BTreeMap::new(),
            provider: ProviderConfig::default(),
            providers: BTreeMap::new(),
            models: BTreeMap::new(),
            defaults: None,
            discovered: BTreeMap::new(),
            provenance: BTreeMap::new(),
        }
    }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        // Hermes's defaults: small enough to stay in every prompt.
        Self { memory_chars: 2200, user_chars: 1375, project_chars: 1500 }
    }
}

impl Default for RouterConfig {
    fn default() -> Self {
        Self {
            mode: "auto".into(),
            jev_model: "jev-latest".into(),
            jev_url: "https://api.typesafe.ai/v1/systemone".into(),
            threshold: 0.5,
        }
    }
}

impl Config {
    pub fn load(paths: &Paths) -> Result<Self> {
        let path = paths.config();
        // The baked and stamped layers sit under the file; env overrides sit
        // over all three. A broken stamp is an error here, before anything runs.
        let stamped = stamp::current()?;
        let mut merged = baked::load(&path, stamped.map(|s| &s.config))?;
        baked::apply_env(&mut merged);
        let mut cfg: Config = toml::Value::Table(merged.table)
            .try_into()
            .with_context(|| format!("in the merged config ({})", path.display()))?;
        cfg.window_pinned = merged.window_pinned;
        cfg.provenance = merged.provenance;
        cfg.context.apply_env();
        cfg.apply_deck_env();
        cfg.base_url = cfg.base_url.trim_end_matches('/').to_string();
        cfg.load_catalog(&paths.models())?;
        // The model's entry (limits, endpoint) is laid over the defaults here,
        // so every consumer sees the resolved values.
        cfg.check_providers().with_context(|| format!("in {}", path.display()))?;
        cfg.select_model(&cfg.model.clone())?;
        cfg.sandbox.memory_max_bytes().map_err(anyhow::Error::msg).with_context(|| format!("in {}", path.display()))?;
        for name in cfg.mcp.keys() {
            mcp::check_server_name(name).with_context(|| format!("in {}", path.display()))?;
        }
        Ok(cfg)
    }

    /// The top-level endpoint, as `config.toml` declares it.
    pub fn declared_endpoint(&self) -> crate::endpoint::Endpoint {
        crate::endpoint::Endpoint::declared(
            &self.base_url,
            &self.api_key_secret,
            self.headers.clone(),
            self.provider.clone(),
        )
    }

    pub fn learn_model(&self) -> &str {
        if self.learn.model.is_empty() { &self.model } else { &self.learn.model }
    }
}
