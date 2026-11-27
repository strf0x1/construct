//! `construct peer --install` on macOS (#389): a launchd job in place of the
//! systemd unit (`service.rs`). A user install is a LaunchAgent in
//! `~/Library/LaunchAgents`, loaded into the login session (`gui/<uid>`);
//! `--system` is a LaunchDaemon in `/Library/LaunchDaemons` (domain
//! `system`), which starts at boot with no one logged in and, with `--user`,
//! runs as that account (`UserName`/`GroupName`).
//!
//! The design choice: launchd cannot restart "after every exit but 77" (its
//! `SuccessfulExit` only tells zero from non-zero). So the job is kept alive
//! while `service::STOPPED` is absent from the peer's home
//! (`KeepAlive`/`PathState`), and the peer writes that file whenever it
//! exits `EXIT_NO_RESTART`. The exit code means the same on both platforms;
//! on macOS the file is what launchd reads. launchd has no journal, so the
//! peer's output goes to `peer.log` in its home, and no `EnvironmentFile=`,
//! so `--env` keys reach the peer through `/bin/sh` sourcing a `0600` file
//! and `exec`ing it (the pid, and so the signals, stay the peer's).

use crate::account::Account;
use crate::service::STOPPED;
use std::ffi::OsString;
use std::io::{Error, ErrorKind, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

/// Where the peer's output goes, in its home.
pub const LOG: &str = "peer.log";

/// Where a user or a system install puts the binary and the job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub system: bool,
    pub bin_dir: PathBuf,
    pub plist_dir: PathBuf,
}

impl Layout {
    /// `~/.local/bin` (or `$XDG_BIN_HOME`) and `~/Library/LaunchAgents`, or
    /// `/usr/local/bin` and `/Library/LaunchDaemons`.
    pub fn new(system: bool, var: impl Fn(&str) -> Option<OsString>) -> Result<Self> {
        if system {
            return Ok(Self { system, bin_dir: "/usr/local/bin".into(), plist_dir: "/Library/LaunchDaemons".into() });
        }
        let set = |k: &str| var(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = set("HOME").ok_or_else(|| Error::new(ErrorKind::NotFound, "HOME is not set"))?;
        Ok(Self {
            system,
            bin_dir: set("XDG_BIN_HOME").unwrap_or_else(|| home.join(".local/bin")),
            plist_dir: home.join("Library/LaunchAgents"),
        })
    }

    pub fn plist_path(&self, id: &str) -> Result<PathBuf> {
        Ok(self.plist_dir.join(format!("{}.plist", label(id)?)))
    }

    /// The launchd domain the job loads into: `system`, or this user's
    /// login session.
    pub fn domain(&self) -> String {
        if self.system {
            return "system".into();
        }
        // SAFETY: getuid cannot fail.
        format!("gui/{}", unsafe { libc::getuid() })
    }
}

/// `construct.peer.<id>`, for an id launchd and a file name take as it is.
pub fn label(id: &str) -> Result<String> {
    let ok = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !ok {
        return Err(Error::new(ErrorKind::InvalidInput, format!("{id:?} cannot name a launchd job")));
    }
    Ok(format!("construct.peer.{id}"))
}

/// What the job runs and with what.
#[derive(Debug, Clone)]
pub struct Job<'a> {
    pub label: &'a str,
    /// The installed binary and its arguments.
    pub binary: &'a Path,
    pub args: &'a [String],
    /// The peer's workspace, and the home holding its key.
    pub cwd: &'a Path,
    pub home: &'a Path,
    /// `HOME` for the peer.
    pub user_home: Option<&'a Path>,
    /// A `0600` shell file of `export NAME='value'` lines (`env_file`).
    pub env_file: Option<&'a Path>,
    /// A daemon's `UserName` and `GroupName` (`--user`); `None` is root.
    pub run_as: Option<&'a Account>,
}

/// The property list.
pub fn render(j: &Job<'_>) -> String {
    let mut argv: Vec<String> = Vec::new();
    if let Some(f) = j.env_file {
        // `$0` is the env file, `$@` the peer: source one, exec the other.
        argv.extend(["/bin/sh".into(), "-c".into(), ". \"$0\" && exec \"$@\"".into(), f.display().to_string()]);
    }
    argv.push(j.binary.display().to_string());
    argv.extend(j.args.iter().cloned());
    let argv: String = argv.iter().map(|a| format!("\t\t<string>{}</string>\n", xml(a))).collect();
    let mut env = vec![("CONSTRUCT_HOME", j.home.display().to_string())];
    if let Some(h) = j.run_as.map(|a| a.home.as_path()).or(j.user_home) {
        env.push(("HOME", h.display().to_string()));
    }
    let env: String =
        env.iter().map(|(k, v)| format!("\t\t<key>{k}</key>\n\t\t<string>{}</string>\n", xml(v))).collect();
    let run_as = j.run_as.map_or(String::new(), |a| {
        format!(
            "\t<key>UserName</key>\n\t<string>{}</string>\n\t<key>GroupName</key>\n\t<string>{}</string>\n",
            xml(&a.name),
            xml(&a.group)
        )
    });
    let stopped = xml(&j.home.join(STOPPED).display().to_string());
    let log = xml(&j.home.join(LOG).display().to_string());
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <!-- Written by construct peer install (#389); construct peer uninstall removes it. -->\n\
         <plist version=\"1.0\">\n\
         <dict>\n\
         \t<key>Label</key>\n\t<string>{label}</string>\n\
         \t<key>ProgramArguments</key>\n\t<array>\n{argv}\t</array>\n\
         \t<key>WorkingDirectory</key>\n\t<string>{cwd}</string>\n\
         \t<key>EnvironmentVariables</key>\n\t<dict>\n{env}\t</dict>\n\
         {run_as}\
         \t<key>RunAtLoad</key>\n\t<true/>\n\
         \t<!-- Restart after every exit until the peer stops for good (exit 77: revoked, refused, keyless), which leaves this file. -->\n\
         \t<key>KeepAlive</key>\n\t<dict>\n\t\t<key>PathState</key>\n\t\t<dict>\n\
         \t\t\t<key>{stopped}</key>\n\t\t\t<false/>\n\t\t</dict>\n\t</dict>\n\
         \t<key>ThrottleInterval</key>\n\t<integer>5</integer>\n\
         \t<key>ExitTimeOut</key>\n\t<integer>30</integer>\n\
         \t<key>StandardOutPath</key>\n\t<string>{log}</string>\n\
         \t<key>StandardErrorPath</key>\n\t<string>{log}</string>\n\
         </dict>\n\
         </plist>\n",
        label = xml(j.label),
        cwd = xml(&j.cwd.display().to_string()),
    )
}

/// The `--env` file for `/bin/sh`: `export NAME='value'`, single-quoted so
/// the shell expands nothing; a value with a newline is refused.
pub fn env_file(vars: &[(String, String)]) -> Result<String> {
    let mut s = String::from("# Written by `construct peer --install` (#389): secrets, mode 0600.\n");
    for (k, v) in vars {
        if v.contains(['\n', '\r']) {
            return Err(Error::new(ErrorKind::InvalidInput, format!("{k} holds a newline")));
        }
        if k.is_empty() || !k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(Error::new(ErrorKind::InvalidInput, format!("{k:?} is not a shell variable name")));
        }
        s.push_str(&format!("export {k}='{}'\n", v.replace('\'', "'\\''")));
    }
    Ok(s)
}

/// Text inside a plist element.
fn xml(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Run `launchctl <args>`; its output is the error.
pub fn launchctl(args: &[&str]) -> Result<()> {
    let out =
        Command::new("launchctl").args(args).output().map_err(|e| Error::new(e.kind(), format!("launchctl: {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    let why = [out.stderr, out.stdout].map(|b| String::from_utf8_lossy(&b).trim().to_string()).join(" ");
    Err(Error::other(format!("launchctl {}: {}", args.join(" "), why.trim())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(env_file: Option<&Path>, run_as: Option<&Account>) -> String {
        let args = ["peer", "--require-key", "--url", "ws://desk:9443", "--id", "old-mac"].map(String::from);
        render(&Job {
            label: "construct.peer.old-mac",
            binary: Path::new("/Users/b/.local/bin/construct-oldmac"),
            args: &args,
            cwd: Path::new("/Users/b/work & play"),
            home: Path::new("/Users/b/.construct"),
            user_home: Some(Path::new("/Users/b")),
            env_file,
            run_as,
        })
    }

    #[test]
    fn an_agent_stays_alive_until_the_peer_marks_itself_stopped() {
        let p = job(None, None);
        for want in [
            "<string>construct.peer.old-mac</string>",
            "\t\t<string>/Users/b/.local/bin/construct-oldmac</string>\n\t\t<string>peer</string>\n\t\t<string>--require-key</string>",
            "<key>WorkingDirectory</key>\n\t<string>/Users/b/work &amp; play</string>",
            "<key>CONSTRUCT_HOME</key>\n\t\t<string>/Users/b/.construct</string>",
            "<key>HOME</key>\n\t\t<string>/Users/b</string>",
            "<key>PathState</key>\n\t\t<dict>\n\t\t\t<key>/Users/b/.construct/peer.stopped</key>\n\t\t\t<false/>",
            "<key>RunAtLoad</key>\n\t<true/>",
            "<key>ThrottleInterval</key>\n\t<integer>5</integer>",
            "<key>StandardErrorPath</key>\n\t<string>/Users/b/.construct/peer.log</string>",
        ] {
            assert!(p.contains(want), "missing {want:?} in\n{p}");
        }
        assert!(!p.contains("UserName") && !p.contains("/bin/sh"), "{p}");
        // An XML comment may not hold `--`.
        for c in p.split("<!--").skip(1) {
            let body = c.split("-->").next().unwrap();
            assert!(!body.contains("--"), "{body:?}");
        }
    }

    #[test]
    fn a_daemon_with_a_user_runs_as_it_and_env_keys_are_sourced_not_inlined() {
        let bob = Account { name: "bob".into(), uid: 501, gid: 20, group: "staff".into(), home: "/Users/bob".into() };
        let p = job(Some(Path::new("/Users/bob/.construct/peer.env")), Some(&bob));
        assert!(p.contains("<key>UserName</key>\n\t<string>bob</string>"), "{p}");
        assert!(p.contains("<key>GroupName</key>\n\t<string>staff</string>"), "{p}");
        assert!(p.contains("<key>HOME</key>\n\t\t<string>/Users/bob</string>"), "{p}");
        assert!(
            p.contains("<string>/bin/sh</string>\n\t\t<string>-c</string>\n\t\t<string>. &quot;$0&quot; &amp;&amp; exec &quot;$@&quot;</string>\n\t\t<string>/Users/bob/.construct/peer.env</string>\n\t\t<string>/Users/b/.local/bin/construct-oldmac</string>"),
            "{p}"
        );
    }

    #[test]
    fn env_files_single_quote_values_and_refuse_what_sh_would_misread() {
        let vars = vec![("OPENAI_API_KEY".to_string(), "sk-'x'$y`z`".to_string())];
        assert!(env_file(&vars).unwrap().contains(r"export OPENAI_API_KEY='sk-'\''x'\''$y`z`'"));
        assert!(env_file(&[("K".into(), "a\nb".into())]).is_err());
        assert!(env_file(&[("A;rm".into(), "v".into())]).is_err());
    }

    #[test]
    fn labels_and_layouts() {
        assert_eq!(label("old-mac").unwrap(), "construct.peer.old-mac");
        for bad in ["", "a b", "x/y", "é", "<x>"] {
            assert!(label(bad).is_err(), "{bad:?}");
        }
        let var = |k: &str| (k == "HOME").then(|| OsString::from("/Users/b"));
        let user = Layout::new(false, var).unwrap();
        assert_eq!(user.bin_dir, Path::new("/Users/b/.local/bin"));
        assert_eq!(user.plist_path("pi").unwrap(), Path::new("/Users/b/Library/LaunchAgents/construct.peer.pi.plist"));
        assert!(user.domain().starts_with("gui/"));
        let sys = Layout::new(true, |_| None).unwrap();
        assert_eq!((sys.plist_dir.as_path(), sys.domain().as_str()), (Path::new("/Library/LaunchDaemons"), "system"));
    }
}
