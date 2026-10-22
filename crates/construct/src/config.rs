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
    pub sandbox: SandboxConfig,
    pub tools: ToolsConfig,
    pub mcp: BTreeMap<String, McpServer>,
    /// Extra headers sent with every model request.
    pub headers: BTreeMap<String, String>,
    pub provider: ProviderConfig,
}

/// What the endpoint needs beyond the plain OpenAI body (`llm::dialect`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct ProviderConfig {
    pub dialect: Dialect,
    pub cache: CacheMode,
}

/// `"auto"` picks by `base_url`: openrouter.ai is `"openrouter"`, anything
/// else `"openai"`, the plain dialect every compatible server speaks (local
/// servers, vLLM, DeepSeek, OpenAI). Name one when a proxy hides the host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Dialect {
    #[default]
    Auto,
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "openrouter")]
    OpenRouter,
}

/// `"auto"` marks prompt-cache breakpoints for models whose provider only
/// caches when asked (Anthropic and Gemini behind OpenRouter); `"off"` never
/// marks any. Providers that cache on their own are unaffected either way.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CacheMode {
    #[default]
    Auto,
    Off,
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

/// An MCP server runs inside the sandbox like a shell command: the workspace
/// policy plus what its entry adds, a credential-free environment plus the
/// secrets it names.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct McpServer {
    pub command: String,
    pub args: Vec<String>,
    /// Literal variables set for the server.
    pub env: BTreeMap<String, String>,
    /// One line on what the server is for; the router reads it.
    pub description: String,
    /// Extra paths this server may read (`~/` expands), on top of [sandbox].
    pub allow_read: Vec<String>,
    /// Extra paths this server may read and write.
    pub allow_write: Vec<String>,
    /// TCP connect and bind. On by default: most servers are API clients.
    pub network: bool,
    /// Secret names resolved like construct's own (environment, then the
    /// keyring) and passed to this server only. No other credential reaches it.
    pub secrets: Vec<String>,
    /// Seconds a tool call may go without a response or a progress
    /// notification before it is cancelled.
    pub timeout: u64,
}

impl Default for McpServer {
    fn default() -> Self {
        Self {
            command: String::new(),
            args: vec![],
            env: BTreeMap::new(),
            description: String::new(),
            allow_read: vec![],
            allow_write: vec![],
            network: true,
            secrets: vec![],
            timeout: 300,
        }
    }
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
            sandbox: SandboxConfig::default(),
            tools: ToolsConfig::default(),
            mcp: BTreeMap::new(),
            headers: BTreeMap::new(),
            provider: ProviderConfig::default(),
        }
    }
}

impl Default for MemoryConfig {
    fn default() -> Self {
        // Hermes's defaults: small enough to stay in every prompt.
        Self { memory_chars: 2200, user_chars: 1375, project_chars: 1500 }
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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct SandboxConfig {
    /// "workspace" (default): tools read and write the working directory and
    /// read system paths and toolchains, nothing else under $HOME.
    /// "read-only": no writes outside construct's temp dir. "off": no boundary.
    pub mode: String,
    /// Extra paths tools may read (`~/` expands).
    pub allow_read: Vec<String>,
    /// Extra paths tools may read and write.
    pub allow_write: Vec<String>,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self { mode: "workspace".into(), allow_read: vec![], allow_write: vec![] }
    }
}

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
    pub hashline: HashlineConfig,
}

impl Default for ToolsConfig {
    fn default() -> Self {
        Self { editor: "replace".into(), disabled: vec![], hashline: HashlineConfig::default() }
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
        if let Ok(v) = std::env::var("CONSTRUCT_SANDBOX") {
            cfg.sandbox.mode = v;
        }
        if let Ok(v) = std::env::var("CONSTRUCT_ROUTER") {
            cfg.router.mode = v;
        }
        if let Ok(v) = std::env::var("CONSTRUCT_TOOLS_EDITOR") {
            cfg.tools.editor = v;
        }
        cfg.base_url = cfg.base_url.trim_end_matches('/').to_string();
        for name in cfg.mcp.keys() {
            crate::mcp::check_server_name(name).with_context(|| format!("in {}", path.display()))?;
        }
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
