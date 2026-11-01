//! Git operations freeze object identities before approval. Commits bypass
//! hooks and signing; pushes run from a private bare repository so workspace
//! configuration cannot choose a host credential helper or command.

use super::prepare::{Body, Prepared, required};
mod context;
#[cfg(test)]
mod tests;
mod transport;

pub use transport::{Push, push};
use transport::{destination, host_config};

use super::runner::{self, Executable};
use crate::permission::OperationAsk;
use crate::tools::{Ctx, ToolError};
use serde_json::Value;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(serde::Serialize)]
pub struct Commit {
    head: Option<String>,
    branch: String,
    tree: String,
    git_dir: String,
    common_dir: String,
    objects: String,
    ref_format: String,
    index: String,
    message: String,
    author: String,
    email: String,
}

pub fn binding(body: &Body) -> Result<Vec<u8>, ToolError> {
    match body {
        Body::Commit(plan) => serde_json::to_vec(plan),
        Body::Push(plan) => serde_json::to_vec(plan),
        Body::Github { .. } => return Err("not a git operation".into()),
    }
    .map_err(|_| "cannot encode git operation".into())
}

fn options() -> Vec<OsString> {
    runner::args(&[
        "--no-optional-locks",
        "-c",
        "core.fsmonitor=false",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "core.attributesFile=/dev/null",
        "-c",
        "commit.gpgsign=false",
        "-c",
        "gc.auto=0",
    ])
}

async fn git(ctx: &Ctx, exe: &Executable, cwd: &Path, args: &[&str]) -> Result<String, ToolError> {
    let mut command = options();
    command.extend(runner::args(args));
    let output = Box::pin(runner::run(ctx, exe, cwd, &command, &[], &[])).await?;
    String::from_utf8(output).map(|s| s.trim_end().to_string()).map_err(|_| "git returned non-UTF-8 metadata".into())
}

fn oid(value: &str) -> Result<String, ToolError> {
    if matches!(value.len(), 40 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(value.into())
    } else {
        Err("git returned an invalid object identity".into())
    }
}

fn canonical(raw: &str) -> Result<PathBuf, ToolError> {
    std::fs::canonicalize(raw).map_err(|_| "git metadata directory is not accessible".into())
}

async fn branch_head(ctx: &Ctx, exe: &Executable, cwd: &Path, branch: &str) -> Result<Option<String>, ToolError> {
    if let Ok(head) = git(ctx, exe, cwd, &["rev-parse", "--verify", &format!("{branch}^{{commit}}")]).await {
        return oid(&head).map(Some);
    }
    match git(ctx, exe, cwd, &["show-ref", "--exists", branch]).await {
        Ok(_) => Err(ToolError::new("invalid_repository", "branch exists but does not resolve to a valid commit")),
        Err(error) if error.data.as_ref().and_then(|data| data["exit_code"].as_i64()) == Some(2) => Ok(None),
        Err(error) => Err(error),
    }
}

async fn ref_format(ctx: &Ctx, exe: &Executable, cwd: &Path) -> Result<String, ToolError> {
    match git(ctx, exe, cwd, &["rev-parse", "--show-ref-format"]).await?.as_str() {
        "files" | "--show-ref-format" => Ok("files".into()),
        "reftable" => Ok("reftable".into()),
        _ => Err("unsupported repository reference format".into()),
    }
}

async fn index_review(
    ctx: &Ctx,
    exe: &Executable,
    cwd: &Path,
    layout: &context::Layout<'_>,
    index: &Path,
) -> Result<context::CommitContext, ToolError> {
    let shared = git(ctx, exe, cwd, &["rev-parse", "--shared-index-path"]).await?;
    let shared = if shared.is_empty() { None } else { Some(cwd.join(shared)) };
    context::CommitContext::index(ctx, layout, index, shared.as_deref())
}

pub async fn prepare(ctx: &Ctx, args: &Value) -> Result<Prepared, ToolError> {
    let raw = match args.get("workdir") {
        None => ".",
        Some(_) => required(args, "workdir")?,
    };
    let cwd = super::super::fs::resolve(ctx, raw).canonicalize().map_err(|_| "workdir must exist")?;
    ctx.policy().check_read(&cwd).map_err(ToolError::sandbox)?;
    let executable = Executable::find("git", ctx)?;
    let root = git(ctx, &executable, &cwd, &["rev-parse", "--show-toplevel"]).await?;
    let cwd = canonical(&root)?;
    ctx.policy().check_read(&cwd).map_err(ToolError::sandbox)?;
    let git_dir = git(ctx, &executable, &cwd, &["rev-parse", "--absolute-git-dir"]).await?;
    let common_dir = git(ctx, &executable, &cwd, &["rev-parse", "--path-format=absolute", "--git-common-dir"]).await?;
    let git_dir = canonical(&git_dir)?;
    let common_dir = canonical(&common_dir)?;
    if [&git_dir, &common_dir]
        .iter()
        .any(|path| crate::permission::protected(path) || path.starts_with(&ctx.paths.home))
    {
        return Err("repository metadata points into a protected directory".into());
    }
    let git_dir = git_dir.display().to_string();
    let common_dir = common_dir.display().to_string();
    let objects = canonical(&format!("{common_dir}/objects"))?;
    if crate::permission::protected(&objects) || objects.starts_with(&ctx.paths.home) {
        return Err("repository objects point into a protected directory".into());
    }
    let operation = required(args, "operation")?;
    let (body, scope, mut detail) = if operation == "commit" {
        let message = required(args, "message")?.to_string();
        if message.len() > 64 * 1024 {
            return Err("commit message exceeds 64 KiB".into());
        }
        let branch = git(ctx, &executable, &cwd, &["symbolic-ref", "-q", "HEAD"]).await?;
        if !branch.starts_with("refs/heads/") {
            return Err("commit requires a checked-out branch".into());
        }
        let head = branch_head(ctx, &executable, &cwd, &branch).await?;
        let ref_format = ref_format(ctx, &executable, &cwd).await?;
        let format = git(ctx, &executable, &cwd, &["rev-parse", "--show-object-format"]).await?;
        let index = canonical(
            &git(ctx, &executable, &cwd, &["rev-parse", "--path-format=absolute", "--git-path", "index"]).await?,
        )?;
        let layout = context::Layout {
            branch: &branch,
            common: &common_dir,
            git_dir: &git_dir,
            objects: &objects,
            refs: &ref_format,
            sha256: format == "sha256",
        };
        let review = index_review(ctx, &executable, &cwd, &layout, &index).await?;
        let tree = oid(&review.git(ctx, &executable, &["write-tree"], &[], &[]).await?)?;
        let previous = match &head {
            Some(parent) => {
                review.git(ctx, &executable, &["rev-parse", &format!("{parent}^{{tree}}")], &[], &[]).await?
            }
            None => review.git(ctx, &executable, &["hash-object", "-w", "-t", "tree", "--stdin"], &[], &[]).await?,
        };
        if tree == previous {
            return Err("there are no staged changes to commit".into());
        }
        let diff = review
            .git(
                ctx,
                &executable,
                &["diff", "--no-ext-diff", "--no-textconv", "--no-color", "--binary", &previous, &tree, "--"],
                &[],
                &[],
            )
            .await?;
        if diff.len() >= 1024 * 1024 {
            return Err("staged diff exceeds the 1 MiB review limit".into());
        }
        let author = git(ctx, &executable, &cwd, &["config", "--get", "user.name"]).await?;
        let email = git(ctx, &executable, &cwd, &["config", "--get", "user.email"]).await?;
        if author.is_empty() || email.is_empty() {
            return Err("host git user.name and user.email are required".into());
        }
        let parent = head.as_deref().unwrap_or("none (initial commit)");
        let detail = format!(
            "Branch: {branch}\nMessage:\n{message}\nStaged diff:\n{diff}\nUnsigned commit; hooks will not run.\nWorktree: {}\nGit directory: {git_dir}\nCommon directory: {common_dir}\nParent: {parent}\nTree: {tree}\nAuthor: {author} <{email}>",
            cwd.display()
        );
        (
            Body::Commit(Commit {
                head,
                branch,
                tree,
                git_dir,
                common_dir,
                objects: objects.display().to_string(),
                ref_format,
                index: index.display().to_string(),
                message,
                author,
                email,
            }),
            cwd.display().to_string(),
            detail,
        )
    } else {
        let remote = required(args, "remote")?;
        if remote.starts_with('-') || remote.contains(char::is_whitespace) {
            return Err("remote must be a configured name".into());
        }
        let branch = required(args, "branch")?.to_string();
        git(ctx, &executable, &cwd, &["check-ref-format", &format!("refs/heads/{branch}")]).await?;
        let oid =
            oid(&git(ctx, &executable, &cwd, &["rev-parse", "--verify", &format!("refs/heads/{branch}^{{commit}}")])
                .await?)?;
        let url = git(ctx, &executable, &cwd, &["remote", "get-url", "--push", "--all", remote]).await?;
        if url.lines().count() != 1 {
            return Err("push requires exactly one configured push URL".into());
        }
        destination(&url)?;
        let host_config = host_config(ctx, &executable, &cwd).await?;
        let scope = format!("{} -> {url} refs/heads/{branch}", cwd.display());
        let detail = format!(
            "Destination: {url}\nBranch: refs/heads/{branch}\nCommit: {oid}\nPush without force; hooks will not run. Host SSH/HTTPS transport and credential helpers may run.\nWorktree: {}\nGit directory: {git_dir}\nCommon directory: {common_dir}\nRemote: {remote}",
            cwd.display()
        );
        (Body::Push(Push { url, branch, oid, objects, host_config }), scope, detail)
    };
    detail.push_str(&format!("\nHost executable: {}", executable.identity()));
    Ok(Prepared {
        request: OperationAsk { agent: String::new(), operation: operation.into(), scope, detail },
        executable,
        cwd,
        operation: body,
    })
}

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
    let review = index_review(ctx, exe, cwd, &layout, Path::new(&plan.index)).await?;
    if review.git(ctx, exe, &["write-tree"], &[], &[]).await? != plan.tree {
        return Err(ToolError::new("changed", "staged index changed while awaiting approval"));
    }
    let shadow = context::CommitContext::new(ctx, plan)?;
    let mut args = vec!["commit-tree", &plan.tree];
    if let Some(parent) = &plan.head {
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
    Ok(format!("Committed {commit} on {}", plan.branch))
}
