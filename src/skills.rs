//! Skills the agent writes and improves. One directory per skill holding a
//! SKILL.md (frontmatter: name, description) plus optional references/,
//! templates/ and scripts/. Nothing is bundled. Deletes are archives, so the
//! curator can never lose work.

use crate::config::{Paths, write_atomic};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

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
}

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Split `---\nkey: value\n---\nbody` frontmatter. Values may be quoted.
pub fn frontmatter(text: &str) -> (BTreeMap<String, String>, &str) {
    let mut map = BTreeMap::new();
    let Some(rest) = text.strip_prefix("---") else { return (map, text) };
    let Some(end) = rest.find("\n---") else { return (map, text) };
    for line in rest[..end].lines() {
        if let Some((k, v)) = line.split_once(':') {
            let v = v.trim().trim_matches('"').trim_matches('\'');
            if !k.starts_with(' ') && !v.is_empty() {
                map.insert(k.trim().to_string(), v.to_string());
            }
        }
    }
    let body = rest[end + 4..].trim_start_matches(['\r', '\n']);
    (map, body)
}

impl Library {
    pub fn new(paths: &Paths) -> Self {
        Self { dir: paths.skills_dir() }
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
            let description = fm
                .get("description")
                .cloned()
                .unwrap_or_else(|| body.lines().find(|l| !l.trim().is_empty() && !l.starts_with('#')).unwrap_or("").to_string());
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

    pub fn create(&self, name: &str, content: &str, author: &str) -> Result<String, String> {
        if !valid_name(name) {
            return Err("skill names are lowercase letters, digits and dashes, at most 64 chars".into());
        }
        if self.dir.join(name).exists() {
            return Err(format!("{name} exists; use patch or edit"));
        }
        let content = normalize(name, content)?;
        write_atomic(&self.dir.join(name).join("SKILL.md"), content.as_bytes()).map_err(|e| e.to_string())?;
        let author = author.to_string();
        self.touch(name, |u| {
            u.created_at = now();
            u.last_used = now();
            u.author = author;
        });
        Ok(format!("created skill {name}"))
    }

    pub fn edit(&self, name: &str, content: &str) -> Result<String, String> {
        let skill = self.get(name).ok_or_else(|| self.not_found(name))?;
        let content = normalize(name, content)?;
        write_atomic(&skill.dir.join("SKILL.md"), content.as_bytes()).map_err(|e| e.to_string())?;
        self.touch(name, |u| u.patches += 1);
        Ok(format!("rewrote {name}/SKILL.md"))
    }

    pub fn patch(&self, name: &str, file: Option<&str>, old: &str, new: &str) -> Result<String, String> {
        let skill = self.get(name).ok_or_else(|| self.not_found(name))?;
        let path = safe_join(&skill.dir, file.unwrap_or("SKILL.md"))?;
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let updated = crate::tools::edit::replace_once(&text, old, new, false)?;
        write_atomic(&path, updated.as_bytes()).map_err(|e| e.to_string())?;
        self.touch(name, |u| u.patches += 1);
        Ok(format!("patched {name}/{}", file.unwrap_or("SKILL.md")))
    }

    pub fn write_file(&self, name: &str, file: &str, content: &str) -> Result<String, String> {
        let skill = self.get(name).ok_or_else(|| self.not_found(name))?;
        if !["references/", "templates/", "scripts/"].iter().any(|p| file.starts_with(p)) {
            return Err("support files go under references/, templates/ or scripts/".into());
        }
        let path = safe_join(&skill.dir, file)?;
        write_atomic(&path, content.as_bytes()).map_err(|e| e.to_string())?;
        self.touch(name, |u| u.patches += 1);
        Ok(format!("wrote {name}/{file}"))
    }

    pub fn remove_file(&self, name: &str, file: &str) -> Result<String, String> {
        let skill = self.get(name).ok_or_else(|| self.not_found(name))?;
        if file == "SKILL.md" {
            return Err("use delete to remove a whole skill".into());
        }
        std::fs::remove_file(safe_join(&skill.dir, file)?).map_err(|e| e.to_string())?;
        Ok(format!("removed {name}/{file}"))
    }

    /// Move a skill to .archive/. `absorbed_into` names the skill that took
    /// over its content, which must already exist.
    pub fn archive(&self, name: &str, absorbed_into: Option<&str>) -> Result<String, String> {
        let skill = self.get(name).ok_or_else(|| self.not_found(name))?;
        if self.usage().get(name).is_some_and(|u| u.pinned) {
            return Err(format!("{name} is pinned"));
        }
        if let Some(into) = absorbed_into.filter(|s| !s.is_empty()) {
            if self.get(into).is_none() {
                return Err(format!("absorbed_into {into:?} doesn't exist; create or patch it first"));
            }
        }
        let dest = self.dir.join(".archive").join(format!("{name}-{}", now()));
        std::fs::create_dir_all(dest.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::rename(&skill.dir, &dest).map_err(|e| e.to_string())?;
        Ok(match absorbed_into {
            Some(i) if !i.is_empty() => format!("archived {name} (absorbed into {i})"),
            _ => format!("archived {name}"),
        })
    }

    pub fn set_pinned(&self, name: &str, pinned: bool) -> Result<(), String> {
        self.get(name).ok_or_else(|| self.not_found(name))?;
        self.touch(name, |u| u.pinned = pinned);
        Ok(())
    }
}

/// Ensure SKILL.md has frontmatter with the right name and a description.
fn normalize(name: &str, content: &str) -> Result<String, String> {
    let (fm, body) = frontmatter(content);
    let description = fm.get("description").cloned().unwrap_or_default();
    if description.trim().is_empty() {
        return Err("SKILL.md needs frontmatter with a description: ---\\nname: x\\ndescription: when to use it\\n---".into());
    }
    if description.len() > 300 {
        return Err("keep the description under 300 chars; put detail in the body".into());
    }
    if body.trim().is_empty() {
        return Err("SKILL.md body is empty".into());
    }
    Ok(format!("---\nname: {name}\ndescription: {description}\n---\n\n{}\n", body.trim_end()))
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

pub fn now_nanos() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}
