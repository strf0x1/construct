//! Install the reviewed tree and parents with an expected-head ref update.
//! Merge cleanup happens only after the integration branch moves.

use super::super::runner::Executable;
use super::{Commit, branch_head, context, git, index_review, merge, oid, ref_format};
use crate::tools::{Ctx, ToolError};
use std::path::Path;

pub async fn commit(ctx: &Ctx, exe: &Executable, cwd: &Path, plan: &Commit) -> Result<String, ToolError> {
    for (args, expected) in [
        (vec!["symbolic-ref", "-q", "HEAD"], &plan.branch),
        (vec!["rev-parse", "--absolute-git-dir"], &plan.git_dir),
        (vec!["rev-parse", "--path-format=absolute", "--git-common-dir"], &plan.common_dir),
    ] {
        if &git(ctx, exe, cwd, &args).await? != expected {
            return Err(ToolError::new(
                "changed",
                "repository changed while approval was pending; prepare and review again",
            ));
        }
    }
    if branch_head(ctx, exe, cwd, &plan.branch).await? != plan.head {
        return Err(ToolError::new("changed", "branch changed while approval was pending; prepare and review again"));
    }
    if ref_format(ctx, exe, cwd).await? != plan.ref_format {
        return Err(ToolError::new("changed", "repository reference format changed while awaiting approval"));
    }
    let layout = context::Layout {
        branch: &plan.branch,
        common: &plan.common_dir,
        git_dir: &plan.git_dir,
        objects: Path::new(&plan.objects),
        refs: &plan.ref_format,
        sha256: plan.tree.len() == 64,
    };
    let index = Path::new(&plan.index);
    let merging = !plan.merge_parents.is_empty();
    let _lock = merge::IndexLock::acquire(Path::new(&plan.git_dir))?;
    check_merge(plan)?;
    let review = index_review(ctx, exe, cwd, &layout, index).await?;
    if review.git(ctx, exe, &["write-tree"], &[], &[]).await? != plan.tree {
        return Err(ToolError::new("changed", "staged index changed while awaiting approval"));
    }
    let shadow = context::CommitContext::new(ctx, plan)?;
    let mut args = vec!["commit-tree", &plan.tree];
    if let Some(parent) = &plan.head {
        args.extend(["-p", parent]);
    }
    for parent in &plan.merge_parents {
        args.extend(["-p", parent]);
    }
    args.extend(["-F", "-"]);
    let env = [
        ("GIT_AUTHOR_NAME", plan.author.clone().into()),
        ("GIT_AUTHOR_EMAIL", plan.email.clone().into()),
        ("GIT_COMMITTER_NAME", plan.author.clone().into()),
        ("GIT_COMMITTER_EMAIL", plan.email.clone().into()),
    ];
    let commit = oid(&shadow.git(ctx, exe, &args, plan.message.as_bytes(), &env).await?)?;
    check_merge(plan)?;
    if merging && plan.ref_format == "reftable" {
        shadow.check_reftable_autostash(ctx, exe, &plan.git_dir).await?;
    }
    let old = plan.head.clone().unwrap_or_else(|| "0".repeat(plan.tree.len()));
    shadow
        .git(
            ctx,
            exe,
            &["update-ref", "--no-deref", "-m", "construct: approved commit", &plan.branch, &commit, &old],
            &[],
            &env,
        )
        .await?;
    let mut result = format!("Committed {commit} on {}", plan.branch);
    if merging && clear_merge(ctx, exe, plan, &shadow).await.is_err() {
        result.push_str(". Git could not clear the merge state. Review it before running git merge --quit.");
    }
    Ok(result)
}

async fn clear_merge(
    ctx: &Ctx,
    exe: &Executable,
    plan: &Commit,
    shadow: &context::CommitContext,
) -> Result<(), ToolError> {
    check_merge(plan)?;
    if plan.ref_format == "reftable" {
        shadow.check_reftable_autostash(ctx, exe, &plan.git_dir).await?;
    }
    shadow.clear_merge(ctx, exe, &plan.git_dir).await?;
    if merge::parents(&plan.git_dir, plan.tree.len())?.is_empty() {
        Ok(())
    } else {
        Err("Git still reports merge parents".into())
    }
}

fn check_merge(plan: &Commit) -> Result<(), ToolError> {
    if merge::parents(&plan.git_dir, plan.tree.len()).is_ok_and(|parents| parents == plan.merge_parents) {
        Ok(())
    } else {
        Err(ToolError::new("changed", "merge state changed while approval was pending. Prepare and review again."))
    }
}
