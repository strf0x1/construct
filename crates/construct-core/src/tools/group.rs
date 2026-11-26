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
    /// Sending work to peers (constructs on other machines, #366); present
    /// only when this home runs a hub.
    Peers,
    /// Deck's boundary tools; present only when the VM runtime set an
    /// endpoint, so it is never offered by the router.
    Boundary,
    /// The coordinator's `deck` tool; present only when Deck set a control
    /// gateway, always on there, and never offered by the router.
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
            Group::Peers => "peers".into(),
            Group::Boundary => "boundary".into(),
            Group::Coordinator => "coordinator".into(),
            Group::Jobs => "jobs".into(),
            Group::Mcp(s) => format!("mcp:{s}"),
        }
    }

    /// Added only by Deck's environment.
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
            "peers" => Group::Peers,
            "skills" => Group::Skills,
            "skill-write" => Group::SkillWrite,
            l => Group::Mcp(l.strip_prefix("mcp:")?.to_string()),
        })
    }

    /// The groups `[tools] initial` turns on from the first request (#187):
    /// built-in ones the model could be given later, plus `jobs`. A label that
    /// is not one is an error, so a mistyped experiment arm fails instead of
    /// measuring nothing.
    pub fn initial(labels: &[String]) -> Result<Vec<Group>, String> {
        labels
            .iter()
            .map(|l| match l.as_str() {
                "jobs" => Ok(Group::Jobs),
                l => Group::parse(l)
                    .filter(|g| !matches!(g, Group::Mcp(_)))
                    .ok_or_else(|| format!("[tools] initial: unknown group {l:?}")),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::Group;

    #[test]
    fn initial_takes_built_in_labels_and_rejects_the_rest() {
        let l = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(Group::initial(&l(&["web", "memory", "jobs"])), Ok(vec![Group::Web, Group::Memory, Group::Jobs]));
        assert!(Group::initial(&l(&["wbe"])).unwrap_err().contains("\"wbe\""));
        assert!(Group::initial(&l(&["mcp:x"])).is_err());
        assert!(Group::initial(&l(&["core"])).is_err());
    }
}
