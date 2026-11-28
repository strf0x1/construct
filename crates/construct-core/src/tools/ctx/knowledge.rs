//! Memory and skill library operations on `Ctx`. Keeps the global library,
//! optional project kit, and memory store encapsulated behind the operations
//! tools and the agent need.

use super::Ctx;
use crate::memory::{Op, Target};
use crate::skills::{Library, Skill};

impl Ctx {
    /// Apply atomic memory edits to `target`.
    pub fn apply_memory(&self, target: Target, ops: &[Op]) -> Result<String, String> {
        self.memory.apply(target, ops)
    }

    /// The project kit (when there is one) and then the global library: the
    /// order a name is looked up in, so a kit skill hides a global one.
    pub fn skill_libs(&self) -> Vec<&Library> {
        self.project_skills.iter().chain(std::iter::once(&self.skills)).collect()
    }

    /// The library that holds `name`, kit first.
    pub(crate) fn skill_lib(&self, name: &str) -> Option<&Library> {
        self.skill_libs().into_iter().find(|l| l.get(name).is_some())
    }

    /// Choose the library a `skill_manage` mutation targets: an existing project
    /// skill stays in the project kit; a new `"project"` skill touches the
    /// project kit directory first; everything else goes to the global library.
    pub fn writable_skill_lib(&self, name: Option<&str>, scope: Option<&str>) -> Result<&Library, String> {
        let project = self.project_skills.as_ref();
        match (scope, name.and_then(|n| project.filter(|p| p.get(n).is_some()))) {
            (_, Some(p)) => Ok(p),
            (Some("project"), None) => match (project, &self.memory.project) {
                (Some(p), Some(pr)) => {
                    pr.touch();
                    Ok(p)
                }
                _ => Err("not in a git repo, so there is no project kit; use scope 'global' (the default)".into()),
            },
            _ => Ok(&self.skills),
        }
    }

    /// A skill as the model reads it: where it lives, then its SKILL.md and
    /// linked files (or the one `file`). The `skill` tool and a user's
    /// `/<skill>` both load through here, so the model sees the same text.
    pub fn load_skill(&self, name: &str, file: Option<&str>) -> Result<String, String> {
        let lib = self.skill_lib(name).unwrap_or(&self.skills);
        let body = lib.view(name, file)?;
        let head = if file.is_none() { format!("[{}]\n", self.locate_skill(lib, name)) } else { String::new() };
        Ok(format!("{head}{body}"))
    }

    /// `global: ~/.construct/skills/x`, for results the model reads back.
    pub fn locate_skill(&self, lib: &Library, name: &str) -> String {
        let scope = if std::ptr::eq(lib, &self.skills) { "global" } else { "project" };
        let path = lib.path_of(name).join("SKILL.md").display().to_string();
        let home = std::env::var("HOME").ok().filter(|h| !h.is_empty());
        let path = match home.as_deref().and_then(|h| path.strip_prefix(h)) {
            Some(rest) => format!("~{rest}"),
            None => path,
        };
        format!("{scope}: {path}")
    }

    /// Every skill the model or the user can reach, read from disk now.
    pub(crate) fn all_skills(&self) -> Vec<Skill> {
        crate::skills::merged(&self.skill_libs())
    }
}
