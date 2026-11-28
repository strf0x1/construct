//! Skills the agent writes and improves. One directory per skill holding a
//! SKILL.md (frontmatter: name, description) plus optional references/,
//! templates/ and scripts/. Nothing is bundled. Deletes are archives, so the
//! curator can never lose work. A `Library` reads; every write goes through a
//! `Writer` and `guard`, because a loaded skill enters the prompt. A session
//! reaches its skills through a `Stack`: the repo's kit over the global library.

use crate::clock::now;
use crate::home::{Paths, write_atomic};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

mod format;
mod stack;
mod write;
pub use format::{frontmatter, valid_name};
pub use stack::{Kit, Stack};
pub use write::Writer;

/// The wire names of the two skill tools (`construct-tools-skills`). They
/// live beside the library because core speaks about them without having
/// them: the learning pass's prompts, and the gate that counts loaded skills
/// (ADR 0064). The tools take their `NAME` from here.
pub const VIEW_TOOL: &str = "skill";
pub const MANAGE_TOOL: &str = "skill_manage";

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub dir: PathBuf,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub views: u32,
    #[serde(default)]
    pub patches: u32,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default)]
    pub last_used: u64,
    /// "agent" for skills the agent wrote; the curator only touches those.
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub pinned: bool,
    /// Times the router put this skill in front of the model. Suggested
    /// often but rarely viewed means the description attracts the wrong
    /// requests.
    #[serde(default)]
    pub suggested: u32,
}

pub struct Library {
    dir: PathBuf,
    /// A project kit's skills/, not the global library: only `locate` says so.
    project: bool,
}

impl Library {
    /// The global library.
    pub fn new(paths: &Paths) -> Self {
        Self { dir: paths.skills_dir(), project: false }
    }

    /// A project kit's skills/ (`Kit`).
    fn kit(dir: PathBuf) -> Self {
        Self { dir, project: true }
    }

    /// `global: ~/.construct/skills/x/SKILL.md`, for results the model reads back.
    pub fn locate(&self, name: &str) -> String {
        let scope = if self.project { "project" } else { "global" };
        let path = self.dir.join(name).join("SKILL.md").display().to_string();
        let home = std::env::var("HOME").ok().filter(|h| !h.is_empty());
        let path = match home.as_deref().and_then(|h| path.strip_prefix(h)) {
            Some(rest) => format!("~{rest}"),
            None => path,
        };
        format!("{scope}: {path}")
    }

    pub fn list(&self) -> Vec<Skill> {
        let mut out = Vec::new();
        let Ok(rd) = std::fs::read_dir(&self.dir) else { return out };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.starts_with('.') || !e.path().is_dir() {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(e.path().join("SKILL.md")) else { continue };
            let (fm, body) = frontmatter(&text);
            let description = fm.get("description").cloned().unwrap_or_else(|| {
                body.lines().find(|l| !l.trim().is_empty() && !l.starts_with('#')).unwrap_or("").to_string()
            });
            out.push(Skill { name, description, dir: e.path() });
        }
        out.sort_by(|a, b| a.name.cmp(&b.name));
        out
    }

    pub fn get(&self, name: &str) -> Option<Skill> {
        self.list().into_iter().find(|s| s.name == name)
    }

    fn usage_path(&self) -> PathBuf {
        self.dir.join(".usage.json")
    }

    pub fn usage(&self) -> BTreeMap<String, Usage> {
        std::fs::read_to_string(self.usage_path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn suggested(&self, name: &str) {
        self.touch(name, |u| u.suggested += 1);
    }

    fn touch(&self, name: &str, f: impl FnOnce(&mut Usage)) {
        let mut all = self.usage();
        let u = all.entry(name.to_string()).or_default();
        f(u);
        if let Ok(text) = serde_json::to_string_pretty(&all) {
            let _ = write_atomic(&self.usage_path(), text.as_bytes());
        }
    }

    /// SKILL.md (or one of its files) plus the list of linked files.
    pub fn view(&self, name: &str, file: Option<&str>) -> Result<String, String> {
        let skill = self.get(name).ok_or_else(|| self.not_found(name))?;
        if let Some(f) = file {
            let p = safe_join(&skill.dir, f)?;
            return std::fs::read_to_string(&p).map_err(|e| format!("{f}: {e}"));
        }
        let text = std::fs::read_to_string(skill.dir.join("SKILL.md")).map_err(|e| e.to_string())?;
        self.touch(name, |u| {
            u.views += 1;
            u.last_used = now();
        });
        let files = linked_files(&skill.dir);
        if files.is_empty() {
            Ok(text)
        } else {
            Ok(format!("{text}\n\n[linked files — view with file=<path>]\n{}", files.join("\n")))
        }
    }

    fn not_found(&self, name: &str) -> String {
        let names: Vec<String> = self.list().into_iter().map(|s| s.name).collect();
        if names.is_empty() {
            format!("no skill named {name:?}; the library is empty")
        } else {
            format!("no skill named {name:?}. Available: {}", names.join(", "))
        }
    }
}

fn safe_join(dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let p = Path::new(rel);
    if p.is_absolute() || p.components().any(|c| matches!(c, std::path::Component::ParentDir)) {
        return Err(format!("{rel}: paths must stay inside the skill"));
    }
    Ok(dir.join(p))
}

fn linked_files(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for sub in ["references", "templates", "scripts"] {
        if let Ok(rd) = std::fs::read_dir(dir.join(sub)) {
            for e in rd.flatten() {
                out.push(format!("{sub}/{}", e.file_name().to_string_lossy()));
            }
        }
    }
    out.sort();
    out
}
