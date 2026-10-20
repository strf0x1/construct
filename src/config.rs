//! Paths and settings. Everything lives under one home directory
//! ($CONSTRUCT_HOME, default ~/.construct) so a fresh home is a fresh harness.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Any OpenAI-compatible endpoint, up to and including `/v1`.
    pub base_url: String,
    pub model: String,
    /// Name of the secret holding the endpoint key (resolved through secretspec).
    pub api_key_secret: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    /// Tool-call rounds per user turn before the agent must stop.
    pub max_steps: u32,
    /// Tokens the model can take; history is compacted at 75% of this.
    pub context_window: u32,
    pub memory: MemoryConfig,
    pub learn: LearnConfig,
    pub router: RouterConfig,
    pub mcp: BTreeMap<String, McpServer>,
    /// Extra headers sent with every model request.
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    pub memory_chars: usize,
    pub user_chars: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LearnConfig {
    pub enabled: bool,
    /// Review memory after this many user turns.
    pub memory_every_turns: u32,
    /// Review skills after this many tool calls.
    pub skill_every_tool_calls: u32,
    /// Model for the learning pass; empty means the main model.
    pub model: String,
    /// Days between curator passes over agent-written skills.
    pub curator_every_days: u32,
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct McpServer {
    pub command: String,
    pub args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// One line on what the server is for; the router reads it.
    pub description: String,
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
            context_window: 131_072,
            memory: MemoryConfig::default(),
            learn: LearnConfig::default(),
            router: RouterConfig::default(),
            mcp: BTreeMap::new(),
            headers: BTreeMap::new(),
        }
    }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        // Hermes's defaults: small enough to stay in every prompt.
        Self { memory_chars: 2200, user_chars: 1375 }
    }
}

impl Default for LearnConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            memory_every_turns: 6,
            skill_every_tool_calls: 10,
            model: String::new(),
            curator_every_days: 7,
        }
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

#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
}

impl Paths {
    pub fn resolve() -> Result<Self> {
        let home = match std::env::var_os("CONSTRUCT_HOME") {
            Some(h) if !h.is_empty() => PathBuf::from(h),
            _ => {
                let user = std::env::var_os("HOME").context("HOME is not set")?;
                PathBuf::from(user).join(".construct")
            }
        };
        Ok(Self { home })
    }
    pub fn config(&self) -> PathBuf {
        self.home.join("config.toml")
    }
    pub fn memory_dir(&self) -> PathBuf {
        self.home.join("memory")
    }
    pub fn skills_dir(&self) -> PathBuf {
        self.home.join("skills")
    }
    pub fn sessions_dir(&self) -> PathBuf {
        self.home.join("sessions")
    }
    pub fn state_file(&self) -> PathBuf {
        self.home.join("state.json")
    }
    pub fn learn_log(&self) -> PathBuf {
        self.home.join("learn.log")
    }
    pub fn spill_dir(&self) -> PathBuf {
        self.home.join("spill")
    }
}

impl Config {
    pub fn load(paths: &Paths) -> Result<Self> {
        let path = paths.config();
        let mut cfg: Config = match std::fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("parsing {}", path.display()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        // Environment overrides make one-off runs and benches easy.
        if let Ok(v) = std::env::var("CONSTRUCT_BASE_URL") {
            cfg.base_url = v;
        }
        if let Ok(v) = std::env::var("CONSTRUCT_MODEL") {
            cfg.model = v;
        }
        cfg.base_url = cfg.base_url.trim_end_matches('/').to_string();
        Ok(cfg)
    }

    pub fn learn_model(&self) -> &str {
        if self.learn.model.is_empty() { &self.model } else { &self.learn.model }
    }
}

/// Write via a temp file and rename so readers never see half a file.
pub fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}
