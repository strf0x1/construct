//! `job`: look at or stop a background job. Starting one is the shell tool's
//! `background = true` (and later a delegate's); the agent is told when a job
//! ends, so this exists to read one that is still running, to re-read a
//! result, and to cancel.
//!
//! The design choice: reading an ended job here counts as the model having its
//! result. The read is journaled as a delivery, so the same ending never also
//! wakes the agent (ADR 0026).

use super::{Call, Ctx, Display, Group, Output, Tool, ToolError, def, obj};
use crate::event::Event;
use crate::jobs::{Cancel, Delivered};
use serde_json::{Value, json};

pub struct JobTool;

impl JobTool {
    pub const NAME: &str = "job";
}

#[async_trait::async_trait]
impl Tool for JobTool {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn group(&self) -> Group {
        Group::Jobs
    }
    fn definition(&self) -> Value {
        def(
            Self::NAME,
            "Look at or stop a background job. You are told when a job ends, so don't poll: use check to read one that is still running, or to read a result again (id left out lists them all). cancel stops a job; a cancelled job never tells you anything.",
            obj(
                json!({
                    "action": { "type": "string", "enum": ["check", "cancel"] },
                    "id": { "type": "string", "description": "The job's id, as the call that started it returned it" }
                }),
                &["action"],
            ),
        )
    }
    fn display(&self) -> Display {
        Display { primary_arg: "id", preview_lines: 6 }
    }
    async fn call(&self, ctx: &Ctx, args: Value) -> Result<Output, ToolError> {
        self.call_with(ctx, &Call::detached(""), args).await
    }
    async fn call_with(&self, ctx: &Ctx, call: &Call, args: Value) -> Result<Output, ToolError> {
        let id = super::arg(&args, "id");
        match (super::arg(&args, "action"), id) {
            (Some("check"), None) => {
                let jobs = ctx.jobs.list();
                if jobs.is_empty() {
                    return Ok(Output::plain("no background jobs"));
                }
                let lines: Vec<String> = jobs
                    .iter()
                    .map(|j| {
                        format!(
                            "{} · {} · {} · {:.1}s · {}",
                            j.id,
                            j.kind.label(),
                            j.status.label(),
                            j.ms as f64 / 1000.0,
                            j.label
                        )
                    })
                    .collect();
                Ok(Output::plain(lines.join("\n")))
            }
            (Some("check"), Some(id)) => {
                let Some((info, consumed)) = ctx.jobs.check(id) else { return Err(unknown(ctx, id)) };
                if consumed {
                    call.emit(Event::JobsDelivered(Delivered {
                        ids: vec![id.to_string()],
                        via: "check".into(),
                        message: None,
                    }));
                }
                Ok(Output::plain(info.report())
                    .with_data(json!({ "job": id, "status": info.status, "consumed": consumed })))
            }
            (Some("cancel"), Some(id)) => match ctx.jobs.cancel(id) {
                Cancel::Sent => Ok(Output::plain(format!("cancelling job {id}; it will not wake you"))
                    .with_data(json!({ "job": id, "cancelled": true }))),
                Cancel::Over(s) => Ok(Output::plain(format!("job {id} had already ended ({})", s.label()))
                    .with_data(json!({ "job": id, "cancelled": false }))),
                Cancel::Unknown => Err(unknown(ctx, id)),
            },
            (Some("cancel"), None) => Err("cancel needs the job's id".into()),
            _ => Err("action must be check or cancel".into()),
        }
    }
}

fn unknown(ctx: &Ctx, id: &str) -> ToolError {
    let known: Vec<String> = ctx.jobs.list().into_iter().map(|j| j.id).collect();
    ToolError::new("unknown_job", format!("no job {id:?}; the jobs are {known:?}"))
}
