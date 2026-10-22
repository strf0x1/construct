//! `Event` to the ACP updates a client understands, and (for a child that is
//! not construct) the other way: an ACP update stream to the `Event`s the
//! parent journals.
//!
//! The design choice: standard `session/update`s carry only what ACP can say
//! (text, thoughts, tool calls and their results). The rest of construct's
//! vocabulary (messages, usage, requests, compaction) has no ACP shape, so
//! a client that asks for it (`clientCapabilities._meta.construct.events`)
//! also gets every event whole as an `_construct/event` notification, the
//! extension mechanism ACP reserves for underscore methods. A construct parent
//! journals those and ignores the lossy updates; any other client never sees
//! them.

use crate::event::Event;
use crate::tools::ToolKind;
use serde_json::{Value, json};

/// The extension notification that carries one `Event` verbatim.
pub const EVENT_METHOD: &str = "_construct/event";
/// Where construct's extensions live in `_meta` objects.
pub const META_KEY: &str = "construct";

/// The `update` payload of a `session/update` for this event, if ACP has a
/// shape for it. `kind_of` names a tool's category (its `Tool::kind`).
pub fn update(ev: &Event, kind_of: &dyn Fn(&str) -> ToolKind) -> Option<Value> {
    Some(match ev {
        Event::Text { text } => chunk("agent_message_chunk", text),
        Event::Reasoning { text } => chunk("agent_thought_chunk", text),
        Event::ToolStart { id, name, args } => json!({
            "sessionUpdate": "tool_call",
            "toolCallId": id,
            "title": title(name, args),
            "kind": kind_of(name),
            "status": "in_progress",
            "rawInput": serde_json::from_str::<Value>(args).unwrap_or_else(|_| Value::String(args.clone())),
        }),
        Event::ToolEnd { id, ok, output, .. } => json!({
            "sessionUpdate": "tool_call_update",
            "toolCallId": id,
            "status": if *ok { "completed" } else { "failed" },
            "content": [{ "type": "content", "content": { "type": "text", "text": output } }],
        }),
        _ => return None,
    })
}

fn chunk(kind: &str, text: &str) -> Value {
    json!({ "sessionUpdate": kind, "content": { "type": "text", "text": text } })
}

fn title(name: &str, args: &str) -> String {
    let arg = serde_json::from_str::<Value>(args)
        .ok()
        .and_then(|v| v.as_object().and_then(|o| o.values().find_map(|x| x.as_str().map(str::to_string))))
        .unwrap_or_default();
    if arg.is_empty() {
        name.to_string()
    } else {
        format!("{name} {}", crate::llm::truncate(&arg.replace('\n', " "), 80))
    }
}

/// The text of a prompt's content blocks. Text and embedded text resources
/// are the content; a link is named, since construct reads files itself.
pub fn prompt_text(blocks: &[Value]) -> String {
    let mut parts = vec![];
    for b in blocks {
        let s = |k: &str| b.get(k).and_then(Value::as_str);
        match s("type") {
            Some("text") => parts.extend(s("text").map(str::to_string)),
            Some("resource_link") => parts.push(match (s("name"), s("uri")) {
                (Some(n), Some(u)) => format!("[{n}]({u})"),
                (_, Some(u)) => u.to_string(),
                _ => continue,
            }),
            Some("resource") => {
                let r = &b["resource"];
                if let Some(t) = r.get("text").and_then(Value::as_str) {
                    let uri = r.get("uri").and_then(Value::as_str).unwrap_or("");
                    parts.push(format!("<context uri=\"{uri}\">\n{t}\n</context>"));
                }
            }
            _ => {}
        }
    }
    parts.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::OutputKind;

    #[test]
    fn text_and_tools_map_and_the_rest_does_not() {
        let t = update(&Event::Text { text: "hi".into() }, &|_| ToolKind::Other).unwrap();
        assert_eq!(
            (t["sessionUpdate"].as_str(), t["content"]["text"].as_str()),
            (Some("agent_message_chunk"), Some("hi"))
        );
        let start = Event::ToolStart { id: "c1".into(), name: "read".into(), args: r#"{"path":"a.txt"}"#.into() };
        let s = update(&start, &|_| ToolKind::Read).unwrap();
        // The category is ACP's, the name stays in the title.
        assert_eq!((s["title"].as_str(), s["kind"].as_str()), (Some("read a.txt"), Some("read")));
        assert_eq!(s["rawInput"]["path"], "a.txt");
        let e = update(
            &Event::ToolEnd {
                id: "c1".into(),
                name: "read".into(),
                ok: false,
                output: "nope".into(),
                ms: 1,
                kind: OutputKind::Plain,
                data: None,
                code: None,
            },
            &|_| ToolKind::Other,
        )
        .unwrap();
        assert_eq!((e["status"].as_str(), e["toolCallId"].as_str()), (Some("failed"), Some("c1")));
        assert!(update(&Event::TurnStart, &|_| ToolKind::Other).is_none());
    }

    #[test]
    fn prompt_blocks_become_one_text() {
        let blocks = vec![
            json!({"type": "text", "text": "fix it"}),
            json!({"type": "resource_link", "name": "main.rs", "uri": "file:///w/main.rs"}),
            json!({"type": "resource", "resource": {"uri": "file:///w/a", "text": "body"}}),
            json!({"type": "image", "data": "x", "mimeType": "image/png"}),
        ];
        let t = prompt_text(&blocks);
        assert!(t.starts_with("fix it\n\n[main.rs](file:///w/main.rs)\n\n<context uri=\"file:///w/a\">\nbody"), "{t}");
    }
}
