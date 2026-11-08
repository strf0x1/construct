//! Repair only missing durable outcomes. Known tool results are reconciled by
//! call id; unknown effects are reported, never retried. Repairs commit together.

use super::{Entry, Journal};
use crate::event::Event;
use crate::llm::Message;
use anyhow::{Result, ensure};
use std::collections::{BTreeMap, BTreeSet};

impl Journal {
    pub(super) fn recover(&mut self, session: &str, entries: &[Entry]) -> Result<()> {
        let repairs = repairs(entries)?;
        if repairs.is_empty() {
            return Ok(());
        }
        let last = self.last_seq(session)?;
        let at = entries.iter().filter(|e| activity(&e.event)).map(|e| e.at_ms).max().unwrap_or(0);
        let tx = self.conn.unchecked_transaction()?;
        for event in repairs {
            self.append(session, &event)?;
        }
        self.conn.execute(
            "UPDATE events SET at_ms = ?1 WHERE session_id = ?2 AND seq > ?3",
            rusqlite::params![at as i64, session, last],
        )?;
        tx.commit()?;
        Ok(())
    }
}

fn repairs(entries: &[Entry]) -> Result<Vec<Event>> {
    let events = || entries.iter().map(|e| &e.event);
    let mut pending_prompt = None;
    let mut open_turn = false;
    let mut starts = BTreeMap::new();
    let mut ends = BTreeSet::new();
    let mut jobs = BTreeMap::new();
    let mut out = vec![];
    for e in events() {
        match e {
            Event::TurnStart => open_turn = true,
            Event::TurnEnd { .. } | Event::Recovered { .. } => open_turn = false,
            Event::Submitted { text } => pending_prompt = Some(text),
            Event::User { .. } => pending_prompt = None,
            Event::ToolStart { id, name, args } => {
                starts.insert(id.clone(), (name.clone(), args.clone()));
            }
            Event::ToolEnd { id, .. } => {
                ends.insert(id.clone());
            }
            Event::JobStart(j) => {
                jobs.insert(j.id.clone(), j);
            }
            Event::JobEnd(j) => {
                jobs.remove(&j.id);
            }
            _ => {}
        }
    }
    // Replay before pairing: cuts preserve complete rounds. Any outstanding
    // calls must be at the very end of the effective context.
    let context = crate::context::replay(events(), crate::context::Check::Strict)?.messages().to_vec();
    let mut pending = BTreeSet::new();
    for m in &context {
        if m.role == "tool" {
            let id = m.tool_call_id.as_deref().unwrap_or_default();
            ensure!(pending.remove(id), "unpaired or duplicate tool result {id}; cannot safely restore history");
        } else {
            ensure!(
                pending.is_empty(),
                "incomplete tool results before a later message; cannot safely restore history"
            );
            for call in &m.tool_calls {
                ensure!(pending.insert(call.id.clone()), "duplicate tool call {}", call.id);
            }
        }
    }
    for id in &pending {
        let known = events().rev().find_map(|e| match e {
            Event::ToolEnd { id: done, output, .. } if done == id => Some(output.clone()),
            _ => None,
        });
        let output = known.unwrap_or_else(||
            "Error: previous execution was interrupted; this tool's outcome is unknown. External effects may already have happened. Inspect the state before deciding whether to retry.".into());
        if !ends.contains(id) {
            let name = starts
                .get(id)
                .map(|s| s.0.clone())
                .or_else(|| {
                    context.iter().flat_map(|m| &m.tool_calls).find(|c| &c.id == id).map(|c| c.function.name.clone())
                })
                .unwrap_or_default();
            out.push(Event::ToolEnd {
                id: id.clone(),
                name,
                ok: false,
                output: output.clone(),
                ms: 0,
                kind: crate::tools::OutputKind::Plain,
                data: None,
                code: Some("interrupted_unknown".into()),
            });
        }
        out.push(Event::ToolResult { id: id.clone(), message: Message::tool(id, output) });
    }
    if let Some(text) = pending_prompt {
        out.push(Event::User { message: Message::user(text) });
    }
    for j in jobs.values() {
        out.push(Event::JobEnd(crate::jobs::Info {
            id: j.id.clone(), kind: j.kind, label: j.label.clone(), status: crate::jobs::Status::Interrupted,
            exit: None, ms: 0, tail: "The previous process stopped; this background job was interrupted. External effects may already have happened.".into(),
        }));
    }
    if open_turn || !out.is_empty() {
        out.push(Event::Recovered { message:
            "Previous turn was interrupted. Committed history was recovered; unrecorded streaming text may be lost. Tool outcomes marked unknown may have had external effects. Nothing was retried; continue explicitly when ready.".into() });
    }
    Ok(out)
}

pub(super) fn activity(e: &Event) -> bool {
    matches!(
        e,
        Event::Submitted { .. }
            | Event::User { .. }
            | Event::Assistant { .. }
            | Event::ToolStart { .. }
            | Event::ToolEnd { .. }
            | Event::ToolResult { .. }
            | Event::TurnStart
            | Event::TurnEnd { .. }
            | Event::JobStart(_)
            | Event::JobEnd(_)
    )
}
