use super::*;
use crate::llm::Message;

fn tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("construct-journal-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d.join("journal.sqlite")
}

#[test]
fn rows_project_back_to_the_conversation_and_rewind_truncates() {
    let f = tmp("project");
    let db = Journal::open(&f).unwrap();
    let msgs = [Message::system("sys"), Message::user("hi"), Message::text("assistant", "hello")];
    for m in &msgs {
        db.append("s", &Event::for_message(m.clone())).unwrap();
        db.append("s", &Event::TurnStart).unwrap();
    }
    // Stream deltas are not kept.
    db.append("s", &Event::Text { text: "hel".into() }).unwrap();
    let all = db.since("s", 0).unwrap();
    assert_eq!(all.len(), 6);
    assert_eq!(project(all.iter().map(|e| &e.event)), msgs);
    db.rewind("s", all[3].seq).unwrap();
    assert_eq!(project(db.since("s", 0).unwrap().iter().map(|e| &e.event)), msgs[..2]);
    assert_eq!(db.last_seq("s").unwrap(), all[3].seq);
    // FTS follows inserts and the rewind's deletes.
    let hits = |q: &str| -> i64 {
        db.conn.query_row("SELECT count(*) FROM events_fts WHERE events_fts MATCH ?1", [q], |r| r.get(0)).unwrap()
    };
    assert_eq!((hits("hi"), hits("hello"), hits("sys")), (1, 0, 0));
}

#[test]
fn readers_skip_unknown_types_and_fields() {
    let f = tmp("unknown");
    let db = Journal::open(&f).unwrap();
    db.append("s", &Event::User { message: Message::user("a") }).unwrap();
    db.conn
        .execute(
            "INSERT INTO events (session_id, type, v, at_ms, payload) VALUES ('s', 'from_the_future', 1, 0, '{\"event\":\"from_the_future\",\"x\":1}')",
            [],
        )
        .unwrap();
    db.conn
        .execute(
            "INSERT INTO events (session_id, type, v, at_ms, payload) VALUES ('s', 'turn_end', 1, 0, '{\"event\":\"turn_end\",\"steps\":1,\"prompt\":0,\"completion\":0,\"ms\":0,\"interrupted\":false,\"new_field\":true}')",
            [],
        )
        .unwrap();
    let got = db.since("s", 0).unwrap();
    assert_eq!(got.len(), 2);
    assert!(matches!(got[1].event, Event::TurnEnd { steps: 1, .. }));
}

#[test]
fn open_migrates_once_and_reopens() {
    let f = tmp("migrate");
    drop(Journal::open(&f).unwrap());
    let db = Journal::open(&f).unwrap();
    let v: i64 = db.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
    let mode: String = db.conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
    assert_eq!((v, mode.as_str()), (VERSION, "wal"));
}

#[test]
fn latest_is_the_newest_head_in_this_directory() {
    let f = tmp("latest");
    let head = |cwd: &str| {
        let cwd = cwd.to_string();
        move |id| Event::Head {
            id,
            harness: "construct".into(),
            version: "0".into(),
            cwd,
            model: "m".into(),
            manifest: serde_json::from_value(serde_json::json!({
                "version": "0", "git_sha": "x", "git_dirty": false, "config_sha256": "", "tools": [],
                "editor": "replace", "model": "m", "router_mode": "local"
            }))
            .unwrap(),
            parent_session: None,
            parent_call_id: None,
        }
    };
    let (a, _) = Writer::create(&f, head("/a")).unwrap();
    let (b, _) = Writer::create(&f, head("/b")).unwrap();
    let db = Journal::open(&f).unwrap();
    assert_eq!(db.latest("/a").unwrap(), Some(a.session));
    assert_eq!(db.latest("/b").unwrap(), Some(b.session));
    assert_eq!(db.latest("/c").unwrap(), None);
}
