//! `delegate`: hand a self-contained task to a configured agent and get its
//! summary back. The agent is a child construct process (`child::run`) with
//! its own prompt, tools, model and sandbox; this tool is only the model-facing
//! shape around it.
//!
//! The design choice: what comes back is the child's last answer, capped, plus
//! `data` with the child's session id. The child's whole transcript is in the
//! parent's journal under that id, so nothing is lost and nothing large
//! enters the parent's context.

use super::{Call, Ctx, Display, Group, Output, Tool, ToolError, def, obj};
use crate::agents::AgentDef;
use crate::child::run::{Spec, Stop, run};
use crate::child::{self, MAX_DEPTH};
use crate::config::Config;
use anyhow::{Context, Result};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// What the model gets of a child's answer; the rest is in the journal.
const SUMMARY_MAX: usize = 8 * 1024;

const DEFAULT_PROMPT: &str = "You are a sub-agent of construct, given one task by another agent. Do it with the tools you have, then reply with a short report of what you found or did: it is all the other agent will see. Be brief and concrete.";

pub struct Delegate {
    parent: Config,
    agents: BTreeMap<String, Prepared>,
}

struct Prepared {
    def: AgentDef,
    config: Config,
    system: String,
    /// The model endpoint's TCP port: the one place a child may connect.
    port: u16,
}

impl Delegate {
    pub const NAME: &str = "delegate";

    /// Check every definition against the parent's tools now, so a bad one
    /// fails the session at start rather than mid-task.
    pub fn new(cfg: &Config, table: &[(String, Group)], cwd: &std::path::Path) -> Result<Self> {
        let mut agents = BTreeMap::new();
        for (name, def) in &cfg.agents {
            let config = def.child_config(name, cfg, table)?;
            let prompt = def.prompt_text(cwd)?;
            let system = if prompt.trim().is_empty() { DEFAULT_PROMPT.to_string() } else { prompt };
            let port = reqwest::Url::parse(&config.base_url)
                .ok()
                .and_then(|u| u.port_or_known_default())
                .with_context(|| format!("[agents.{name}] base_url {:?} has no port", config.base_url))?;
            agents.insert(name.clone(), Prepared { def: def.clone(), config, system, port });
        }
        Ok(Self { parent: cfg.clone(), agents })
    }
}

#[async_trait::async_trait]
impl Tool for Delegate {
    fn name(&self) -> &str {
        Self::NAME
    }
    fn group(&self) -> Group {
        Group::Core
    }
    fn definition(&self) -> Value {
        let list: Vec<String> = self
            .agents
            .iter()
            .map(|(n, p)| if p.def.description.is_empty() { n.clone() } else { format!("{n}: {}", p.def.description) })
            .collect();
        def(
            Self::NAME,
            &format!(
                "Give a self-contained task to another agent and get its summary back. It starts with none of your context, so say everything it needs, and what to report. Agents: {}.",
                list.join("; ")
            ),
            obj(
                json!({
                    "agent": { "type": "string", "enum": self.agents.keys().collect::<Vec<_>>() },
                    "task": { "type": "string", "description": "What to do and what to report back" }
                }),
                &["agent", "task"],
            ),
        )
    }
    fn display(&self) -> Display {
        Display { primary_arg: "agent", preview_lines: 6 }
    }
    async fn call(&self, ctx: &Ctx, args: Value) -> Result<Output, ToolError> {
        self.call_with(ctx, &Call::detached(""), args).await
    }
    async fn call_with(&self, ctx: &Ctx, call: &Call, args: Value) -> Result<Output, ToolError> {
        if child::depth() >= MAX_DEPTH {
            return Err(ToolError::new("depth", "delegation is not available from inside a delegated agent"));
        }
        let (Some(name), Some(task)) = (super::arg(&args, "agent"), super::arg(&args, "task")) else {
            return Err("agent and task are required".into());
        };
        let Some(p) = self.agents.get(name) else {
            let known: Vec<&String> = self.agents.keys().collect();
            return Err(ToolError::new("unknown_agent", format!("no agent {name:?}; the agents are {known:?}")));
        };
        let tag: String = call.id.chars().filter(char::is_ascii_alphanumeric).collect();
        let policy = ctx.sandbox.for_agent(&p.def, &ctx.paths, &tag, p.port).map_err(ToolError::sandbox)?;
        // Where the child works: the parent's directory if it was granted
        // that, else a scratch directory of its own.
        let cwd = if policy.check_read(&ctx.cwd).is_ok() { ctx.cwd.clone() } else { policy.tmp.join("work") };
        std::fs::create_dir_all(&cwd)
            .map_err(|e| ToolError::new("spawn", format!("creating {}: {e}", cwd.display())))?;
        let spec = Spec {
            name,
            def: &p.def,
            config: p.config.clone(),
            system: p.system.clone(),
            policy,
            cwd: &cwd,
            parent: &self.parent,
        };
        let started = std::time::Instant::now();
        let out = run(spec, task, call, ctx.cancel.clone()).await.map_err(|e| ToolError::new("spawn", e))?;
        let (summary, truncated) = cap(&out.summary);
        let data = json!({
            "agent": name,
            "session": out.session,
            "steps": out.steps,
            "prompt_tokens": out.prompt,
            "completion_tokens": out.completion,
            "ms": started.elapsed().as_millis() as u64,
            "truncated": truncated,
        });
        let mut pointer =
            out.session.as_deref().map(|s| format!("\n[full transcript: journal session {s}]")).unwrap_or_default();
        if out.stopped.is_some() && !out.diagnostics.is_empty() {
            pointer.push_str(&format!("\n[its stderr ended: {}]", crate::llm::truncate(&out.diagnostics, 600)));
        }
        match (out.stopped, out.error) {
            (Some(Stop::Timeout), _) => Err(ToolError::new(
                "timeout",
                format!("agent {name} timed out after {}s. Its last answer: {summary}{pointer}", p.def.timeout),
            )
            .with_data(data)),
            (Some(Stop::Interrupted), _) => Err(ToolError::new(
                "interrupted",
                format!("agent {name} was interrupted. Its last answer: {summary}{pointer}"),
            )
            .with_data(data)),
            (None, Some(e)) => {
                Err(ToolError::new("child_failed", format!("agent {name} failed: {e}{pointer}")).with_data(data))
            }
            (None, None) if summary.trim().is_empty() => {
                Err(ToolError::new("empty", format!("agent {name} finished without an answer{pointer}"))
                    .with_data(data))
            }
            (None, None) => Ok(Output::plain(format!("{summary}{pointer}")).with_data(data)),
        }
    }
}

/// The summary at most `SUMMARY_MAX` bytes, and whether it was cut.
fn cap(text: &str) -> (String, bool) {
    if text.len() <= SUMMARY_MAX {
        return (text.to_string(), false);
    }
    let mut end = SUMMARY_MAX;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}\n[summary cut at {SUMMARY_MAX} bytes]", &text[..end]), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_long_summary_is_cut_on_a_char_boundary() {
        let (s, cut) = cap(&"é".repeat(SUMMARY_MAX));
        assert!(cut && s.len() < SUMMARY_MAX + 64 && s.contains("summary cut"));
        assert_eq!(cap("short"), ("short".to_string(), false));
    }
}
