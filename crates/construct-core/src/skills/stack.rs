//! The skills one session can reach: the current repo's kit (when construct
//! runs in a repo) stacked over the global library.
//!
//! The design choice: the kit comes first everywhere a name is looked up, so
//! a kit skill hides a global one of the same name. Every reader (the tools,
//! the router, the prompt, the learning pass, the TUI's slash palette) asks a
//! `Stack` instead of rebuilding the order from a `Project`.

use super::{Library, Skill, Writer};
use crate::home::Paths;
use crate::project::Project;

/// A repo's own skills: its project kit's `skills/`, and the project they
/// belong to.
pub struct Kit {
    lib: Library,
    project: Project,
}

impl Kit {
    pub fn new(project: &Project) -> Self {
        Self { lib: Library::kit(project.skills_dir()), project: project.clone() }
    }

    /// The repo's name, as the prompt heads its kit with.
    pub fn name(&self) -> &str {
        &self.project.name
    }

    pub fn list(&self) -> Vec<Skill> {
        self.lib.list()
    }

    pub fn get(&self, name: &str) -> Option<Skill> {
        self.lib.get(name)
    }

    /// The kit, for writing: records which repo the kit belongs to first, so
    /// a kit is never written without its `project.json`.
    pub fn writable(&self) -> Writer<'_> {
        self.project.touch();
        Writer::new(&self.lib)
    }
}

/// The kit (when there is one) over the global library.
pub struct Stack {
    kit: Option<Kit>,
    global: Library,
}

impl Stack {
    pub fn new(paths: &Paths, project: Option<&Project>) -> Self {
        Self { kit: project.map(Kit::new), global: Library::new(paths) }
    }

    pub fn kit(&self) -> Option<&Kit> {
        self.kit.as_ref()
    }

    /// The libraries in lookup order: kit first.
    pub fn libs(&self) -> Vec<&Library> {
        self.kit.iter().map(|k| &k.lib).chain(std::iter::once(&self.global)).collect()
    }

    fn find(&self, name: &str) -> Option<&Library> {
        self.libs().into_iter().find(|l| l.get(name).is_some())
    }

    /// The router put `name` in front of the model: count it where it lives.
    pub fn suggested(&self, name: &str) {
        if let Some(lib) = self.find(name) {
            lib.suggested(name);
        }
    }

    /// The library a `skill_manage` mutation targets: an existing kit skill
    /// stays in the kit; a new `"project"` skill goes to the kit (touching it
    /// first); everything else goes to the global library.
    pub fn writable(&self, name: Option<&str>, scope: Option<&str>) -> Result<Writer<'_>, String> {
        let kit = self.kit.as_ref();
        match (scope, name.and_then(|n| kit.filter(|k| k.get(n).is_some()))) {
            (_, Some(k)) => Ok(Writer::new(&k.lib)),
            (Some("project"), None) => match kit {
                Some(k) => Ok(k.writable()),
                None => Err("not in a git repo, so there is no project kit; use scope 'global' (the default)".into()),
            },
            _ => Ok(Writer::new(&self.global)),
        }
    }

    /// A skill as the model reads it: where it lives, then its SKILL.md and
    /// linked files (or the one `file`). The `skill` tool and a user's
    /// `/<skill>` both load through here, so the model sees the same text.
    pub fn load(&self, name: &str, file: Option<&str>) -> Result<String, String> {
        let lib = self.find(name).unwrap_or(&self.global);
        let body = lib.view(name, file)?;
        let head = if file.is_none() { format!("[{}]\n", lib.locate(name)) } else { String::new() };
        Ok(format!("{head}{body}"))
    }

    /// Every reachable skill, sorted by name, read from disk now. A kit skill
    /// hides a global one of the same name.
    pub fn all(&self) -> Vec<Skill> {
        let mut out: Vec<Skill> = Vec::new();
        for lib in self.libs() {
            for s in lib.list() {
                if !out.iter().any(|o| o.name == s.name) {
                    out.push(s);
                }
            }
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }
}
