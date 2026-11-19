//! What the model reads of a control reply (§13.4): the coordinator's
//! projects, then one line per task summary, with the last message capped,
//! and nothing unbounded.
//!
//! The design choice: replies are open, so every field is read leniently and
//! an unknown one is ignored; a summary that lacks a field still prints.

use super::{INTEGRATE, MAX_CONTRIBUTORS, MAX_INTEGRATION_REASON};
use serde_json::{Value, json};

/// How much of a task's last message a listing line shows.
pub const LINE_MESSAGE: usize = 200;
/// The most projects a `tasks` reply's projects line names.
const MAX_PROJECTS: usize = 32;

/// The text and the journal's data for a successful reply to `body`.
pub fn reply(body: &Value, reply: &Value) -> (String, Value) {
    let operation = body["operation"].as_str().unwrap_or_default();
    let id = body["id"].as_str().unwrap_or_default();
    match operation {
        "tasks" => {
            let tasks = reply["tasks"].as_array().map(Vec::as_slice).unwrap_or_default();
            let truncated = reply["truncated"].as_bool().unwrap_or(false);
            // Since 1.5.0 every reply names the projects; a 1.4 Deck does not.
            let mut text = projects(&reply["projects"]).map(|p| format!("{p}\n")).unwrap_or_default();
            text.push_str(&match (tasks.len(), truncated) {
                (0, _) => "No tasks in scope.".to_string(),
                (n, false) => format!("{n} task{}:", if n == 1 { "" } else { "s" }),
                (n, true) => format!("{n} most recent tasks (more exist; narrow with project):"),
            });
            for task in tasks {
                text.push('\n');
                text.push_str(&line(task, true));
            }
            let ids: Vec<&Value> = tasks.iter().map(|t| &t["id"]).collect();
            let project_ids: Vec<&Value> = reply["projects"]
                .as_array()
                .map(Vec::as_slice)
                .unwrap_or_default()
                .iter()
                .take(MAX_PROJECTS)
                .map(|p| &p["id"])
                .collect();
            (
                text,
                json!({ "operation": operation, "count": tasks.len(), "truncated": truncated, "ids": ids, "projects": project_ids, "changed": false }),
            )
        }
        "task" => (detail(&reply["task"]), read(operation, reply)),
        "wait" => {
            let ms = body["wait_ms"].as_u64().unwrap_or(0);
            let head = if reply["timed_out"].as_bool().unwrap_or(false) {
                format!("Still going after {ms} ms; wait again or do something else.")
            } else {
                "Woke:".to_string()
            };
            (format!("{head}\n{}", detail(&reply["task"])), read(operation, reply))
        }
        "create" => {
            let id = reply["id"].as_str().unwrap_or_default();
            let place = body["project"].as_str().map(|p| format!(" in {p}")).unwrap_or_default();
            let text = format!(
                "Created task {id}{place}; its brief is its first prompt. Call deck wait with id {id} to see that turn end."
            );
            (text, json!({ "operation": operation, "id": id }))
        }
        INTEGRATE => {
            let id = reply["id"].as_str().unwrap_or_default();
            let text =
                format!("Integration task {id}. Call deck wait with its id to see its progress or waiting reason.");
            (text, json!({ "operation": operation, "id": id }))
        }
        "prompt" => {
            let turn = reply["turn"].as_u64().unwrap_or(0);
            let text = format!(
                "Task {id} began turn {turn}. Call deck wait with id {id} and after_turn {} to see it end.",
                turn.saturating_sub(1)
            );
            (text, json!({ "operation": operation, "id": id, "turn": turn }))
        }
        _ => {
            let state = reply["state"].as_str().unwrap_or("unknown");
            (format!("Task {id} is {state}."), json!({ "operation": operation, "id": id, "state": state }))
        }
    }
}

/// `Your projects: app (git:…), …`, or `None` when the reply names none
/// (a Deck older than 1.5.0).
fn projects(list: &Value) -> Option<String> {
    let list = list.as_array().filter(|l| !l.is_empty())?;
    let mut names: Vec<String> = list
        .iter()
        .take(MAX_PROJECTS)
        .map(|p| {
            let id = p["id"].as_str().unwrap_or("?");
            match p["name"].as_str().filter(|n| !n.is_empty() && *n != id) {
                Some(name) => format!("{} ({id})", crate::llm::truncate(name, 64)),
                None => id.to_string(),
            }
        })
        .collect();
    if list.len() > MAX_PROJECTS {
        names.push(format!("and {} more", list.len() - MAX_PROJECTS));
    }
    let hint = if list.len() == 1 { " (create uses it when you leave project out)" } else { "" };
    Some(format!("Your projects: {}{hint}", names.join(", ")))
}

fn read(operation: &str, reply: &Value) -> Value {
    let mut data = reply.clone();
    data["operation"] = json!(operation);
    data["changed"] = json!(false);
    data
}

/// One task: its line, then its whole last message (Deck caps it at 2048 bytes).
fn detail(task: &Value) -> String {
    let mut out = line(task, false);
    let integration = &task["integration"];
    let worktree = &task["worktree"];
    if integration.is_object() {
        out.push_str(&integration_detail(integration));
    } else if let Some(commit) = worktree["base_commit"].as_str() {
        out.push_str(&format!("\nStarting commit: {}", crate::llm::truncate(commit, 64)));
    }
    if let Some(target) = worktree["target_ref"].as_str().filter(|_| !integration.is_object()) {
        out.push_str(&format!("\nTarget ref: {}", crate::llm::truncate(target, 200)));
    }
    if let Some(commit) = worktree["review_commit"].as_str() {
        out.push_str(&format!("\nReview commit: {}", crate::llm::truncate(commit, 64)));
    }
    if let Some(message) = task["last_message"].as_str().filter(|m| !m.is_empty()) {
        let cut = if task["last_message_truncated"].as_bool().unwrap_or(false) { " (cut by Deck)" } else { "" };
        out.push_str(&format!("\nLast message{cut}:\n{message}"));
    }
    out
}

fn integration_detail(integration: &Value) -> String {
    let mut text = String::new();
    let repo = integration["repository"].as_str().unwrap_or("?");
    let destination = &integration["destination"];
    let place = match destination["kind"].as_str() {
        Some("local_branch") => format!("local branch {}", destination["branch"].as_str().unwrap_or("?")),
        Some("new_pull_request") => {
            format!("new draft PR to {repo}:{}", integration["base_branch"].as_str().unwrap_or("?"))
        }
        Some("existing_pull_request") => {
            format!("existing PR {repo}#{}", destination["number"].as_u64().unwrap_or_default())
        }
        _ => "unknown".to_string(),
    };
    text.push_str(&format!("\nDestination: {}", crate::llm::truncate(&place, 480)));
    if let Some(branch) = integration["branch"].as_str() {
        text.push_str(&format!("\nReview branch: {}", crate::llm::truncate(branch, 200)));
    }
    if let Some(commit) = integration["base_commit"].as_str() {
        text.push_str(&format!("\nStarting commit: {}", crate::llm::truncate(commit, 64)));
    }
    if let Some(contributors) = integration["contributors"].as_array() {
        text.push_str("\nContributors in merge order:");
        for contributor in contributors.iter().take(MAX_CONTRIBUTORS) {
            let id = contributor["id"].as_str().unwrap_or("?");
            let commit = contributor["commit"].as_str().unwrap_or("waiting for commit");
            text.push_str(&format!("\n- {}: {}", crate::llm::truncate(id, 128), crate::llm::truncate(commit, 64)));
        }
    }
    if let Some(reason) = integration["reason"].as_str() {
        text.push_str(&format!("\nWaiting for input: {}", crate::llm::truncate(reason, MAX_INTEGRATION_REASON)));
    }
    text
}

/// One line for a task summary; `message` adds its last message, capped.
pub fn line(task: &Value, message: bool) -> String {
    let s = |k: &str| task[k].as_str();
    let mut out = format!("{} [{}] {}", s("id").unwrap_or("?"), s("project").unwrap_or("?"), s("state").unwrap_or("?"));
    let turn = &task["turn"];
    match turn["number"].as_u64() {
        Some(n) => {
            out.push_str(&format!(", turn {n} {}", turn["state"].as_str().unwrap_or("?")));
            if let Some(reason) = turn["stopReason"].as_str() {
                out.push_str(&format!(" ({reason})"));
            }
        }
        None => out.push_str(", no turn yet"),
    }
    if let Some(title) = s("title") {
        out.push_str(&format!(" \"{}\"", crate::llm::truncate(title, 80)));
    }
    match s("attention") {
        Some(known @ ("approval" | "question" | "finished" | "failed")) => {
            out.push_str(&format!("; needs you: {known}"))
        }
        Some("integration_input") => out.push_str(". Integration needs input"),
        Some(other) => out.push_str(&format!("; needs a person: {}", crate::llm::truncate(other, 64))),
        None => {}
    }
    if let Some(n) = task["pending_approvals"].as_u64().filter(|n| *n > 0) {
        out.push_str(&format!("; {n} approval{} pending with the user", if n == 1 { "" } else { "s" }));
    }
    let pr = &task["pull_request"];
    let publication = &task["publication"];
    if let Some(n) = pr["number"].as_u64() {
        let state = match (pr["merged"].as_bool(), pr["draft"].as_bool()) {
            (Some(true), _) => "merged",
            (_, Some(true)) => "draft",
            _ => pr["state"].as_str().unwrap_or("?"),
        };
        out.push_str(&format!("; PR #{n} {state}"));
    } else if publication["kind"] == "pull_request" {
        out.push_str(&format!("; PR #{}", publication["number"]));
    } else if publication["kind"] == "branch" {
        let commit = publication["commit"].as_str().unwrap_or_default();
        let branch = publication["branch"].as_str().unwrap_or("?");
        out.push_str(&format!("; published {branch}@{}", crate::text::head(commit, 12)));
    }
    if let Some(phase) = task["integration"]["phase"].as_str() {
        out.push_str(&format!(". Integration: {}", crate::llm::truncate(phase, 32)));
    } else if let Some(branch) = task["worktree"]["branch"].as_str() {
        out.push_str(&format!(". Local branch {}", crate::llm::truncate(branch, 200)));
    }
    if message && let Some(last) = s("last_message").filter(|m| !m.is_empty()) {
        let flat = last.split_whitespace().collect::<Vec<_>>().join(" ");
        out.push_str(&format!(" | last: {}", crate::llm::truncate(&flat, LINE_MESSAGE)));
    }
    out
}
