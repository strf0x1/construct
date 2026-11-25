//! Stamped peer binaries (#363) as a user meets them: `construct build
//! --from` the real binary, then the stamped copy run in a fresh home with no
//! config file. Every assertion reads what the stamped binary prints and
//! exits with, what the hub reports (`hub status --json`), the tool results
//! the scripted model received, or the files left in the work dir.

use crate::common::*;
use crate::hub::{command, peer, spawn_hub, wait_status};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const KEY: &str = "4e1d6a0b3c5f7e9a2b4d6f8a0c2e4f6a8b0d2f4a6c8e0a2c4e6a8c0e2a4c6e8a";

/// Stamp the test binary as peer `name` into the env's dir; the host home is
/// the env's own. Returns the stamped binary and what `build` printed.
fn stamp(env: &Env, name: &str, extra: &[&str]) -> (PathBuf, String) {
    let out = env.dir.join(format!("construct-{name}"));
    let from = construct().display().to_string();
    let path = out.display().to_string();
    let mut args = vec!["build", "--from", &from, "--name", name, "--out", &path];
    args.extend_from_slice(extra);
    let got = command(env, &args).output().unwrap();
    assert!(got.status.success(), "build failed: {}", text(&got));
    (out, String::from_utf8_lossy(&got.stdout).to_string())
}

/// The stamped binary in a fresh home with no config file.
fn stamped(env: &Env, bin: &Path, home: &str, args: &[&str]) -> Command {
    let mut fresh = Command::new(bin);
    fresh.args(args).current_dir(&env.work);
    for (k, v) in env.env() {
        if v.is_empty() {
            fresh.env_remove(k);
        } else {
            fresh.env(k, v);
        }
    }
    fresh.env("CONSTRUCT_HOME", env.dir.join(home));
    fresh
}

fn run(env: &Env, bin: &Path, home: &str, args: &[&str]) -> Output {
    stamped(env, bin, home, args).output().unwrap()
}

#[test]
fn a_stamped_binary_shows_its_stamped_settings_with_no_config_file() {
    let env = Env::new("stamp-config", json!({"steps": []}));
    let (bin, printed) = stamp(&env, "old-pc", &["--url", "ws://10.0.0.2:7300", "--hub-key", KEY]);
    assert!(printed.contains("as peer old-pc") && printed.contains("llm = \"hub\""), "{printed}");

    let out = run(&env, &bin, "fresh", &["config"]);
    assert!(out.status.success(), "{}", text(&out));
    let t = text(&out);
    assert!(!env.dir.join("fresh/config.toml").exists(), "no config file was written");
    assert!(t.contains("llm = \"hub\"") && t.contains("# llm = stamped"), "{t}");
    assert!(t.contains(&format!("# stamped peer old-pc: dials ws://10.0.0.2:7300, hub key {KEY}")), "{t}");

    // A file in the field still wins over the stamp, key by key.
    std::fs::write(env.dir.join("fresh/config.toml"), "llm = \"local\"\n").unwrap();
    let t = text(&run(&env, &bin, "fresh", &["config"]));
    assert!(t.contains("# llm = file"), "{t}");

    // Arguments mean the usual commands, not a dial.
    let v = run(&env, &bin, "fresh", &["--version"]);
    assert!(v.status.success() && text(&v).contains("construct"), "{}", text(&v));
}

#[test]
fn a_stamped_binary_run_bare_dials_its_hub() {
    let env = Env::new("stamp-dial", json!({"steps": []}));
    let mut hub = spawn_hub(&env);
    let s = wait_status(&env, "the hub to run", |s| s["running"] == true);
    let url = s["listen"].as_str().unwrap();

    // The hub's own home stamps it: its key is the default, and --invite
    // registers the slot through the running hub.
    let (bin, printed) = stamp(&env, "old-pc", &["--url", url, "--invite"]);
    let line = printed.lines().find(|l| l.contains("CONSTRUCT_JOIN=")).unwrap_or_else(|| panic!("{printed}"));
    let token = line.trim().strip_prefix("CONSTRUCT_JOIN=").unwrap().strip_suffix(" ./construct-old-pc").unwrap();
    assert_eq!(peer(&wait_status(&env, "the slot", |_| true), "old-pc").unwrap()["state"], "expected");

    // No arguments at all: the stamped binary is a peer.
    let log = std::fs::File::create(env.dir.join("peer.stderr")).unwrap();
    let mut child = stamped(&env, &bin, "peer-home", &[])
        .env("CONSTRUCT_JOIN", token)
        .stdout(Stdio::null())
        .stderr(log)
        .spawn()
        .unwrap();
    let row = wait_status(&env, "old-pc online", |s| peer(s, "old-pc").is_some_and(|p| p["online"] == true));
    assert_eq!(peer(&row, "old-pc").unwrap()["state"], "pinned");
    let said = std::fs::read_to_string(env.dir.join("peer.stderr")).unwrap();
    assert!(said.contains("model calls go through the hub"), "the stamp's llm = \"hub\" applies: {said}");
    child.kill().unwrap();
    let _ = child.wait();
    let _ = command(&env, &["hub", "stop"]).output();
    let _ = hub.wait();
}

#[test]
fn a_corrupt_stamp_fails_closed() {
    let env = Env::new("stamp-corrupt", json!({"steps": []}));
    let (bin, _) =
        stamp(&env, "old-pc", &["--url", "ws://10.0.0.2:7300", "--hub-key", KEY, "--sandbox", "readonly-host"]);
    let good = std::fs::read(&bin).unwrap();
    let broken = |name: &str, bytes: &[u8], want: &str| {
        let path = env.dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
        let out = run(&env, &path, "fresh", &["config"]);
        assert!(!out.status.success(), "{name} must not run unstamped: {}", text(&out));
        let t = text(&out);
        assert!(t.contains(want) && t.contains("cannot be trusted"), "{name}: {t}");
    };

    // A bit flip inside valid TOML: the id still parses, the checksum does not.
    let mut flipped = good.clone();
    let at = flipped.windows(13).rposition(|w| w == b"id = \"old-pc\"").unwrap() + 6;
    flipped[at] ^= 0x20;
    broken("flipped", &flipped, "SHA-256");

    // A length that points before the file starts.
    let mut long = good.clone();
    let n = long.len();
    long[n - 12..n - 8].copy_from_slice(&u32::MAX.to_le_bytes());
    broken("long", &long, "corrupt or truncated");

    // A copy cut off inside the trailer's marker.
    broken("cut", &good[..good.len() - 3], "truncated");
}

#[test]
fn readonly_host_reads_the_host_and_blocks_writes() {
    let env = Env::new(
        "stamp-readonly",
        json!({"steps": [
            {"call": "shell", "args": {"command": "ls /var/log > /dev/null && echo host-readable"}},
            {"call": "write", "args": {"path": "x.txt", "content": "hi"}},
            {"text": "done"},
        ]}),
    );
    // Models through this machine's endpoint, so the scripted model answers.
    let settings = env.dir.join("settings.toml");
    std::fs::write(&settings, "llm = \"local\"\n").unwrap();
    let s = settings.display().to_string();
    let (bin, _) = stamp(
        &env,
        "diag",
        &["--url", "ws://10.0.0.2:7300", "--hub-key", KEY, "--sandbox", "readonly-host", "--settings", &s],
    );

    let t = text(&run(&env, &bin, "fresh", &["config"]));
    assert!(t.contains("mode = \"read-only\"") && t.contains("# sandbox.mode = stamped"), "{t}");
    assert!(t.contains("# sandbox.allow_read = stamped") && t.contains("#   sandbox preset readonly-host"), "{t}");

    let out = run(&env, &bin, "fresh", &["run", "--no-learn", "look around"]);
    assert!(out.status.success(), "{}", text(&out));
    let reqs: Vec<Value> = env.requests();
    assert!(last_tool_result(&reqs[1]).contains("host-readable"), "{}", last_tool_result(&reqs[1]));
    let wrote = last_tool_result(&reqs[2]);
    assert!(wrote.starts_with("Error"), "a write outside the sandbox is refused headless: {wrote}");
    assert!(!env.work.join("x.txt").exists());
}
