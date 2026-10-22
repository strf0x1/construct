//! What produced a result: the build, the effective config and the tools.
//! Carried by every session's head event in the journal, sent as the first
//! event of a turn, and printed by `construct manifest` for the bench to record.

use crate::config::{CacheMode, CompactionConfig, Config, HashlineConfig};
use crate::tools::Catalog;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub version: String,
    pub git_sha: String,
    pub git_dirty: bool,
    /// Only from `construct manifest`: hashing a debug binary takes too long
    /// to do on every session start, and the git sha already names the build.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary_sha256: Option<String>,
    /// Hash of the effective config (file plus environment overrides).
    pub config_sha256: String,
    pub tools: Vec<String>,
    pub editor: String,
    /// The editor's knobs, when it has any, so bench arms that differ only
    /// in them (`anchor`, `window_radius`) are told apart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hashline: Option<HashlineConfig>,
    pub model: String,
    pub router_mode: String,
    /// Configured agent definitions a model can delegate to (`[agents.*]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<String>,
    /// The window compaction plans against, and its policy, so compaction
    /// arms are told apart. Absent in manifests from before the planner.
    #[serde(default)]
    pub context_window: u32,
    #[serde(default)]
    pub compaction: CompactionConfig,
    /// The resolved request dialect (`llm::dialect`), and whether it may
    /// mark cache breakpoints.
    #[serde(default)]
    pub dialect: String,
    #[serde(default)]
    pub cache: CacheMode,
}

impl Manifest {
    /// The manifest of a session construct didn't run (an ACP agent from
    /// another vendor): only what the parent knows of it.
    pub fn foreign(cfg: &Config) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").into(),
            git_sha: env!("CONSTRUCT_GIT_SHA").into(),
            git_dirty: env!("CONSTRUCT_GIT_DIRTY") == "true",
            binary_sha256: None,
            config_sha256: sha256(&serde_json::to_vec(cfg).unwrap_or_default()),
            tools: vec![],
            editor: String::new(),
            hashline: None,
            model: String::new(),
            router_mode: String::new(),
            agents: vec![],
            context_window: 0,
            compaction: Default::default(),
            dialect: String::new(),
            cache: Default::default(),
        }
    }

    pub fn new(cfg: &Config, catalog: &Catalog) -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").into(),
            git_sha: env!("CONSTRUCT_GIT_SHA").into(),
            git_dirty: env!("CONSTRUCT_GIT_DIRTY") == "true",
            binary_sha256: None,
            config_sha256: sha256(&serde_json::to_vec(cfg).unwrap_or_default()),
            tools: catalog.names(),
            editor: cfg.tools.editor.clone(),
            hashline: (cfg.tools.editor == "hashline").then(|| cfg.tools.hashline.clone()),
            model: cfg.model.clone(),
            router_mode: cfg.router.mode.clone(),
            agents: cfg.agents.keys().cloned().collect(),
            context_window: cfg.context_window,
            compaction: cfg.compaction.clone(),
            dialect: crate::llm::Provider::new(&cfg.provider, &cfg.base_url).name().into(),
            cache: cfg.provider.cache,
        }
    }

    /// Add the running binary's hash.
    pub fn with_binary_hash(mut self) -> Self {
        self.binary_sha256 =
            std::env::current_exe().ok().and_then(|p| std::fs::read(p).ok()).map(|bytes| sha256(&bytes));
        self
    }
}

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect()
}
