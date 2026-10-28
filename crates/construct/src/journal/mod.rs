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

use crate::event::{Event, Events};
use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

mod export;
mod read;
mod show;
mod totals;

pub use export::export;
pub use read::{Entry, project};
pub use show::{find, human, rows_text, share, status_rows, transcript};
pub use totals::Totals;

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
}

impl Journal {
    /// Open (creating if needed) and migrate. WAL so readers never block the
    /// writer, and a busy timeout so concurrent sessions in one project wait
    /// for each other instead of failing.
    pub fn open(file: &Path) -> Result<Self> {
        if let Some(dir) = file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let conn = Connection::open(file).with_context(|| format!("opening {}", file.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        let mut j = Self { conn };
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
        // WAL + NORMAL: a crash of construct loses nothing; only power loss
        // can lose the last commits. No fsync per row.
        self.conn.pragma_update(None, "synchronous", "normal")?;
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
        Ok(())
    }

    /// Drop every row of `session` after `seq`: rewind is truncate and
    /// re-project, never re-deriving what happened.
    pub fn rewind(&self, session: &str, seq: i64) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM events WHERE session_id = ?1 AND seq > ?2", params![session, seq])?)
    }
}

/// The agent's handle on its session: appends every event it emits, in the
/// order the UI sees them.
pub struct Writer {
    pub session: String,
    db: Mutex<Journal>,
}

impl Writer {
    /// Start a new session: a fresh id and its head row.
    pub fn create(file: &Path, head: impl FnOnce(String) -> Event) -> Result<(Self, Event)> {
        let db = Journal::open(file)?;
        let session = new_id();
        let head = head(session.clone());
        db.append(&session, &head)?;
        Ok((Self { session, db: Mutex::new(db) }, head))
    }

    /// Reopen `session` and project its conversation.
    pub fn resume(file: &Path, session: &str) -> Result<(Self, Vec<crate::llm::Message>)> {
        let db = Journal::open(file)?;
        let entries = db.since(session, 0)?;
        anyhow::ensure!(!entries.is_empty(), "no session {session} in {}", file.display());
        let msgs = project(entries.iter().map(|e| &e.event));
        Ok((Self { session: session.to_string(), db: Mutex::new(db) }, msgs))
    }

    /// Journal `ev`, then send it. The lock spans both so the rows and the
    /// stream agree on order even when tools finish in parallel. A journal
    /// that can't be written never stops the turn.
    pub fn emit(&self, ev: Event, tx: &Events) {
        // A poisoned lock means a panic mid-append; the connection is still usable.
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let _ = db.append(&self.session, &ev);
        let _ = tx.send(ev);
    }

    /// Journal a child session's event under the child's own session id, then
    /// send it wrapped as `Event::Child` so the UI and `--json` see the tree.
    /// The parent is the only writer: the child never holds this file.
    pub fn relay(&self, call_id: &str, child_session: &str, ev: Event, tx: &Events) {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let _ = db.append(child_session, &ev);
        let _ = tx.send(Event::Child { id: call_id.to_string(), inner: Box::new(ev) });
    }

    /// Journal without sending: events from before there was a UI to send to.
    pub fn record(&self, ev: &Event) {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        let _ = db.append(&self.session, ev);
    }

    /// The last seq this session wrote, for "learned up to here".
    pub fn last_seq(&self) -> i64 {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        db.last_seq(&self.session).unwrap_or(0)
    }
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
    let ms = now_ms();
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
    format!(
        "{y:04}{m:02}{d:02}-{:02}{:02}{:02}-{:04x}",
        rem / 3600,
        (rem / 60) % 60,
        rem % 60,
        (crate::skills::now_nanos() as u64 ^ std::process::id() as u64) & 0xffff
    )
}

#[cfg(test)]
mod tests;
