//! `push` through Deck's gateway: the local task branch as one git bundle.
//!
//! The design choice: the bundle is incremental from the tip `refs` reports
//! whenever that tip is in the local history, so a push carries only new
//! commits, and a branch that no longer contains the tip is refused here with
//! the commit to rebase onto, before anything is sent. The gateway checks
//! the same rules again; this side only saves a doomed upload.

use super::super::git::{git, oid};
use super::super::runner::{self, Executable};
use super::client::MAX_BUNDLE;
use crate::tools::{Ctx, ToolError};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};

/// What a push sends.
pub struct Bundle {
    pub branch: String,
    pub commit: String,
    pub head: Value,
    pub bytes: Vec<u8>,
}

/// The longest summary subject the contract accepts, in characters.
const MAX_SUBJECT: usize = 256;

/// The repository a push reads, resolved the way `commit` resolves it.
async fn repository(ctx: &Ctx, exe: &Executable, args: &Value) -> Result<PathBuf, ToolError> {
    let raw = args.get("workdir").and_then(Value::as_str).unwrap_or(".");
    let cwd = crate::tools::fs::resolve(ctx, raw).canonicalize().map_err(|_| "workdir must exist")?;
    ctx.policy().check_read(&cwd).map_err(ToolError::sandbox)?;
    let root = git(ctx, exe, &cwd, &["rev-parse", "--show-toplevel"]).await?;
    std::fs::canonicalize(root).map_err(|_| "the repository root is not accessible".into())
}

/// Whether `ancestor` is in `descendant`'s history (`merge-base` exits 1 when
/// it is not).
async fn contains(
    ctx: &Ctx,
    exe: &Executable,
    cwd: &Path,
    ancestor: &str,
    descendant: &str,
) -> Result<bool, ToolError> {
    match git(ctx, exe, cwd, &["merge-base", "--is-ancestor", ancestor, descendant]).await {
        Ok(_) => Ok(true),
        Err(error) if error.data.as_ref().and_then(|d| d["exit_code"].as_i64()) == Some(1) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Build the bundle for `branch`, or `None` when the remote already has it.
pub async fn bundle(ctx: &Ctx, args: &Value, refs: &Value, branch: &str) -> Result<Option<Bundle>, ToolError> {
    let exe = Executable::find("git", ctx)?;
    let cwd = repository(ctx, &exe, args).await?;
    let reference = format!("refs/heads/{branch}");
    let local =
        git(ctx, &exe, &cwd, &["rev-parse", "--verify", &format!("{reference}^{{commit}}")]).await.map_err(|_| {
            ToolError::new(
                "invalid_argument",
                format!("there is no local branch {branch}; commit on it first (git switch -c {branch})"),
            )
        })?;
    let local = oid(&local)?;
    let tip = refs["branches"]
        .as_array()
        .and_then(|all| all.iter().find(|b| b["branch"] == branch))
        .and_then(|b| b["commit"].as_str())
        .map(str::to_string);
    if tip.as_deref() == Some(local.as_str()) {
        return Ok(None);
    }
    let known = match &tip {
        Some(tip) => git(ctx, &exe, &cwd, &["cat-file", "-e", &format!("{tip}^{{commit}}")]).await.is_ok(),
        None => false,
    };
    let since = match (&tip, known) {
        (Some(tip), true) if contains(ctx, &exe, &cwd, tip, &local).await? => Some(tip.clone()),
        (Some(tip), true) => {
            return Err(ToolError::new(
                "rebase",
                format!(
                    "{branch} on GitHub is at {tip}, which is not in your branch: rebase onto {tip} and push again"
                ),
            ));
        }
        // An unknown tip: send the whole branch and let the gateway decide.
        _ => None,
    };
    let range = since.as_ref().map_or(local.clone(), |tip| format!("{tip}..{local}"));
    let count: u64 = git(ctx, &exe, &cwd, &["rev-list", "--count", &range]).await?.parse().unwrap_or(1);
    let subject: String =
        git(ctx, &exe, &cwd, &["log", "-1", "--format=%s", &local]).await?.chars().take(MAX_SUBJECT).collect();
    let revs = since.map_or(reference.clone(), |tip| format!("^{tip} {reference}"));
    let mut command = super::super::git::options();
    command.extend(runner::args(&["bundle", "create", "--quiet", "-"]));
    command.extend(runner::args(&revs.split(' ').collect::<Vec<_>>()));
    let bytes = runner::run_limited(ctx, &exe, &cwd, &command, &[], &[], MAX_BUNDLE + 1).await?;
    if bytes.len() > MAX_BUNDLE {
        return Err(ToolError::new("too_large", format!("the bundle for {branch} exceeds {MAX_BUNDLE} bytes")));
    }
    let head = json!({
        "operation": "publish", "branch": branch,
        "summary": { "subject": subject, "count": count.max(1) },
    });
    Ok(Some(Bundle { branch: branch.to_string(), commit: local, head, bytes }))
}
