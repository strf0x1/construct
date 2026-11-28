//! `ctx.state`: a small persisted key-value store per script, in
//! `$CONSTRUCT_HOME/scripts/state.sqlite` (ADR 0079). It is how a script
//! remembers across events, reloads and restarts, since every call runs in a
//! fresh runtime and module-level variables die with it.
//!
//! The design choice: a call's writes go to a `Txn` and are committed only if
//! the call succeeds, in one SQLite transaction, so a script that throws or
//! runs out of time halfway leaves its state as it found it.

use rusqlite::{Connection, OptionalExtension, params};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// The longest key, in bytes.
pub const KEY_MAX: usize = 256;
/// The largest value (its JSON text), in bytes.
pub const VALUE_MAX: usize = 64 << 10;
/// The most one script may keep, keys and values together, in bytes.
pub const SCRIPT_MAX: usize = 1 << 20;

/// The store file, shared by every script in a home.
pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    pub fn open(file: &Path) -> Result<Arc<Self>, String> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let conn = Connection::open(file).map_err(|e| format!("{}: {e}", file.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(2)).map_err(|e| e.to_string())?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             CREATE TABLE IF NOT EXISTS kv (
                 script TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL,
                 PRIMARY KEY (script, key)
             );",
        )
        .map_err(|e| e.to_string())?;
        Ok(Arc::new(Self { conn: Mutex::new(conn) }))
    }

    fn get(&self, script: &str, key: &str) -> Result<Option<String>, String> {
        let conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        conn.query_row("SELECT value FROM kv WHERE script = ?1 AND key = ?2", params![script, key], |r| r.get(0))
            .optional()
            .map_err(|e| e.to_string())
    }

    fn commit(&self, script: &str, writes: &BTreeMap<String, Option<String>>) -> Result<(), String> {
        if writes.is_empty() {
            return Ok(());
        }
        let mut conn = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        let tx = conn.transaction().map_err(|e| e.to_string())?;
        for (key, value) in writes {
            match value {
                Some(v) => tx.execute(
                    "INSERT INTO kv (script, key, value) VALUES (?1, ?2, ?3)
                     ON CONFLICT (script, key) DO UPDATE SET value = excluded.value",
                    params![script, key, v],
                ),
                None => tx.execute("DELETE FROM kv WHERE script = ?1 AND key = ?2", params![script, key]),
            }
            .map_err(|e| e.to_string())?;
        }
        let used: i64 = tx
            .query_row(
                "SELECT COALESCE(SUM(length(CAST(key AS BLOB)) + length(CAST(value AS BLOB))), 0)
                 FROM kv WHERE script = ?1",
                params![script],
                |r| r.get(0),
            )
            .map_err(|e| e.to_string())?;
        if used as usize > SCRIPT_MAX {
            return Err(format!("state would hold {used} bytes, over the {SCRIPT_MAX}-byte limit"));
        }
        tx.commit().map_err(|e| e.to_string())
    }
}

/// One call's view of a script's state: reads see its own writes first.
pub struct Txn {
    store: Arc<Store>,
    script: String,
    writes: BTreeMap<String, Option<String>>,
}

impl Txn {
    pub fn new(store: Arc<Store>, script: &str) -> Self {
        Self { store, script: script.to_string(), writes: BTreeMap::new() }
    }

    /// The JSON text stored under `key`, if any.
    pub fn get(&self, key: &str) -> Result<Option<String>, String> {
        match self.writes.get(key) {
            Some(v) => Ok(v.clone()),
            None => self.store.get(&self.script, key),
        }
    }

    pub fn set(&mut self, key: &str, json: String) -> Result<(), String> {
        check_key(key)?;
        if json.len() > VALUE_MAX {
            return Err(format!("state value for {key:?} is {} bytes, over the {VALUE_MAX}-byte limit", json.len()));
        }
        self.writes.insert(key.to_string(), Some(json));
        Ok(())
    }

    pub fn delete(&mut self, key: &str) -> Result<(), String> {
        check_key(key)?;
        self.writes.insert(key.to_string(), None);
        Ok(())
    }

    pub fn commit(self) -> Result<(), String> {
        self.store.commit(&self.script, &self.writes)
    }
}

fn check_key(key: &str) -> Result<(), String> {
    if key.is_empty() || key.len() > KEY_MAX {
        return Err(format!("state keys are 1 to {KEY_MAX} bytes"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (Arc<Store>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("cs-state-{}-{}", std::process::id(), rand_suffix()));
        let s = Store::open(&dir.join("state.sqlite")).unwrap();
        (s, dir)
    }

    fn rand_suffix() -> u128 {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
    }

    #[test]
    fn commits_only_what_a_call_keeps() {
        let (s, dir) = store();
        let mut t = Txn::new(s.clone(), "a");
        t.set("n", "1".into()).unwrap();
        assert_eq!(t.get("n").unwrap().as_deref(), Some("1"));
        drop(t); // a failed call never commits
        assert_eq!(Txn::new(s.clone(), "a").get("n").unwrap(), None);
        let mut t = Txn::new(s.clone(), "a");
        t.set("n", "2".into()).unwrap();
        t.commit().unwrap();
        assert_eq!(Txn::new(s.clone(), "a").get("n").unwrap().as_deref(), Some("2"));
        // Scripts never see each other's keys.
        assert_eq!(Txn::new(s.clone(), "b").get("n").unwrap(), None);
        let mut t = Txn::new(s.clone(), "a");
        t.delete("n").unwrap();
        t.commit().unwrap();
        assert_eq!(Txn::new(s, "a").get("n").unwrap(), None);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn enforces_the_caps() {
        let (s, dir) = store();
        let mut t = Txn::new(s.clone(), "a");
        assert!(t.set("", "1".into()).is_err());
        assert!(t.set("k", "x".repeat(VALUE_MAX + 1)).is_err());
        for i in 0..(SCRIPT_MAX / VALUE_MAX + 1) {
            t.set(&format!("k{i}"), "x".repeat(VALUE_MAX)).unwrap();
        }
        assert!(t.commit().is_err());
        assert_eq!(Txn::new(s, "a").get("k0").unwrap(), None);
        let _ = std::fs::remove_dir_all(dir);
    }
}
