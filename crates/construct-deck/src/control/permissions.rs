//! Bounded model text for optional, read-only permission observations.
use serde_json::Value;

const LIMIT: usize = 16;

fn text(value: &Value, fallback: &str, limit: usize) -> String {
    let value = value.as_str().unwrap_or(fallback);
    let flat: String = value.chars().take(limit).map(|c| if c.is_control() { ' ' } else { c }).collect();
    flat
}

pub(super) fn append(out: &mut String, view: &Value) {
    let valid_id = |value: &Value| value.as_str().is_some_and(|s| !s.is_empty() && s.chars().count() <= 128);
    let entries = |key: &str| view[key].as_array().filter(|items| items.len() <= LIMIT);
    let (Some(count), Some(pending), Some(resolved)) =
        (view["pending_count"].as_u64(), entries("pending"), entries("resolved"))
    else {
        return;
    };
    if !valid_id(&view["cursor"]) || count < pending.len() as u64 {
        return;
    }
    if pending.iter().chain(resolved).any(|item| {
        !valid_id(&item["observation_id"]) || (item.get("turn").is_some() && item["turn"].as_u64().is_none())
    }) {
        return;
    }
    if pending
        .iter()
        .any(|item| !item["subject"].is_string() || !item["kind"].is_string() || !item["truncated"].is_boolean())
        || resolved.iter().any(|item| !item["verdict"].is_string() || !item["source"].is_string())
    {
        return;
    }
    if let Some(count) = view["pending_count"].as_u64() {
        out.push_str(&format!("; user permissions pending: {count}"));
    }
    if let Some(pending) = view["pending"].as_array() {
        for question in pending.iter().take(LIMIT) {
            let id = text(&question["observation_id"], "?", 128);
            let kind = question["kind"]
                .as_str()
                .filter(|kind| ["read", "edit", "delete", "move", "search", "execute", "think", "fetch"].contains(kind))
                .unwrap_or("other");
            let subject = text(&question["subject"], "a tool", 256);
            let cut = if question["truncated"] == true { " (cut by Deck)" } else { "" };
            out.push_str(&format!("; question {id} pending with user: {kind} {subject:?}{cut}"));
        }
        if view["pending_count"].as_u64().is_some_and(|n| n > pending.len().min(LIMIT) as u64) {
            out.push_str("; more questions omitted");
        }
    }
    if let Some(resolved) = view["resolved"].as_array() {
        for question in resolved.iter().take(LIMIT) {
            let id = text(&question["observation_id"], "?", 128);
            let verdict = match question["verdict"].as_str() {
                Some(verdict @ ("allowed" | "denied" | "cancelled")) if question["source"] == "response" => verdict,
                _ => "unknown",
            };
            let source = match question["source"].as_str() {
                Some("response") => "observed client response",
                Some("retired") => "retired without observed answer",
                _ => "unrecognized observation",
            };
            out.push_str(&format!("; question {id}: {verdict} ({source})"));
        }
        if !resolved.is_empty() {
            out.push_str("; responses do not confirm delivery or execution");
        }
    }
}
