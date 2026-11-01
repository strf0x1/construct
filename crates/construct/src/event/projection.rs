//! Message and durability projections of the additive Event vocabulary.
//! Compaction replay uses only these model-facing message carriers.

use super::Event;
use crate::llm::Message;

impl Event {
    /// Whether the journal keeps it. Stream deltas are not kept: the
    /// assistant message that follows holds the whole text, and a row per
    /// token would be most of the journal.
    pub fn durable(&self) -> bool {
        !matches!(
            self,
            Event::Opened
                | Event::Text { .. }
                | Event::Reasoning { .. }
                | Event::ToolPending { .. }
                | Event::Learn { .. }
                | Event::Child { .. }
                | Event::Ask { .. }
        )
    }

    /// The model-facing message this event carries, if any. This is the
    /// whole projector's vocabulary: resume rebuilds context from these.
    pub fn message(&self) -> Option<&Message> {
        match self {
            Event::System { message }
            | Event::User { message }
            | Event::Assistant { message }
            | Event::ToolResult { message, .. } => Some(message),
            Event::JobsDelivered(d) => d.message.as_ref(),
            _ => None,
        }
    }

    /// The event for a message the agent appends to the conversation.
    pub fn for_message(message: Message) -> Event {
        match message.role.as_str() {
            "system" => Event::System { message },
            "assistant" => Event::Assistant { message },
            "tool" => Event::ToolResult { id: message.tool_call_id.clone().unwrap_or_default(), message },
            _ => Event::User { message },
        }
    }
}
