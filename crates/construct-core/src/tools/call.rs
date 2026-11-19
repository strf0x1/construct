//! A running tool call's handle on its session: its id and, for a tool that
//! runs work of its own (a delegated child), the way to stream that work's
//! events up.
//!
//! The design choice: the stream goes through the parent's `journal::Writer`
//! and never through the tool's own storage. A child's events are journaled by
//! the parent under the child's session id (learned from its `Head`), so the
//! parent is the only journal writer and the child never needs the file.
//! Tools that do not stream take `Tool::call` and never see this.

use crate::event::{Event, Events};
use crate::journal::Writer;
use std::sync::{Arc, Mutex};

pub struct Call {
    pub id: String,
    sink: Option<(Arc<Writer>, Events)>,
    /// The child session this call is running, once its `Head` has arrived.
    child: Mutex<Option<String>>,
}

impl Call {
    pub fn new(id: &str, journal: Arc<Writer>, tx: Events) -> Self {
        Self { id: id.to_string(), sink: Some((journal, tx)), child: Mutex::new(None) }
    }

    /// The session this call belongs to, for a child's head row.
    pub fn session(&self) -> Option<&str> {
        self.sink.as_ref().map(|(journal, _)| journal.session.as_str())
    }

    /// The session's journal and event stream, for a tool that starts work
    /// which reports on its own later (`jobs`).
    pub fn sink(&self) -> Option<(Arc<Writer>, Events)> {
        self.sink.clone()
    }

    /// Journal and send one event of the session's own.
    pub fn emit(&self, ev: Event) {
        if let Some((journal, tx)) = &self.sink {
            journal.emit(ev, tx);
        }
    }

    /// A call with nowhere to stream to (tests, the learning pass).
    pub fn detached(id: &str) -> Self {
        Self { id: id.to_string(), sink: None, child: Mutex::new(None) }
    }

    /// Pass one event of the child's up: journaled under the child's session
    /// id and sent to the UI wrapped in `Event::Child`. Returns whether it
    /// went anywhere. Events before the child's `Head` have no session to
    /// belong to and are dropped. A child's own children are not journaled
    /// here (depth is capped at one).
    pub fn forward(&self, ev: Event) -> bool {
        let Some((journal, tx)) = &self.sink else { return false };
        // A poisoned lock means a panic mid-assignment; the value is still usable.
        let mut child = self.child.lock().unwrap_or_else(|e| e.into_inner());
        if let Event::Head { id, .. } = &ev {
            *child = Some(id.clone());
        }
        match child.as_deref() {
            Some(session) => {
                journal.relay(&self.id, session, ev, tx);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::journal::Journal;
    use crate::llm::Message;
    use crate::manifest::Manifest;
    use crate::tools::Catalog;

    fn head(id: &str) -> Event {
        Event::Head {
            id: id.into(),
            harness: "construct".into(),
            version: "0".into(),
            cwd: "/w".into(),
            model: "m".into(),
            manifest: {
                let cfg = Config::default();
                Manifest::new(&cfg, &Catalog::build(&cfg.tools, None).unwrap())
            },
            parent_session: Some("parent".into()),
            parent_call_id: Some("call-1".into()),
        }
    }

    #[test]
    fn forwarded_events_are_journaled_under_the_child_and_sent_wrapped() {
        let dir = std::env::temp_dir().join(format!("construct-call-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let file = dir.join("journal.sqlite");
        let (writer, _) = Writer::create(&file, |id| head(&id)).unwrap();
        let parent = writer.session.clone();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let call = Call::new("call-1", Arc::new(writer), tx);

        // Before the child's head there is no session to file it under.
        assert!(!call.forward(Event::TurnStart));
        assert!(call.forward(head("child")));
        assert!(call.forward(Event::User { message: Message::user("go") }));
        assert!(call.forward(Event::Text { text: "tok".into() }));

        let db = Journal::open(&file).unwrap();
        let rows = db.since("child", 0).unwrap();
        // The head and the message; the stream delta is not durable.
        assert_eq!(rows.len(), 2);
        assert!(matches!(rows[0].event, Event::Head { .. }));
        assert!(matches!(rows[1].event, Event::User { .. }));

        let mut sent = vec![];
        while let Ok(ev) = rx.try_recv() {
            sent.push(ev);
        }
        assert_eq!(sent.len(), 3);
        assert!(sent.iter().all(|e| matches!(e, Event::Child { id, .. } if id == "call-1")));
        // The parent's own session holds only its head: no wrapper rows.
        assert_eq!(db.since(&parent, 0).unwrap().len(), 1);
    }

    #[test]
    fn a_detached_call_forwards_nowhere() {
        let call = Call::detached("c");
        assert!(!call.forward(head("child")));
    }
}
