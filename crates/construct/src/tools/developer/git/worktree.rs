//! Worktree-local status, staging and merge start. Each operation uses fixed
//! Git arguments and the native worktree index. Commit approval stays separate.

mod scoped;

use crate::tools::{Ctx, Output, ToolError};
use scoped::Checkout;
use serde::Deserialize;
use serde_json::{Value, json};

pub(in crate::tools::developer) const STATUS: &str = "status";
pub(in crate::tools::developer) const STAGE: &str = "stage";
pub(in crate::tools::developer) const MERGE_START: &str = "merge_start";

pub(in crate::tools::developer) fn is_operation(name: &str) -> bool {
    matches!(name, STATUS | STAGE | MERGE_START)
}

#[derive(Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Status {},
    Stage { paths: Vec<String> },
    MergeStart { commit: String },
}

pub(in crate::tools::developer) async fn call(ctx: &Ctx, args: Value) -> Result<Output, ToolError> {
    let request: Request =
        serde_json::from_value(args).map_err(|error| ToolError::new("invalid_argument", error.to_string()))?;
    let checkout = Checkout::open(ctx, !matches!(request, Request::Status {})).await?;
    match request {
        Request::Status {} => {
            let status = checkout.git(ctx, &["status", "--porcelain=v1", "--untracked-files=all"], &[]).await?;
            Ok(Output::plain(if status.trim().is_empty() { "Working tree clean.".into() } else { status }))
        }
        Request::Stage { paths } => stage(ctx, &checkout, &paths).await,
        Request::MergeStart { commit } => merge_start(ctx, &checkout, &commit).await,
    }
}

fn file_paths(checkout: &Checkout, paths: &[String]) -> Result<Vec<u8>, ToolError> {
    if paths.is_empty() || paths.len() > 64 {
        return Err("stage needs between 1 and 64 file paths".into());
    }
    let mut input = Vec::new();
    for path in paths {
        if path.len() > 4096
            || path.contains('\0')
            || path
                .split('/')
                .any(|part| part.is_empty() || matches!(part, "." | "..") || part.eq_ignore_ascii_case(".git"))
            || checkout.root.join(path).symlink_metadata().is_ok_and(|entry| entry.is_dir())
        {
            return Err("stage takes exact relative file paths inside this checkout".into());
        }
        input.extend_from_slice(path.as_bytes());
        input.push(0);
    }
    Ok(input)
}

async fn stage(ctx: &Ctx, checkout: &Checkout, paths: &[String]) -> Result<Output, ToolError> {
    let input = file_paths(checkout, paths)?;
    let attributes = checkout.git(ctx, &["check-attr", "-z", "--stdin", "filter"], &input).await?;
    if attributes.split_terminator('\0').skip(2).step_by(3).any(|value| !matches!(value, "unspecified" | "unset")) {
        return Err("A selected file uses a Git filter. Use Git directly to stage it.".into());
    }
    checkout.git(ctx, &["--literal-pathspecs", "add", "--pathspec-from-file=-", "--pathspec-file-nul"], &input).await?;
    Ok(Output::plain(format!("Staged {} paths.", paths.len())))
}

async fn merge_start(ctx: &Ctx, checkout: &Checkout, commit: &str) -> Result<Output, ToolError> {
    if commit.len() != checkout.oid_len
        || !commit.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || checkout.git(ctx, &["cat-file", "-t", commit], &[]).await?.trim() != "commit"
    {
        return Err("merge_start needs the full object ID of an available commit".into());
    }
    if ["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD", "rebase-apply", "rebase-merge", "sequencer"]
        .iter()
        .any(|name| checkout.git_dir.join(name).symlink_metadata().is_ok())
    {
        return Err("Finish the current Git operation before starting a merge.".into());
    }
    let status = checkout.git(ctx, &["status", "--porcelain=v1", "--untracked-files=no"], &[]).await?;
    if !status.is_empty() {
        return Err("Start the merge with a clean index and clean tracked files.".into());
    }
    let head = checkout.git(ctx, &["rev-parse", "--verify", "HEAD"], &[]).await?;
    let result = checkout
        .git(ctx, &["merge", "--no-ff", "--no-commit", "--no-edit", "--no-autostash", "--no-stat", "--", commit], &[])
        .await;
    let parents = checkout.git(ctx, &["rev-parse", "--verify", "-q", "MERGE_HEAD"], &[]).await;
    let merging = parents.is_ok_and(|value| value.trim() == commit);
    if merging
        && (checkout.git(ctx, &["rev-parse", "--verify", "ORIG_HEAD"], &[]).await? != head
            || checkout.git(ctx, &["rev-parse", "--verify", "HEAD"], &[]).await? != head)
    {
        return Err(
            "The branch moved while starting the merge. Review the index and merge state before continuing.".into()
        );
    }
    let (state, message) = match result {
        Ok(_) if merging => ("ready_to_commit", "Merge staged. Use developer commit to finish it."),
        Ok(_) => ("already_included", "This commit is already included in HEAD."),
        Err(error) if merging && error.data.as_ref().and_then(|data| data["exit_code"].as_i64()) == Some(1) => {
            let unmerged = checkout.git(ctx, &["ls-files", "--unmerged"], &[]).await?;
            if unmerged.is_empty() {
                return Err(error);
            }
            ("conflicts", "Merge has conflicts. Inspect status, resolve the files, then stage and commit.")
        }
        Err(error) => return Err(error),
    };
    Ok(Output::plain(message).with_data(json!({"state":state,"commit":commit})))
}
