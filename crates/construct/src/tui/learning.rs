//! Learning bookkeeping belongs to one session. Its events use that session
//! channel, so late results cannot land in a different conversation.

use crate::agent::{Agent, Event};
use crate::learn;
use tokio::sync::mpsc;

/// Hand any unreviewed tail of the session to a detached learning process.
pub(super) fn finish(agent: &Agent, (reviewed, seq): (usize, i64)) {
    if !agent.cfg.learn.enabled || reviewed >= agent.messages().len() {
        return;
    }
    let tail = &agent.messages()[reviewed..];
    let users = tail.iter().filter(|m| m.role == "user").count() as u32;
    let calls = tail.iter().map(|m| m.tool_calls.len() as u32).sum();
    if learn::kind_for(users, calls).is_some() {
        let mut st = learn::state(&agent.paths);
        st["reviewed_seq"][&agent.journal.session] = serde_json::json!(seq);
        learn::save_state(&agent.paths, &st);
        learn::spawn_detached(&agent.journal.session);
    }
}

pub(super) fn maybe_learn(
    agent: &mut Agent,
    tx: &mpsc::UnboundedSender<Event>,
    reviewed: &mut (usize, i64),
    force: bool,
) {
    if !agent.cfg.learn.enabled && !force {
        return;
    }
    let mem = agent.turns_since_memory >= agent.cfg.learn.memory_every_turns;
    let skill = agent.tool_calls_since_skill >= agent.cfg.learn.skill_every_tool_calls;
    let kind = match (mem || force, skill || force) {
        (true, true) => learn::Kind::Both,
        (true, false) => learn::Kind::Memory,
        (false, true) => learn::Kind::Skills,
        _ => return,
    };
    if reviewed.0 >= agent.messages().len() {
        return;
    }
    agent.turns_since_memory = 0;
    agent.tool_calls_since_skill = 0;
    let slice = agent.messages()[reviewed.0..].to_vec();
    *reviewed = (agent.messages().len(), agent.journal.last_seq());
    let (cfg, paths, tx, id, upto) =
        (agent.cfg.clone(), agent.paths.clone(), tx.clone(), agent.journal.session.clone(), reviewed.1);
    tokio::spawn(async move {
        let _ = tx.send(Event::Learn { status: "learning from this session".into() });
        if let Some(r) = learn::distill(&cfg, &paths, &id, &slice).await {
            let status = match r {
                Ok(o) => {
                    learn::log_line(&paths, &format!("session {id} (live): {} | {}", o.summary, o.actions.join("; ")));
                    if o.actions.is_empty() { "learned nothing new".to_string() } else { o.summary }
                }
                Err(e) => format!("learning failed: {e}"),
            };
            let mut st = learn::state(&paths);
            st["reviewed_seq"][&id] = serde_json::json!(upto);
            learn::save_state(&paths, &st);
            let _ = tx.send(Event::Learn { status });
            return;
        }
        let gate = learn::gate(&cfg, &slice).await;
        learn::log_line(&paths, &format!("session {id} (live): gate {} {} -> {:?}", gate.source, gate.note, gate.kind));
        // The counters said "time to look"; the gate says whether there's anything to see.
        let kind = match (gate.kind, force) {
            (Some(k), _) => k,
            (None, true) => kind,
            (None, false) => {
                let mut st = learn::state(&paths);
                st["reviewed_seq"][&id] = serde_json::json!(upto);
                learn::save_state(&paths, &st);
                let _ = tx.send(Event::Learn { status: "nothing new to learn".into() });
                return;
            }
        };
        let status = match learn::review(&cfg, &paths, &id, &slice, kind).await {
            Ok(o) => {
                learn::log_line(&paths, &format!("session {id} (live): {} | {}", o.summary, o.actions.join("; ")));
                if o.actions.is_empty() {
                    "learned nothing new".to_string()
                } else {
                    format!(
                        "learned: {}",
                        o.actions.iter().map(|a| a.split(':').next().unwrap_or("")).collect::<Vec<_>>().join(", ")
                    )
                }
            }
            Err(e) => format!("learning failed: {e}"),
        };
        let mut st = learn::state(&paths);
        st["reviewed_seq"][&id] = serde_json::json!(upto);
        learn::save_state(&paths, &st);
        let _ = tx.send(Event::Learn { status });
    });
}
