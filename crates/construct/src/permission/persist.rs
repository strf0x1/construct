//! Writing an approval into `config.toml`, so the next session starts with it.
//!
//! Edited in place (`toml_edit`), not re-serialized: the file is the user's,
//! comments and ordering included, and a prompt must not rewrite more of it
//! than the one entry it adds.

use crate::config::{Paths, write_atomic};
use anyhow::{Context, Result, bail};
use std::path::Path;
use toml_edit::{Array, DocumentMut, Item, Table, Value};

/// `p` as it should read in the file: `~/…` under $HOME.
fn tilde(p: &Path) -> String {
    match std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .and_then(|h| p.strip_prefix(&h).ok().map(|r| r.to_path_buf()))
    {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => p.display().to_string(),
    }
}

/// Add `value` to the string array `table.key` (`table` is `["sandbox"]` or
/// `["agents", name]`), creating what is missing. Already there: nothing changes.
fn add(paths: &Paths, table: &[&str], key: &str, value: &str) -> Result<()> {
    let file = paths.config();
    let text = match std::fs::read_to_string(&file) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", file.display())),
    };
    let mut doc: DocumentMut = text.parse().with_context(|| format!("parsing {}", file.display()))?;
    let mut cur: &mut Table = doc.as_table_mut();
    for (depth, name) in table.iter().enumerate() {
        let item = cur.entry(name).or_insert_with(|| {
            let mut t = Table::new();
            t.set_implicit(depth + 1 < table.len());
            Item::Table(t)
        });
        let Some(next) = item.as_table_mut() else { bail!("{}: [{}] is not a table", file.display(), table.join(".")) };
        cur = next;
    }
    let item = cur.entry(key).or_insert(Item::Value(Value::Array(Array::new())));
    let Some(list) = item.as_array_mut() else { bail!("{}: {} is not a list", file.display(), key) };
    if !list.iter().any(|v| v.as_str() == Some(value)) {
        list.push(value);
    }
    write_atomic(&file, doc.to_string().as_bytes())
}

/// The session's agent may reach `p`: `[sandbox] allow_read` / `allow_write`.
pub fn allow_path(paths: &Paths, agent: &str, write: bool, p: &Path) -> Result<()> {
    let key = if write { "allow_write" } else { "allow_read" };
    let value = tilde(p);
    if agent.is_empty() { add(paths, &["sandbox"], key, &value) } else { add(paths, &["agents", agent], key, &value) }
}

/// A delegated agent's calls of this ACP kind need no question: `allow_kinds`.
pub fn allow_kind(paths: &Paths, agent: &str, kind: &str) -> Result<()> {
    add(paths, &["agents", agent], "allow_kinds", kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(tag: &str) -> Paths {
        let dir = std::env::temp_dir().join(format!("construct-persist-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Paths { home: dir }
    }

    #[test]
    fn an_entry_is_added_without_touching_the_rest_of_the_file() {
        let paths = home("sandbox");
        std::fs::write(
            paths.config(),
            "# mine\nmodel = \"m\"\n\n[sandbox]\nmode = \"workspace\" # keep\nallow_read = [\"~/a\"]\n",
        )
        .unwrap();
        allow_path(&paths, "", false, Path::new("/srv/data")).unwrap();
        allow_path(&paths, "", false, Path::new("/srv/data")).unwrap();
        allow_path(&paths, "", true, Path::new("/srv/out")).unwrap();
        let text = std::fs::read_to_string(paths.config()).unwrap();
        assert!(text.starts_with("# mine\nmodel"), "{text}");
        assert!(text.contains("mode = \"workspace\" # keep"), "{text}");
        let cfg: toml::Value = toml::from_str(&text).unwrap();
        assert_eq!(cfg["sandbox"]["allow_read"].as_array().unwrap().len(), 2, "no duplicate: {text}");
        assert_eq!(cfg["sandbox"]["allow_write"][0].as_str(), Some("/srv/out"));
    }

    #[test]
    fn an_agent_entry_and_a_missing_file_are_both_fine() {
        let paths = home("agent");
        allow_kind(&paths, "codex", "read").unwrap();
        allow_path(&paths, "scout", false, Path::new("/srv/x")).unwrap();
        let cfg: toml::Value = toml::from_str(&std::fs::read_to_string(paths.config()).unwrap()).unwrap();
        assert_eq!(cfg["agents"]["codex"]["allow_kinds"][0].as_str(), Some("read"));
        assert_eq!(cfg["agents"]["scout"]["allow_read"][0].as_str(), Some("/srv/x"));
    }

    #[test]
    fn paths_under_home_are_written_with_a_tilde() {
        let h = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
        assert_eq!(tilde(&h.join(".aws/config")), "~/.aws/config");
        assert_eq!(tilde(Path::new("/srv/x")), "/srv/x");
    }
}
