//! The `developer` tool in a Deck VM task with a repository: GitHub
//! operations go to Deck's gateway (`DECK_GITHUB_URL`, contract 1.2.0 §11,
//! Deck ADR 0008 §5), and never to host `git push` or `gh`, which have no
//! credential and no route there.
//!
//! The design choice: the gateway is the authority on where a task may
//! publish, so every write starts from `refs`, which names the repository,
//! the base, the task branch and the writable branches, and the tool fills
//! its defaults from that answer rather than from anything in the guest.
//! In Deck mode (`DECK_MANAGED`) Construct asks no approval of its own for
//! these writes: Deck's gateway decides each one, and it cannot be skipped.

mod client;
mod push;

use super::prepare::required;
use crate::permission::OperationAsk;
use crate::tools::{Ctx, Output, ToolError};
use client::{CREATE_PR, Gateway, REFS, REVIEW, UPDATE_PR};
use serde_json::{Value, json};

/// The operations the tool offers in a gateway task.
pub const OPERATIONS: &[&str] = &["commit", "push", "pr_create", "pr_update", "pr_review", "issue_create"];

pub const DESCRIPTION: &str = "Commit locally and publish this task's work through Deck's GitHub gateway. In this sandbox `git push` and `gh` fail by design: this tool is the only route to GitHub, so never look for another remote or credential. commit makes an unsigned local commit of what you staged with shell (hooks skipped). push publishes your local task branch (the gateway names it; default) as new commits only: if it reports the branch moved, rebase onto the commit it names and push again. pr_create opens a pull request from the task branch to the project's base, as a draft unless draft is false. pr_update changes the title, body or base, or marks it ready with draft false. pr_review reads its state, mergeability and checks. issue_create is not available here.";

fn allowed(operation: &str) -> &'static [&'static str] {
    match operation {
        "push" => &["operation", "workdir", "branch", "remote"],
        "pr_create" => &["operation", "repo", "title", "body", "base", "head", "draft"],
        "pr_update" => &["operation", "number", "title", "body", "base", "draft"],
        "pr_review" => &["operation", "number"],
        _ => &[],
    }
}

fn text<'a>(args: &'a Value, name: &str) -> Option<&'a str> {
    args.get(name).and_then(Value::as_str)
}

fn number(args: &Value) -> Result<u64, ToolError> {
    args.get("number")
        .and_then(Value::as_u64)
        .filter(|n| *n > 0)
        .ok_or_else(|| ToolError::new("invalid_argument", "number must be the pull request number"))
}

/// Run one non-commit operation through the gateway at `url`.
pub async fn call(ctx: &Ctx, url: &str, args: Value) -> Result<Output, ToolError> {
    let operation = required(&args, "operation")?.to_string();
    if operation == "issue_create" {
        return Err(ToolError::new(
            "not_available",
            "issue_create is not available in a Deck task: Deck's GitHub gateway publishes this task's branch and pull request only. Mention the issue in the pull request body or tell the user.",
        ));
    }
    if allowed(&operation).is_empty() {
        return Err(ToolError::new("invalid_argument", format!("operation must be one of {}", OPERATIONS.join(", "))));
    }
    if args.as_object().is_none_or(|map| map.keys().any(|key| !allowed(&operation).contains(&key.as_str()))) {
        return Err("unexpected developer-operation argument".into());
    }
    let gateway = Gateway::new(url)?;
    if operation == "pr_review" {
        let reply = gateway.call(REVIEW, &json!({ "operation": "review", "number": number(&args)? })).await?;
        return Ok(Output::plain(review_text(&reply)).with_data(reply));
    }
    let refs = gateway.call(REFS, &json!({ "operation": "refs" })).await?;
    let repository = refs["repository"].as_str().unwrap_or_default().to_string();
    let task_branch = refs["task_branch"].as_str().unwrap_or_default().to_string();
    let writable: Vec<&str> =
        refs["writable"].as_array().map(|w| w.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    let (path, body, scope) = match operation.as_str() {
        "push" => {
            let branch = text(&args, "branch").unwrap_or(&task_branch).to_string();
            if !writable.contains(&branch.as_str()) {
                return Err(ToolError::new(
                    "invalid_argument",
                    format!("this task may push only to {}", writable.join(", ")),
                ));
            }
            let Some(bundle) = push::bundle(ctx, &args, &refs, &branch).await? else {
                return Ok(Output::plain(format!("{repository}#{branch} already has your branch; nothing to push"))
                    .with_data(json!({ "operation": "push", "branch": branch, "published": false })));
            };
            approve(ctx, "push", &repository, &format!("Publish {} to {repository}#{branch}", bundle.commit)).await?;
            let reply = gateway.publish(&bundle.head, &bundle.bytes).await?;
            let how = match reply["previous"].as_str() {
                Some(previous) => format!("fast-forwarded from {previous}"),
                None => "created".into(),
            };
            let text = format!("Published {} to {repository}#{} ({how})", bundle.commit, bundle.branch);
            return Ok(Output::plain(text).with_data(reply));
        }
        "pr_create" => {
            if let Some(repo) = text(&args, "repo")
                && !repo.eq_ignore_ascii_case(&repository)
            {
                return Err(ToolError::new(
                    "invalid_argument",
                    format!("this task opens pull requests on {repository} only; omit repo"),
                ));
            }
            let mut body = json!({
                "operation": "create_pr",
                "head": text(&args, "head").unwrap_or(&task_branch),
                "base": text(&args, "base").or(refs["base"].as_str()).unwrap_or_default(),
                "title": required(&args, "title")?,
                "draft": args.get("draft").and_then(Value::as_bool).unwrap_or(true),
            });
            if let Some(text) = text(&args, "body") {
                body["body"] = json!(text);
            }
            (CREATE_PR, body, "open a pull request")
        }
        _ => {
            let mut body = json!({ "operation": "update_pr", "number": number(&args)? });
            for field in ["title", "body", "base"] {
                if let Some(value) = text(&args, field) {
                    body[field] = json!(value);
                }
            }
            if let Some(draft) = args.get("draft").and_then(Value::as_bool) {
                body["draft"] = json!(draft);
            }
            (UPDATE_PR, body, "update a pull request")
        }
    };
    approve(ctx, &operation, &repository, &format!("{scope} on {repository}: {body}")).await?;
    let reply = gateway.call(path, &body).await?;
    Ok(Output::plain(pull_request_text(&reply)).with_data(reply))
}

/// Construct's own approval, outside Deck mode only: in Deck mode the
/// gateway's decision is the one that holds (Deck ADR 0008 §5).
async fn approve(ctx: &Ctx, operation: &str, repository: &str, detail: &str) -> Result<(), ToolError> {
    if crate::secrets::managed() {
        return Ok(());
    }
    let request = OperationAsk {
        agent: String::new(),
        operation: operation.into(),
        scope: format!("deck-gateway/{repository}"),
        detail: format!("{detail}\nThrough Deck's GitHub gateway; it decides again."),
    };
    if ctx.perms.operation(request, ctx.cancel.clone()).await {
        Ok(())
    } else {
        Err(ToolError::new("denied", "Developer operation was not approved. The requested action was not executed."))
    }
}

fn pull_request_text(reply: &Value) -> String {
    let state = match (reply["merged"].as_bool(), reply["draft"].as_bool()) {
        (Some(true), _) => "merged",
        (_, Some(true)) => "draft",
        _ => reply["state"].as_str().unwrap_or("open"),
    };
    format!(
        "Pull request #{} \"{}\" ({state}, {} -> {}): {}",
        reply["number"],
        reply["title"].as_str().unwrap_or_default(),
        reply["head"].as_str().unwrap_or_default(),
        reply["base"].as_str().unwrap_or_default(),
        reply["html_url"].as_str().unwrap_or_default()
    )
}

fn review_text(reply: &Value) -> String {
    let mut out = pull_request_text(reply);
    let mergeable = match reply["mergeable"].as_bool() {
        Some(true) => "mergeable",
        Some(false) => "not mergeable",
        None => "mergeability not computed yet",
    };
    out.push_str(&format!("\nHead {}; {mergeable}.", reply["head_commit"].as_str().unwrap_or("unknown")));
    match reply["checks"]["runs"].as_array() {
        Some(runs) if !runs.is_empty() => {
            out.push_str(&format!("\nChecks ({} total):", reply["checks"]["total"]));
            for run in runs {
                let verdict = run["conclusion"].as_str().unwrap_or(run["status"].as_str().unwrap_or("unknown"));
                out.push_str(&format!("\n- {}: {verdict}", run["name"].as_str().unwrap_or("check")));
            }
        }
        _ => out.push_str("\nNo checks reported."),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_review_reads_as_state_mergeability_and_each_check() {
        let reply = json!({
            "number": 7, "title": "Add b", "state": "open", "draft": true, "merged": false,
            "head": "deck/t-1", "base": "main", "head_commit": "a".repeat(40), "mergeable": null,
            "html_url": "https://github.com/o/r/pull/7",
            "checks": { "total": 2, "runs": [
                { "name": "ci", "status": "completed", "conclusion": "failure" },
                { "name": "macos", "status": "in_progress", "conclusion": null }
            ] }
        });
        let text = review_text(&reply);
        assert!(text.starts_with("Pull request #7 \"Add b\" (draft, deck/t-1 -> main)"), "{text}");
        assert!(text.contains("mergeability not computed yet"), "{text}");
        assert!(text.contains("- ci: failure") && text.contains("- macos: in_progress"), "{text}");
        let merged = json!({ "number": 7, "merged": true, "draft": false, "state": "closed", "checks": null });
        assert!(review_text(&merged).contains("(merged,") && review_text(&merged).contains("No checks"));
    }

    #[test]
    fn each_operation_takes_its_own_arguments() {
        assert!(allowed("push").contains(&"remote"), "remote is accepted and ignored (ADR 0008 §5)");
        assert!(!allowed("pr_review").contains(&"title"));
        assert!(allowed("issue_create").is_empty() && allowed("merge").is_empty());
    }
}
