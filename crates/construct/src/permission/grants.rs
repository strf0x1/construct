//! What has been approved for the session, and what no prompt may offer.
//!
//! Protected places (`~/.ssh`, `~/.gnupg`, the bench key files) are never
//! shown in a prompt, never approved by a session grant, and never inside a
//! folder grant: the only way in is a path typed into `config.toml`, which
//! `Policy::new` still honours. An agent that has read the bench keys once is
//! the reason this list exists (docs/adr/0004).

use super::Access;
use crate::sandbox::Policy;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Under `$HOME`; a prompt can't be the way in to any of them.
const PROTECTED: &[&str] = &[".ssh", ".gnupg", ".config/construct-bench.key", ".config/construct-bench-jev.key"];

fn protected_roots() -> Vec<PathBuf> {
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else { return vec![] };
    PROTECTED.iter().map(|r| Policy::real(&home.join(r))).collect()
}

/// Whether `p` is a place a prompt can't approve.
pub fn protected(p: &Path) -> bool {
    let real = Policy::real(p);
    protected_roots().iter().any(|root| real.starts_with(root))
}

/// The folder holding `path`, if approving that folder instead of the one
/// path is reasonable: it isn't `$HOME` or above it, and holds nothing
/// protected. A path that is itself a folder has none.
pub fn folder_for(path: &Path) -> Option<PathBuf> {
    if path.is_dir() {
        return None;
    }
    let parent = path.parent()?;
    if parent.components().count() < 3 {
        return None;
    }
    let home = std::env::var_os("HOME").map(|h| Policy::real(Path::new(&h)));
    if home.is_some_and(|h| h.starts_with(parent)) {
        return None;
    }
    if protected_roots().iter().any(|r| r.starts_with(parent)) {
        return None;
    }
    Some(parent.to_path_buf())
}

/// Paths approved for one agent, as roots the sandbox policy takes.
#[derive(Debug, Clone, Default)]
pub struct Roots {
    pub read: Vec<PathBuf>,
    pub write: Vec<PathBuf>,
}

impl Roots {
    fn add(&mut self, access: Access, p: PathBuf) {
        let list = match access {
            Access::Read => &mut self.read,
            Access::Write => &mut self.write,
        };
        if !list.contains(&p) {
            list.push(p);
        }
    }
}

/// Session memory. An agent is named by its definition; the session's own
/// agent is the empty name.
#[derive(Default)]
pub struct Grants {
    paths: BTreeMap<String, Roots>,
    tools: BTreeSet<(String, String)>,
}

impl Grants {
    pub fn roots(&self, agent: &str) -> Roots {
        self.paths.get(agent).cloned().unwrap_or_default()
    }

    pub fn add_path(&mut self, agent: &str, access: Access, p: PathBuf) {
        self.paths.entry(agent.to_string()).or_default().add(access, p);
    }

    pub fn has_tool(&self, agent: &str, key: &str) -> bool {
        self.tools.contains(&(agent.to_string(), key.to_string()))
    }

    pub fn add_tool(&mut self, agent: &str, key: String) {
        self.tools.insert((agent.to_string(), key));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_and_ssh_are_protected_and_never_inside_an_offered_folder() {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap());
        assert!(protected(&home.join(".ssh/id_ed25519")));
        assert!(protected(&home.join(".gnupg")));
        assert!(protected(&home.join(".config/construct-bench.key")));
        assert!(!protected(&home.join(".aws/config")));
        // The folder holding a protected file is not on offer; one that
        // merely sits beside it is.
        assert_eq!(folder_for(&home.join(".config/construct-bench.key")), None);
        assert_eq!(folder_for(&home.join("some-sibling-repo/src/lib.rs")), Some(home.join("some-sibling-repo/src")));
        // Nothing at or above $HOME is offered.
        assert_eq!(folder_for(&home.join("notes.txt")), None);
    }

    #[test]
    fn grants_are_per_agent() {
        let mut g = Grants::default();
        g.add_path("", Access::Read, "/a".into());
        g.add_path("scout", Access::Write, "/b".into());
        g.add_path("scout", Access::Write, "/b".into());
        assert_eq!(g.roots("").read, [PathBuf::from("/a")]);
        assert_eq!(g.roots("scout").write, [PathBuf::from("/b")]);
        assert!(g.roots("other").read.is_empty());
        g.add_tool("codex", "kind:edit".into());
        assert!(g.has_tool("codex", "kind:edit") && !g.has_tool("claude", "kind:edit"));
    }
}
