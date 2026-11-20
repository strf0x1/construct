//! `[learn]`: when the background learning pass runs, on which model, and
//! the strategy that turns a session into knowledge. The strategy names are
//! declared here, with the setting they are values of; `learn` matches on them.

use serde::{Deserialize, Serialize};

/// `[learn] strategy` values.
pub const SKILLS: &str = "skills";
pub const CORRECTIONS: &str = "corrections";
pub const PLAYBOOK: &str = "playbook";
pub const WORKFLOW: &str = "workflow";
pub const HYBRID: &str = "hybrid";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct LearnConfig {
    pub enabled: bool,
    /// Review memory after this many user turns.
    pub memory_every_turns: u32,
    /// Review skills after this many tool calls.
    pub skill_every_tool_calls: u32,
    /// Model for the learning pass; empty means the main model.
    pub model: String,
    /// Days between curator passes over agent-written skills.
    pub curator_every_days: u32,
    /// How a session becomes something the next one can use. "hybrid" (the
    /// default, ADR 0022): facts and conventions as playbook bullets,
    /// procedures as project skills with a pointer bullet. The others stay as
    /// bench arms: "skills" (the gate, ledger and recurrence-gated skills),
    /// "corrections", "playbook", "workflow": the constants above.
    pub strategy: String,
}

impl Default for LearnConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            memory_every_turns: 6,
            skill_every_tool_calls: 10,
            model: String::new(),
            curator_every_days: 7,
            strategy: HYBRID.into(),
        }
    }
}
