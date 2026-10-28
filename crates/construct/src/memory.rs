//! Capped memory of the user and their box, after Hermes's design: two small
//! files of `§`-separated entries with hard character caps. The prompt gets a
//! snapshot frozen at session start so the prefix cache holds; writes land on
//! disk immediately and show up next session.

use crate::config::{MemoryConfig, Paths, write_atomic};
use anyhow::Result;
use serde::Deserialize;
use std::path::PathBuf;

pub const DELIM: &str = "\n§\n";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Target {
    Memory,
    User,
    /// Notes on the repo construct is running in (its project kit).
    Project,
}

#[derive(Debug, Deserialize)]
pub struct Op {
    pub action: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub old_text: Option<String>,
}

/// One store's entries, titled as the prompt titles them.
pub struct Section {
    pub target: Target,
    pub title: String,
    pub used: usize,
    pub limit: usize,
    pub entries: Vec<String>,
}

pub struct Store {
    dir: PathBuf,
    cfg: MemoryConfig,
    pub project: Option<crate::project::Project>,
}

impl Store {
    pub fn new(paths: &Paths, cfg: &MemoryConfig) -> Self {
        Self { dir: paths.memory_dir(), cfg: cfg.clone(), project: None }
    }

    /// Also hold notes for the repo at `cwd`, when it is one.
    pub fn with_project(mut self, paths: &Paths, cwd: &std::path::Path) -> Self {
        self.project = crate::project::detect(paths, cwd);
        self
    }

    fn path(&self, t: Target) -> PathBuf {
        match t {
            Target::Memory => self.dir.join("MEMORY.md"),
            Target::User => self.dir.join("USER.md"),
            Target::Project => self.project.as_ref().map(|p| p.notes()).unwrap_or_else(|| self.dir.join("none")),
        }
    }

    pub fn limit(&self, t: Target) -> usize {
        match t {
            Target::Memory => self.cfg.memory_chars,
            Target::User => self.cfg.user_chars,
            Target::Project => self.cfg.project_chars,
        }
    }

    pub fn entries(&self, t: Target) -> Vec<String> {
        std::fs::read_to_string(self.path(t))
            .map(|raw| raw.split(DELIM).map(|e| e.trim().to_string()).filter(|e| !e.is_empty()).collect())
            .unwrap_or_default()
    }

    fn size(entries: &[String]) -> usize {
        entries.join(DELIM).chars().count()
    }

    /// Apply operations atomically: the cap is checked once, on the result,
    /// so one call can evict stale entries and add new ones together.
    pub fn apply(&self, t: Target, ops: &[Op]) -> Result<String, String> {
        if t == Target::Project {
            match &self.project {
                Some(p) => p.touch(),
                None => {
                    return Err(
                        "not in a git repo, so there is no project to keep notes for; use target 'memory'".into()
                    );
                }
            }
        }
        let mut work = self.entries(t);
        for op in ops {
            match op.action.as_str() {
                "add" => {
                    let c = op.content.as_deref().map(str::trim).unwrap_or("");
                    if c.is_empty() {
                        return Err("add needs content".into());
                    }
                    if let Some(why) = scan(c) {
                        return Err(why);
                    }
                    if work.iter().any(|e| e == c) {
                        continue;
                    }
                    work.push(c.to_string());
                }
                "replace" | "remove" => {
                    let old = op.old_text.as_deref().unwrap_or("").trim();
                    if old.is_empty() {
                        return Err(format!("{} needs old_text", op.action));
                    }
                    let hits: Vec<usize> =
                        work.iter().enumerate().filter(|(_, e)| e.contains(old)).map(|(i, _)| i).collect();
                    let i = match hits.as_slice() {
                        [i] => *i,
                        [] => return Err(format!("no entry contains {old:?}")),
                        _ => return Err(format!("{} entries contain {old:?}; use a longer substring", hits.len())),
                    };
                    if op.action == "remove" {
                        work.remove(i);
                    } else {
                        let c = op.content.as_deref().map(str::trim).unwrap_or("");
                        if c.is_empty() {
                            return Err("replace needs content".into());
                        }
                        if let Some(why) = scan(c) {
                            return Err(why);
                        }
                        work[i] = c.to_string();
                    }
                }
                other => return Err(format!("unknown action {other:?}")),
            }
        }
        let limit = self.limit(t);
        let used = Self::size(&work);
        if used > limit {
            return Err(format!(
                "would be {used}/{limit} chars, over the cap. Nothing was written. Current entries:\n{}",
                self.render_entries(t)
            ));
        }
        let path = self.path(t);
        if let Some(d) = path.parent() {
            let _ = std::fs::create_dir_all(d);
        }
        write_atomic(&path, work.join(DELIM).as_bytes()).map_err(|e| e.to_string())?;
        Ok(format!("saved. {} now at {used}/{limit} chars ({} entries)", label(t), work.len()))
    }

    fn render_entries(&self, t: Target) -> String {
        self.entries(t).iter().map(|e| format!("- {}", e.replace('\n', " "))).collect::<Vec<_>>().join("\n")
    }

    /// The block that goes into the system prompt, or nothing when empty.
    pub fn prompt_block(&self) -> String {
        let mut out = String::new();
        for s in self.sections() {
            out.push_str(&format!("\n# {} [{}/{} chars]\n", s.title, s.used, s.limit));
            for e in s.entries {
                out.push_str(&format!("- {}\n", e.replace('\n', "\n  ")));
            }
        }
        out
    }

    /// The non-empty stores in prompt order, with their usage against the cap.
    pub fn sections(&self) -> Vec<Section> {
        let mut out = Vec::new();
        for t in [Target::User, Target::Memory, Target::Project] {
            let entries = self.entries(t);
            if entries.is_empty() {
                continue;
            }
            let title = match (t, &self.project) {
                (Target::Project, Some(p)) => format!("Notes on this project ({})", p.name),
                _ => label(t).to_string(),
            };
            out.push(Section { target: t, title, used: Self::size(&entries), limit: self.limit(t), entries });
        }
        out
    }
}

fn label(t: Target) -> &'static str {
    match t {
        Target::User => "What you know about the user",
        Target::Memory => "Your notes on this machine and its tools",
        Target::Project => "Notes on this project",
    }
}

/// Memory enters every future prompt, so refuse content that tries to steer
/// the agent or smuggle secrets out.
fn scan(content: &str) -> Option<String> {
    let lower = content.to_lowercase();
    const BAD: &[&str] = &[
        "ignore previous instructions",
        "ignore all previous",
        "disregard the system prompt",
        "you are now",
        "<system",
        "begin private key",
    ];
    for b in BAD {
        if lower.contains(b) {
            return Some(format!("refused: memory entries can't contain {b:?}"));
        }
    }
    let looks_like_key = content
        .split_whitespace()
        .any(|w| (w.starts_with("sk-") || w.starts_with("ghp_") || w.starts_with("xoxb-")) && w.len() > 20);
    if looks_like_key {
        return Some("refused: that looks like a credential; secrets belong in secretspec".into());
    }
    None
}
