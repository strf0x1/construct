//! A tool an MCP server listed, as a catalog entry like any native tool.
//!
//! The design choice a reader must know: these exist only once their server
//! has started. `Catalog::start_mcp` adds them when the router or the model
//! switches the server's group on; before that, the server's tools are not in
//! the catalog, and the only trace of them is the server's line in
//! `enable_tools`.

use super::{Ctx, Group, Output, Tool, ToolError};
use crate::mcp::{Manager, RemoteTool, wire_name};
use serde_json::{Value, json};
use std::sync::Arc;

pub struct McpTool {
    name: String,
    server: String,
    remote: String,
    definition: Value,
    manager: Arc<Manager>,
}

impl McpTool {
    pub fn new(manager: &Arc<Manager>, server: &str, t: RemoteTool) -> Self {
        let name = wire_name(server, &t.name);
        let definition = json!({
            "type": "function",
            "function": {
                "name": name,
                "description": crate::llm::truncate(&t.description, 1024),
                "parameters": t.schema,
            }
        });
        Self { name, server: server.to_string(), remote: t.name, definition, manager: manager.clone() }
    }
}

#[async_trait::async_trait]
impl Tool for McpTool {
    fn name(&self) -> &str {
        &self.name
    }
    fn group(&self) -> Group {
        Group::Mcp(self.server.clone())
    }
    fn definition(&self) -> Value {
        self.definition.clone()
    }
    async fn call(&self, _ctx: &Ctx, args: Value) -> Result<Output, ToolError> {
        self.manager.call(&self.server, &self.remote, args).await.map(Output::plain).map_err(ToolError::from)
    }
}
