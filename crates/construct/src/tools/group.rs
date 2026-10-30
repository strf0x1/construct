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
            Group::Jobs => "jobs".into(),
            Group::Mcp(s) => format!("mcp:{s}"),
        }
    }

    /// The inverse of `label` for the groups the model may ask for.
    pub fn parse(label: &str) -> Option<Group> {
        Some(match label {
            "web" => Group::Web,
            "memory" => Group::Memory,
            "history" => Group::History,
            "skills" => Group::Skills,
            "skill-write" => Group::SkillWrite,
            l => Group::Mcp(l.strip_prefix("mcp:")?.to_string()),
        })
    }
}
