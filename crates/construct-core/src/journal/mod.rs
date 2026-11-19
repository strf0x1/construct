//! The durable journal: one SQLite WAL database per project at
//! `$CONSTRUCT_HOME/projects/<id>/journal.sqlite`, one table of rows
//! `(seq, session_id, type, v, at_ms, payload)` where `payload` is a
//! serialized [`Event`] (docs/adr/0008, decisions in #29).
//!
//! The design choice a reader must know: sessions are derived, not stored.
//! There is no sessions table; a session is the rows sharing its id, and its
//! first row is the `head` event. The table shape is versioned with
//! `PRAGMA user_version` and migrated on open; payloads only ever grow, and
//! readers skip what they don't understand. The parent process is the only
//! writer, and the file sits outside every sandbox root.

use crate::event::Event;
use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

mod browser;
mod export;
pub mod notes;
mod read;
mod recovery;
mod show;
mod totals;
mod writer;

pub use browser::{Session, browse, prompts, saved_model, validate_directory};
pub use export::export;
pub use read::{Entry, project};
pub use show::{find, human, rows_text, share, status_rows, transcript};
pub use totals::Totals;
pub use writer::Writer;

/// The table shape this build writes. Bump it with a new arm in `migrate`.
const VERSION: i64 = 1;
/// The payload version written with every row. Bumped only for a breaking
/// payload change, which the additive rule should make rare; readers upcast
/// old shapes on read.
const PAYLOAD_V: i64 = 1;

/// Version 1. The FTS table indexes the text the model and user exchanged
/// (not the system prompt, which repeats in every session); it is external
/// content over a view, so the text is stored once, in `payload`.
const SCHEMA_V1: &str = "
CREATE TABLE events (
    seq INTEGER PRIMARY KEY,
    session_id TEXT NOT NULL,
    type TEXT NOT NULL,
    v INTEGER NOT NULL,
    at_ms INTEGER NOT NULL,
    payload TEXT NOT NULL
);
CREATE INDEX events_session ON events(session_id, seq);
CREATE INDEX events_type ON events(type);
CREATE INDEX events_id ON events(json_extract(payload, '$.id'));
CREATE VIEW event_text(seq, text) AS
    SELECT seq, json_extract(payload, '$.message.content') FROM events
    WHERE type IN ('user', 'assistant', 'tool_result');
CREATE VIRTUAL TABLE events_fts USING fts5(text, content = 'event_text', content_rowid = 'seq');
CREATE TRIGGER events_fts_insert AFTER INSERT ON events
    WHEN new.type IN ('user', 'assistant', 'tool_result') BEGIN
    INSERT INTO events_fts(rowid, text) VALUES (new.seq, json_extract(new.payload, '$.message.content'));
END;
CREATE TRIGGER events_fts_delete AFTER DELETE ON events
    WHEN old.type IN ('user', 'assistant', 'tool_result') BEGIN
    INSERT INTO events_fts(events_fts, rowid, text)
        VALUES ('delete', old.seq, json_extract(old.payload, '$.message.content'));
END;
";

/// The journal file for the project containing `cwd`.
pub fn path(paths: &crate::config::Paths, cwd: &Path) -> PathBuf {
    crate::project::dir_for(paths, cwd).join("journal.sqlite")
}

pub struct Journal {
    conn: Connection,
    file: PathBuf,
    /// The first commit may create `-wal`/`-shm`; their directory entries are
    /// synced once, after that commit (see `append`).
    dir_synced: AtomicBool,
}

impl Journal {
    /// Open (creating if needed) and migrate. WAL so readers never block the
    /// writer, and a busy timeout so concurrent sessions in one project wait
    /// for each other instead of failing.
    pub fn open(file: &Path) -> Result<Self> {
        if let Some(dir) = file.parent() {
            create_dir_all_synced(dir)?;
        }
        let conn = Connection::open(file).with_context(|| format!("opening {}", file.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        let mut j = Self { conn, file: file.to_path_buf(), dir_synced: AtomicBool::new(false) };
        // Switching a new file to WAL, and the first migration, can report
        // busy without waiting when several sessions create it at once.
        let mut tries = 0;
        loop {
            match j.setup() {
                Err(e) if tries < 100 && is_busy(&e) => {
                    tries += 1;
                    std::thread::sleep(std::time::Duration::from_millis(20));
                }
                r => return r.map(|()| j).with_context(|| format!("opening {}", file.display())),
            }
        }
    }

    fn setup(&mut self) -> rusqlite::Result<()> {
        self.conn.pragma_update(None, "journal_mode", "wal")?;
        // WAL + FULL: every commit fsyncs the WAL before `append` returns, so
        // a row is on disk before `Writer::emit` sends its event. NORMAL only
        // survives the death of this process; it leaves the commit in the page
        // cache, which dies with the machine or the VM holding it (#161).
        self.conn.pragma_update(None, "synchronous", "full")?;
        self.migrate()
    }

    fn migrate(&mut self) -> rusqlite::Result<()> {
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let v: i64 = tx.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        // A newer build's shape is a superset; read and append what we can.
        if v >= VERSION {
            return Ok(());
        }
        if v < 1 {
            tx.execute_batch(SCHEMA_V1)?;
        }
        tx.pragma_update(None, "user_version", VERSION)?;
        tx.commit()?;
        Ok(())
    }

    /// Append one event to `session`. Non-durable events are ignored.
    pub fn append(&self, session: &str, ev: &Event) -> Result<()> {
        if !ev.durable() {
            return Ok(());
        }
        let v = serde_json::to_value(ev)?;
        let ty = v["event"].as_str().unwrap_or_default().to_string();
        self.conn
            .prepare_cached("INSERT INTO events (session_id, type, v, at_ms, payload) VALUES (?1, ?2, ?3, ?4, ?5)")?
            .execute(params![session, ty, PAYLOAD_V, now_ms() as i64, v.to_string()])?;
        if !self.dir_synced.load(Ordering::Relaxed) {
            // SQLite fsyncs the WAL file, never the directory entry that makes
            // it findable. Without this, a VMM death drops the entry even
            // though the commit's contents reached the disk (#163).
            if let Some(dir) = self.file.parent() {
                sync_dir(dir)?;
            }
            self.dir_synced.store(true, Ordering::Relaxed);
        }
        Ok(())
    }

    /// Drop every row of `session` after `seq`: rewind is truncate and
    /// re-project, never re-deriving what happened.
    pub fn rewind(&self, session: &str, seq: i64) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM events WHERE session_id = ?1 AND seq > ?2", params![session, seq])?)
    }
}

/// fsync a directory, so an entry just added to it (a project directory, the
/// journal, its WAL) survives the loss of the page cache that held it. SQLite
/// fsyncs the files it writes, never their directory (#163).
fn sync_dir(dir: &Path) -> Result<()> {
    std::fs::File::open(dir)?.sync_all().with_context(|| format!("syncing {}", dir.display()))?;
    Ok(())
}

/// `create_dir_all`, then fsync the parent of every directory it created, so
/// each new entry is on disk before anything is written below it. Fsyncing
/// only the deepest would leave its ancestors' entries in the page cache,
/// which is exactly what a VMM death drops (#163).
fn create_dir_all_synced(dir: &Path) -> Result<()> {
    let missing: Vec<&Path> = dir.ancestors().take_while(|a| !a.is_dir()).collect();
    if missing.is_empty() {
        return Ok(());
    }
    std::fs::create_dir_all(dir)?;
    for created in missing.iter().rev() {
        if let Some(parent) = created.parent() {
            sync_dir(parent)?;
        }
    }
    Ok(())
}

fn is_busy(e: &rusqlite::Error) -> bool {
    matches!(e.sqlite_error_code(), Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked))
}

pub fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// `YYYYMMDD-HHMMSS-xxxx` (UTC): sorts by start time, and stays the shape
/// chatstrata and old session files know.
fn new_id() -> String {
    let (y, m, d, hour, minute, second) = utc_parts(now_ms());
    format!(
        "{y:04}{m:02}{d:02}-{hour:02}{minute:02}{second:02}-{:04x}",
        (crate::skills::now_nanos() as u64 ^ std::process::id() as u64) & 0xffff
    )
}

/// Exact UTC timestamp for saved activity, retaining millisecond precision.
pub fn timestamp(ms: u64) -> String {
    let (y, m, d, h, min, sec) = utc_parts(ms);
    format!("{y:04}-{m:02}-{d:02} {h:02}:{min:02}:{sec:02}.{:03} UTC", ms % 1000)
}

fn utc_parts(ms: u64) -> (i64, i64, i64, i64, i64, i64) {
    let secs = (ms / 1000) as i64;
    // Civil date from unix seconds (UTC), Howard Hinnant's algorithm.
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + if m <= 2 { 1 } else { 0 };
    (y, m, d, rem / 3600, (rem / 60) % 60, rem % 60)
}

#[cfg(test)]
mod tests;
