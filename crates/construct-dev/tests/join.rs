//! Joining a hub (#361) as a user and a peer meet it: the real `construct
//! hub` and real `construct peer` processes in fresh homes on loopback, and,
//! where a test plays an attacker with a leaked hub key, the wire library
//! dialing directly. Every assertion reads what the hub reports (`hub status
//! --json`), what a peer prints and exits with, or the files they leave.

use crate::common::*;
use crate::hub::{command, dial, peer, spawn_hub, status, wait_status};
use construct_peer::{Control, Hello, KEY_LEN, Keypair, PINNED_MAX_ENVELOPE, PublicKey, connect_ws};
use serde_json::{Value, json};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

/// A peer process's view: its home and the stderr it writes.
struct Peer {
    home: PathBuf,
    log: PathBuf,
}

impl Peer {
    fn new(env: &Env, name: &str) -> Self {
        let home = env.dir.join(format!("peer-{name}"));
        Self { log: env.dir.join(format!("peer-{name}.{}.stderr", std::process::id())), home }
    }

    fn command(&self, env: &Env, hub: &Value, id: &str, token: Option<&str>) -> Command {
        let (url, key) = (hub["listen"].as_str().unwrap(), hub["key"].as_str().unwrap());
        let mut c = command(env, &["peer", "--url", url, "--hub-key", key, "--id", id]);
        c.env("CONSTRUCT_HOME", &self.home);
        if let Some(t) = token {
            c.env("CONSTRUCT_JOIN", t);
        }
        c
    }

    /// Start a peer that stays connected; its stderr goes to `self.log`.
    fn spawn(&self, env: &Env, hub: &Value, id: &str, token: Option<&str>) -> Child {
        let log = std::fs::File::create(&self.log).unwrap();
        self.command(env, hub, id, token).stdout(Stdio::null()).stderr(log).spawn().unwrap()
    }

    /// Run a peer the hub is expected to turn away.
    fn refused(&self, env: &Env, hub: &Value, id: &str, token: Option<&str>) -> String {
        let out: Output = self.command(env, hub, id, token).output().unwrap();
        assert!(!out.status.success(), "peer {id} should have been refused: {}", text(&out));
        text(&out)
    }

    fn wait_log(&self, what: &str) -> String {
        let deadline = Instant::now() + Duration::from_millis(budget(10_000));
        loop {
            let log = std::fs::read_to_string(&self.log).unwrap_or_default();
            if log.contains(what) {
                return log;
            }
            assert!(Instant::now() < deadline, "peer never printed {what:?}; it printed:\n{log}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn key_file(&self) -> PathBuf {
        self.home.join("peer.key")
    }

    fn keypair(&self) -> Keypair {
        let raw: [u8; KEY_LEN] = std::fs::read(self.key_file()).unwrap().try_into().unwrap();
        Keypair::from_private(raw)
    }
}

fn hub_cmd(env: &Env, args: &[&str]) -> String {
    let out = command(env, args).output().unwrap();
    assert!(out.status.success(), "{args:?} failed: {}", text(&out));
    String::from_utf8_lossy(&out.stdout).to_string()
}

fn invite(env: &Env, id: &str, extra: &[&str]) -> Value {
    let mut args = vec!["hub", "invite", id, "--json"];
    args.extend_from_slice(extra);
    serde_json::from_str(&hub_cmd(env, &args)).unwrap()
}

fn start(env: &Env) -> (Child, Value) {
    let hub = spawn_hub(env);
    let s = wait_status(env, "the hub to run", |s| s["running"] == true);
    (hub, s)
}

fn stop(env: &Env, mut hub: Child) {
    let _ = command(env, &["hub", "stop"]).output();
    let _ = hub.wait();
}

fn exits(child: &mut Child) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_millis(budget(10_000));
    loop {
        if let Some(code) = child.try_wait().unwrap() {
            return code;
        }
        assert!(Instant::now() < deadline, "process did not exit");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The SAS code a peer printed: `code XXX-XXX`.
fn code_in(log: &str) -> String {
    let at = log.find("code ").expect("a code") + "code ".len();
    log.get(at..at + 7).unwrap().to_string()
}

/// The first control message the hub sends a raw dialer.
async fn first(url: &str, hub: PublicKey, id: &str, token: Option<&str>) -> Control {
    let mut hello = Hello::new(id);
    if let Some(t) = token {
        hello = hello.with_join_token(t);
    }
    let ws = connect_ws(url).await.unwrap();
    let key = Keypair::generate().unwrap();
    let mut link = construct_peer::handshake_initiator(ws, &key, hub, &hello, PINNED_MAX_ENVELOPE).await.unwrap();
    let env = tokio::time::timeout(Duration::from_secs(5), link.recv()).await.unwrap().unwrap();
    env.decode_control().unwrap()
}

fn closed_with(c: &Control, why: &str) -> bool {
    matches!(c, Control::Close { reason: Some(r) } if r.contains(why))
}

#[test]
fn an_invite_pins_on_first_connect_and_reconnects_need_no_token() {
    let env = Env::new("join-invite", json!({"steps": []}));
    let (hub, s) = start(&env);

    let inv = invite(&env, "old-pc", &[]);
    let token = inv["token"].as_str().unwrap();
    let cmd = inv["command"].as_str().unwrap();
    let want = format!(
        "CONSTRUCT_JOIN={token} construct peer --url {} --hub-key {} --id old-pc",
        s["listen"].as_str().unwrap(),
        s["key"].as_str().unwrap()
    );
    assert_eq!(cmd, want, "a ready-to-paste command");
    assert_eq!(peer(&status(&env), "old-pc").unwrap()["state"], "expected");

    // The peer makes its key on first run, joins with the token, and is pinned.
    let p = Peer::new(&env, "old-pc");
    let mut child = p.spawn(&env, &s, "old-pc", Some(token));
    p.wait_log("joined the hub as old-pc");
    assert_eq!(std::fs::metadata(p.key_file()).unwrap().permissions().mode() & 0o777, 0o600);
    let key = p.keypair().public().to_hex();
    let row = wait_status(&env, "old-pc online", |s| peer(s, "old-pc").is_some_and(|p| p["online"] == true));
    let row = peer(&row, "old-pc").unwrap();
    assert_eq!((row["state"].as_str(), row["key"].as_str()), (Some("pinned"), Some(key.as_str())));
    assert!(row.get("invite").is_none(), "the token is spent: {row}");
    child.kill().unwrap();
    let _ = child.wait();
    wait_status(&env, "old-pc offline", |s| peer(s, "old-pc").is_some_and(|p| p["online"] == false));

    // Later runs reuse the key and need no token.
    let mut again = p.spawn(&env, &s, "old-pc", None);
    p.wait_log("joined the hub as old-pc");
    assert_eq!(p.keypair().public().to_hex(), key, "the key is reused");
    again.kill().unwrap();
    let _ = again.wait();
    stop(&env, hub);
}

#[test]
fn a_walk_up_join_shows_the_same_code_at_both_ends_and_approve_pins_it() {
    let env = Env::new("join-sas", json!({"steps": []}));
    let (hub, s) = start(&env);

    // An expected slot (no token in hand): the peer waits with a code.
    hub_cmd(&env, &["hub", "invite", "old-pc"]);
    let p = Peer::new(&env, "old-pc");
    let mut child = p.spawn(&env, &s, "old-pc", None);
    let code = code_in(&p.wait_log("waiting for approval"));
    let st = wait_status(&env, "old-pc pending", |s| peer(s, "old-pc").is_some_and(|p| p["state"] == "pending"));
    assert_eq!(peer(&st, "old-pc").unwrap()["code"], code.as_str(), "the hub shows the peer's code");
    assert!(hub_cmd(&env, &["hub", "pending"]).contains(&format!("old-pc  code {code}")));

    // Approving at the hub pins the key; nothing is typed on the peer.
    hub_cmd(&env, &["hub", "approve", "old-pc"]);
    p.wait_log("joined the hub as old-pc");
    let st = status(&env);
    let row = peer(&st, "old-pc").unwrap();
    assert_eq!(
        (row["state"].as_str(), row["key"].as_str()),
        (Some("pinned"), Some(p.keypair().public().to_hex().as_str()))
    );

    // Inside a pairing window an unknown id may ask too; deny turns it away.
    hub_cmd(&env, &["hub", "pair", "--for", "1m"]);
    let nosy = Peer::new(&env, "nosy");
    let mut nosy_child = nosy.spawn(&env, &s, "nosy", None);
    nosy.wait_log("waiting for approval");
    hub_cmd(&env, &["hub", "deny", "nosy"]);
    assert!(!exits(&mut nosy_child).success());
    assert!(nosy.wait_log("denied").contains("denied at the hub"));
    assert!(peer(&status(&env), "nosy").is_none(), "a denied join leaves no row");

    child.kill().unwrap();
    let _ = child.wait();
    stop(&env, hub);
}

#[test]
fn wrong_tokens_burn_the_slot_and_an_expired_token_is_refused() {
    let env = Env::new("join-strikes", json!({"steps": []}));
    let (hub, s) = start(&env);
    let inv = invite(&env, "box", &[]);
    let p = Peer::new(&env, "box");
    for i in 1..5 {
        assert!(p.refused(&env, &s, "box", Some("0000")).contains("wrong join token"));
        assert_eq!(peer(&status(&env), "box").unwrap()["strikes"], i);
    }
    assert!(p.refused(&env, &s, "box", Some("0000")).contains("burnt"));
    assert!(peer(&status(&env), "box").is_none(), "the slot is gone");
    // Even the real token is useless now.
    assert!(p.refused(&env, &s, "box", inv["token"].as_str()).contains("no invite for box"));

    let late = invite(&env, "late", &["--expires", "1s"]);
    std::thread::sleep(Duration::from_millis(2100));
    assert!(p.refused(&env, &s, "late", late["token"].as_str()).contains("expired"));
    stop(&env, hub);
}

#[test]
fn a_leaked_hub_key_cannot_spam_pending_joins_or_dodge_the_strike_limit() {
    let env = Env::new("join-spam", json!({"steps": []}));
    let (hub, s) = start(&env);
    let url = s["listen"].as_str().unwrap().to_string();
    let hub_key = PublicKey::from_hex(s["key"].as_str().unwrap()).unwrap();
    let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();

    // A stolen stamped binary trying made-up ids, with or without a token,
    // gets no pending join and no slot to strike against.
    for i in 0..10 {
        let id = format!("rand-{i}");
        let token = (i % 2 == 0).then_some("guess");
        let got = rt.block_on(first(&url, hub_key, &id, token));
        let why = if token.is_some() { "no invite" } else { "not expected" };
        assert!(closed_with(&got, why), "{id}: {got:?}");
    }
    let st = status(&env);
    assert!(st["peers"].as_array().unwrap().is_empty(), "{st:#}");

    // Even with a pairing window open, only a few joins may wait at once.
    hub_cmd(&env, &["hub", "pair", "--for", "1m"]);
    let mut held = Vec::new();
    for i in 0..5 {
        let id = format!("walk-{i}");
        let mut link = rt.block_on(dial(&url, hub_key, &Keypair::generate().unwrap(), &id));
        let got = rt.block_on(async { tokio::time::timeout(Duration::from_secs(5), link.recv()).await });
        assert_eq!(got.unwrap().unwrap().decode_control().unwrap(), Control::Pending, "{id}");
        held.push(link);
    }
    let got = rt.block_on(first(&url, hub_key, "walk-5", None));
    assert!(closed_with(&got, "too many"), "{got:?}");
    let st = status(&env);
    let pending = st["peers"].as_array().unwrap().iter().filter(|p| p["state"] == "pending").count();
    assert_eq!(pending, 5, "{st:#}");
    drop(held);
    stop(&env, hub);
}

#[test]
fn a_revoked_peer_deletes_its_key_and_cannot_come_back() {
    let env = Env::new("join-revoke", json!({"steps": []}));
    let (hub, s) = start(&env);
    let inv = invite(&env, "old-pc", &[]);
    let p = Peer::new(&env, "old-pc");
    let mut child = p.spawn(&env, &s, "old-pc", inv["token"].as_str());
    p.wait_log("joined the hub as old-pc");
    let old_key = p.keypair();

    hub_cmd(&env, &["hub", "revoke", "old-pc"]);
    assert_eq!(exits(&mut child).code(), Some(77), "a revoked peer stops with the no-restart code (#367)");
    assert!(p.wait_log("revoked").contains("deleted"));
    assert!(!p.key_file().exists(), "the peer deleted its key");
    assert_eq!(peer(&status(&env), "old-pc").unwrap()["state"], "revoked");

    // Run again, it makes a new key: a known id with a new key is refused.
    assert!(p.refused(&env, &s, "old-pc", None).contains("was revoked"));
    // The old key, under any id, is told it is revoked and nothing else, so
    // a peer that was offline when it happened stops for good (#367).
    let url = s["listen"].as_str().unwrap().to_string();
    let hub_key = PublicKey::from_hex(s["key"].as_str().unwrap()).unwrap();
    let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
    let told = rt.block_on(async {
        let ws = connect_ws(&url).await.unwrap();
        let hello = Hello::new("new-name");
        let mut link =
            construct_peer::handshake_initiator(ws, &old_key, hub_key, &hello, PINNED_MAX_ENVELOPE).await.unwrap();
        let first = tokio::time::timeout(Duration::from_secs(5), link.recv()).await.unwrap().unwrap();
        let then = tokio::time::timeout(Duration::from_secs(5), link.recv()).await.unwrap();
        let closed = then.map_or(true, |e| matches!(e.decode_control(), Ok(Control::Close { .. })));
        (first.decode_control().unwrap(), closed)
    });
    assert!(matches!(told.0, Control::Revoke { .. }), "{told:?}");
    assert!(told.1, "the link closes after the revoke");
    assert!(peer(&status(&env), "new-name").is_none(), "no row for the new name");
    stop(&env, hub);
}
