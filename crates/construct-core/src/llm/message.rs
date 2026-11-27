//! The conversation as the model sees it: one `Message` per turn of the
//! exchange, kept verbatim in the journal, and its wire form. The one choice
//! to know: an assistant's reasoning goes back under the key its provider
//! streamed it in, and only then (ADR 0044), so a server is sent nothing it
//! did not send first.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// The delta key of OpenRouter's structured reasoning, and the message key it
/// goes back under.
pub(super) const DETAILS: &str = "reasoning_details";

/// The delta keys reasoning text streams under, in order of preference.
pub(super) const REASONING_KEYS: [&str; 2] = ["reasoning_content", "reasoning"];

/// Wire value for an ephemeral prompt-cache breakpoint.
pub(super) fn ephemeral() -> Value {
    json!({ "type": "ephemeral" })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Message {
    pub role: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// The message as content parts, sent instead of `content` when non-empty.
    /// A marked part is where the provider should cache (ADR 0043 decision 6).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<Part>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
    /// Reasoning text some providers stream separately. Sent back on the
    /// assistant message under `reasoning_field`, the key it arrived in, so a
    /// thinking model keeps its chain across tool calls (ADR 0044).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    /// The delta key `reasoning` streamed under (`reasoning_content`,
    /// `reasoning`). None for rows journaled before it was recorded, which
    /// are never echoed: a server is only sent the field it sent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_field: Option<String>,
    /// OpenRouter's structured reasoning (signed or encrypted blocks),
    /// returned verbatim on the next request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_details: Option<Value>,
}

/// One text part of a [`Message`]. `mark` asks the dialect to put the cache
/// breakpoint on this part; only a strategy that splits a message sets it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Part {
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub mark: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type", default = "function_type")]
    pub kind: String,
    pub function: FunctionCall,
    /// Provider data riding on the call, sent back as it came: Gemini's
    /// thought signature (`extra_content.google.thought_signature`), which
    /// Gemini 3 and later require on the current turn's calls (ADR 0044).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extra_content: Option<Value>,
}

fn function_type() -> String {
    "function".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

impl Message {
    pub fn system(text: impl Into<String>) -> Self {
        Self::text("system", text)
    }
    pub fn user(text: impl Into<String>) -> Self {
        Self::text("user", text)
    }
    pub fn text(role: &str, text: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: Some(text.into()),
            parts: vec![],
            tool_calls: vec![],
            tool_call_id: None,
            reasoning: None,
            reasoning_field: None,
            reasoning_details: None,
        }
    }
    /// A message sent as content parts rather than one string.
    pub fn parts(role: &str, parts: Vec<Part>) -> Self {
        Self { content: None, parts, ..Self::text(role, "") }
    }
    pub fn tool(id: &str, text: impl Into<String>) -> Self {
        Self { tool_call_id: Some(id.into()), ..Self::text("tool", text) }
    }
    /// The wire form: empty assistant content as null, and an assistant's
    /// reasoning under the key its provider streamed it in.
    pub(super) fn wire(&self) -> Value {
        let mut v = json!({ "role": self.role });
        if self.parts.is_empty() {
            match (&self.content, self.role.as_str()) {
                (Some(c), _) => v["content"] = json!(c),
                (None, "assistant") => v["content"] = Value::Null,
                (None, _) => v["content"] = json!(""),
            }
        } else {
            let parts: Vec<Value> = self
                .parts
                .iter()
                .filter(|p| !p.text.is_empty())
                .map(|p| {
                    let mut part = json!({ "type": "text", "text": p.text });
                    if p.mark {
                        part["cache_control"] = ephemeral();
                    }
                    part
                })
                .collect();
            v["content"] = json!(parts);
        }
        if !self.tool_calls.is_empty() {
            v["tool_calls"] = json!(self.tool_calls);
        }
        if let Some(id) = &self.tool_call_id {
            v["tool_call_id"] = json!(id);
        }
        // The structured form, when there is one, is the whole reasoning:
        // sending the plain text beside it would send it twice.
        if self.role == "assistant" {
            match (&self.reasoning_details, &self.reasoning, &self.reasoning_field) {
                (Some(d), _, _) => v[DETAILS] = d.clone(),
                (None, Some(r), Some(field)) => v[field.as_str()] = json!(r),
                _ => {}
            }
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn part(text: &str, mark: bool) -> Part {
        Part { text: text.into(), mark }
    }

    #[test]
    fn a_message_without_parts_serializes_as_before() {
        let v = serde_json::to_value(Message::user("hi")).unwrap();
        assert!(v.get("parts").is_none());
        // Old journal rows have no `parts` key and still load.
        let old: Message = serde_json::from_value(json!({ "role": "user", "content": "hi" })).unwrap();
        assert_eq!(old, Message::user("hi"));
    }

    #[test]
    fn marked_parts_wire_as_content_parts() {
        let m = Message::parts("user", vec![part("a", false), part("b", true)]);
        let v = m.wire();
        assert_eq!(v["content"][0], json!({ "type": "text", "text": "a" }));
        assert_eq!(v["content"][1], json!({ "type": "text", "text": "b", "cache_control": { "type": "ephemeral" } }));
    }

    #[test]
    fn empty_parts_are_skipped() {
        let m = Message::parts("user", vec![part("a", true), part("", false), part("", true)]);
        let v = m.wire();
        assert_eq!(v["content"].as_array().unwrap().len(), 1);
        assert_eq!(v["content"][0]["text"], "a");
    }

    fn thinking() -> Message {
        Message {
            reasoning: Some("why".into()),
            reasoning_field: Some("reasoning_content".into()),
            reasoning_details: Some(json!([{ "type": "reasoning.text", "text": "why" }])),
            ..Message::text("assistant", "ok")
        }
    }

    #[test]
    fn an_assistant_sends_its_reasoning_back_under_its_own_key() {
        let v = Message { reasoning_details: None, ..thinking() }.wire();
        assert_eq!(v["reasoning_content"], "why");
        assert!(v.get("reasoning").is_none() && v.get("reasoning_details").is_none());
    }

    #[test]
    fn structured_reasoning_goes_back_alone() {
        let v = thinking().wire();
        assert_eq!(v["reasoning_details"][0]["text"], "why");
        assert!(v.get("reasoning_content").is_none(), "the text once, not twice: {v}");
    }

    #[test]
    fn a_tool_calls_extra_content_goes_back_as_it_came() {
        let sig = json!({ "google": { "thought_signature": "c2ln" } });
        let call = ToolCall {
            id: "c1".into(),
            kind: "function".into(),
            function: FunctionCall { name: "read".into(), arguments: "{}".into() },
            extra_content: Some(sig.clone()),
        };
        let v = Message { tool_calls: vec![call], ..Message::text("assistant", "") }.wire();
        assert_eq!(v["tool_calls"][0]["extra_content"], sig);
    }

    #[test]
    fn reasoning_without_a_recorded_key_stays_home() {
        // A row journaled before the key was recorded.
        let v = Message { reasoning_field: None, reasoning_details: None, ..thinking() }.wire();
        assert_eq!(v, json!({ "role": "assistant", "content": "ok" }));
        // And only an assistant echoes.
        let v = Message { role: "user".into(), ..thinking() }.wire();
        assert_eq!(v, json!({ "role": "user", "content": "ok" }));
    }
}
