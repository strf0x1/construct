//! GitHub writes use fixed REST endpoints with JSON stdin. Creating a pull
//! request this way cannot implicitly push a local branch.

use super::prepare::{Body, Prepared, required};
use super::runner::Executable;
use crate::permission::OperationAsk;
use crate::tools::{Ctx, ToolError};
use serde_json::{Value, json};

pub(super) fn repository(repo: &str) -> Result<String, ToolError> {
    let parts: Vec<_> = repo.split('/').collect();
    if parts.len() != 2
        || parts.iter().any(|s| {
            s.is_empty()
                || *s == "."
                || *s == ".."
                || !s.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        })
    {
        return Err(ToolError::new("invalid_argument", "repo must be an explicit OWNER/REPOSITORY on github.com"));
    }
    Ok(repo.to_ascii_lowercase())
}

pub fn prepare(ctx: &Ctx, args: &Value) -> Result<Prepared, ToolError> {
    let operation = required(args, "operation")?;
    let repo = repository(required(args, "repo")?)?;
    let title = required(args, "title")?;
    let body = args
        .get("body")
        .and_then(Value::as_str)
        .filter(|s| !s.contains('\0'))
        .ok_or("body must be an explicit string without NUL bytes")?;
    if title.len() + body.len() > 1024 * 1024 {
        return Err("title and body exceed the 1 MiB review limit".into());
    }
    let mut input = json!({"title": title, "body": body});
    let mut detail = format!("Repository: github.com/{repo}");
    let endpoint = if operation == "issue_create" {
        format!("repos/{repo}/issues")
    } else {
        let base = required(args, "base")?;
        let head = required(args, "head")?;
        input["base"] = json!(base);
        input["head"] = json!(head);
        detail.push_str(&format!("\nBase: {base}\nHead: {head}\nNo branch will be pushed."));
        format!("repos/{repo}/pulls")
    };
    detail.push_str(&format!("\nTitle: {title}\nBody:\n{body}"));
    let executable = Executable::find("gh", ctx)?;
    let cwd = super::runner::private_root(ctx)?;
    detail.push_str(&format!("\nHost executable: {}\nWorking directory: {}\nCommand: api --hostname github.com --method POST {endpoint} --input -", executable.identity(), cwd.display()));
    Ok(Prepared {
        request: OperationAsk {
            agent: String::new(),
            operation: operation.into(),
            scope: format!("github.com/{repo}"),
            detail,
        },
        executable,
        cwd,
        operation: Body::Github {
            endpoint,
            input: serde_json::to_vec(&input).map_err(|_| "cannot encode GitHub operation")?,
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_identity_is_case_insensitive_and_excludes_paths_or_urls() {
        assert_eq!(repository("Owner/Repository").unwrap(), "owner/repository");
        for invalid in ["owner", "owner/repo/extra", "https://github.com/owner/repo", "../repo", "owner/.."] {
            assert!(repository(invalid).is_err(), "{invalid}");
        }
    }
}
