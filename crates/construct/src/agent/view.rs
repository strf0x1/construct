//! The view the context strategy sends to the model. The strategy owns the
//! message list (ADR 0042); the rest of the agent reads it through here so no
//! caller depends on how a strategy stores it.

use super::*;

impl Agent {
    /// The view the strategy sends to the model.
    pub fn messages(&self) -> &[Message] {
        self.context.messages()
    }
}
