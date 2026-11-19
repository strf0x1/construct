//! memory: durable, capped notes on the user, the machine and the current
//! repo, injected into future sessions.

use super::{Ctx, Display, Group, Output, Tool, ToolError, def, obj};
use crate::memory::{self, Target};
use serde_json::{Value, json};

pub struct Memory;

impl Memory {
    pub const NAME: &str = "memory";
}

#[async_trait::async_trait]
impl Tool for Memory {
    fn name(&self) -> &'static str {
        Self::NAME
    }
    fn group(&self) -> Group {
        Group::Memory
    }
    fn definition(&self) -> Value {
        def(
            Self::NAME,
            "Durable memory injected into future sessions. target 'user' = who the user is and how they like to work; 'memory' = facts about this machine and its tools; 'project' = how the current repo works (build/test steps, landmines, conventions), shown only when working in this repo. Both are capped: pass all changes as one operations array (applied atomically, cap checked on the result), removing stale entries to make room. Save preferences, corrections and stable facts; skip task progress and things easy to rediscover.",
            obj(
                json!({
                    "target": { "type": "string", "enum": ["user", "memory", "project"] },
                    "operations": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "action": { "type": "string", "enum": ["add", "replace", "remove"] },
                                "content": { "type": "string" },
                                "old_text": { "type": "string", "description": "Unique substring of the entry to replace/remove" }
                            },
                            "required": ["action"]
                        }
                    }
                }),
                &["target", "operations"],
            ),
        )
    }
    fn display(&self) -> Display {
        Display { primary_arg: "target", ..Display::default() }
    }
    async fn call(&self, ctx: &Ctx, a: Value) -> Result<Output, ToolError> {
        run(ctx, &a).map(Output::plain).map_err(ToolError::from)
    }
}

fn run(ctx: &Ctx, a: &Value) -> Result<String, String> {
    let target: Target =
        serde_json::from_value(a["target"].clone()).map_err(|_| "target must be user, memory or project")?;
    let ops: Vec<memory::Op> = match a.get("operations") {
        Some(v) => serde_json::from_value(v.clone()).map_err(|e| format!("operations: {e}"))?,
        None => vec![serde_json::from_value(a.clone()).map_err(|e| format!("{e}"))?],
    };
    ctx.memory.apply(target, &ops)
}
