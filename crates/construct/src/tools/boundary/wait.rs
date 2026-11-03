//! `boundary_wait`: the agent chooses to wait for a Deck decision.

use super::client::{Boundary, MAX_WAIT, Outcome};
use super::note::status_text;
use super::{WAIT, WITHDRAW};
use crate::tools::{Ctx, Group, Output, Tool, ToolError, arg, def, num, obj};
use serde_json::{Value, json};

pub struct Wait;

#[async_trait::async_trait]
impl Tool for Wait {
    fn name(&self) -> &str {
        WAIT
    }
    fn group(&self) -> Group {
        Group::Boundary
    }
    fn definition(&self) -> Value {
        def(
            WAIT,
            &format!(
                "Wait for Deck's decision on a boundary approval named in a tool result. Use it when the task needs the blocked destination; if it does not, carry on or call {WITHDRAW} instead. Never look for another route to the same resource."
            ),
            obj(
                json!({
                    "id": { "type": "string", "description": "The approval id from the boundary note." },
                    "timeout_s": {
                        "type": "integer",
                        "minimum": 1,
                        "maximum": MAX_WAIT,
                        "description": "How long to wait; the host caps it at 300 s."
                    }
                }),
                &["id"],
            ),
        )
    }
    async fn call(&self, ctx: &Ctx, args: Value) -> Result<Output, ToolError> {
        let Some(b) = &ctx.boundary else {
            return Err(ToolError::new("no_boundary", "this session has no Deck boundary endpoint"));
        };
        let id = arg(&args, "id").ok_or_else(|| ToolError::new("bad_args", "boundary_wait needs the approval id"))?;
        let timeout = num(&args, "timeout_s").unwrap_or(60).clamp(1, MAX_WAIT as usize) as u64;
        let out = wait(b, id, timeout, ctx).await.map_err(|e| ToolError::new("boundary", e))?;
        if out.outcome.is_empty() || out.outcome == "pending" || out.outcome == "timeout" {
            return Ok(Output::plain(format!(
                "timeout after {timeout}s: approval {id} is still pending with the user"
            ))
            .with_data(json!({ "approval": id, "outcome": "timeout" })));
        }
        if out.terminal() {
            b.mark_seen(id);
        }
        Ok(Output::plain(status_text(id, &out)).with_data(json!({ "approval": id, "outcome": out.outcome })))
    }
}

/// A wait the user can interrupt: `session/cancel` flips the watch, and the
/// request future is dropped rather than finished.
async fn wait(b: &Boundary, id: &str, timeout: u64, ctx: &Ctx) -> Result<Outcome, String> {
    let mut cancel = ctx.cancel.clone();
    if *cancel.borrow() {
        return Err("boundary_wait was interrupted by the user".into());
    }
    let fut = b.approval(id, Some(timeout));
    tokio::pin!(fut);
    loop {
        tokio::select! {
            r = &mut fut => return r,
            changed = cancel.changed() => {
                if changed.is_err() || *cancel.borrow() {
                    return Err("boundary_wait was interrupted by the user".into());
                }
            }
        }
    }
}
