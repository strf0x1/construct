//! `construct hub` (#360) as a user and a peer meet it: the real binary in a
//! fresh home, peers dialing it over the real wire (`construct-peer`: ws,
//! Noise IK, 1-RTT `Hello`), and every assertion read back from what the hub
//! reports (`hub status --json`), the files it leaves, or what a peer receives.

use crate::common::*;
use construct_peer::{Control, FrameStream, Hello, Keypair, PINNED_MAX_ENVELOPE, PeerLink, PublicKey, connect_ws};
use serde_json::{Value, json};
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

pub(crate) fn command(env: &Env, args: &[&str]) -> Command {
    let mut c = Command::new(construct());
    c.args(args).current_dir(&env.work);
    for (k, v) in env.env() {
        if v.is_empty() {
            c.env_remove(k);
        } else {
            c.env(k, v);
        }
    }
    c
}

pub(crate) fn spawn_hub(env: &Env) -> Child {
    let log = std::fs::File::create(env.dir.join("hub.stderr")).unwrap();
    command(env, &["hub", "--listen", "127.0.0.1:0"]).stdout(Stdio::null()).stderr(log).spawn().unwrap()
}

pub(crate) fn status(env: &Env) -> Value {
    let out = command(env, &["hub", "status", "--json"]).output().unwrap();
    assert!(out.status.success(), "hub status failed: {}", text(&out));
    serde_json::from_slice(&out.stdout).unwrap()
}

pub(crate) fn peer<'a>(s: &'a Value, id: &str) -> Option<&'a Value> {
    s["peers"].as_array().unwrap().iter().find(|p| p["id"] == id)
}

/// Poll `hub status` until `ok` holds, or fail with the last status and log.
pub(crate) fn wait_status(env: &Env, what: &str, ok: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_millis(budget(10_000));
    loop {
        let s = status(env);
        if ok(&s) {
            return s;
        }
        if Instant::now() > deadline {
            let log = std::fs::read_to_string(env.dir.join("hub.stderr")).unwrap_or_default();
            panic!("timed out waiting for {what}; last status {s:#}\nhub stderr:\n{log}");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

pub(crate) async fn dial(url: &str, hub: PublicKey, key: &Keypair, id: &str) -> PeerLink<impl FrameStream + use<>> {
    let ws = connect_ws(url).await.unwrap();
    construct_peer::handshake_initiator(ws, key, hub, &Hello::new(id), PINNED_MAX_ENVELOPE).await.unwrap()
}

#[test]
fn hub_admits_peers_reports_them_and_runs_once_per_home() {
    let env = Env::new("hub", json!({"steps": []}));
    // A peer pinned earlier (pinning itself is the join flow, #361).
    let desk_pi = Keypair::generate().unwrap();
    std::fs::create_dir_all(env.home.join("hub")).unwrap();
    std::fs::write(
        env.home.join("hub/peers.json"),
        serde_json::to_vec(&json!({"version": 1, "peers": [
            {"id": "desk-pi", "name": "desk-pi", "state": "pinned", "key": desk_pi.public().to_hex()}
        ]}))
        .unwrap(),
    )
    .unwrap();

    let mut hub = spawn_hub(&env);
    let s = wait_status(&env, "the hub to run", |s| s["running"] == true);

    // The control socket lives in the home, not /tmp, and only its owner may use it.
    let sock = env.home.join("hub.sock");
    assert_eq!(s["socket"], sock.display().to_string());
    let meta = std::fs::metadata(&sock).unwrap();
    assert!(meta.file_type().is_socket());
    assert_eq!(meta.permissions().mode() & 0o777, 0o600, "socket mode");
    assert_eq!(std::fs::metadata(env.home.join("hub.key")).unwrap().permissions().mode() & 0o777, 0o600);

    // `hub key` prints the key the running hub answers with.
    let key = command(&env, &["hub", "key"]).output().unwrap();
    let key_hex = String::from_utf8(key.stdout).unwrap().trim().to_string();
    assert_eq!(s["key"], key_hex.as_str());

    // A second hub on the same home refuses to start, naming the first.
    let second = command(&env, &["hub", "--listen", "127.0.0.1:0"]).output().unwrap();
    assert!(!second.status.success());
    let why = text(&second);
    assert!(why.contains("already running") && why.contains(&hub.id().to_string()), "{why}");

    let url = s["listen"].as_str().unwrap().to_string();
    assert!(url.starts_with("ws://127.0.0.1:"), "{url}");
    let hub_key = PublicKey::from_hex(&key_hex).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();

    // Inside a pairing window an unknown peer completes Noise IK + Hello and
    // waits as pending, with a code; the pinned one is online at once.
    let pair = command(&env, &["hub", "pair", "--for", "1m"]).output().unwrap();
    assert!(pair.status.success(), "{}", text(&pair));
    let old_pc = Keypair::generate().unwrap();
    let _old = rt.block_on(dial(&url, hub_key, &old_pc, "old-pc"));
    let _pi = rt.block_on(dial(&url, hub_key, &desk_pi, "desk-pi"));
    let s = wait_status(&env, "both peers online", |s| {
        peer(s, "old-pc").is_some_and(|p| p["online"] == true)
            && peer(s, "desk-pi").is_some_and(|p| p["online"] == true)
    });
    let old = peer(&s, "old-pc").unwrap();
    assert_eq!(old["state"], "pending");
    assert_eq!(old["key"], old_pc.public().to_hex().as_str());
    assert_eq!(old["code"].as_str().map(str::len), Some(7), "{old}");
    assert_eq!(peer(&s, "desk-pi").unwrap()["state"], "pinned");

    // Someone else claiming desk-pi's id with their own key is closed, not trusted.
    let impostor = rt.block_on(async {
        let mut link = dial(&url, hub_key, &Keypair::generate().unwrap(), "desk-pi").await;
        tokio::time::timeout(Duration::from_secs(5), link.recv()).await.unwrap()
    });
    match impostor {
        Ok(env) => match env.decode_control().unwrap() {
            Control::Close { reason } => assert!(reason.unwrap_or_default().contains("another key")),
            other => panic!("expected Close, got {other:?}"),
        },
        Err(e) => panic!("expected a Close with a reason, got {e}"),
    }
    let s = status(&env);
    assert_eq!(peer(&s, "desk-pi").unwrap()["key"], desk_pi.public().to_hex().as_str(), "pinned key untouched");

    // `hub stop` shuts it down cleanly: the process exits 0 and the socket goes.
    let stop = command(&env, &["hub", "stop"]).output().unwrap();
    assert!(stop.status.success(), "{}", text(&stop));
    let deadline = Instant::now() + Duration::from_millis(budget(10_000));
    let code = loop {
        if let Some(code) = hub.try_wait().unwrap() {
            break code;
        }
        assert!(Instant::now() < deadline, "hub did not exit after stop");
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(code.success(), "hub exit {code:?}");
    assert!(!sock.exists(), "socket left behind");

    // With no hub, status still answers from the files. A join that waited
    // for a human is gone with its link: pending is never written.
    let s = status(&env);
    assert_eq!(s["running"], false);
    assert!(peer(&s, "old-pc").is_none(), "{s:#}");
    let pi = peer(&s, "desk-pi").unwrap();
    assert_eq!((pi["state"].as_str(), pi["online"].as_bool()), (Some("pinned"), Some(false)));
    assert!(pi["last_seen"].as_u64().is_some());
    let lines = command(&env, &["hub", "status"]).output().unwrap();
    let lines = String::from_utf8_lossy(&lines.stdout).to_string();
    assert!(lines.contains("hub: not running") && lines.contains("desk-pi  pinned  offline"), "{lines}");

    // And a new hub may start on the same home with the same key.
    let mut again = spawn_hub(&env);
    let s = wait_status(&env, "the hub to run again", |s| s["running"] == true);
    assert_eq!(s["key"], key_hex.as_str());
    let _ = command(&env, &["hub", "stop"]).output();
    let _ = again.wait();
}

#[test]
fn hub_status_and_key_work_with_no_hub() {
    let env = Env::new("hub-offline", json!({"steps": []}));
    let s = status(&env);
    assert_eq!(s["running"], false);
    assert!(s.get("key").is_none(), "no key until one is asked for: {s}");
    let key = command(&env, &["hub", "key"]).output().unwrap();
    assert!(key.status.success(), "{}", text(&key));
    let hex = String::from_utf8(key.stdout).unwrap().trim().to_string();
    assert_eq!(hex.len(), 64);
    assert_eq!(status(&env)["key"], hex.as_str());
    let stop = command(&env, &["hub", "stop"]).output().unwrap();
    assert!(!stop.status.success() && text(&stop).contains("no hub is running"), "{}", text(&stop));

    // Invites and revocations work before a hub ever ran (the command takes
    // the hub's lock for a moment); approving needs the live link.
    let inv = command(&env, &["hub", "invite", "old-pc"]).output().unwrap();
    let out = String::from_utf8_lossy(&inv.stdout).to_string();
    assert!(
        inv.status.success() && out.contains("CONSTRUCT_JOIN=") && out.contains("--url ws://127.0.0.1:9443"),
        "{out}"
    );
    assert_eq!(peer(&status(&env), "old-pc").unwrap()["state"], "expected");
    let rev = command(&env, &["hub", "revoke", "old-pc"]).output().unwrap();
    assert!(rev.status.success(), "{}", text(&rev));
    assert_eq!(peer(&status(&env), "old-pc").unwrap()["state"], "revoked");
    let approve = command(&env, &["hub", "approve", "old-pc"]).output().unwrap();
    assert!(!approve.status.success() && text(&approve).contains("no hub is running"), "{}", text(&approve));
}
