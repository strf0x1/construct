//! Session writer ownership and ordered durable event delivery. The OS lock
//! lives as long as the writer; crashes release it without stale-lock cleanup.

use super::{Journal, new_id, project};
use crate::event::{Event, Events};
use anyhow::{Context, Result};
use sha2::{Digest, Sha256};
use std::fs::{File, OpenOptions};
use std::path::Path;
use std::sync::Mutex;

/// The agent's handle on its session: appends every event it emits, in the
/// order the UI sees them.
pub struct Writer {
    pub session: String,
    db: Mutex<Journal>,
    _ownership: File,
    failure: Mutex<Option<String>>,
}

impl Writer {
    /// Start a new session: a fresh id and its head row.
    pub fn create(file: &Path, head: impl FnOnce(String) -> Event) -> Result<(Self, Event)> {
        let db = Journal::open(file)?;
        let session = new_id();
        let ownership = ownership(file, &session)?;
        let head = head(session.clone());
        db.append(&session, &head)?;
        Ok((Self { session, db: Mutex::new(db), _ownership: ownership, failure: Mutex::default() }, head))
    }

    /// Reopen `session` and project its conversation.
    pub fn resume(file: &Path, session: &str) -> Result<(Self, Vec<crate::llm::Message>)> {
        let ownership = ownership(file, session)?;
        let mut db = Journal::open(file)?;
        let entries = db.strict(session)?;
        db.recover(session, &entries)?;
        let entries = db.strict(session)?;
        anyhow::ensure!(!entries.is_empty(), "no session {session} in {}", file.display());
        let msgs = project(entries.iter().map(|e| &e.event));
        Ok((
            Self { session: session.to_string(), db: Mutex::new(db), _ownership: ownership, failure: Mutex::default() },
            msgs,
        ))
    }

    /// Journal `ev`, then send it. The lock spans both so the rows and the
    /// stream agree on order even when tools finish in parallel. A journal
    /// that can't be written never stops the turn.
    pub fn emit(&self, ev: Event, tx: &Events) {
        // A poisoned lock means a panic mid-append; the connection is still usable.
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        self.report(db.append(&self.session, &ev), Some(tx));
        let _ = tx.send(ev);
    }

    /// Journal a child session's event under the child's own session id, then
    /// send it wrapped as `Event::Child` so the UI and `--json` see the tree.
    /// The parent is the only writer: the child never holds this file.
    pub fn relay(&self, call_id: &str, child_session: &str, ev: Event, tx: &Events) {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        self.report(db.append(child_session, &ev), Some(tx));
        let _ = tx.send(Event::Child { id: call_id.to_string(), inner: Box::new(ev) });
    }

    /// Journal without sending: events from before there was a UI to send to.
    pub fn record(&self, ev: &Event) {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        self.report(db.append(&self.session, ev), None);
    }

    /// Rename under the session's writer ownership, with errors returned to the UI.
    pub fn rename(&self, title: &str) -> Result<()> {
        let title = title.trim();
        anyhow::ensure!(!title.is_empty(), "usage: /rename <non-empty title>");
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        db.append(&self.session, &Event::SessionName { title: title.to_string() })
    }

    /// The full journal, independent of model compaction.
    pub fn entries(&self) -> Result<Vec<super::Entry>> {
        self.db.lock().unwrap_or_else(|e| e.into_inner()).strict(&self.session)
    }

    /// Surface failures even when they happened before a UI channel existed.
    pub fn take_failure(&self) -> Option<String> {
        self.failure.lock().unwrap_or_else(|e| e.into_inner()).take()
    }

    fn report(&self, result: Result<()>, tx: Option<&Events>) {
        if let Err(e) = result {
            let message = format!("journal write failed; recovery is degraded: {e:#}");
            if let Some(tx) = tx {
                let _ = tx.send(Event::Error { message });
            } else {
                *self.failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(message);
            }
        }
    }

    /// The last seq this session wrote, for "learned up to here".
    pub fn last_seq(&self) -> i64 {
        let db = self.db.lock().unwrap_or_else(|e| e.into_inner());
        db.last_seq(&self.session).unwrap_or(0)
    }
}

fn lock_file(file: &Path, session: &str) -> Result<File> {
    let dir = file.with_extension("locks");
    std::fs::create_dir_all(&dir)?;
    let name = format!("{:x}.lock", Sha256::digest(session.as_bytes()));
    Ok(OpenOptions::new().read(true).write(true).create(true).truncate(false).open(dir.join(name))?)
}

fn ownership(file: &Path, session: &str) -> Result<File> {
    let lock = lock_file(file, session)?;
    lock.try_lock().with_context(|| format!("session {session} is in use by another process; wait for it to exit"))?;
    Ok(lock)
}

pub(super) fn in_use(file: &Path, session: &str) -> bool {
    // Only probe existing locks: listing an unused archive is read-only.
    let name = format!("{:x}.lock", Sha256::digest(session.as_bytes()));
    let path = file.with_extension("locks").join(name);
    let Ok(lock) = OpenOptions::new().read(true).write(true).open(path) else { return false };
    lock.try_lock().is_err()
}
