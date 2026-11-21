//! `construct sessions export`: a session in the JSONL shape the old
//! `sessions/<id>.jsonl` files had (`session`, `message`, `usage`, `route`,
//! `tool`, `compact` rows), so chatstrata's `construct` source keeps working
//! until it attaches the SQLite file directly. A dump of the record, never a
//! second record.

use super::Entry;
use crate::event::Event;
use serde_json::{Value, json};

/// `state_tool` is the one tool whose end switches state, not the world: it
/// never had a `tool` record, so it is skipped (the caller names it).
pub fn export(entries: &[Entry], state_tool: &str) -> Vec<Value> {
    let mut model = String::new();
    let mut out = Vec::new();
    for Entry { at_ms: at, event, .. } in entries {
        let line = match event {
            Event::Head { id, harness, version, cwd, model: m, manifest, .. } => {
                model = m.clone();
                json!({ "type": "session", "id": id, "harness": harness, "version": version, "cwd": cwd, "model": m, "manifest": manifest, "at": at })
            }
            Event::Request { model: m, .. } => {
                model = m.clone();
                continue;
            }
            Event::Usage { prompt, completion, cached, cache_write, cost, .. } => json!({
                "type": "usage", "at": at, "model": model,
                "usage": {
                    "prompt_tokens": prompt, "completion_tokens": completion, "cached_tokens": cached,
                    "cache_write_tokens": cache_write, "cost": cost,
                },
            }),
            Event::Route { groups, skills, source, ms, note } => {
                // The old rows held skills as [name, score] pairs.
                let skills: Vec<Value> = skills
                    .iter()
                    .map(|s| match s.rsplit_once(' ').and_then(|(n, p)| Some((n, p.parse::<f64>().ok()?))) {
                        Some((n, p)) => json!([n, p]),
                        None => json!([s, 0.0]),
                    })
                    .collect();
                json!({ "type": "route", "at": at, "data": { "groups": groups, "skills": skills, "source": source, "ms": ms, "note": note } })
            }
            Event::ToolEnd { name, .. } if name == state_tool => continue,
            Event::ToolEnd { id, name, ok, ms, code, data, .. } => json!({
                "type": "tool", "at": at,
                "data": { "id": id, "name": name, "ok": ok, "ms": ms, "code": code, "data": data },
            }),
            Event::Compacted { before, after, .. } => {
                json!({ "type": "compact", "at": at, "data": { "before": before, "after": after } })
            }
            e => match e.message() {
                Some(m) => json!({ "type": "message", "at": at, "message": m }),
                None => continue,
            },
        };
        out.push(line);
    }
    out
}
