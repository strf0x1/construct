//! Push transport runs in a private bare context with frozen host-only
//! authentication config; repository helpers and URL rewrites never execute.

use super::super::runner::{self, Executable, Scratch};
use super::{git, options};
use crate::tools::{Ctx, ToolError};
use std::path::{Path, PathBuf};

#[derive(serde::Serialize)]
pub struct Push {
    pub url: String,
    pub branch: String,
    pub oid: String,
    pub(super) objects: PathBuf,
    pub(super) host_config: String,
}

fn quoted(value: &str) -> String {
    format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n").replace('\t', "\\t").replace('\r', "\\r")
    )
}

pub(super) async fn host_config(ctx: &Ctx, exe: &Executable, cwd: &Path) -> Result<String, ToolError> {
    // Evaluate trusted includeIf rules for this repository, then discard all
    // repository/worktree values, including files they include themselves.
    let raw = git(ctx, exe, cwd, &["config", "--null", "--list", "--show-scope"]).await?;
    let fields: Vec<_> = raw.split('\0').collect();
    let mut config = String::new();
    for pair in fields.as_chunks::<2>().0 {
        if matches!(pair[0], "local" | "worktree" | "command") {
            continue;
        }
        let Some((key, value)) = pair[1].split_once('\n') else { continue };
        if !(key.starts_with("credential.") || key.starts_with("http.") || key == "core.sshcommand") {
            continue;
        }
        let Some((section, rest)) = key.split_once('.') else { continue };
        let (header, name) = match rest.rsplit_once('.') {
            Some((subsection, name)) => (format!("[{section} {}]", quoted(subsection)), name),
            None => (format!("[{section}]"), rest),
        };
        if !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            continue;
        }
        config.push_str(&format!("{header}\n{name} = {}\n", quoted(value)));
    }
    Ok(config)
}

pub(super) fn destination(url: &str) -> Result<(), ToolError> {
    if url.starts_with("https://") || url.starts_with("ssh://") {
        let parsed = reqwest::Url::parse(url).map_err(|_| "invalid remote URL")?;
        if parsed.host_str().is_none()
            || parsed.password().is_some()
            || (parsed.scheme() == "https" && !parsed.username().is_empty())
            || parsed.query().is_some()
            || parsed.fragment().is_some()
        {
            return Err("remote URL must not contain credentials, query parameters, or fragments".into());
        }
        return Ok(());
    }
    // SCP-style SSH URLs are common. Reject remote helper syntax and option
    // forms; the part after ':' is a repository path, never a command.
    if !url.contains("://")
        && !url.contains("::")
        && !url.starts_with('-')
        && !url.contains(char::is_whitespace)
        && let Some((host, path)) = url.split_once(':')
        && !host.is_empty()
        && !path.is_empty()
        && !host.contains('/')
        && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '-' | '_'))
    {
        return Ok(());
    }
    Err("only explicit HTTPS and SSH push destinations are supported; local transports and remote helpers are refused"
        .into())
}

pub async fn push(ctx: &Ctx, exe: &Executable, _cwd: &Path, plan: &Push) -> Result<String, ToolError> {
    let scratch = Scratch::new(ctx)?;
    // A hand-built bare context avoids templates, init hooks and repository
    // config. Only host global config supplies transport authentication.
    std::fs::create_dir(scratch.0.join("objects")).map_err(|_| "cannot prepare push objects")?;
    std::fs::create_dir_all(scratch.0.join("refs/heads")).map_err(|_| "cannot prepare push refs")?;
    std::fs::write(scratch.0.join("HEAD"), "ref: refs/heads/construct\n").map_err(|_| "cannot prepare push HEAD")?;
    let config = if plan.oid.len() == 64 {
        "[core]\nrepositoryformatversion = 1\nbare = true\n[extensions]\nobjectFormat = sha256\n"
    } else {
        "[core]\nrepositoryformatversion = 0\nbare = true\n"
    };
    std::fs::write(scratch.0.join("config"), config).map_err(|_| "cannot prepare push config")?;
    let host_config = scratch.0.join("host-config");
    std::fs::write(&host_config, &plan.host_config).map_err(|_| "cannot freeze host transport configuration")?;
    let mut args = options();
    args.extend(runner::args(&[
        "-c",
        "protocol.allow=never",
        "-c",
        "protocol.https.allow=always",
        "-c",
        "protocol.ssh.allow=always",
        "push",
        "--no-verify",
        "--porcelain",
    ]));
    args.extend(runner::args(&["--", &plan.url, &format!("{}:refs/heads/{}", plan.oid, plan.branch)]));
    let env = [
        ("GIT_DIR", scratch.0.clone().into_os_string()),
        ("GIT_ALTERNATE_OBJECT_DIRECTORIES", plan.objects.clone().into_os_string()),
        ("GIT_CONFIG_GLOBAL", host_config.into_os_string()),
        ("GIT_CONFIG_NOSYSTEM", "1".into()),
    ];
    runner::run(ctx, exe, &scratch.0, &args, &[], &env).await?;
    Ok(format!("Pushed {} to {} refs/heads/{}", plan.oid, plan.url, plan.branch))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn push_destinations_exclude_helpers_and_secrets() {
        for url in [
            "https://github.com/owner/repo.git",
            "git@github.com:owner/repo.git",
            "ssh://git@github.com/owner/repo.git",
        ] {
            assert!(destination(url).is_ok(), "{url}");
        }
        for url in [
            "ext::sh -c evil",
            "file:///tmp/repo",
            "/tmp/repo",
            "https://token@github.com/o/r",
            "https://x:secret@github.com/o/r",
            "https://github.com/o/r?token=secret",
            "--upload-pack=evil",
        ] {
            assert!(destination(url).is_err(), "{url}");
        }
    }
}
