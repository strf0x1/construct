//! `boundary_withdraw`: the agent no longer needs the blocked resource.

use super::WITHDRAW;
use super::client::check_id;
use crate::tools::{Ctx, Group, Output, Tool, ToolError, arg, def, obj};
use serde_json::{Value, json};

pub struct Withdraw;

#[async_trait::async_trait]
impl Tool for Withdraw {
    fn name(&self) -> &str {
        WITHDRAW
    }
    fn group(&self) -> Group {
        Group::Boundary
    }
    fn definition(&self) -> Value {
        def(
            WITHDRAW,
            "Withdraw a Deck boundary approval you no longer need. The item leaves the user's queue and the audit names the agent; it grants nothing.",
            obj(
                json!({
                    "id": { "type": "string", "description": "The approval id from the boundary note." }
                }),
                &["id"],
            ),
        )
    }
    async fn call(&self, ctx: &Ctx, args: Value) -> Result<Output, ToolError> {
        let Some(b) = &ctx.boundary else {
            return Err(ToolError::new("no_boundary", "this session has no Deck boundary endpoint"));
        };
        let id =
            arg(&args, "id").ok_or_else(|| ToolError::new("bad_args", "boundary_withdraw needs the approval id"))?;
        check_id(id).map_err(|e| ToolError::new("bad_args", e))?;
        b.withdraw(id).await.map_err(|e| ToolError::new("boundary", e))?;
        b.mark_seen(id);
        Ok(Output::plain(format!("withdrew approval {id}; it is no longer in the user's queue"))
            .with_data(json!({ "approval": id, "outcome": "withdrawn" })))
    }
}
