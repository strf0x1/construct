//! Memory and skill operations on `Ctx`. The memory store stays behind
//! `apply_memory`; skill lookup (kit first, then global) is the session's
//! `skills::Stack`, which tools, the agent and the router read directly.

use super::Ctx;
use crate::memory::{Op, Target};
use crate::skills::Stack;

impl Ctx {
    /// Apply atomic memory edits to `target`.
    pub fn apply_memory(&self, target: Target, ops: &[Op]) -> Result<String, String> {
        self.memory.apply(target, ops)
    }

    /// The skills this session reaches: the repo's kit over the global library.
    pub fn skills(&self) -> &Stack {
        &self.skills
    }
}
