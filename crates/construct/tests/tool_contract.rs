//! Contract: every tool in the catalog, checked without a model. A new tool
//! is covered by being in the catalog: its schema is well-formed, a call with
//! no arguments is refused in prose that names the missing one, file tools
//! refuse what the sandbox denies, and output stays under `OUTPUT_MAX`.
//! Each check runs under every editor in `EDITORS`.

use construct::config::{MemoryConfig, Paths, SandboxConfig, ToolsConfig};
use construct::memory::Store;
use construct::sandbox::Policy;
use construct::tools::{Catalog, Ctx, Group, OUTPUT_MAX};
use serde_json::{Value, json};
use std::path::PathBuf;

const EDITORS: &[&str] = &["replace", "hashline"];

struct Fixture {
    root: PathBuf,
    ctx: Ctx,
    catalog: Catalog,
}

impl Fixture {
    fn new(name: &str) -> Self {
        Self::with(name, "replace")
    }

    fn with(name: &str, editor: &str) -> Self {
        let root = std::env::temp_dir().join(format!("construct-contract-{name}-{editor}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(root.join("outside")).unwrap();
        std::fs::write(root.join("outside/secret.txt"), "secret\n").unwrap();
        let paths = Paths { home: root.join("home") };
        let memory = Store::new(&paths, &MemoryConfig::default());
        let sandbox = Policy::new(&SandboxConfig::default(), &work, &paths);
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let ctx = Ctx::new(work, paths, memory, rx, None, sandbox);
        let catalog = Catalog::build(&ToolsConfig { editor: editor.into(), ..Default::default() }, None).unwrap();
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

fn all_groups() -> Vec<Group> {
    vec![Group::Core, Group::Web, Group::Memory, Group::Skills, Group::SkillWrite, Group::History]
}

#[test]
fn every_schema_is_a_well_formed_function() {
    for editor in EDITORS {
        schemas_are_well_formed(&Fixture::with("schema", editor));
    }
}

fn schemas_are_well_formed(f: &Fixture) {
    let defs = f.catalog.definitions(&all_groups());
    assert_eq!(defs.len(), f.catalog.names().len(), "every tool belongs to a group");
    for (d, name) in defs.iter().zip(f.catalog.names()) {
        assert_eq!(d["type"], "function");
        assert_eq!(d["function"]["name"], name, "definition order follows the catalog");
        assert!(!d["function"]["description"].as_str().unwrap_or("").is_empty(), "{name} has a description");
        let params = &d["function"]["parameters"];
        assert_eq!(params["type"], "object", "{name}");
        let props = params["properties"].as_object().unwrap_or_else(|| panic!("{name} has properties"));
        for r in params["required"].as_array().unwrap() {
            assert!(props.contains_key(r.as_str().unwrap()), "{name}: required {r} is a property");
        }
        for (k, p) in props {
            assert!(p.get("type").is_some(), "{name}.{k} has a type");
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_arguments_are_refused_in_prose() {
    for editor in EDITORS {
        let f = Fixture::with("args", editor);
        for d in f.catalog.definitions(&all_groups()) {
            let name = d["function"]["name"].as_str().unwrap();
            let first = d["function"]["parameters"]["required"][0].as_str().unwrap();
            let (msg, _) = f.call(name, json!({})).await.expect_err(name);
            assert!(msg.contains(first), "{editor} {name}: {msg:?} should name {first}");
        }
    }
    let f = Fixture::new("args");
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
    for editor in EDITORS {
        sandbox_refusals(&Fixture::with("sandbox", editor), editor).await;
    }
}

async fn sandbox_refusals(f: &Fixture, editor: &str) {
    let secret = f.root.join("outside/secret.txt");
    let outside = f.root.join("outside");
    let s = |p: &PathBuf| p.display().to_string();
    let cases = [
        ("read", json!({ "path": s(&secret) })),
        ("write", json!({ "path": s(&outside.join("new.txt")), "content": "x" })),
        match editor {
            "hashline" => {
                ("edit", json!({ "path": s(&secret), "edits": [{ "op": "set", "anchor": "1:aa", "text": "public" }] }))
            }
            _ => ("edit", json!({ "path": s(&secret), "old_string": "secret", "new_string": "public" })),
        },
        ("grep", json!({ "pattern": "secret", "path": s(&outside) })),
        ("glob", json!({ "pattern": "*.txt", "path": s(&outside) })),
        ("shell", json!({ "command": "true", "workdir": s(&outside) })),
    ];
    for (name, args) in cases {
        let (msg, code) = f.call(name, args).await.expect_err(name);
        assert_eq!(code, "sandbox", "{editor} {name}: {msg}");
        assert!(msg.contains("blocked by the sandbox"), "{editor} {name}: {msg}");
    }
    assert_eq!(std::fs::read_to_string(&secret).unwrap(), "secret\n");
    assert!(!outside.join("new.txt").exists());
}

#[tokio::test(flavor = "multi_thread")]
async fn output_stays_under_the_cap() {
    for editor in EDITORS {
        output_capped(&Fixture::with("cap", editor), editor).await;
    }
}

async fn output_capped(f: &Fixture, editor: &str) {
    let line = format!("{}\n", "needle ".repeat(40));
    std::fs::write(f.ctx.cwd.join("big.txt"), line.repeat(3000)).unwrap();
    for (name, args) in [("read", json!({ "path": "big.txt" })), ("grep", json!({ "pattern": "needle" }))] {
        let out = f.call(name, args).await.unwrap();
        assert!(out.len() <= OUTPUT_MAX, "{editor} {name}: {} bytes", out.len());
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
    assert_eq!(out.kind, construct::tools::OutputKind::Diff);
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
    assert!(msg.contains("shell"), "{msg}");
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
    let cfg =
        ToolsConfig { disabled: vec!["write".into(), "web_search".into(), "web_fetch".into()], ..Default::default() };
    let c = Catalog::build(&cfg, None).unwrap();
    assert!(c.get("write").is_none());
    assert_eq!(c.group_of("web_fetch"), None, "a disabled tool can't switch its group on");
    let offered: Vec<String> = c.offerable(&Default::default()).into_iter().map(|(g, _)| g.label()).collect();
    assert!(!offered.contains(&"web".to_string()), "an empty group isn't offered: {offered:?}");
    let bad = ToolsConfig { disabled: vec!["wirte".into()], ..Default::default() };
    assert!(Catalog::build(&bad, None).err().unwrap().to_string().contains("unknown tool \"wirte\""));
    let bad = ToolsConfig { editor: "nope".into(), ..Default::default() };
    assert!(Catalog::build(&bad, None).is_err());
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
