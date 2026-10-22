//! A minimal MCP client over stdio (protocol 2025-06-18): initialize,
//! tools/list, tools/call, ping, progress and cancellation, with
//! newline-delimited JSON-RPC. Servers start lazily the first time the router
//! switches their group on, so an unused server costs nothing at startup.
//!
//! The design choice a reader must know: a server is a model-chosen action
//! like a shell command, so it runs the same way, under `construct __sandbox`
//! with the workspace policy plus what its entry adds, a scrubbed environment
//! plus the secrets it names, and the helper sweeping whatever it started.
//! Each server has its own slot, so one slow start never blocks another, and
//! a server that died is started again on its next call.
//!
//! Not supported: resources, prompts, sampling, the HTTP transports, and
//! `notifications/tools/list_changed` (tools are listed once per start, and
//! groups only grow, ADR 0005).

use crate::config::{McpServer, Paths, SandboxConfig};
use crate::sandbox::Policy;
use anyhow::{Context, Result, anyhow, ensure};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Mutex;

mod conn;
mod content;

/// What OpenAI-compatible APIs accept as a function name: `[A-Za-z0-9_-]`,
/// at most 64 characters.
const NAME_MAX: usize = 64;

/// The wire name of a server's tool: `mcp__<server>__<tool>`, with any
/// character an API would reject replaced by `_`, and shortened to 64 with a
/// hash of the original when it is longer. Declared here and nowhere else;
/// `server_of` is its inverse (server names are checked at config load, so
/// they never need either).
pub fn wire_name(server: &str, tool: &str) -> String {
    let safe: String = tool.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
    let name = format!("mcp__{server}__{safe}");
    if name.len() <= NAME_MAX {
        return name;
    }
    // FNV-1a: stable across builds, and two long names sharing a prefix
    // still get distinct wire names.
    let h = tool.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3));
    format!("{}_{:08x}", &name[..NAME_MAX - 9], h as u32)
}

/// The server a wire name belongs to, if it is an MCP tool's.
pub fn server_of(name: &str) -> Option<&str> {
    name.strip_prefix("mcp__")?.split_once("__").map(|(s, _)| s)
}

/// A server name must survive inside a function name and split back out of
/// one: letters, digits, `-` and single `_`, at most 32 characters.
pub fn check_server_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty()
            && name.len() <= 32
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            && !name.contains("__")
            && !name.starts_with('_')
            && !name.ends_with('_'),
        "[mcp.{name}]: a server name is 1-32 letters, digits, '-' or single '_' (it becomes part of tool names)"
    );
    Ok(())
}

/// One tool a server listed, as `tools/list` described it.
pub struct RemoteTool {
    pub name: String,
    pub description: String,
    pub schema: Value,
    /// The server's `readOnlyHint`. A hint from the server, so it only ever
    /// decides scheduling (read-only calls may run in parallel).
    pub readonly: bool,
}

pub struct Manager {
    servers: BTreeMap<String, McpServer>,
    policies: BTreeMap<String, Policy>,
    slots: BTreeMap<String, Mutex<Option<Arc<conn::Conn>>>>,
}

impl Manager {
    pub fn new(servers: BTreeMap<String, McpServer>, sandbox: &SandboxConfig, cwd: &Path, paths: &Paths) -> Self {
        let policies = servers.iter().map(|(n, s)| (n.clone(), Policy::for_mcp(sandbox, s, cwd, paths))).collect();
        let slots = servers.keys().map(|n| (n.clone(), Mutex::new(None))).collect();
        Self { servers, policies, slots }
    }

    pub fn servers(&self) -> &BTreeMap<String, McpServer> {
        &self.servers
    }

    /// Start a server if needed; the tools it listed.
    pub async fn ensure(&self, name: &str) -> Result<Vec<RemoteTool>> {
        let conn = self.conn(name).await?;
        Ok(conn
            .tools
            .iter()
            .map(|t| {
                let mut schema = t.get("inputSchema").cloned().unwrap_or(json!({ "type": "object" }));
                // Some providers reject a request whose schema names a dialect.
                if let Some(o) = schema.as_object_mut() {
                    o.remove("$schema");
                }
                RemoteTool {
                    name: t["name"].as_str().unwrap_or("").to_string(),
                    description: t["description"].as_str().unwrap_or("").to_string(),
                    schema,
                    readonly: t["annotations"]["readOnlyHint"].as_bool().unwrap_or(false),
                }
            })
            .collect())
    }

    /// The server's live connection, starting it (again, if it died) under
    /// its own lock only.
    async fn conn(&self, name: &str) -> Result<Arc<conn::Conn>> {
        let slot = self.slots.get(name).ok_or_else(|| anyhow!("no MCP server {name}"))?;
        let mut slot = slot.lock().await;
        if let Some(c) = slot.as_ref().filter(|c| c.alive()) {
            return Ok(c.clone());
        }
        *slot = None;
        let c = Arc::new(
            conn::start(&self.servers[name], &self.policies[name])
                .await
                .with_context(|| format!("starting MCP server {name}"))?,
        );
        *slot = Some(c.clone());
        Ok(c)
    }

    pub async fn call(&self, server: &str, tool: &str, args: Value) -> Result<String, String> {
        let conn = self.conn(server).await.map_err(|e| format!("{e:#}"))?;
        let idle = Duration::from_secs(self.servers[server].timeout.max(1));
        let res = conn
            .request("tools/call", json!({ "name": tool, "arguments": args }), idle)
            .await
            .map_err(|e| format!("{e:#}"))?;
        let text = content::render(&res);
        if res["isError"].as_bool().unwrap_or(false) { Err(text) } else { Ok(text) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_names_are_api_safe_and_distinct() {
        assert_eq!(wire_name("docs", "resolve-library-id"), "mcp__docs__resolve-library-id");
        assert_eq!(wire_name("fs", "files.read"), "mcp__fs__files_read");
        let a = wire_name("server", &format!("{}_one", "x".repeat(70)));
        let b = wire_name("server", &format!("{}_two", "x".repeat(70)));
        assert!(a.len() == NAME_MAX && b.len() == NAME_MAX && a != b, "{a} {b}");
        assert_eq!(server_of(&a), Some("server"));
    }

    #[test]
    fn server_names_split_back_out() {
        for ok in ["context7", "my-server", "a_b"] {
            assert!(check_server_name(ok).is_ok(), "{ok}");
        }
        for bad in ["a__b", "_a", "a_", "has.dot", "", &"x".repeat(33)] {
            assert!(check_server_name(bad).is_err(), "{bad}");
        }
    }
}
