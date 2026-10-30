//! Agent definitions: the `[agents.<name>]` tables a model can delegate to.
//! A definition is data; [`AgentDef::child_config`] turns one into the
//! effective `Config` of the child process.
//!
//! The design choice: containment is decided by subtraction from the parent.
//! A child's tools are the parent catalog minus everything its allowlist
//! leaves out, minus whole groups no child may have (memory, skill writes,
//! history); a child has no MCP servers and no agent definitions, so it
//! cannot delegate; and it never learns. Nothing here can widen what the
//! parent has.

use crate::config::Config;
use crate::tools::Group;
use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Variables construct sets for a child; a definition's `env` may not.
const RESERVED_ENV: &[&str] = &["CONSTRUCT_HOME", crate::child::DEPTH_ENV, "TMPDIR"];

/// Groups a child never has, whatever its allowlist says: memory and skill
/// writes change what future sessions are told, and history is the parent's
/// journal, which a child with a narrowed sandbox must not read.
const DENIED: &[Group] = &[Group::Memory, Group::SkillWrite, Group::History];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentDef {
    /// One line on what this agent is for; the model reads it to choose.
    pub description: String,
    /// How the parent talks to the child: `"jsonl"` (construct's own event
    /// lines, the default) or `"acp"` (the Agent Client Protocol).
    pub protocol: String,
    /// Run this program instead of construct (`protocol = "acp"` only): a
    /// codex, claude, gemini or opencode ACP adapter. Its tools, prompt and
    /// model are its own, so `tools`, `prompt`, `model` and `base_url` are
    /// refused; what the parent decides is the sandbox, the environment, the
    /// timeout and `on_permission`.
    pub command: Vec<String>,
    /// What to answer the child's `session/request_permission`: `"ask"` (the
    /// default: the user is asked in the TUI, and a session without one
    /// rejects), `"reject"` or `"allow"`. Either way the request and the
    /// answer are journaled (`Event::Permission`).
    pub on_permission: String,
    /// ACP tool kinds (`read`, `edit`, `execute`, ...) answered with an allow
    /// without asking, whatever `on_permission` says. "Always" in the prompt
    /// adds a kind here.
    pub allow_kinds: Vec<String>,
    /// The child's system prompt. `prompt_file` is read when it is empty.
    pub prompt: String,
    pub prompt_file: String,
    /// Empty: the parent's.
    pub model: String,
    pub base_url: String,
    /// Secret holding the endpoint key, passed to the child. Empty with no
    /// `base_url` means the parent's; empty with a `base_url` means no key,
    /// so the parent's credential never reaches a different host.
    pub api_key_secret: String,
    /// Tools the child has. Empty means none: a child gets what it is given.
    pub tools: Vec<String>,
    /// Paths the child may read and write (`~/` expands), each inside what the
    /// parent may. The parent's working directory is not implied.
    pub allow_read: Vec<String>,
    pub allow_write: Vec<String>,
    /// TCP connect and bind. Off by default.
    pub network: bool,
    /// Secret names resolved like construct's own and passed to this child only.
    pub secrets: Vec<String>,
    /// Plain environment variables for the child (`CODEX_HOME`, a model
    /// choice). Values are literals, so a credential belongs in `secrets`;
    /// the names construct sets itself are refused.
    pub env: BTreeMap<String, String>,
    /// Tokens the model can take; 0 is the model's catalog entry, else the
    /// parent's. Compaction plans against it.
    pub context_window: u32,
    /// Tool-call rounds before the child must stop.
    pub max_turns: u32,
    /// Seconds before the parent kills the child.
    pub timeout: u64,
    /// Calls to this agent that may run in the background at once (counted
    /// against `[jobs] limit` too). 0 turns background runs off.
    pub max_background: usize,
}

impl Default for AgentDef {
    fn default() -> Self {
        Self {
            description: String::new(),
            protocol: String::new(),
            command: vec![],
            on_permission: String::new(),
            allow_kinds: vec![],
            prompt: String::new(),
            prompt_file: String::new(),
            model: String::new(),
            base_url: String::new(),
            api_key_secret: String::new(),
            tools: vec![],
            allow_read: vec![],
            allow_write: vec![],
            network: false,
            secrets: vec![],
            env: BTreeMap::new(),
            context_window: 0,
            max_turns: 30,
            timeout: 600,
            max_background: 2,
        }
    }
}

pub fn check_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name.len() <= 32 && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "[agents.{name}]: an agent name is 1-32 letters, digits, '-' or '_'"
    );
    Ok(())
}

/// ACP's tool kinds, which `allow_kinds` draws from.
pub const ACP_KINDS: &[&str] =
    &["read", "edit", "delete", "move", "search", "execute", "think", "fetch", "switch_mode", "other"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OnPermission {
    /// Ask the user; with nobody to ask, reject.
    Ask,
    Allow,
    Reject,
}

/// A definition's standing answer to `session/request_permission`.
#[derive(Debug, Clone)]
pub struct Gate {
    pub on: OnPermission,
    pub kinds: Vec<String>,
}

impl Gate {
    pub fn reject() -> Self {
        Self { on: OnPermission::Reject, kinds: vec![] }
    }
}

/// Which wire a definition's child speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Jsonl,
    Acp,
}

impl AgentDef {
    /// The wire, or why the definition names an impossible one.
    pub fn protocol(&self, name: &str) -> Result<Protocol> {
        let p = match self.protocol.as_str() {
            "" | "jsonl" => Protocol::Jsonl,
            "acp" => Protocol::Acp,
            other => bail!("[agents.{name}] protocol: {other:?} is not \"jsonl\" or \"acp\""),
        };
        if !self.command.is_empty() {
            ensure!(p == Protocol::Acp, "[agents.{name}] command needs protocol = \"acp\"");
            for (field, set) in [
                ("tools", !self.tools.is_empty()),
                ("prompt", !self.prompt.is_empty() || !self.prompt_file.is_empty()),
                ("model", !self.model.is_empty()),
                ("base_url", !self.base_url.is_empty()),
            ] {
                ensure!(!set, "[agents.{name}] {field} configures a construct child; a command runs its own");
            }
        }
        ensure!(
            matches!(self.on_permission.as_str(), "" | "ask" | "reject" | "allow"),
            "[agents.{name}] on_permission: {:?} is not \"ask\", \"reject\" or \"allow\"",
            self.on_permission
        );
        for k in &self.allow_kinds {
            ensure!(ACP_KINDS.contains(&k.as_str()), "[agents.{name}] allow_kinds: {k:?} is not one of {ACP_KINDS:?}");
        }
        Ok(p)
    }

    /// How this definition answers an ACP child's permission requests.
    pub fn gate(&self) -> Gate {
        let on = match self.on_permission.as_str() {
            "allow" => OnPermission::Allow,
            "reject" => OnPermission::Reject,
            _ => OnPermission::Ask,
        };
        Gate { on, kinds: self.allow_kinds.clone() }
    }

    /// The system prompt: inline, else the file (`~/` expands, relative to `cwd`).
    pub fn prompt_text(&self, cwd: &Path) -> Result<String> {
        if !self.prompt.is_empty() || self.prompt_file.is_empty() {
            return Ok(self.prompt.clone());
        }
        let p = crate::sandbox::expand_home(&self.prompt_file);
        let p = if p.is_absolute() { p } else { cwd.join(p) };
        std::fs::read_to_string(&p).with_context(|| format!("reading prompt_file {}", p.display()))
    }

    /// The secret names the child receives: its own, plus the endpoint key.
    pub fn secret_names(&self, parent: &Config) -> Vec<String> {
        let mut names = self.secrets.clone();
        let key = self.key_secret(parent);
        if !key.is_empty() && !names.contains(&key) {
            names.push(key);
        }
        names
    }

    /// Whether the child uses the parent's endpoint key, so it may also be
    /// handed the key the parent's fallback found (`secrets::api_key`).
    /// Not when its model's catalog entry is on another endpoint: that
    /// endpoint takes its own key.
    pub fn inherits_key(&self, parent: &Config) -> bool {
        self.api_key_secret.is_empty()
            && self.base_url.is_empty()
            && self.command.is_empty()
            && self.model_config(parent).base_url == parent.base_url
    }

    /// `parent` with this definition's model selected.
    fn model_config(&self, parent: &Config) -> Config {
        let mut cfg = parent.clone();
        if !self.model.is_empty() {
            cfg.select_model(&self.model);
        }
        cfg
    }

    fn key_secret(&self, parent: &Config) -> String {
        match (self.api_key_secret.as_str(), self.base_url.is_empty() && self.command.is_empty()) {
            ("", true) => self.model_config(parent).api_key_secret,
            (s, _) => s.to_string(),
        }
    }

    /// The `Config` the child runs with, or why this definition can't run
    /// under `parent`'s tools (`Catalog::table`).
    pub fn child_config(&self, name: &str, parent: &Config, table: &[(String, Group)]) -> Result<Config> {
        self.protocol(name)?;
        for key in self.env.keys() {
            ensure!(
                !key.is_empty() && !key.contains('=') && !RESERVED_ENV.contains(&key.as_str()),
                "[agents.{name}.env] {key:?} is not a variable a definition may set"
            );
        }
        let group_of = |t: &str| table.iter().find(|(n, _)| n == t).map(|(_, g)| g);
        let mut names = vec![];
        for t in &self.tools {
            let Some(group) = group_of(t) else { bail!("[agents.{name}] tools: the parent has no tool {t:?}") };
            if matches!(group, Group::Mcp(_)) {
                bail!("[agents.{name}] tools: {t:?} is an MCP tool; a child has no MCP servers");
            }
            ensure!(!DENIED.contains(group), "[agents.{name}] tools: a child may not have {t:?} ({})", group.label());
            names.push(t.clone());
        }
        let mut cfg = parent.clone();
        // The model's catalog entry supplies its window and endpoint; what
        // this definition says itself, below, comes after and wins.
        if !self.model.is_empty() {
            cfg.select_model(&self.model);
        }
        if !self.base_url.is_empty() {
            cfg.base_url = self.base_url.trim_end_matches('/').to_string();
            // Headers may carry the parent's credential, and a dialect is
            // about the parent's host; both stay with it.
            cfg.headers.clear();
            cfg.provider = Default::default();
        }
        cfg.api_key_secret = self.key_secret(parent);
        cfg.tools.disabled = table
            .iter()
            .filter(|(n, g)| !names.contains(n) && !matches!(g, Group::Mcp(_)))
            .map(|(n, _)| n.clone())
            .collect();
        if self.context_window > 0 {
            cfg.context_window = self.context_window;
            cfg.window_pinned = true;
        }
        cfg.max_steps = self.max_turns;
        cfg.mcp.clear();
        cfg.agents.clear();
        cfg.learn.enabled = false;
        Ok(cfg)
    }

    /// Where `~/` paths in `allow_read` and `allow_write` land.
    pub fn expanded(paths: &[String]) -> Vec<PathBuf> {
        paths.iter().map(|p| crate::sandbox::expand_home(p)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parent() -> (Config, Vec<(String, Group)>) {
        let cfg = Config::default();
        let table = crate::tools::Catalog::build(&cfg.tools, None).unwrap().table();
        (cfg, table)
    }

    #[test]
    fn a_child_has_only_its_allowlist_and_never_the_denied_groups() {
        let (cfg, table) = parent();
        let def = AgentDef { tools: vec!["read".into(), "grep".into()], ..Default::default() };
        let c = def.child_config("scout", &cfg, &table).unwrap();
        let child = crate::tools::Catalog::build(&c.tools, None).unwrap();
        assert_eq!(child.names(), vec!["read", "grep"]);
        assert!(c.mcp.is_empty() && c.agents.is_empty() && !c.learn.enabled);
        for denied in ["memory", "skill_manage", "recall_result", "journal_query"] {
            let def = AgentDef { tools: vec![denied.into()], ..Default::default() };
            let err = def.child_config("scout", &cfg, &table).unwrap_err().to_string();
            assert!(err.contains("may not have"), "{denied}: {err}");
        }
        let def = AgentDef { tools: vec!["nope".into()], ..Default::default() };
        assert!(def.child_config("scout", &cfg, &table).is_err());
    }

    #[test]
    fn a_childs_model_brings_its_catalog_limits_and_an_explicit_window_wins() {
        let table = parent().1;
        let mut cfg = toml::from_str::<Config>(
            "context_window = 131072\n[models.small]\ncontext_window = 32768\nmax_tokens = 2048\n\
             [models.remote]\nbase_url = \"http://other/v1\"\napi_key_secret = \"REMOTE_KEY\"\n",
        )
        .unwrap();
        cfg.select_model(&cfg.model.clone());
        let child = |def: AgentDef| def.child_config("c", &cfg, &table).unwrap();
        let small = child(AgentDef { model: "small".into(), ..Default::default() });
        assert_eq!((small.context_window, small.max_tokens), (32768, Some(2048)));
        let explicit = child(AgentDef { model: "small".into(), context_window: 8000, ..Default::default() });
        assert_eq!(explicit.context_window, 8000);
        let unlisted = child(AgentDef { model: "nope".into(), ..Default::default() });
        assert_eq!(unlisted.context_window, 131_072, "an id with no entry gets the parent's defaults");
    }

    #[test]
    fn a_childs_model_on_another_endpoint_takes_that_endpoints_key_not_the_parents() {
        let mut cfg: Config = toml::from_str(
            "[models.remote]\nbase_url = \"http://other/v1\"\napi_key_secret = \"REMOTE_KEY\"\n\
             [models.bare]\nbase_url = \"http://local/v1\"\n",
        )
        .unwrap();
        cfg.select_model(&cfg.model.clone());
        let remote = AgentDef { model: "remote".into(), ..Default::default() };
        assert_eq!(remote.secret_names(&cfg), vec!["REMOTE_KEY"]);
        assert!(!remote.inherits_key(&cfg), "no fallback key for another host");
        let bare = AgentDef { model: "bare".into(), ..Default::default() };
        assert!(bare.secret_names(&cfg).is_empty(), "a keyless endpoint is handed nothing");
        let (table, plain) = (parent().1, AgentDef::default());
        let c = remote.child_config("c", &cfg, &table).unwrap();
        assert_eq!((c.base_url.as_str(), c.api_key_secret.as_str()), ("http://other/v1", "REMOTE_KEY"));
        assert!(plain.inherits_key(&cfg), "a child on the parent's own endpoint still inherits");
    }

    #[test]
    fn the_endpoint_key_follows_the_endpoint() {
        let (cfg, _) = parent();
        let same = AgentDef::default();
        assert_eq!(same.secret_names(&cfg), vec![cfg.api_key_secret.clone()]);
        let elsewhere = AgentDef { base_url: "http://other/v1".into(), ..Default::default() };
        assert!(elsewhere.secret_names(&cfg).is_empty());
        let keyed =
            AgentDef { base_url: "http://other/v1".into(), api_key_secret: "OTHER_KEY".into(), ..Default::default() };
        assert_eq!(keyed.secret_names(&cfg), vec!["OTHER_KEY"]);
    }

    #[test]
    fn a_childs_sandbox_is_a_subset_and_omits_the_parents_workspace() {
        use crate::config::{Paths, SandboxConfig};
        let dir = std::env::temp_dir().join(format!("construct-agent-policy-{}", std::process::id()));
        let (work, extra, outside) = (dir.join("work"), dir.join("extra"), dir.join("outside"));
        for d in [&work, &extra, &outside] {
            std::fs::create_dir_all(d).unwrap();
        }
        let paths = Paths { home: dir.join("home") };
        let cfg = SandboxConfig { allow_read: vec![extra.display().to_string()], ..Default::default() };
        let parent = crate::sandbox::Policy::new(&cfg, &work, &paths);

        let none = AgentDef::default().into_policy(&parent, &paths);
        assert!(none.check_read(&work).is_err(), "the parent's workspace is not implied");
        assert!(none.no_network);
        std::fs::write(none.tmp.join("scratch"), "x").unwrap();
        assert!(none.check_write(&none.tmp.join("scratch")).is_ok());

        let some = AgentDef { allow_read: vec![extra.display().to_string()], ..Default::default() };
        let p = some.into_policy(&parent, &paths);
        assert!(p.check_read(&extra).is_ok() && p.check_write(&extra).is_err());

        // Anything the parent itself cannot reach cannot be granted.
        let wide = AgentDef { allow_read: vec![outside.display().to_string()], ..Default::default() };
        assert!(parent.for_agent(&wide, &paths, "t", 443).is_err());
        let wide = AgentDef { allow_write: vec![extra.display().to_string()], ..Default::default() };
        assert!(parent.for_agent(&wide, &paths, "t", 443).is_err());
    }

    impl AgentDef {
        fn into_policy(self, parent: &crate::sandbox::Policy, paths: &crate::config::Paths) -> crate::sandbox::Policy {
            parent.for_agent(&self, paths, "t", 443).unwrap()
        }
    }

    #[test]
    fn a_command_runs_its_own_agent_and_gets_no_implicit_key() {
        let acp = |f: &dyn Fn(&mut AgentDef)| {
            let mut d = AgentDef { protocol: "acp".into(), command: vec!["codex-acp".into()], ..Default::default() };
            f(&mut d);
            d.protocol("x").map(|_| ()).unwrap_err().to_string()
        };
        let (cfg, table) = parent();
        let ok = AgentDef { protocol: "acp".into(), command: vec!["codex-acp".into()], ..Default::default() };
        assert!(ok.child_config("x", &cfg, &table).is_ok());
        assert!(ok.secret_names(&cfg).is_empty(), "the parent's model key is not for another vendor's agent");
        assert!(acp(&|d| d.tools = vec!["read".into()]).contains("tools"));
        assert!(acp(&|d| d.model = "m".into()).contains("model"));
        assert!(acp(&|d| d.protocol = "jsonl".into()).contains("protocol"));
        assert!(acp(&|d| d.on_permission = "maybe".into()).contains("on_permission"));
        assert!(acp(&|d| d.allow_kinds = vec!["bash".into()]).contains("allow_kinds"));
        let mut asks = AgentDef { protocol: "acp".into(), command: vec!["x".into()], ..Default::default() };
        assert_eq!(asks.gate().on, OnPermission::Ask, "asking is the default; with nobody to ask it rejects");
        asks.on_permission = "allow".into();
        asks.allow_kinds = vec!["read".into()];
        assert!(asks.protocol("x").is_ok());
        assert_eq!((asks.gate().on, asks.gate().kinds), (OnPermission::Allow, vec!["read".to_string()]));
        assert!(AgentDef { protocol: "grpc".into(), ..Default::default() }.protocol("x").is_err());
    }

    #[test]
    fn env_is_plain_variables_and_not_the_ones_construct_sets() {
        let (cfg, table) = parent();
        let mut def = AgentDef::default();
        def.env.insert("CODEX_HOME".into(), "/x".into());
        assert!(def.child_config("a", &cfg, &table).is_ok());
        for bad in ["CONSTRUCT_HOME", "CONSTRUCT_DEPTH", "TMPDIR", "A=B", ""] {
            let mut def = AgentDef::default();
            def.env.insert(bad.into(), "v".into());
            assert!(def.child_config("a", &cfg, &table).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn names_are_wire_safe() {
        assert!(check_name("code-scout_2").is_ok());
        for bad in ["", "a b", "a.b", &"x".repeat(33)] {
            assert!(check_name(bad).is_err(), "{bad}");
        }
    }
}
