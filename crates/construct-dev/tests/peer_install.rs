//! `construct peer --install` and `--uninstall` (#367, #389, ADR 0076, 0077)
//! as a user runs them, on whichever platform the test runs: the real binary
//! in a fresh `HOME`, with stand-in `systemctl`, `loginctl` and `launchctl`
//! on `PATH` that log what they were asked and succeed, so the host's own
//! service manager is never touched. Assertions read the files `--install`
//! leaves (binary, systemd unit or launchd plist, env file, stop marker) and
//! the service-manager calls it made.

use crate::common::*;
use crate::hub::command;
use serde_json::json;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Output;

const HUB_KEY: &str = "4e1d6a0b3c5f7e9a2b4d6f8a0c2e4f6a8b0d2f4a6c8e0a2c4e6a8c0e2a4c6e8a";

/// A directory of stand-in service managers that append their argv to
/// `calls.log` beside them.
fn fake_managers(env: &Env) -> PathBuf {
    let bin = env.dir.join("fakebin");
    std::fs::create_dir_all(&bin).unwrap();
    for tool in ["systemctl", "loginctl", "launchctl"] {
        let script = format!("#!/bin/sh\necho \"{tool} $*\" >> \"{}\"\n", env.dir.join("calls.log").display());
        std::fs::write(bin.join(tool), script).unwrap();
        std::fs::set_permissions(bin.join(tool), std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    bin
}

/// `construct peer <args>` with the fake managers first on `PATH` and
/// `HOME` in the env's dir.
fn peer(env: &Env, args: &[&str], extra: &[(&str, &str)]) -> Output {
    let path = format!("{}:{}", fake_managers(env).display(), std::env::var("PATH").unwrap_or_default());
    let mut all = vec!["peer", "--id", "old-pc", "--url", "ws://127.0.0.1:9", "--hub-key", HUB_KEY];
    all.extend_from_slice(args);
    let mut c = command(env, &all);
    c.env("PATH", path).env("HOME", env.dir.join("user")).env_remove("XDG_BIN_HOME").env_remove("XDG_CONFIG_HOME");
    for (k, v) in extra {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn calls(env: &Env) -> String {
    std::fs::read_to_string(env.dir.join("calls.log")).unwrap_or_default()
}

fn mode(p: &Path) -> u32 {
    std::fs::metadata(p).unwrap().permissions().mode() & 0o777
}

#[test]
fn install_writes_a_service_that_restarts_on_all_but_the_stop_and_uninstall_removes_it() {
    let env = Env::new("peer-install", json!({"steps": []}));
    let cwd = env.work.display().to_string();
    let user = env.dir.join("user");

    // Not joined: nothing is written, nothing is started.
    let out = peer(&env, &["--install", "--cwd", &cwd], &[]);
    assert!(!out.status.success() && text(&out).contains("has not joined yet"), "{}", text(&out));
    assert_eq!(calls(&env), "");

    // `--user` is for system services only, and must name a real account.
    let out = peer(&env, &["--install", "--user", "nobody", "--cwd", &cwd], &[]);
    assert!(!out.status.success() && text(&out).contains("--system"), "{}", text(&out));
    let out = peer(&env, &["--install", "--system", "--user", "no-such-user-389", "--cwd", &cwd], &[]);
    assert!(!out.status.success() && text(&out).contains("no user named"), "{}", text(&out));

    // Joined (a key in the home), and stopped for good once before.
    construct_peer::keyfile::load_or_create(&env.home.join("peer.key")).unwrap();
    std::fs::write(env.home.join("peer.stopped"), "the hub revoked old-pc\n").unwrap();
    let out = peer(&env, &["--install", "--cwd", &cwd, "--env", "PEER_TEST_KEY"], &[("PEER_TEST_KEY", "sk-'x'$y")]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!env.home.join("peer.stopped").exists(), "a new install forgets the last stop");

    // The binary is copied out of the workspace, executable.
    let binary = user.join(".local/bin").join(construct().file_name().unwrap());
    assert_eq!(mode(&binary), 0o755);
    let env_file = env.home.join("peer.env");
    assert_eq!(mode(&env_file), 0o600);
    let secrets = std::fs::read_to_string(&env_file).unwrap();
    let home = env.home.display().to_string();
    let exec =
        [&binary.display().to_string(), "peer", "--require-key", "--url", "ws://127.0.0.1:9", "--hub-key", HUB_KEY]
            .join(" ");

    let service = if cfg!(target_os = "macos") {
        let plist = user.join("Library/LaunchAgents/construct.peer.old-pc.plist");
        let p = std::fs::read_to_string(&plist).unwrap();
        let strings = p.lines().map(str::trim).collect::<Vec<_>>().join("\n");
        for want in [
            format!("<key>{home}/peer.stopped</key>\n<false/>"),
            format!("<string>{home}/peer.log</string>"),
            format!("<key>CONSTRUCT_HOME</key>\n<string>{home}</string>"),
            format!("<string>{}</string>\n<string>{}</string>", env_file.display(), binary.display()),
            format!("<string>--cwd</string>\n<string>{cwd}</string>"),
        ] {
            assert!(strings.contains(&want), "missing {want:?} in\n{p}");
        }
        assert!(!p.contains("sk-"), "keys are sourced, never in the plist: {p}");
        assert!(secrets.contains(r"export PEER_TEST_KEY='sk-'\''x'\''$y'"), "{secrets}");
        let c = calls(&env);
        assert!(c.contains("launchctl bootstrap gui/") && c.contains(&plist.display().to_string()), "{c}");
        assert!(c.contains("launchctl enable gui/") && c.contains("/construct.peer.old-pc"), "{c}");
        plist
    } else {
        let unit = user.join(".config/systemd/user/construct-peer-old-pc.service");
        let u = std::fs::read_to_string(&unit).unwrap();
        for want in [
            format!("Environment=CONSTRUCT_HOME={home}"),
            format!("EnvironmentFile={}", env_file.display()),
            "RestartPreventExitStatus=77".to_string(),
            "Restart=always".to_string(),
        ] {
            assert!(u.lines().any(|l| l == want), "missing {want:?} in\n{u}");
        }
        assert!(u.lines().any(|l| l.starts_with(&format!("ExecStart={exec}"))), "{u}");
        assert!(!u.contains("sk-") && !u.contains("User="), "{u}");
        assert!(secrets.contains(r#"PEER_TEST_KEY="sk-'x'$y""#), "{secrets}");
        let c = calls(&env);
        assert!(c.contains("systemctl --user daemon-reload"), "{c}");
        assert!(c.contains("systemctl --user enable --now construct-peer-old-pc.service"), "{c}");
        assert!(c.contains("loginctl enable-linger"), "{c}");
        unit
    };

    // Uninstall stops the job and removes its file; binary, key and home stay.
    std::fs::remove_file(env.dir.join("calls.log")).unwrap();
    let out = peer(&env, &["--uninstall"], &[]);
    assert!(out.status.success(), "{}", text(&out));
    assert!(!service.exists() && binary.exists() && env.home.join("peer.key").exists());
    let c = calls(&env);
    if cfg!(target_os = "macos") {
        assert!(c.contains("launchctl bootout gui/") && c.contains("/construct.peer.old-pc"), "{c}");
    } else {
        assert!(c.contains("systemctl --user disable --now construct-peer-old-pc.service"), "{c}");
    }
    let again = peer(&env, &["--uninstall"], &[]);
    assert!(!again.status.success() && text(&again).contains("is not installed"), "{}", text(&again));
}
