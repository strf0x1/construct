//! Session discovery and switching through the real CLI and TUI. Requests and
//! journal rows prove isolation and persistence, independently of mock replies.

use crate::common::*;
use construct::event::Event;
use construct::journal::{self, Journal, Writer};
use construct::llm::Message;
use construct_dev::drive;
use serde_json::json;

fn play(d: &mut drive::Driver, script: &str) {
    let mut log = vec![];
    if let Err(e) = drive::run_script(d, script, &mut log) {
        panic!("{e:#}\n{}", String::from_utf8_lossy(&log));
    }
}

fn tui(env: &Env, args: &[&str], cols: u16, rows: u16) -> drive::Driver {
    let mut command = vec![construct().display().to_string()];
    command.extend(args.iter().map(|s| s.to_string()));
    drive::Driver::spawn(&command, &env.env(), Some(env.work.to_str().unwrap()), cols, rows).unwrap()
}

fn paths(env: &Env) -> construct::config::Paths {
    construct::config::Paths { home: env.home.clone() }
}

fn seed(env: &Env, label: &str, cwd: &str, child: bool, prompt: bool) -> String {
    let cfg = construct::config::Config::default();
    let (writer, _) = Writer::create(&env.journal_path(&env.work), |id| Event::Head {
        id,
        harness: "construct".into(),
        version: "test".into(),
        cwd: cwd.into(),
        model: cfg.model.clone(),
        manifest: construct::manifest::Manifest::foreign(&cfg),
        parent_session: child.then(|| "parent".into()),
        parent_call_id: None,
    })
    .unwrap();
    if prompt {
        writer.record(&Event::Submitted { text: label.into() });
        writer.record(&Event::User {
            message: Message::user(format!("{label}\n\n<system-reminder>injected</system-reminder>")),
        });
    }
    writer.session.clone()
}

#[test]
fn startup_cancel_and_empty_browser_need_no_credentials_and_create_no_session() {
    let env = Env::new("sessions-cancel", json!({"steps":[]}));
    env.config("api_key_secret = 'MISSING_SESSION_BROWSER_KEY'\n[learn]\nenabled = false\n");
    let mut d = tui(&env, &["--resume"], 80, 24);
    play(&mut d, "wait \"No sessions in this directory\" 5000\nkey esc\n");
    for _ in 0..30 {
        if d.exited() {
            break;
        }
        d.pump(std::time::Duration::from_millis(20));
    }
    assert!(d.exited(), "startup escape exits cleanly");
    assert!(env.sessions().is_empty());
    assert!(env.requests().is_empty());
    assert!(!env.journal_path(&env.work).exists(), "even the journal was not created");
}

#[test]
fn archive_filters_all_sessions_and_navigates_old_unicode_rows() {
    let env = Env::new("sessions-archive", json!({"steps":[]}));
    env.config("[learn]\nenabled = false\n");
    let cwd = env.work.display().to_string();
    let oldest = seed(&env, "雪だるま first original", &cwd, false, true);
    for i in 0..65 {
        seed(&env, &format!("Archived prompt {i:02}"), &cwd, false, true);
    }
    seed(&env, "hidden child", &cwd, true, true);
    seed(&env, "unused head", &cwd, false, false);
    seed(&env, "wrong directory", "/another/directory", false, true);
    let db = Journal::open(&env.journal_path(&env.work)).unwrap();
    db.append(&oldest, &Event::SessionName { title: "A manual 雪 title".into() }).unwrap();
    db.append(&oldest, &Event::Submitted { text: "last unique request".into() }).unwrap();
    let rows = journal::browse(&paths(&env), &env.work).unwrap();
    assert_eq!(rows.len(), 66);
    assert_eq!(rows[0].id, oldest, "activity rather than start time");
    assert_eq!(rows[0].turns, 2);
    assert_eq!(rows[0].first, "雪だるま first original");
    assert!(!rows[0].first.contains("injected"));
    let mut d = tui(&env, &["--resume"], 100, 30);
    play(
        &mut d,
        "wait \"Search sessions\" 5000\nwait \"Archived prompt\" 5000\nkey end\nkey pgup\nkey pgdn\nkey home\ntype last unique request\nwait \"1/66\" 3000\nexpect \"A manual 雪 title\"\nkey ctrl-u\ntype zzz-never-matches\nwait \"No matching sessions\" 3000\nkey ctrl-u\ntype 雪だるま\nwait \"1/66\" 3000\nresize 46 18\nwait \"A manual 雪\" 3000\nsnap narrow-session\nexpect \"A manual 雪\"\nkey esc\n",
    );
    assert_eq!(env.sessions().len(), 69, "browsing never writes a head");
    assert!(env.requests().is_empty());
}

#[test]
fn in_app_a_b_a_restores_transcripts_context_models_and_names() {
    let env = Env::new(
        "sessions-switch",
        json!({"steps":[
            {"call":"read","args":{"path":"note.txt"}}, {"text":"A historical answer"},
            {"text":"B isolated answer"}, {"text":"A continued answer"}, {"text":"B continued answer"}
        ]}),
    );
    env.config("model = 'model-a'\n[router]\nmode = 'local'\n[learn]\nenabled = false\n");
    std::fs::write(env.work.join("note.txt"), "original A tool output").unwrap();
    let mut d = tui(&env, &[], 110, 38);
    play(
        &mut d,
        "wait \"ask construct\" 5000\ntype First A prompt\nkey enter\nwait \"A historical answer\" 5000\ntype /rename Session A\nkey enter\nwait \"Session renamed\" 3000\ntype /new\nkey enter\ngone \"A historical answer\" 3000\ntype /model model-b\nkey enter\nwait \"model → model-b\" 3000\ntype First B prompt\nkey enter\nwait \"B isolated answer\" 5000\n",
    );
    let ids = env.sessions();
    assert_eq!(ids.len(), 2);
    play(
        &mut d,
        "type /resume\nkey enter\nwait \"Resume session\" 3000\nwait \"Session A\" 3000\ntype Session A\nkey enter\nwait \"A historical answer\" 5000\nexpect \"First A prompt\"\nkey ctrl-o\nwait \"original A tool output\" 3000\ntype Continue A\nkey enter\nwait \"A continued answer\" 5000\n",
    );
    let reqs = env.requests();
    let a = reqs.last().unwrap();
    assert_eq!(a["model"], "model-a");
    assert!(a.to_string().contains("original A tool output"));
    assert!(!a.to_string().contains("First B prompt"));
    play(
        &mut d,
        &format!(
            "type /resume {}\nkey enter\nwait \"B isolated answer\" 5000\ngone \"First A prompt\" 3000\ntype Continue B\nkey enter\nwait \"B continued answer\" 5000\n",
            ids[1]
        ),
    );
    let reqs = env.requests();
    let b = reqs.last().unwrap();
    assert_eq!(b["model"], "model-b");
    assert!(!b.to_string().contains("First A prompt"));
    assert!(b.to_string().contains("First B prompt"));
    let before = env.journal().len();
    play(
        &mut d,
        &format!(
            "type /resume {}\nkey enter\ntype /resume\nkey enter\nwait \"Resume session\" 3000\nkey esc\ntype /rename\nkey enter\nkey enter\nwait \"non-empty title\" 3000\n",
            ids[1]
        ),
    );
    assert_eq!(env.journal().len(), before, "cancel/current/empty name are no-ops");
    play(&mut d, "type /quit\nkey enter\n");
    drop(d);
    let rows = journal::browse(&paths(&env), &env.work).unwrap();
    assert!(rows.iter().any(|r| r.id == ids[0] && r.title == "Session A" && r.turns == 2));
    let mut d = tui(&env, &["--resume"], 110, 38);
    play(
        &mut d,
        "wait \"Resume session\" 5000\nwait \"Session A\" 3000\ntype Session A\nkey enter\nwait \"A continued answer\" 5000\ntype /quit\nkey enter\n",
    );
    assert_eq!(env.requests().len(), reqs.len(), "selection never generates");
}

#[test]
fn cli_policy_conflicts_directory_isolation_override_and_continue_selection() {
    let env =
        Env::new("sessions-cli", json!({"steps":[{"text":"one"},{"text":"two"},{"text":"three"},{"text":"four"}]}));
    env.config("model = 'old-model'\n[router]\nmode = 'local'\n[learn]\nenabled = false\n");
    assert!(env.run(&["run", "A"]).status.success());
    assert!(env.run(&["run", "B"]).status.success());
    let ids = env.sessions();
    // Activity on A must not change the selection rule of -c.
    assert!(env.run(&["--resume", &ids[0], "--model", "override-model", "run", "A again"]).status.success());
    assert!(env.run(&["-c", "run", "continue newest started"]).status.success());
    let reqs = env.requests();
    assert_eq!(reqs[2]["model"], "override-model");
    assert_eq!(reqs[3]["model"], "old-model");
    assert!(reqs[3].to_string().contains("\"B\""));
    assert!(!reqs[3].to_string().contains("A again"));
    let bare = env.run(&["--resume"]);
    assert!(!bare.status.success());
    assert!(text(&bare).contains("requires an interactive terminal"));
    let headless = env.run(&["--resume", "run", "prompt"]);
    assert!(!headless.status.success());
    let conflict = env.run(&["--resume", &ids[0], "-c", "run", "x"]);
    assert!(!conflict.status.success());
    assert!(text(&conflict).contains("cannot be used with"));
    let other = env.dir.join("other");
    std::fs::create_dir_all(&other).unwrap();
    let mismatch = env.run_in(&other, &[], &["--resume", &ids[0], "run", "x"]);
    assert!(!mismatch.status.success());
    assert!(text(&mismatch).contains(&env.work.display().to_string()), "{}", text(&mismatch));
    assert!(text(&mismatch).contains("relaunch there"));
    assert_eq!(env.requests().len(), 4);
}

#[test]
fn rename_and_open_do_not_change_activity_and_duplicate_names_are_allowed() {
    let env = Env::new("sessions-recency", json!({"steps":[]}));
    let cwd = env.work.display().to_string();
    let a = seed(&env, "first A", &cwd, false, true);
    let b = seed(&env, "first B", &cwd, false, true);
    let before = journal::browse(&paths(&env), &env.work).unwrap();
    let at = |id: &str, rows: &[journal::Session]| rows.iter().find(|r| r.id == id).unwrap().at_ms;
    for id in [&a, &b] {
        let (writer, _) = Writer::resume(&env.journal_path(&env.work), id).unwrap();
        assert!(writer.rename("   ").is_err());
        writer.rename("same name").unwrap();
    }
    let after = journal::browse(&paths(&env), &env.work).unwrap();
    assert_eq!(after.iter().map(|r| r.title.as_str()).collect::<Vec<_>>(), ["same name", "same name"]);
    assert_eq!(at(&a, &before), at(&a, &after));
    assert_eq!(at(&b, &before), at(&b, &after));
}

#[test]
fn busy_or_queued_turns_refuse_switching_without_a_delayed_switch() {
    let env = Env::new(
        "sessions-busy",
        json!({"steps":[
            {"text":"first turn finished","delay_ms":600}, {"text":"queued turn finished","delay_ms":600}
        ]}),
    );
    env.config("[router]\nmode = 'local'\n[learn]\nenabled = false\n");
    let b = seed(&env, "Destination B", &env.work.display().to_string(), false, true);
    let mut d = tui(&env, &[], 110, 40);
    play(
        &mut d,
        &format!(
            "wait \"ask construct\" 5000\ntype Current A\nkey enter\ntype /resume {b}\nkey enter\nwait \"Cannot switch sessions\" 3000\ntype Queued A\nkey enter\ntype /resume\nkey enter\nwait \"queued turn finished\" 5000\nsleep 300\nexpect \"Current A\"\n"
        ),
    );
    let rows = env.journal();
    let current = env.sessions().last().unwrap().clone();
    assert!(rows.iter().filter(|r| r.ty == "submitted" && r.session != b).all(|r| r.session == current));
    assert!(env.requests().iter().all(|r| !r.to_string().contains("Destination B")));
    assert!(!d.text().contains("Resume session ·"), "browser was not queued");
}

#[test]
fn unavailable_saved_provider_requires_an_explicit_replacement() {
    let env = Env::new(
        "sessions-provider",
        json!({"steps":[{"text":"saved provider answer"},{"text":"chosen provider answer"}]}),
    );
    env.config(&format!("model = 'archived/model'\n[providers.archived]\nbase_url = 'http://127.0.0.1:{}/v1'\napi_key_secret = 'OPENAI_API_KEY'\n[providers.archived.models.model]\ncontext_window = 32000\n[router]\nmode = 'local'\n[learn]\nenabled = false\n", env.port));
    let out = env.run(&["run", "saved provider prompt"]);
    assert!(out.status.success(), "{}", text(&out));
    let id = env.sessions()[0].clone();
    env.config("model = 'current-default'\n[models.replacement]\ncontext_window = 64000\n[router]\nmode = 'local'\n[learn]\nenabled = false\n");
    let out = env.run(&["--resume", &id, "run", "cannot send here"]);
    assert!(!out.status.success());
    assert!(text(&out).contains("provider \"archived\" is unavailable"), "{}", text(&out));
    assert_eq!(env.requests().len(), 1);
    let mut d = tui(&env, &["--resume", &id], 110, 36);
    play(&mut d, "wait \"unavailable\" 5000\ntype /model replacement\nkey enter\n");
    // Model completion takes a second Enter when it matches the catalog.
    play(
        &mut d,
        "key enter\nwait \"saved provider answer\" 5000\ntype Continue with chosen model\nkey enter\nwait \"chosen provider answer\" 5000\n",
    );
    assert_eq!(env.requests().last().unwrap()["model"], "replacement");
    assert_eq!(env.sessions(), [id]);
}

#[test]
fn compacted_model_view_keeps_the_full_historical_tui_transcript() {
    let env = Env::new("sessions-compacted-ui", json!({"steps":[{"text":"continued with compacted context"}]}));
    env.config("[router]\nmode = 'local'\n[learn]\nenabled = false\n");
    let cfg = construct::config::Config::default();
    let (w, _) = Writer::create(&env.journal_path(&env.work), |id| Event::Head {
        id,
        harness: "construct".into(),
        version: "test".into(),
        cwd: env.work.display().to_string(),
        model: cfg.model.clone(),
        manifest: construct::manifest::Manifest::foreign(&cfg),
        parent_session: None,
        parent_call_id: None,
    })
    .unwrap();
    w.record(&Event::System { message: Message::system("old system instructions") });
    for (user, assistant) in [
        ("A very old original user request", "A very old original assistant answer"),
        ("Latest user request", "Latest assistant answer"),
    ] {
        w.record(&Event::Submitted { text: user.into() });
        w.record(&Event::User { message: Message::user(user) });
        w.record(&Event::Assistant { message: Message::text("assistant", assistant) });
    }
    w.record(
        &serde_json::from_value::<Event>(json!({"event":"compacted", "before":5, "after":5,
        "mode":"summarize", "cut":{"summary":{"upto":3,"text":"Saved compact summary"}}}))
        .unwrap(),
    );
    let id = w.session.clone();
    drop(w);
    let mut d = tui(&env, &["--resume", &id], 110, 42);
    play(
        &mut d,
        "wait \"A very old original user request\" 5000\nexpect \"A very old original assistant answer\"\nexpect \"Latest assistant answer\"\ntype Continue after compaction\nkey enter\nwait \"continued with compacted context\" 5000\n",
    );
    let reqs = env.requests();
    assert_eq!(reqs.len(), 1);
    let request = reqs[0].to_string();
    assert!(request.contains("Saved compact summary"));
    assert!(!request.contains("A very old original user request"));
    assert!(!request.contains("A very old original assistant answer"));
    assert!(!request.contains("old system instructions"), "system instructions are rebuilt");
    assert!(request.contains("Latest user request"));
}

#[test]
fn a_running_background_job_refuses_switching_without_queueing_it() {
    let env = Env::new(
        "sessions-busy-job",
        json!({"steps":[
            {"call":"shell","args":{"command":"sleep 300","background":true}}, {"text":"Background is running"},
            {"call":"job","args":{"action":"cancel","id":"j1"}}, {"text":"Background is cancelled"}
        ]}),
    );
    env.config("[router]\nmode = 'local'\n[learn]\nenabled = false\n");
    let b = seed(&env, "Destination B", &env.work.display().to_string(), false, true);
    let mut d = tui(&env, &[], 110, 38);
    play(
        &mut d,
        &format!(
            "wait \"ask construct\" 5000\ntype start background work\nkey enter\nwait \"Background is running\" 5000\ntype /resume {b}\nkey enter\nwait \"Cannot switch sessions\" 3000\ntype cancel background work\nkey enter\nwait \"Background is cancelled\" 5000\nsleep 300\nexpect \"start background work\"\n"
        ),
    );
    assert!(env.journal().iter().any(|r| r.ty == "job_end" && r.payload["status"] == "cancelled"));
    assert!(env.requests().iter().all(|r| !r.to_string().contains("Destination B")));
}

#[test]
fn switching_builds_fresh_permissions_including_when_returning_to_a_session() {
    let outside = Env::dir_for("sessions-permissions").join("outside.txt");
    let read = json!({"call":"read","args":{"path":outside}});
    let env = Env::new(
        "sessions-permissions",
        json!({"steps":[
            read.clone(), {"text":"A permission granted"}, read.clone(), {"text":"B permission granted"},
            read, {"text":"A permission rebuilt"}
        ]}),
    );
    std::fs::write(&outside, "outside content").unwrap();
    env.config("[router]\nmode = 'local'\n[learn]\nenabled = false\n");
    let mut d = tui(&env, &[], 110, 40);
    play(
        &mut d,
        "wait \"ask construct\" 5000\ntype A read outside\nkey enter\nwait \"PERMISSION\" 5000\nsleep 600\ntype s\nwait \"A permission granted\" 5000\ntype /new\nkey enter\ngone \"A permission granted\" 3000\ntype B read outside\nkey enter\nwait \"PERMISSION\" 5000\nsleep 600\ntype y\nwait \"B permission granted\" 5000\n",
    );
    let a = env.sessions()[0].clone();
    play(
        &mut d,
        &format!(
            "type /resume {a}\nkey enter\nwait \"A permission granted\" 5000\ntype A read outside again\nkey enter\nwait \"PERMISSION\" 5000\nsleep 600\ntype y\nwait \"A permission rebuilt\" 5000\n"
        ),
    );
    let grants: Vec<_> = env.journal().into_iter().filter(|r| r.ty == "access").collect();
    assert_eq!(grants.len(), 3, "permissions were requested anew in each runtime");
}
