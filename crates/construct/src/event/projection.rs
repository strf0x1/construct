//! Message and durability projections of the additive Event vocabulary.
//! Compaction replay uses only these model-facing message carriers.

use super::Event;
use crate::llm::Message;
use serde::{Deserialize, Serialize};

/// One node of the `optchat` tree as the journal records it (ADR 0043). A
/// leaf has `l == 0`; a merge has `l >= 1`. `fallback` marks a line that was
/// written without the summary model succeeding.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct MemoryNode {
    pub l: u32,
    pub i: u64,
    pub text: String,
    pub fallback: bool,
}

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
            Event::DeckBoundary { message, .. } => Some(message),
            Event::Cutoff { message } => Some(message),
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
