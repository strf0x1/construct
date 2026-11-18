//! The named sets tools are switched on in. The core group is always present;
//! the others are turned on per turn by the router, or by the model, and stay
//! on once on (`tools::Catalog`).

#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Group {
    Core,
    Web,
    Memory,
    Skills,
    SkillWrite,
    History,
    /// Checking and cancelling background jobs; on once the session has one.
    Jobs,
    Developer,
    /// Deck's boundary tools; present only when the VM runtime set an
    /// endpoint, so it is never offered by the router.
    Boundary,
    /// The coordinator's `deck` tool; present only when Deck set a control
    /// gateway, always on there, and never offered by the router or given to
    /// a delegated child.
    Coordinator,
    Mcp(String),
}

impl Group {
    pub fn label(&self) -> String {
        match self {
            Group::Core => "core".into(),
            Group::Web => "web".into(),
            Group::Memory => "memory".into(),
            Group::Skills => "skills".into(),
            Group::SkillWrite => "skill-write".into(),
            Group::History => "history".into(),
            Group::Developer => "developer".into(),
            Group::Boundary => "boundary".into(),
            Group::Coordinator => "coordinator".into(),
            Group::Jobs => "jobs".into(),
            Group::Mcp(s) => format!("mcp:{s}"),
        }
    }

    /// Added only by Deck's environment, so a delegated child never has it.
    pub fn from_deck(&self) -> bool {
        matches!(self, Group::Boundary | Group::Coordinator)
    }

    /// The inverse of `label` for the groups the model may ask for.
    pub fn parse(label: &str) -> Option<Group> {
        Some(match label {
            "web" => Group::Web,
            "memory" => Group::Memory,
            "history" => Group::History,
            "developer" => Group::Developer,
            "skills" => Group::Skills,
            "skill-write" => Group::SkillWrite,
            l => Group::Mcp(l.strip_prefix("mcp:")?.to_string()),
        })
    }
}
