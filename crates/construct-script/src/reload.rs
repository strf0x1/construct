//! Hot-reload of one script directory with last-known-good fallback (ADR
//! 0079). The caller decides when a boundary is (between event firings for a
//! wake script) and calls `refresh` there; nothing reloads mid-call.
//!
//! The design choice: a file is compared by content, not mtime, and a new
//! version replaces the live one only once it loads cleanly. A broken edit
//! leaves the previous version running and is reported once per broken
//! content, so a filter that fires every second does not repeat it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// The largest script file read, in bytes.
pub const FILE_MAX: u64 = 256 << 10;

/// What one `refresh` changed, for status lines and notes.
#[derive(Debug, PartialEq, Eq)]
pub enum Change {
    Loaded {
        name: String,
    },
    Removed {
        name: String,
    },
    /// `kept` is whether an earlier version is still live.
    Failed {
        name: String,
        error: String,
        kept: bool,
    },
}

/// The live scripts of one directory, by file stem, in name order.
pub struct Library<T> {
    dir: PathBuf,
    live: BTreeMap<String, Live<T>>,
    /// The content of the last version of each file that failed, so it is
    /// reported once.
    broken: BTreeMap<String, String>,
}

struct Live<T> {
    source: String,
    value: Arc<T>,
}

impl<T> Library<T> {
    pub fn new(dir: PathBuf) -> Self {
        Self { dir, live: BTreeMap::new(), broken: BTreeMap::new() }
    }

    /// Re-read the directory: load what is new or changed with `load`
    /// (name, source), drop what was removed. A missing directory is empty.
    pub fn refresh(&mut self, load: impl Fn(&str, &str) -> Result<T, String>) -> Vec<Change> {
        let found = scan(&self.dir);
        let mut changes = Vec::new();
        let gone: Vec<String> = self.live.keys().filter(|k| !found.contains_key(*k)).cloned().collect();
        for name in gone {
            self.live.remove(&name);
            changes.push(Change::Removed { name });
        }
        self.broken.retain(|k, _| found.contains_key(k));
        for (name, read) in found {
            let source = match read {
                Ok(s) => s,
                Err(error) => {
                    changes.extend(self.fail(&name, String::new(), error));
                    continue;
                }
            };
            if self.live.get(&name).is_some_and(|l| l.source == source) {
                self.broken.remove(&name);
                continue;
            }
            if self.broken.get(&name) == Some(&source) {
                continue;
            }
            match load(&name, &source) {
                Ok(value) => {
                    self.broken.remove(&name);
                    self.live.insert(name.clone(), Live { source, value: Arc::new(value) });
                    changes.push(Change::Loaded { name });
                }
                Err(error) => changes.extend(self.fail(&name, source, error)),
            }
        }
        changes
    }

    fn fail(&mut self, name: &str, source: String, error: String) -> Option<Change> {
        if self.broken.get(name) == Some(&source) {
            return None;
        }
        self.broken.insert(name.to_string(), source);
        Some(Change::Failed { name: name.to_string(), error, kept: self.live.contains_key(name) })
    }

    /// The live scripts, in file name order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &Arc<T>)> {
        self.live.iter().map(|(k, v)| (k.as_str(), &v.value))
    }
}

/// Every `*.js` file directly in `dir`, by stem, with its content or why it
/// could not be read.
fn scan(dir: &Path) -> BTreeMap<String, Result<String, String>> {
    let mut out = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("js") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else { continue };
        let read = match std::fs::metadata(&path) {
            Ok(m) if !m.is_file() => continue,
            Ok(m) if m.len() > FILE_MAX => Err(format!("{} bytes, over the {FILE_MAX}-byte limit", m.len())),
            Ok(_) => std::fs::read_to_string(&path).map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        out.insert(stem.to_string(), read);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn load(_: &str, src: &str) -> Result<String, String> {
        if src.contains("broken") { Err("syntax".into()) } else { Ok(src.to_string()) }
    }

    fn live(lib: &Library<String>) -> Vec<(String, String)> {
        lib.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn keeps_the_last_good_version_and_reports_once() {
        let dir = std::env::temp_dir().join(format!("cs-reload-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut lib = Library::new(dir.clone());
        assert!(lib.refresh(load).is_empty(), "a missing dir is empty");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.js"), "v1").unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();
        assert_eq!(lib.refresh(load), vec![Change::Loaded { name: "a".into() }]);
        assert!(lib.refresh(load).is_empty(), "unchanged content does not reload");

        std::fs::write(dir.join("a.js"), "broken").unwrap();
        let changes = lib.refresh(load);
        assert_eq!(changes, vec![Change::Failed { name: "a".into(), error: "syntax".into(), kept: true }]);
        assert!(lib.refresh(load).is_empty(), "the same broken content is reported once");
        assert_eq!(live(&lib), vec![("a".into(), "v1".into())]);

        std::fs::write(dir.join("a.js"), "v2").unwrap();
        assert_eq!(lib.refresh(load), vec![Change::Loaded { name: "a".into() }]);
        assert_eq!(live(&lib), vec![("a".into(), "v2".into())]);

        std::fs::remove_file(dir.join("a.js")).unwrap();
        assert_eq!(lib.refresh(load), vec![Change::Removed { name: "a".into() }]);
        let _ = std::fs::remove_dir_all(dir);
    }
}
