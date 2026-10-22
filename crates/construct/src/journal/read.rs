//! Reading the journal back: a session's events after a seq, the newest
//! session in a directory, and the projector that rebuilds the conversation
//! for resume. Rows whose payload this build can't parse (a newer variant,
//! a type it never knew) are skipped, never an error.

use super::Journal;
use crate::event::Event;
use crate::llm::Message;
use anyhow::Result;
use rusqlite::params;

/// One parsed row.
#[derive(Debug, Clone)]
pub struct Entry {
    pub seq: i64,
    pub at_ms: u64,
    pub event: Event,
}

impl Journal {
    /// `session`'s events with seq greater than `seq`, in order. `since(s, 0)`
    /// is the whole session.
    pub fn since(&self, session: &str, seq: i64) -> Result<Vec<Entry>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT seq, at_ms, payload FROM events WHERE session_id = ?1 AND seq > ?2 ORDER BY seq")?;
        let rows = st.query_map(params![session, seq], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?))
        })?;
        Ok(rows
            .flatten()
            .filter_map(|(seq, at, payload)| {
                // Upcasting an old payload shape goes here, keyed on the
                // row's `v`, the first time `v` is bumped.
                let event = serde_json::from_str(&payload).ok()?;
                Some(Entry { seq, at_ms: at as u64, event })
            })
            .collect())
    }

    /// The highest seq `session` has written.
    pub fn last_seq(&self, session: &str) -> Result<i64> {
        Ok(self
            .conn
            .prepare_cached("SELECT coalesce(max(seq), 0) FROM events WHERE session_id = ?1")?
            .query_row(params![session], |r| r.get(0))?)
    }

    /// The newest session started in `cwd`. Clones and worktrees share a
    /// journal, so "this directory" is narrower than "this project".
    pub fn latest(&self, cwd: &str) -> Result<Option<String>> {
        let mut st = self.conn.prepare_cached("SELECT payload FROM events WHERE type = 'head' ORDER BY seq DESC")?;
        let heads = st.query_map([], |r| r.get::<_, String>(0))?;
        for p in heads.flatten() {
            if let Ok(Event::Head { id, cwd: c, .. }) = serde_json::from_str(&p)
                && c == cwd
            {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }
}

/// The conversation as the model saw it, from the message-bearing events.
/// Nothing else is consulted: what was decided is in the messages.
pub fn project<'a>(events: impl IntoIterator<Item = &'a Event>) -> Vec<Message> {
    events.into_iter().filter_map(Event::message).cloned().collect()
}
