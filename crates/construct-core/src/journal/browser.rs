//! Recognizable sessions derived from the journal, scoped to the exact working
//! directory. Browsing is read-only and searches the whole eligible archive.

use super::{Entry, Journal, writer};
use crate::config::Paths;
use crate::event::{Event, SessionOrigin};
use anyhow::{Context, Result, bail, ensure};
use rusqlite::Connection;
use std::path::Path;

#[derive(Debug, Clone)]
pub struct Session {
    pub id: String,
    pub title: String,
    pub first: String,
    pub latest: String,
    pub cwd: String,
    pub model: String,
    pub provider: Option<String>,
    pub at_ms: u64,
    pub turns: usize,
    pub interrupted: bool,
    pub in_use: bool,
    pub unreadable: Option<String>,
    pub origin: Option<SessionOrigin>,
}

impl Journal {
    /// Do not create or migrate a journal merely to inspect it.
    pub fn read_only(file: &Path) -> Result<Self> {
        let conn = Connection::open_with_flags(file, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("reading {}", file.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        Ok(Self { conn, file: file.to_path_buf(), dir_synced: std::sync::atomic::AtomicBool::new(false) })
    }

    /// Restoration must fail on unknown, incompatible or malformed rows,
    /// even though general inspection tolerates newer additive variants.
    pub fn strict(&self, session: &str) -> Result<Vec<Entry>> {
        let mut st =
            self.conn.prepare_cached("SELECT seq, at_ms, v, payload FROM events WHERE session_id = ?1 ORDER BY seq")?;
        let rows = st.query_map([session], |r| {
            Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?, r.get::<_, String>(3)?))
        })?;
        let mut out = vec![];
        for row in rows {
            let (seq, at_ms, v, payload) = row?;
            ensure!(v == super::PAYLOAD_V, "session {session}, row {seq}: unsupported payload version {v}");
            let event = serde_json::from_str(&payload)
                .with_context(|| format!("session {session}, row {seq}: unreadable or incompatible history"))?;
            out.push(Entry { seq, at_ms: at_ms as u64, event });
        }
        ensure!(!out.is_empty(), "no session {session} in {}", self.file.display());
        Ok(out)
    }
}

/// Raw prompts for new journals, legacy user messages for old ones. Paired
/// model-facing user rows never inflate the count or replace the original.
pub fn prompts(entries: &[Entry]) -> Vec<String> {
    let mut out = vec![];
    let mut submitted = false;
    for e in entries {
        match &e.event {
            Event::Submitted(crate::event::Submission { text, .. }) => {
                out.push(text.clone());
                submitted = true;
            }
            Event::User { message } => {
                if !submitted {
                    out.push(
                        message
                            .content
                            .as_deref()
                            .unwrap_or("")
                            .split("\n\n<system-reminder>")
                            .next()
                            .unwrap_or("")
                            .to_string(),
                    );
                }
                submitted = false;
            }
            _ => {}
        }
    }
    out
}

pub fn saved_model(entries: &[Entry]) -> Option<(String, Option<String>)> {
    entries.iter().rev().find_map(|e| match &e.event {
        Event::Model { model, provider, .. } => Some((model.clone(), provider.clone())),
        Event::Head { model, manifest, .. } => Some((model.clone(), manifest.provider.clone())),
        _ => None,
    })
}

/// Direct ids obey the same workspace boundary as the browser. Quote a
/// relaunch directory as a shell argument, including embedded apostrophes.
pub fn validate_directory(entries: &[Entry], cwd: &Path) -> Result<()> {
    let Some(Entry { event: Event::Head { cwd: original, .. }, .. }) = entries.first() else {
        bail!("session has no readable head; cannot restore it safely");
    };
    ensure!(
        original == &cwd.display().to_string(),
        "session belongs to {original}; relaunch there: cd '{}' && construct --resume <id>",
        original.replace('\'', "'\\''")
    );
    Ok(())
}

/// Find the latest session for a dedicated wake lane (`Head.origin` matching
/// `cwd` and `trigger`, ADR 0079). Read-only; returns `None` when none exists.
pub fn find_dedicated(paths: &Paths, cwd: &Path, trigger: &str) -> Result<Option<String>> {
    let file = super::path(paths, cwd);
    if !file.exists() {
        return Ok(None);
    }
    let db = Journal::read_only(&file)?;
    let mut st = db.conn.prepare(
        "SELECT session_id FROM events WHERE type = 'head' \
         AND json_extract(payload, '$.cwd') = ?1 \
         AND json_extract(payload, '$.origin.kind') = 'wake' \
         AND json_extract(payload, '$.origin.trigger') = ?2 \
         AND json_extract(payload, '$.parent_session') IS NULL \
         ORDER BY seq DESC LIMIT 1",
    )?;
    let mut rows = st.query([cwd.display().to_string(), trigger.to_string()])?;
    Ok(rows.next()?.map(|r| r.get::<_, String>(0)).transpose()?)
}

fn head_origin(entries: &[Entry]) -> Option<SessionOrigin> {
    entries.first().and_then(|e| match &e.event {
        Event::Head { origin, .. } => origin.as_deref().cloned(),
        _ => None,
    })
}

fn wake_labels(entries: &[Entry]) -> Vec<String> {
    let mut out = vec![];
    for e in entries {
        match &e.event {
            Event::WakeReceived(w) => {
                let label = if w.summary.trim().is_empty() {
                    format!("[wake:{}/{}]", w.kind, w.source)
                } else {
                    w.summary.clone()
                };
                out.push(label);
            }
            Event::WakesDelivered(d) if out.is_empty() => {
                out.push(format!("[wake:{}/{}]", d.kind, d.source));
            }
            _ => {}
        }
    }
    out
}

pub fn browse(paths: &Paths, cwd: &Path) -> Result<Vec<Session>> {
    let file = super::path(paths, cwd);
    if !file.exists() {
        return Ok(vec![]);
    }
    let db = Journal::read_only(&file)?;
    let mut st = db.conn.prepare(
        "SELECT session_id, payload FROM events WHERE type = 'head' \
         AND json_extract(payload, '$.cwd') = ?1 \
         AND json_extract(payload, '$.parent_session') IS NULL ORDER BY seq DESC",
    )?;
    let heads = st.query_map([cwd.display().to_string()], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
    let mut out = vec![];
    for row in heads {
        let (id, _) = row?;
        let entries = db.since(&id, 0)?;
        let origin = head_origin(&entries);
        let user = prompts(&entries);
        let wakes = wake_labels(&entries);
        let is_wake = matches!(origin, Some(SessionOrigin::Wake { .. }));
        if user.is_empty() && wakes.is_empty() {
            continue;
        }
        let first = user.first().or_else(|| wakes.first()).cloned().unwrap_or_default();
        let latest = user.last().or_else(|| wakes.last()).cloned().unwrap_or_default();
        let default_title = match &origin {
            Some(SessionOrigin::Wake { trigger, .. }) if user.is_empty() => {
                if first.starts_with("[wake:") {
                    format!("[wake:{trigger}]")
                } else {
                    format!("[wake:{trigger}] {first}")
                }
            }
            _ => first.clone(),
        };
        let title = entries
            .iter()
            .rev()
            .find_map(|e| match &e.event {
                Event::SessionName { title } => Some(title.clone()),
                _ => None,
            })
            .unwrap_or(default_title);
        let (model, provider) = saved_model(&entries).unwrap_or_default();
        // Administrative, repair, model selection and learning events do not
        // change the conversational recency of a session.
        let at_ms = entries.iter().filter(|e| super::recovery::activity(&e.event)).map(|e| e.at_ms).max().unwrap_or(0);
        let wake_turns = entries.iter().filter(|e| matches!(e.event, Event::WakesDelivered(_))).count();
        let turns = if is_wake && user.is_empty() { wake_turns.max(1) } else { user.len() + wake_turns };
        let interrupted = entries
            .iter()
            .rev()
            .find_map(|e| match e.event {
                Event::Recovered { .. } => Some(true),
                Event::TurnStart => Some(true),
                Event::TurnEnd { interrupted, .. } => Some(interrupted),
                _ => None,
            })
            .unwrap_or(false);
        out.push(Session {
            in_use: writer::in_use(&file, &id),
            unreadable: db.strict(&id).err().map(|e| format!("{e:#}")),
            id,
            title,
            first,
            latest,
            cwd: cwd.display().to_string(),
            model,
            provider,
            at_ms,
            turns,
            interrupted,
            origin,
        });
    }
    out.sort_by(|a, b| b.at_ms.cmp(&a.at_ms).then_with(|| b.id.cmp(&a.id)));
    Ok(out)
}
