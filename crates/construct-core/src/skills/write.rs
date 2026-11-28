//! Every change to a skill's files or record: the write side of a `Library`.
//!
//! The design choice: a `Writer` is handed out only where a write is meant to
//! land (`Stack::writable`, `Kit::writable`), and each write passes `guard`
//! before it touches disk, since a loaded skill enters the prompt. Reading
//! stays on `Library`.

use super::format::normalize;
use super::{Library, safe_join, valid_name};
use crate::clock::now;
use crate::home::write_atomic;

/// One library, for writing.
pub struct Writer<'a> {
    lib: &'a Library,
}

impl<'a> Writer<'a> {
    pub(super) fn new(lib: &'a Library) -> Self {
        Self { lib }
    }

    /// Where a skill lives, for results the model reads back.
    pub fn locate(&self, name: &str) -> String {
        self.lib.locate(name)
    }

    /// A new skill, `author` recorded for the curator ("agent" for skills the
    /// agent wrote).
    pub fn create(&self, name: &str, content: &str, author: &str) -> Result<String, String> {
        if !valid_name(name) {
            return Err("skill names are lowercase letters, digits and dashes, at most 64 chars".into());
        }
        if self.lib.dir.join(name).exists() {
            return Err(format!("{name} exists; use patch or edit"));
        }
        let content = normalize(name, content)?;
        write_atomic(&self.lib.dir.join(name).join("SKILL.md"), content.as_bytes()).map_err(|e| e.to_string())?;
        let author = author.to_string();
        self.lib.touch(name, |u| {
            u.created_at = now();
            u.last_used = now();
            u.author = author;
        });
        Ok(format!("created skill {name}"))
    }

    /// Rewrite a skill's SKILL.md whole.
    pub fn edit(&self, name: &str, content: &str) -> Result<String, String> {
        let skill = self.lib.get(name).ok_or_else(|| self.lib.not_found(name))?;
        let content = normalize(name, content)?;
        write_atomic(&skill.dir.join("SKILL.md"), content.as_bytes()).map_err(|e| e.to_string())?;
        self.lib.touch(name, |u| u.patches += 1);
        Ok(format!("rewrote {name}/SKILL.md"))
    }

    /// Rewrite one file of a skill. `new` is what the guard checks, before
    /// anything is read; `apply` turns the file's text into the updated text
    /// (the skill tool passes the editor's exact-match replace), so the library
    /// does not depend on `tools`.
    pub fn patch(
        &self,
        name: &str,
        file: Option<&str>,
        new: &str,
        apply: impl FnOnce(&str) -> Result<String, String>,
    ) -> Result<String, String> {
        let skill = self.lib.get(name).ok_or_else(|| self.lib.not_found(name))?;
        let path = safe_join(&skill.dir, file.unwrap_or("SKILL.md"))?;
        if let Some(why) = crate::guard::check(new) {
            return Err(why);
        }
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let updated = apply(&text)?;
        write_atomic(&path, updated.as_bytes()).map_err(|e| e.to_string())?;
        self.lib.touch(name, |u| u.patches += 1);
        Ok(format!("patched {name}/{}", file.unwrap_or("SKILL.md")))
    }

    /// Add or replace a support file under references/, templates/ or scripts/.
    pub fn write_file(&self, name: &str, file: &str, content: &str) -> Result<String, String> {
        let skill = self.lib.get(name).ok_or_else(|| self.lib.not_found(name))?;
        if !["references/", "templates/", "scripts/"].iter().any(|p| file.starts_with(p)) {
            return Err("support files go under references/, templates/ or scripts/".into());
        }
        let path = safe_join(&skill.dir, file)?;
        if let Some(why) = crate::guard::check(content) {
            return Err(why);
        }
        write_atomic(&path, content.as_bytes()).map_err(|e| e.to_string())?;
        self.lib.touch(name, |u| u.patches += 1);
        Ok(format!("wrote {name}/{file}"))
    }

    /// Remove one support file; a whole skill is archived instead.
    pub fn remove_file(&self, name: &str, file: &str) -> Result<String, String> {
        let skill = self.lib.get(name).ok_or_else(|| self.lib.not_found(name))?;
        if file == "SKILL.md" {
            return Err("use delete to remove a whole skill".into());
        }
        std::fs::remove_file(safe_join(&skill.dir, file)?).map_err(|e| e.to_string())?;
        Ok(format!("removed {name}/{file}"))
    }

    /// Move a skill to .archive/. `absorbed_into` names the skill that took
    /// over its content, which must already exist.
    pub fn archive(&self, name: &str, absorbed_into: Option<&str>) -> Result<String, String> {
        let skill = self.lib.get(name).ok_or_else(|| self.lib.not_found(name))?;
        if self.lib.usage().get(name).is_some_and(|u| u.pinned) {
            return Err(format!("{name} is pinned"));
        }
        if let Some(into) = absorbed_into.filter(|s| !s.is_empty())
            && self.lib.get(into).is_none()
        {
            return Err(format!("absorbed_into {into:?} doesn't exist; create or patch it first"));
        }
        let dest = self.lib.dir.join(".archive").join(format!("{name}-{}", now()));
        let dir = dest.parent().ok_or_else(|| format!("{name}: archive path has no parent"))?;
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        std::fs::rename(&skill.dir, &dest).map_err(|e| e.to_string())?;
        Ok(match absorbed_into {
            Some(i) if !i.is_empty() => format!("archived {name} (absorbed into {i})"),
            _ => format!("archived {name}"),
        })
    }

    /// Pin or unpin a skill: the curator never archives a pinned one.
    pub fn pin(&self, name: &str, pinned: bool) -> Result<(), String> {
        self.lib.get(name).ok_or_else(|| self.lib.not_found(name))?;
        self.lib.touch(name, |u| u.pinned = pinned);
        Ok(())
    }
}
