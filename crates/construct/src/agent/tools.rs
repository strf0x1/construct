//! Running the tools the model asked for: read-only ones in parallel, the rest
//! in order, and one tool message per call.
//!
//! The one choice a reader must know: the model-facing switches run first,
//! without the catalog. `ENABLE_TOOLS` changes state and a strategy's own
//! tools (`optchat`'s `zoom`) are answered by the strategy's `Context::call`;
//! both are journaled exactly like a catalog call.

use super::Agent;
use super::deck;
use crate::event::{Event, Events};
use crate::llm::{self, Message};
use crate::tools::{Call, ENABLE_TOOLS, Group, OutputKind};
use futures::future::join_all;
use serde_json::Value;
use std::time::Instant;

impl Agent {
    /// Run `calls` and push one tool message per call.
    pub(super) async fn run_tools(&mut self, calls: &[llm::ToolCall], tx: &Events) {
        // Model-facing switches first: they change state, not the world.
        // Each slot is the text the model gets back.
        let mut results: Vec<Option<String>> = vec![None; calls.len()];
        let own: Vec<String> = self
            .context
            .tools()
            .iter()
            .filter_map(|d| d.pointer("/function/name").and_then(Value::as_str).map(str::to_string))
            .collect();
        for (i, c) in calls.iter().enumerate() {
            if c.function.name == ENABLE_TOOLS {
                let v: Value = serde_json::from_str(&c.function.arguments).unwrap_or_default();
                let want: Vec<Group> = v["groups"]
                    .as_array()
                    .map(|a| a.iter().filter_map(Value::as_str).filter_map(Group::parse).collect())
                    .unwrap_or_default();
                let labels: Vec<String> = want.iter().map(Group::label).collect();
                self.enable(want, tx).await;
                let out = format!("enabled: {}. The new tools are available from your next step.", labels.join(", "));
                self.journal.emit(
                    Event::ToolStart {
                        id: c.id.clone(),
                        name: c.function.name.clone(),
                        args: c.function.arguments.clone(),
                    },
                    tx,
                );
                self.journal.emit(
                    Event::ToolEnd {
                        id: c.id.clone(),
                        name: c.function.name.clone(),
                        ok: true,
                        output: out.clone(),
                        ms: 0,
                        kind: OutputKind::Plain,
                        data: None,
                        code: None,
                    },
                    tx,
                );
                results[i] = Some(out);
            } else if own.contains(&c.function.name) {
                let Some(outcome) = self.context.call(&c.function.name, &c.function.arguments) else {
                    continue;
                };
                let (ok, out) = match outcome {
                    Ok(text) => (true, text),
                    Err(refusal) => (false, refusal),
                };
                self.journal.emit(
                    Event::ToolStart {
                        id: c.id.clone(),
                        name: c.function.name.clone(),
                        args: c.function.arguments.clone(),
                    },
                    tx,
                );
                self.journal.emit(
                    Event::ToolEnd {
                        id: c.id.clone(),
                        name: c.function.name.clone(),
                        ok,
                        output: out.clone(),
                        ms: 0,
                        kind: OutputKind::Plain,
                        data: None,
                        code: None,
                    },
                    tx,
                );
                results[i] = Some(out);
            } else if let Some(g) = self.catalog.group_of(&c.function.name) {
                // The model called a tool it wasn't shown: honour it and keep the group on.
                if !self.groups.contains(&g) {
                    self.enable([g], tx).await;
                }
            }
        }
        let parallel = calls.iter().all(|c| {
            self.catalog.readonly(&c.function.name) || c.function.name == ENABLE_TOOLS || own.contains(&c.function.name)
        });
        let ctx = self.ctx.clone();
        let catalog = self.catalog.clone();
        let journal = self.journal.clone();
        let run = |c: llm::ToolCall| {
            let ctx = ctx.clone();
            let catalog = catalog.clone();
            let journal = journal.clone();
            let tx = tx.clone();
            async move {
                journal.emit(
                    Event::ToolStart {
                        id: c.id.clone(),
                        name: c.function.name.clone(),
                        args: c.function.arguments.clone(),
                    },
                    &tx,
                );
                // Attribution is by time: the cursor before the call, the
                // gateway's events after it.
                let before = deck::before_tool(&ctx).await;
                let t = Instant::now();
                let call = Call::new(&c.id, journal.clone(), tx.clone());
                let r = catalog.call_with(&ctx, &call, &c.function.name, &c.function.arguments).await;
                let ms = t.elapsed().as_millis() as u64;
                let (ok, mut out, kind, data, code) = match r {
                    Ok(o) => (true, o.text, o.kind, o.data, None),
                    Err(e) => (false, format!("Error: {}", e.message), e.kind, e.data, Some(e.code.to_string())),
                };
                deck::after_tool(&ctx, before, &mut out).await;
                // What scorers read back from the journal: codes and data,
                // never the prose.
                journal.emit(
                    Event::ToolEnd {
                        id: c.id.clone(),
                        name: c.function.name.clone(),
                        ok,
                        output: out.clone(),
                        ms,
                        kind,
                        data,
                        code,
                    },
                    &tx,
                );
                out
            }
        };
        let pending: Vec<(usize, llm::ToolCall)> =
            calls.iter().cloned().enumerate().filter(|(i, _)| results[*i].is_none()).collect();
        self.ctx.size_results(self.budget(), pending.len(), self.bytes_per_token);
        if parallel {
            let outs = join_all(pending.iter().map(|(_, c)| run(c.clone()))).await;
            for ((i, _), o) in pending.iter().zip(outs) {
                results[*i] = Some(o);
            }
        } else {
            for (i, c) in pending {
                if self.cancelled() {
                    results[i] = Some("Error: interrupted by the user".into());
                    continue;
                }
                results[i] = Some(run(c).await);
            }
        }
        for (c, r) in calls.iter().zip(results) {
            let out = r.unwrap_or_else(|| "Error: not run".into());
            self.push(Message::tool(&c.id, out), tx);
            self.tool_calls += 1;
            self.tool_calls_since_skill += 1;
        }
        self.offer_job_tool();
    }
}
