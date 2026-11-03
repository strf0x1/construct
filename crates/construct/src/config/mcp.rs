//! One `[mcp.<name>]` entry: how a stdio server is spawned inside the
//! sandbox, what it may reach, and the secrets it alone receives.

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
