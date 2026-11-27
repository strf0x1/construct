//! Keeping a joined peer running across reboots and crashes (#367, ADR 0076):
//! `construct peer --install` places the binary outside the peer's
//! workspace and writes a systemd unit (user or system) that starts it at
//! boot and restarts it after a crash; `--uninstall` stops and removes the
//! unit.
//!
//! The design choice: the unit restarts the peer after every exit except
//! `EXIT_NO_RESTART`, which the peer uses only when it can never get back in
//! on its own (revoked, refused, denied, its key gone). `Restart=always`
//! then covers crashes, a hub that is down at boot and a lost network, and
//! never tight-loops a peer the hub will not take back. The unit holds no
//! secret: with `llm = "hub"` there is none on the target, and the keys an
//! `llm = "local"` peer needs go in a separate `0600` `EnvironmentFile=`.
//!
//! A system unit runs as root unless `--user` names an account (#389): then
//! it carries `User=`/`Group=`, and `account::hand_over` gives that account
//! the peer's home. The no-restart exit also leaves `STOPPED` in the home,
//! which is how launchd (`launchd.rs`, which cannot match an exit code)
//! learns the same thing.

use crate::account::Account;
use std::ffi::OsString;
use std::io::{Error, ErrorKind, Result, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;

/// The exit code that means "do not restart me": `RestartPreventExitStatus=`.
/// sysexits' `EX_NOPERM`, which the journal shows as `status=77/NOPERM`.
pub const EXIT_NO_RESTART: i32 = 77;

/// Left in the peer's home beside `EXIT_NO_RESTART`, holding why; `--install`
/// removes it. launchd keeps the job alive only while it is absent.
pub const STOPPED: &str = "peer.stopped";

/// Record that this peer stopped for good, and why (`0600`).
pub fn mark_stopped(home: &Path, why: &str) -> Result<()> {
    write_file(&home.join(STOPPED), &format!("{why}\n"), 0o600)
}

/// Forget a stop, so the service manager may start the peer again.
pub fn clear_stopped(home: &Path) -> Result<()> {
    match std::fs::remove_file(home.join(STOPPED)) {
        Err(e) if e.kind() != ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Where a user or a system install puts the binary and the unit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Layout {
    pub system: bool,
    pub bin_dir: PathBuf,
    pub unit_dir: PathBuf,
}

impl Layout {
    /// `~/.local/bin` and `~/.config/systemd/user` (XDG overrides honoured),
    /// or `/usr/local/bin` and `/etc/systemd/system`.
    pub fn new(system: bool, var: impl Fn(&str) -> Option<OsString>) -> Result<Self> {
        if system {
            return Ok(Self { system, bin_dir: "/usr/local/bin".into(), unit_dir: "/etc/systemd/system".into() });
        }
        let set = |k: &str| var(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = set("HOME").ok_or_else(|| Error::new(ErrorKind::NotFound, "HOME is not set"))?;
        Ok(Self {
            system,
            bin_dir: set("XDG_BIN_HOME").unwrap_or_else(|| home.join(".local/bin")),
            unit_dir: set("XDG_CONFIG_HOME").unwrap_or_else(|| home.join(".config")).join("systemd/user"),
        })
    }

    pub fn unit_path(&self, id: &str) -> Result<PathBuf> {
        Ok(self.unit_dir.join(unit_name(id)?))
    }
}

/// `construct-peer-<id>.service`, for an id systemd takes as it is.
pub fn unit_name(id: &str) -> Result<String> {
    let ok = !id.is_empty() && id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'));
    if !ok {
        return Err(Error::new(ErrorKind::InvalidInput, format!("{id:?} cannot name a systemd unit")));
    }
    Ok(format!("construct-peer-{id}.service"))
}

/// What the unit runs and with what.
#[derive(Debug, Clone)]
pub struct Unit<'a> {
    pub id: &'a str,
    pub url: &'a str,
    pub system: bool,
    /// The installed binary and its arguments.
    pub binary: &'a Path,
    pub args: &'a [String],
    /// The peer's workspace, and the home holding its key.
    pub cwd: &'a Path,
    pub home: &'a Path,
    /// `HOME` for a system unit that runs as root, which has none set.
    pub user_home: Option<&'a Path>,
    pub env_file: Option<&'a Path>,
    /// A system unit's `User=` and `Group=` (`--user`); `None` is root.
    pub run_as: Option<&'a Account>,
}

/// The unit file.
pub fn render(u: &Unit<'_>) -> String {
    let exec: Vec<String> = std::iter::once(u.binary.display().to_string()).chain(u.args.iter().cloned()).collect();
    let exec: Vec<String> = exec.iter().map(|a| quote(a)).collect();
    let mut s = format!(
        "# Written by `construct peer --install` (#367); `--uninstall` removes it.\n\
         [Unit]\n\
         Description=construct peer {id} (hub {url})\n\
         Wants=network-online.target\n\
         After=network-online.target\n\
         # Never give up: a hub that is down at boot comes back eventually.\n\
         StartLimitIntervalSec=0\n\
         \n\
         [Service]\n\
         Type=simple\n\
         ExecStart={exec}\n\
         WorkingDirectory={cwd}\n\
         Environment={home}\n",
        id = escape(u.id),
        url = escape(u.url),
        exec = exec.join(" "),
        cwd = quote(&u.cwd.display().to_string()),
        home = quote(&format!("CONSTRUCT_HOME={}", u.home.display())),
    );
    match u.run_as.filter(|_| u.system) {
        // systemd sets HOME (and USER) from the account.
        Some(a) => s.push_str(&format!("User={}\nGroup={}\n", quote(&a.name), quote(&a.group))),
        // Root, and systemd sets no HOME then.
        None => {
            if let Some(home) = u.user_home.filter(|_| u.system) {
                s.push_str(&format!("Environment={}\n", quote(&format!("HOME={}", home.display()))));
            }
        }
    }
    if let Some(f) = u.env_file {
        s.push_str(&format!("EnvironmentFile={}\n", quote(&f.display().to_string())));
    }
    s.push_str(&format!(
        "Restart=always\n\
         RestartSec=5\n\
         # Revoked, refused or keyless: restarting cannot help.\n\
         RestartPreventExitStatus={EXIT_NO_RESTART}\n\
         TimeoutStopSec=30\n\
         \n\
         [Install]\n\
         WantedBy={}\n",
        if u.system { "multi-user.target" } else { "default.target" }
    ));
    s
}

/// An `EnvironmentFile=` holding `vars`; a value with a newline is refused.
pub fn env_file(vars: &[(String, String)]) -> Result<String> {
    let mut s = String::from("# Written by `construct peer --install` (#367): secrets, mode 0600.\n");
    for (k, v) in vars {
        if v.contains(['\n', '\r']) {
            return Err(Error::new(ErrorKind::InvalidInput, format!("{k} holds a newline")));
        }
        s.push_str(&format!("{k}=\"{}\"\n", v.replace('\\', "\\\\").replace('"', "\\\"")));
    }
    Ok(s)
}

/// One `ExecStart=`/value word in systemd's syntax: quoted when it has to be,
/// with `%` and `$` escaped so systemd expands nothing.
fn quote(word: &str) -> String {
    let w = escape(word).replace('$', "$$");
    if !w.is_empty() && !w.contains(|c: char| c.is_whitespace() || matches!(c, '"' | '\'' | '\\' | ';')) {
        return w;
    }
    format!("\"{}\"", w.replace('\\', "\\\\").replace('"', "\\\""))
}

/// `%` is a specifier everywhere in a unit file.
fn escape(text: &str) -> String {
    text.replace('%', "%%")
}

/// Copy `exe` into `dir` under its own name (atomically, mode `0755`), unless
/// it already runs from there; returns where it is now.
pub fn place_binary(exe: &Path, dir: &Path) -> Result<PathBuf> {
    let name = exe.file_name().ok_or_else(|| Error::other(format!("{} has no file name", exe.display())))?;
    std::fs::create_dir_all(dir)?;
    let dest = dir.join(name);
    if same_file(exe, &dest) {
        return Ok(dest);
    }
    // A rename never writes into a running binary (ETXTBSY on reinstall).
    let tmp = dir.join(format!(".{}.tmp.{}", name.to_string_lossy(), std::process::id()));
    std::fs::copy(exe, &tmp).map_err(|e| Error::new(e.kind(), format!("copy to {}: {e}", tmp.display())))?;
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(&tmp, &dest).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(dest)
}

fn same_file(a: &Path, b: &Path) -> bool {
    matches!((a.canonicalize(), b.canonicalize()), (Ok(x), Ok(y)) if x == y)
}

/// Write `contents` to `path` with `mode`, whole or not at all.
pub fn write_file(path: &Path, contents: &str, mode: u32) -> Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension(format!("tmp.{}", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).mode(mode).open(&tmp)?;
    f.write_all(contents.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(&tmp, path).map_err(|e| Error::new(e.kind(), format!("write {}: {e}", path.display())))
}

/// Run `systemctl [--user] <args>`; its stderr is the error.
pub fn systemctl(system: bool, args: &[&str]) -> Result<()> {
    let mut cmd = Command::new("systemctl");
    if !system {
        cmd.arg("--user");
    }
    let out = cmd.args(args).output().map_err(|e| Error::new(e.kind(), format!("systemctl: {e}")))?;
    if out.status.success() {
        return Ok(());
    }
    let why = String::from_utf8_lossy(&out.stderr).trim().to_string();
    Err(Error::other(format!("systemctl {}: {why}", args.join(" "))))
}

/// `loginctl enable-linger`: a user unit starts at boot only with lingering.
pub fn enable_linger() -> Result<()> {
    let out = Command::new("loginctl").arg("enable-linger").output()?;
    if out.status.success() {
        return Ok(());
    }
    Err(Error::other(String::from_utf8_lossy(&out.stderr).trim().to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(system: bool, env_file: Option<&Path>, cwd: &Path) -> String {
        let args = ["peer", "--require-key", "--url", "ws://desk:9443", "--id", "old-pc"].map(String::from);
        render(&Unit {
            id: "old-pc",
            url: "ws://desk:9443",
            system,
            binary: Path::new("/home/b/.local/bin/construct-oldpc"),
            args: &args,
            cwd,
            home: Path::new("/home/b/.construct"),
            user_home: Some(Path::new("/root")),
            env_file,
            run_as: None,
        })
    }

    #[test]
    fn a_user_unit_restarts_on_everything_but_the_no_restart_code() {
        let u = unit(false, None, Path::new("/home/b/work"));
        for line in [
            "ExecStart=/home/b/.local/bin/construct-oldpc peer --require-key --url ws://desk:9443 --id old-pc",
            "WorkingDirectory=/home/b/work",
            "Environment=CONSTRUCT_HOME=/home/b/.construct",
            "Restart=always",
            "RestartSec=5",
            "RestartPreventExitStatus=77",
            "StartLimitIntervalSec=0",
            "After=network-online.target",
            "WantedBy=default.target",
        ] {
            assert!(u.lines().any(|l| l == line), "missing {line:?} in\n{u}");
        }
        assert!(!u.contains("EnvironmentFile") && !u.contains("User=") && !u.contains("HOME=/root"), "{u}");
        assert!(!u.contains("TOKEN") && !u.contains("KEY="), "no secret in the unit: {u}");
    }

    #[test]
    fn a_system_unit_wants_multi_user_and_an_env_file_is_referenced_not_inlined() {
        let u = unit(true, Some(Path::new("/root/.construct/peer.env")), Path::new("/srv/diag"));
        assert!(u.lines().any(|l| l == "WantedBy=multi-user.target"), "{u}");
        assert!(u.lines().any(|l| l == "EnvironmentFile=/root/.construct/peer.env"), "{u}");
        assert!(u.lines().any(|l| l == "Environment=HOME=/root"), "{u}");
    }

    #[test]
    fn a_system_unit_with_a_user_runs_as_it_and_sets_no_root_home() {
        let bob = Account { name: "bob".into(), uid: 1000, gid: 1000, group: "users".into(), home: "/home/bob".into() };
        let args = ["peer".to_string()];
        let render_as = |system| {
            render(&Unit {
                id: "nas",
                url: "ws://desk:9443",
                system,
                binary: Path::new("/usr/local/bin/construct-nas"),
                args: &args,
                cwd: Path::new("/srv/diag"),
                home: Path::new("/home/bob/.construct"),
                user_home: Some(Path::new("/root")),
                env_file: None,
                run_as: Some(&bob),
            })
        };
        let u = render_as(true);
        assert!(u.lines().any(|l| l == "User=bob") && u.lines().any(|l| l == "Group=users"), "{u}");
        assert!(!u.contains("HOME=/root"), "{u}");
        assert!(!render_as(false).contains("User="), "a user unit already runs as its user");
    }

    #[test]
    fn a_stop_is_recorded_and_forgotten() {
        let d = std::env::temp_dir().join(format!("construct-peer-stopped-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        clear_stopped(&d).unwrap();
        mark_stopped(&d, "revoked").unwrap();
        assert_eq!(std::fs::read_to_string(d.join(STOPPED)).unwrap(), "revoked\n");
        assert_eq!(std::fs::metadata(d.join(STOPPED)).unwrap().permissions().mode() & 0o777, 0o600);
        clear_stopped(&d).unwrap();
        assert!(!d.join(STOPPED).exists());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn words_are_quoted_and_specifiers_escaped() {
        assert_eq!(quote("plain/path"), "plain/path");
        assert_eq!(quote("/home/b/my work"), "\"/home/b/my work\"");
        assert_eq!(quote("50%"), "50%%");
        assert_eq!(quote("$HOME"), "$$HOME");
        assert_eq!(quote(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(quote(""), "\"\"");
        let u = unit(false, None, Path::new("/home/b/my work"));
        assert!(u.lines().any(|l| l == "WorkingDirectory=\"/home/b/my work\""), "{u}");
    }

    #[test]
    fn unit_names_and_layouts() {
        assert_eq!(unit_name("old-pc").unwrap(), "construct-peer-old-pc.service");
        for bad in ["", "a b", "x/y", "../up", "é"] {
            assert!(unit_name(bad).is_err(), "{bad:?}");
        }
        let var = |k: &str| (k == "HOME").then(|| OsString::from("/home/b"));
        let user = Layout::new(false, var).unwrap();
        assert_eq!(user.bin_dir, Path::new("/home/b/.local/bin"));
        assert_eq!(user.unit_path("pi").unwrap(), Path::new("/home/b/.config/systemd/user/construct-peer-pi.service"));
        let xdg = |k: &str| match k {
            "HOME" => Some(OsString::from("/home/b")),
            "XDG_CONFIG_HOME" => Some(OsString::from("/cfg")),
            "XDG_BIN_HOME" => Some(OsString::from("/bin2")),
            _ => None,
        };
        let user = Layout::new(false, xdg).unwrap();
        assert_eq!(
            (user.bin_dir.as_path(), user.unit_dir.as_path()),
            (Path::new("/bin2"), Path::new("/cfg/systemd/user"))
        );
        let sys = Layout::new(true, |_| None).unwrap();
        assert_eq!(sys.unit_dir, Path::new("/etc/systemd/system"));
        assert!(Layout::new(false, |_| None).is_err());
    }

    #[test]
    fn env_files_quote_values_and_refuse_newlines() {
        let vars = vec![("OPENAI_API_KEY".to_string(), r#"sk-"x"\y"#.to_string())];
        assert!(env_file(&vars).unwrap().contains(r#"OPENAI_API_KEY="sk-\"x\"\\y""#));
        assert!(env_file(&[("K".into(), "a\nb".into())]).is_err());
    }

    #[test]
    fn placing_copies_once_and_writes_are_private() {
        let d = std::env::temp_dir().join(format!("construct-peer-service-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("src")).unwrap();
        let exe = d.join("src/construct-oldpc");
        std::fs::write(&exe, b"binary").unwrap();
        let placed = place_binary(&exe, &d.join("bin")).unwrap();
        assert_eq!(std::fs::read(&placed).unwrap(), b"binary");
        assert_eq!(std::fs::metadata(&placed).unwrap().permissions().mode() & 0o777, 0o755);
        assert_eq!(place_binary(&placed, &d.join("bin")).unwrap(), placed, "already there: left alone");
        let env = d.join("home/peer.env");
        write_file(&env, "K=\"v\"\n", 0o600).unwrap();
        assert_eq!(std::fs::metadata(&env).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&d);
    }
}
