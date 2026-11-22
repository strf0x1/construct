//! The path questions a tool puts to `Perms`: a file tool wanting a path the
//! sandbox refuses, and a shell command naming some. Split from `perms` only
//! by concern; the state they read and write is `Perms`'s own.

use super::super::grants::{folder_for, in_construct_home, protected};
use super::super::{Access, PathAsk, Verdict};
use super::Perms;
use crate::sandbox::Policy;
use std::path::{Path, PathBuf};
use tokio::sync::watch;

impl Perms {
    /// Let a file tool reach `path`, asking if the sandbox says no. The error
    /// is the model's: why, and who declined.
    pub async fn path(
        &self,
        tool: &str,
        access: Access,
        path: &Path,
        cancel: watch::Receiver<bool>,
    ) -> Result<(), String> {
        self.path_asked(tool, access, path, cancel).await.map(|_| ())
    }

    /// `path`, saying what the user approved: the real path when the sandbox
    /// said no and the user said yes (possibly for this call only, so it is
    /// not in `policy()`), None when the policy already allowed it. The file
    /// tools open under exactly that, never under a second look at the name.
    pub async fn path_asked(
        &self,
        tool: &str,
        access: Access,
        path: &Path,
        cancel: watch::Receiver<bool>,
    ) -> Result<Option<PathBuf>, String> {
        let policy = self.policy();
        let denied = match access {
            Access::Read => policy.check_read(path),
            Access::Write => policy.check_write(path),
        };
        let Err(msg) = denied else { return Ok(None) };
        let real = Policy::real(path);
        if in_construct_home(&real) {
            let t = &self.tools;
            let names: Vec<&str> =
                [t.skill_view, t.skill_manage, t.memory].into_iter().chain(t.journal_query).collect();
            let (last, rest) = names.split_last().unwrap_or((&"", &[]));
            return Err(format!(
                "{msg} That is construct's own home: use the {} and {last} tools for what lives there.",
                rest.join(", ")
            ));
        }
        if protected(&real) {
            return Err(format!("{msg} That place is protected; only a path typed into config.toml opens it."));
        }
        let req = PathAsk {
            tool: tool.into(),
            access,
            folder: folder_for(&real).map(|f| f.display().to_string()),
            paths: vec![real.display().to_string()],
            detail: String::new(),
            rerun: false,
            ahead: false,
        };
        match self.ask_paths(req, cancel).await {
            Some((v, _)) if v.allows() => Ok(Some(real)),
            Some((Verdict::Deny, _)) => Err(format!("{msg} The user was asked and declined.")),
            _ => Err(msg),
        }
    }

    /// A shell command names paths outside the policy (`ahead`: before it
    /// runs), or failed on them: ask whether to run it with them. The paths to
    /// add to this one run, if yes.
    pub async fn shell(
        &self,
        command: &str,
        access: Access,
        paths: Vec<PathBuf>,
        ahead: bool,
        cancel: watch::Receiver<bool>,
    ) -> Option<Vec<PathBuf>> {
        let req = PathAsk {
            tool: self.tools.shell.into(),
            access,
            paths: paths.iter().map(|p| p.display().to_string()).collect(),
            folder: None,
            detail: command.to_string(),
            rerun: !ahead,
            ahead,
        };
        let (verdict, roots) = self.ask_paths(req, cancel).await?;
        verdict.allows().then_some(roots)
    }
}
