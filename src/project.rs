//! Project kits: what construct learns about one repo, kept in
//! `$CONSTRUCT_HOME/projects/<id>/` (never in the repo itself):
//! - NOTES.md: capped `§` notes on how this repo works (memory target "project");
//! - skills/: procedures that only make sense here (skill_manage scope "project").
//!
//! In that repo the notes and a one-line index of its skills are always in the
//! system prompt; nowhere else. Project knowledge is triggered by where you
//! are, not by what you ask, so it isn't routed.

use crate::config::Paths;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Project {
    pub name: String,
    pub root: PathBuf,
    /// `$CONSTRUCT_HOME/projects/<id>`
    pub dir: PathBuf,
}

/// The repo containing `cwd`, identified by its git remote when it has one
/// (so two clones share a kit) and by its root path otherwise.
pub fn detect(paths: &Paths, cwd: &Path) -> Option<Project> {
    let root = cwd.ancestors().find(|d| is_repo(d))?.to_path_buf();
    let remote = remote(&root);
    // Name from the remote when there is one: clones in differently named
    // directories are the same project.
    let name = match &remote {
        Some(r) => r.rsplit(['/', ':']).next().unwrap_or(r).to_string(),
        None => root.file_name()?.to_string_lossy().to_string(),
    };
    let identity = remote.unwrap_or_else(|| root.display().to_string());
    let id = format!("{}-{:08x}", slug(&name), fnv(&identity) as u32);
    Some(Project { name, root, dir: paths.home.join("projects").join(id) })
}

impl Project {
    pub fn notes(&self) -> PathBuf {
        self.dir.join("NOTES.md")
    }

    pub fn skills_dir(&self) -> PathBuf {
        self.dir.join("skills")
    }

    /// Record which repo this kit belongs to, the first time it's written.
    pub fn touch(&self) {
        let f = self.dir.join("project.json");
        if !f.exists() {
            let _ = std::fs::create_dir_all(&self.dir);
            let v = serde_json::json!({ "name": self.name, "root": self.root, "remote": remote(&self.root) });
            let _ = crate::config::write_atomic(&f, v.to_string().as_bytes());
        }
    }
}

/// A real repo: `.git/HEAD`, or a `.git` file pointing at one (worktrees,
/// submodules). An empty `.git` directory left in /tmp is not a project.
fn is_repo(d: &Path) -> bool {
    let g = d.join(".git");
    g.join("HEAD").is_file() || std::fs::read_to_string(&g).is_ok_and(|t| t.starts_with("gitdir:"))
}

/// `remote.origin.url` read straight from .git/config: no git process on the
/// startup path.
fn remote(root: &Path) -> Option<String> {
    let cfg = std::fs::read_to_string(root.join(".git").join("config")).ok()?;
    let mut in_origin = false;
    for line in cfg.lines().map(str::trim) {
        if line.starts_with('[') {
            in_origin = line == "[remote \"origin\"]";
        } else if in_origin
            && let Some(url) = line.strip_prefix("url").map(|r| r.trim_start().trim_start_matches('=').trim())
        {
            return Some(normalize(url));
        }
    }
    None
}

/// `git@host:owner/repo.git`, `ssh://git@host/owner/repo` and
/// `https://host/owner/repo` are one repo: `host/owner/repo`.
fn normalize(url: &str) -> String {
    let u = url.trim().trim_end_matches('/').trim_end_matches(".git");
    let u = u.split_once("://").map(|(_, rest)| rest).unwrap_or(u);
    let u = u.rsplit_once('@').map(|(_, rest)| rest).unwrap_or(u);
    let u = match u.split_once(':') {
        // scp-style host:path, not host:port/path
        Some((host, path)) if !path.starts_with(|c: char| c.is_ascii_digit()) => format!("{host}/{path}"),
        _ => u.to_string(),
    };
    u.to_lowercase()
}

fn slug(s: &str) -> String {
    let s: String = s.to_lowercase().chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    s.trim_matches('-').chars().take(40).collect()
}

fn fnv(s: &str) -> u64 {
    s.bytes().fold(0xcbf29ce484222325u64, |h, b| (h ^ b as u64).wrapping_mul(0x100000001b3))
}

#[cfg(test)]
mod tests {
    #[test]
    fn remotes_normalize_to_one_identity() {
        for u in [
            "git@github.com:Me/Repo.git",
            "https://github.com/me/repo",
            "ssh://git@github.com/me/repo.git",
            "https://user@github.com/me/repo/",
        ] {
            assert_eq!(super::normalize(u), "github.com/me/repo", "{u}");
        }
    }
}
