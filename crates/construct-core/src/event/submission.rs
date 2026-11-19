//! A prompt as submitted: the original text, and the client's idempotency key
//! when it sent one (contract 1.3.0 §12, docs/adr/0045).
//!
//! The design choice to know: the key is journaled on the same row as the
//! prompt, before routing or the model sees anything, so a prompt and its key
//! are durable together. A crash before that row means the prompt never ran
//! and a resend runs it once. A crash after it means the key is already on
//! record. The row's JSON is unchanged from when `Submitted` was a struct
//! variant: the fields sit beside the `event` tag, and a row without a key
//! reads and writes exactly as before.

use super::Event;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Submission {
    pub text: String,
    /// Never sent back over ACP (§12.4): `Submission::private` strips it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idempotency_key: Option<String>,
}

impl Event {
    /// A submitted prompt without a key, as the TUI and `run` submit.
    pub fn submitted(text: impl Into<String>) -> Self {
        Event::Submitted(Submission { text: text.into(), idempotency_key: None })
    }

    /// This event as it may cross the ACP wire: a submission loses its key.
    pub fn private(&self) -> std::borrow::Cow<'_, Event> {
        match self {
            Event::Submitted(s) if s.idempotency_key.is_some() => {
                std::borrow::Cow::Owned(Event::Submitted(Submission { text: s.text.clone(), idempotency_key: None }))
            }
            _ => std::borrow::Cow::Borrowed(self),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_row_without_a_key_keeps_the_struct_variant_shape() {
        let json = serde_json::to_value(Event::submitted("go")).unwrap();
        assert_eq!(json, serde_json::json!({ "event": "submitted", "text": "go" }));
        let back: Event = serde_json::from_value(json).unwrap();
        assert!(matches!(back, Event::Submitted(Submission { idempotency_key: None, .. })));
    }

    #[test]
    fn a_key_is_journaled_but_never_sent() {
        let keyed = Event::Submitted(Submission { text: "go".into(), idempotency_key: Some("k1".into()) });
        let json = serde_json::to_value(&keyed).unwrap();
        assert_eq!(json["idempotency_key"], "k1");
        let wire = serde_json::to_value(keyed.private()).unwrap();
        assert_eq!(wire, serde_json::json!({ "event": "submitted", "text": "go" }));
    }
}
