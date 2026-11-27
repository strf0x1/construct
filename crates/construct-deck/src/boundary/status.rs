//! `boundary_status`: the same answer a wait would give, now.

use super::STATUS;
use super::note::status_text;
use construct_core::tools::boundary::BoundaryHost;
use construct_core::tools::{Ctx, Group, Output, Tool, ToolError, arg, def, obj};
use serde_json::{Value, json};
use std::sync::Arc;

pub struct Status {
    boundary: Arc<dyn BoundaryHost>,
}

impl Status {
    pub fn new(boundary: Arc<dyn BoundaryHost>) -> Self {
        Self { boundary }
    }
}

#[async_trait::async_trait]
impl Tool for Status {
    fn name(&self) -> &str {
        STATUS
    }
    fn group(&self) -> Group {
        Group::Boundary
    }
    fn definition(&self) -> Value {
        def(
            STATUS,
            "Check a Deck boundary approval now, without waiting. Use it when the task moved on and you want to know whether the user decided; do not poll it in a loop.",
            obj(
                json!({
                    "id": { "type": "string", "description": "The approval id from the boundary note." }
                }),
                &["id"],
            ),
        )
    }
    async fn call(&self, _ctx: &Ctx, args: Value) -> Result<Output, ToolError> {
        let Some(b) = super::downcast(self.boundary.as_ref()) else {
            return Err(ToolError::new("no_boundary", "this session has no Deck boundary endpoint"));
        };
        let id = arg(&args, "id").ok_or_else(|| ToolError::new("bad_args", "boundary_status needs the approval id"))?;
        let out = b.approval(id, None).await.map_err(|e| ToolError::new("boundary", e))?;
        if out.terminal() {
            b.mark_seen(id);
        }
        let mut data = json!({ "approval": id, "outcome": out.outcome });
        if let Some(scope) = &out.scope {
            data["scope"] = json!(scope);
        }
        if let Some(expires_at_ms) = out.expires_at_ms {
            data["expires_at_ms"] = json!(expires_at_ms);
        }
        Ok(Output::plain(status_text(id, &out)).with_data(data))
    }
}
