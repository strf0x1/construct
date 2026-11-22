//! Unit tests for the `deck` tool: request shaping, wait bounds, the
//! listing line and the catalog gate. The wire itself is pinned by the
//! contract test `tests/deck_control.rs`.

use super::summary::{LINE_MESSAGE, line, reply};
use super::*;
use crate::DeckTools;
use construct_core::config::ToolsConfig;
use construct_core::tools::Deck;

#[test]
fn each_operation_takes_exactly_the_wire_s_fields() {
    let ok = request(
        &json!({ "operation": "create", "project": "p", "brief": "b", "title": "t", "budget": { "tokens": 5 } }),
    );
    assert_eq!(
        ok.unwrap(),
        json!({ "operation": "create", "project": "p", "brief": "b", "title": "t", "budget": { "tokens": 5 } })
    );
    let refused = |args: Value| request(&args).unwrap_err().message;
    assert!(refused(json!({ "operation": "stop", "id": "t", "force": true })).contains("does not take force"));
    assert!(refused(json!({ "operation": "destroy", "id": "t" })).contains("operation must be one of"));
    assert!(refused(json!({ "id": "t" })).contains("operation must be one of"));
    assert!(refused(json!({ "operation": "prompt", "id": "t" })).contains("prompt needs text"));
    assert!(refused(json!({ "operation": "create", "project": "p", "brief": "" })).contains("brief"));
    assert!(refused(json!({ "operation": "create", "project": "p", "brief": "b", "budget": {} })).contains("budget"));
    assert!(
        refused(json!({ "operation": "create", "project": "p", "brief": "b", "budget": { "tokens": 0 } }))
            .contains("positive")
    );
    assert!(
        refused(json!({ "operation": "create", "project": "p", "brief": "b", "budget": { "cost": 1 } }))
            .contains("cost")
    );
    assert!(refused(json!({ "operation": "wait", "id": "t", "after_turn": -1 })).contains("after_turn"));
    assert_eq!(
        request(&json!({ "operation": "create", "brief": "b" })).unwrap(),
        json!({ "operation": "create", "brief": "b" }),
        "a create may leave its project to Deck (1.5.0)"
    );
    assert!(refused(json!({ "operation": "create", "project": "p" })).contains("create needs brief"));
    assert_eq!(request(&json!({ "operation": "tasks" })).unwrap(), json!({ "operation": "tasks" }));
    assert_eq!(request(&json!({ "operation": "task", "id": "t", "project": null })).unwrap()["id"], "t");
}

#[test]
fn a_wait_is_bounded_and_its_http_timeout_outlasts_it() {
    let w = request(&json!({ "operation": "wait", "id": "t" })).unwrap();
    assert_eq!(w["wait_ms"], json!(DEFAULT_WAIT_MS), "the wire requires wait_ms, so the tool fills it");
    for ms in [0, 1000, MAX_WAIT_MS] {
        let body = request(&json!({ "operation": "wait", "id": "t", "wait_ms": ms })).unwrap();
        assert!(timeout(&body) > Duration::from_millis(ms), "{ms}");
    }
    let over = request(&json!({ "operation": "wait", "id": "t", "wait_ms": MAX_WAIT_MS + 1 })).unwrap_err();
    assert!(over.message.contains("at most 300000"), "never clamped: {}", over.message);
    assert!(
        timeout(&json!({ "operation": "prompt" })) > Duration::from_secs(60),
        "prompt waits up to 60 s for a session"
    );
}

#[test]
fn a_listing_line_is_bounded_and_says_what_needs_a_person() {
    let task = json!({
        "id": "w1", "project": "git:abc", "title": "Fix it", "state": "running",
        "turn": { "number": 3, "state": "idle", "stopReason": "end_turn" },
        "attention": "rebooting", "pending_approvals": 2,
        "publication": { "kind": "branch", "branch": "deck/w1", "commit": "a".repeat(40) },
        "pull_request": null, "created_by": "c1",
        "last_message": format!("line one\nline two {}", "x".repeat(4000)), "future": true
    });
    let text = line(&task, true);
    assert!(text.starts_with("w1 [git:abc] running, turn 3 idle (end_turn) \"Fix it\""), "{text}");
    assert!(text.contains("needs a person: rebooting") && text.contains("2 approvals pending"), "{text}");
    assert!(text.contains("published deck/w1@aaaaaaaaaaaa") && text.contains("last: line one line two"), "{text}");
    assert!(!text.contains('\n') && text.len() < 400 + LINE_MESSAGE, "{} bytes", text.len());
    let bare = json!({ "id": "w2", "project": "p", "state": "created", "turn": null, "title": null });
    assert_eq!(line(&bare, true), "w2 [p] created, no turn yet");
}

#[test]
fn replies_read_as_what_to_do_next() {
    let (text, data) = reply(&json!({ "operation": "prompt", "id": "w1" }), &json!({ "turn": 2 }));
    assert!(text.contains("began turn 2") && text.contains("after_turn 1"), "{text}");
    assert!(data["turn"] == 2);
    let (text, data) = reply(&json!({ "operation": "tasks" }), &json!({ "tasks": [], "truncated": false }));
    assert_eq!((text.as_str(), &data["changed"]), ("No tasks in scope.", &json!(false)), "a 1.4 reply has no projects");
    let one = json!({ "tasks": [], "truncated": false, "projects": [{ "id": "git:abc", "name": "app" }] });
    let (text, data) = reply(&json!({ "operation": "tasks" }), &one);
    assert_eq!(
        text, "Your projects: app (git:abc) (create uses it when you leave project out)\nNo tasks in scope.",
        "an empty listing still names the projects"
    );
    assert_eq!(data["projects"], json!(["git:abc"]));
    let two = json!({ "tasks": [{ "id": "w1", "project": "git:abc", "state": "created" }],
        "projects": [{ "id": "git:abc", "name": "app" }, { "id": "git:def" }] });
    let (text, _) = reply(&json!({ "operation": "tasks" }), &two);
    assert!(text.starts_with("Your projects: app (git:abc), git:def\n1 task:\nw1 [git:abc]"), "{text}");
    let (text, _) = reply(&json!({ "operation": "create", "brief": "b" }), &json!({ "id": "w9" }));
    assert!(text.starts_with("Created task w9; its brief"), "{text}");
    let task = json!({ "id": "w1", "project": "p", "state": "running", "last_message": "done", "last_message_truncated": true });
    let (text, _) =
        reply(&json!({ "operation": "wait", "id": "w1", "wait_ms": 5 }), &json!({ "timed_out": true, "task": task }));
    assert!(
        text.starts_with("Still going after 5 ms") && text.ends_with("Last message (cut by Deck):\ndone"),
        "{text}"
    );
    let (text, _) = reply(&json!({ "operation": "stop", "id": "w1" }), &json!({ "id": "w1", "state": "stopped" }));
    assert_eq!(text, "Task w1 is stopped.");
}

#[test]
fn the_tool_exists_only_with_a_control_url() {
    use construct_core::tools::{Catalog, ToolSources};
    let cfg = ToolsConfig::default();
    let sources = ToolSources::default().with(DeckTools);
    let build = |deck: &Deck| Catalog::build_extended(&cfg, None, deck, &sources).unwrap();
    assert!(build(&Deck::default()).get(NAME).is_none(), "an ordinary session has no deck tool");
    let github = Deck { boundary: true, github: true, control: None };
    assert!(build(&github).get(NAME).is_none(), "nor a worker task");
    let coordinator = Deck { control: Some("http://127.0.0.1:9".into()), ..Deck::default() };
    let catalog = build(&coordinator);
    assert_eq!(catalog.group_of(NAME), Some(Group::Coordinator));
    assert_eq!(catalog.kind_of(NAME), ToolKind::Execute);
    assert_eq!(catalog.names().last().map(String::as_str), Some(NAME), "it stays last, as it was");
    let off = ToolsConfig { disabled: vec![NAME.into()], ..ToolsConfig::default() };
    let catalog = Catalog::build_extended(&off, None, &coordinator, &sources).unwrap();
    assert!(catalog.get(NAME).is_none(), "[tools] disabled reaches a registered tool");
}

#[test]
fn permission_previews_and_observed_answers_are_bounded_and_informational() {
    let task = json!({"id":"w","project":"p","state":"running","permissions":{
    "cursor":"x:3","pending_count":20,"pending":(0..16).map(|i|json!({
        "observation_id":format!("x:{i}"),"subject":"Run\nchecks","kind":"execute","truncated":false
    })).collect::<Vec<_>>(),"resolved":[
        {"observation_id":"old","verdict":"allowed","source":"response"},
        {"observation_id":"retired","verdict":"denied","source":"retired"},
        {"observation_id":"future","verdict":"allow_future","source":"response"}
    ]}});
    for op in ["task", "tasks", "wait"] {
        let body = json!({"operation":op,"wait_ms":0});
        let response = if op == "tasks" { json!({"tasks":[task]}) } else { json!({"task":task}) };
        let (text, _) = reply(&body, &response);
        assert!(text.contains("user permissions pending: 20"), "{text}");
        assert!(text.contains("execute \"Run checks\"") && text.contains("more questions omitted"), "{text}");
        assert!(!text.contains("question x:16 pending"));
        assert!(text.contains("question old: allowed (observed client response)"));
        assert!(text.contains("question retired: unknown (retired without observed answer)"));
        assert!(text.contains("question future: unknown (observed client response)"));
        assert!(text.contains("responses do not confirm delivery or execution"));
    }
    let (text, _) = reply(&json!({"operation":"task"}), &json!({"task":{"id":"old","project":"p","state":"running"}}));
    assert!(!text.contains("permissions"), "missing is unavailable, not an empty view");
}

#[test]
fn incomplete_permission_views_are_unavailable() {
    for view in
        [json!({}), json!({"pending_count":0}), json!({"cursor":"x","pending_count":0,"pending":[],"resolved":null})]
    {
        let (text, _) = reply(&json!({"operation":"task"}), &json!({"task":{"id":"w","permissions":view}}));
        assert!(!text.contains("permissions pending"), "{text}");
    }
}
