//! One `[mcp.<name>]` entry: how a stdio server is spawned inside the
//! sandbox, what it may reach, and the secrets it alone receives.

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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

#[cfg(test)]
mod tests {
    use super::*;

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
