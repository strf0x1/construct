//! Contract: every tool in the catalog, checked without a model. A new tool
//! is covered by being in the catalog: its schema is well-formed, a call with
//! no arguments is refused in prose that names the missing one, file tools
//! refuse what the sandbox denies, and output stays under `OUTPUT_MAX`.

use construct_core::config::{MemoryConfig, Paths, ToolsConfig};
use construct_core::memory::Store;
use construct_core::sandbox::Policy;
use construct_core::tools::contract::{self, all_groups};
use construct_core::tools::{Catalog, Ctx, Deck, Group, OUTPUT_MAX};
use serde_json::{Value, json};
use std::path::PathBuf;

struct Fixture {
    root: PathBuf,
    ctx: Ctx,
    catalog: Catalog,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("construct-contract-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(root.join("outside")).unwrap();
        std::fs::write(root.join("outside/secret.txt"), "secret\n").unwrap();
        let paths = Paths { home: root.join("home") };
        let memory = Store::new(&paths, &MemoryConfig::default());
        // This contract tests tool enforcement, not the default allowlist.
        // A production policy allows caches/toolchains that may contain
        // TMPDIR, which would also allow our sibling `outside` directory.
        let sandbox = Policy {
            read: vec![work.canonicalize().unwrap()],
            write: vec![work.canonicalize().unwrap()],
            ..Default::default()
        };
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let ctx = Ctx::new(work, paths, memory, rx, None, sandbox);
        // A coordinator's catalog, so the `deck` tool is checked too; its
        // gateway is never reached, since every call here is refused first.
        let deck = Deck { control: Some("http://127.0.0.1:9".into()), ..Deck::default() };
        let catalog = Catalog::build_with(&ToolsConfig::default(), None, &deck).unwrap();
        Self { root, ctx, catalog }
    }

    async fn call(&self, name: &str, args: Value) -> Result<String, (String, &'static str)> {
        self.catalog.call(&self.ctx, name, &args.to_string()).await.map(|o| o.text).map_err(|e| (e.message, e.code))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[test]
fn every_schema_is_a_well_formed_function() {
    schemas_are_well_formed(&Fixture::new("schema"));
}

fn schemas_are_well_formed(f: &Fixture) {
    contract::schemas_are_well_formed(&f.catalog, &all_groups());
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_arguments_are_refused_in_prose() {
    let f = Fixture::new("args");
    contract::missing_arguments_are_refused(&f.catalog, &f.ctx, &all_groups()).await;
    let (msg, code) = f.call("read", json!("not an object")).await.expect_err("bad args");
    assert!(msg.contains("path is required"), "{msg}");
    assert_eq!(code, "error");
    let err = f.catalog.call(&f.ctx, "read", "{not json").await.err().unwrap();
    assert_eq!(err.code, "bad_json");
    assert!(err.message.contains("resend the call"));
    let err = f.catalog.call(&f.ctx, "no_such_tool", "{}").await.err().unwrap();
    assert_eq!((err.code, err.message.as_str()), ("unknown_tool", "unknown tool no_such_tool"));
}

#[tokio::test(flavor = "multi_thread")]
async fn file_tools_refuse_what_the_sandbox_denies() {
    sandbox_refusals(&Fixture::new("sandbox")).await;
}

#[test]
fn sandbox_refusals_with_tmpdir_in_an_allowed_cache() {
    // Change environment only in a child: other contracts run concurrently.
    // The old fixture's Policy::new allowed HOME/.cache, including `outside`.
    let f = Fixture::new("cache-parent");
    let cache = f.root.join(".cache");
    std::fs::create_dir_all(&cache).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "tool_contract::file_tools_refuse_what_the_sandbox_denies"])
        .env("HOME", &f.root)
        .env("TMPDIR", &cache)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "cache TMPDIR contract failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn sandbox_refusals(f: &Fixture) {
    let secret = f.root.join("outside/secret.txt");
    let outside = f.root.join("outside");
    assert!(f.ctx.sandbox.check_read(&secret).is_err());
    assert!(f.ctx.sandbox.check_write(&outside.join("new.txt")).is_err());
    let s = |p: &PathBuf| p.display().to_string();
    let cases = [
        ("read", json!({ "path": s(&secret) })),
        ("write", json!({ "path": s(&outside.join("new.txt")), "content": "x" })),
        ("edit", json!({ "path": s(&secret), "old_string": "secret", "new_string": "public" })),
        ("grep", json!({ "pattern": "secret", "path": s(&outside) })),
        ("glob", json!({ "pattern": "*.txt", "path": s(&outside) })),
        ("bash", json!({ "command": "true", "workdir": s(&outside) })),
    ];
    for (name, args) in cases {
        let (msg, code) = f.call(name, args).await.expect_err(name);
        assert_eq!(code, "sandbox", "{name}: {msg}");
        assert!(msg.contains("blocked by the sandbox"), "{name}: {msg}");
    }
    assert_eq!(std::fs::read_to_string(&secret).unwrap(), "secret\n");
    assert!(!outside.join("new.txt").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn output_stays_under_the_cap() {
    output_capped(&Fixture::new("cap")).await;
}

async fn output_capped(f: &Fixture) {
    let line = format!("{}\n", "needle ".repeat(40));
    std::fs::write(f.ctx.cwd.join("big.txt"), line.repeat(3000)).unwrap();
    for (name, args) in [("read", json!({ "path": "big.txt" })), ("grep", json!({ "pattern": "needle" }))] {
        let out = f.call(name, args).await.unwrap();
        assert!(out.len() <= OUTPUT_MAX, "{name}: {} bytes", out.len());
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn edit_reports_stable_codes() {
    let f = Fixture::new("edit");
    std::fs::write(f.ctx.cwd.join("a.txt"), "x x\n").unwrap();
    let edit = |old: &str, new: &str| json!({ "path": "a.txt", "old_string": old, "new_string": new });
    assert_eq!(f.call("edit", edit("y", "z")).await.unwrap_err().1, "not_found");
    assert_eq!(f.call("edit", edit("x", "z")).await.unwrap_err().1, "ambiguous");
    assert_eq!(f.call("edit", edit("x", "x")).await.unwrap_err().1, "no_change");
    let out = f.catalog.call(&f.ctx, "edit", &edit("x x", "y").to_string()).await.unwrap();
    assert_eq!(out.kind, construct_core::tools::OutputKind::Diff);
    assert!(out.text.starts_with("edited "), "{}", out.text);
    assert_eq!(std::fs::read_to_string(f.ctx.cwd.join("a.txt")).unwrap(), "y\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn other_encodings_are_refused_with_the_way_out() {
    let f = Fixture::new("encoding");
    std::fs::write(f.ctx.cwd.join("latin1.txt"), b"caf\xe9\n").unwrap();
    let args = json!({ "path": "latin1.txt", "old_string": "caf", "new_string": "bar" });
    let (msg, code) = f.call("edit", args).await.unwrap_err();
    assert_eq!(code, "not_utf8");
    assert!(msg.contains("bash"), "{msg}");
    assert_eq!(std::fs::read(f.ctx.cwd.join("latin1.txt")).unwrap(), b"caf\xe9\n");
    std::fs::write(f.ctx.cwd.join("utf16.txt"), b"\xff\xfea\0\n\0").unwrap();
    for (name, args) in [
        ("read", json!({ "path": "utf16.txt" })),
        ("edit", json!({ "path": "utf16.txt", "old_string": "a", "new_string": "b" })),
    ] {
        let (msg, code) = f.call(name, args).await.unwrap_err();
        assert_eq!(code, "utf16", "{name}: {msg}");
        assert!(msg.contains("iconv -f UTF-16"), "{name}: {msg}");
    }
}

#[test]
fn disabled_tools_are_not_in_the_catalog() {
    // An empty group not being offered or routed is the tool crates' to
    // show: core has only the core group now (construct-tools-memory).
    let cfg = ToolsConfig { disabled: vec!["write".into(), "glob".into()], ..Default::default() };
    let c = Catalog::build(&cfg, None).unwrap();
    assert!(c.get("write").is_none());
    assert_eq!(c.group_of("glob"), None, "a disabled tool can't switch its group on");
    let bad = ToolsConfig { disabled: vec!["wirte".into()], ..Default::default() };
    assert!(Catalog::build(&bad, None).err().unwrap().to_string().contains("unknown tool \"wirte\""));
}

#[test]
fn the_shell_tools_old_name_still_reaches_it() {
    // ADR 0046: `shell` became `bash`. Old configs, agent definitions,
    // journals and models still say `shell`; only `bash` is offered.
    let c = Catalog::build(&ToolsConfig::default(), None).unwrap();
    assert!(c.names().contains(&"bash".to_string()) && !c.names().contains(&"shell".to_string()));
    assert_eq!(c.get("shell").map(|t| t.name().to_string()).as_deref(), Some("bash"));
    assert_eq!(c.group_of("shell"), Some(Group::Core));
    let off = Catalog::build(&ToolsConfig { disabled: vec!["shell".into()], ..Default::default() }, None).unwrap();
    assert!(off.get("bash").is_none() && off.get("shell").is_none());
    assert_eq!(off.group_of("shell"), None);
    assert_eq!(off.group_of("bash"), None);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_call_by_the_old_name_runs_the_shell_tool() {
    // The shell tool's own workdir check answers, not "unknown tool".
    let f = Fixture::new("old-shell-name");
    let outside = f.root.join("outside").display().to_string();
    let (msg, code) = f.call("shell", json!({ "command": "true", "workdir": outside })).await.unwrap_err();
    assert_eq!(code, "sandbox", "{msg}");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_search_tool_can_be_renamed_and_nothing_else_changes() {
    let f = Fixture::new("rename");
    std::fs::write(f.ctx.cwd.join("a.txt"), "needle\n").unwrap();
    let cfg = ToolsConfig { search_name: "search".into(), ..Default::default() };
    let renamed = Catalog::build(&cfg, None).unwrap();
    let names = renamed.names();
    assert!(names.contains(&"search".to_string()) && !names.contains(&"grep".to_string()), "{names:?}");
    // Same schema and description under the new name: only the name differs.
    let def = |c: &Catalog, n: &str| {
        let mut d = c.definitions(&[Group::Core]).into_iter().find(|d| d["function"]["name"] == n).unwrap();
        d["function"]["name"] = json!("-");
        d
    };
    assert_eq!(def(&renamed, "search"), def(&f.catalog, "grep"));
    let out = renamed.call(&f.ctx, "search", &json!({ "pattern": "needle" }).to_string()).await.unwrap();
    assert_eq!(out.text, "a.txt:1: needle");
    assert!(renamed.call(&f.ctx, "grep", &json!({ "pattern": "needle" }).to_string()).await.is_err());
    // A name that is another tool, or not a name at all, is refused.
    for bad in ["read", "", "has space"] {
        let cfg = ToolsConfig { search_name: bad.into(), ..Default::default() };
        assert!(Catalog::build(&cfg, None).is_err(), "{bad:?} was accepted");
    }
    // Without the search tool, the prompt does not name it.
    let off = ToolsConfig { disabled: vec!["grep".into()], ..Default::default() };
    assert_eq!(off.search_tool(), None);
    assert_eq!(cfg.search_tool(), Some("search"));
}
