//! Contract: `editor = "hashline"`, through the catalog as the agent calls
//! it. The edit- and read-tool cases of omp-hashline-strict's suite: anchors
//! shown are anchors accepted, any stale anchor rejects the whole batch with
//! fresh ones, nothing is relocated, and the file round-trips byte for byte.

use construct::config::{MemoryConfig, Paths, SandboxConfig, ToolsConfig};
use construct::memory::Store;
use construct::sandbox::Policy;
use construct::tools::edit::hashline::anchors;
use construct::tools::{Catalog, Ctx, ToolError};
use serde_json::{Value, json};
use std::path::PathBuf;

struct Fixture {
    root: PathBuf,
    ctx: Ctx,
    catalog: Catalog,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("construct-hashline-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let paths = Paths { home: root.join("home") };
        let memory = Store::new(&paths, &MemoryConfig::default());
        let sandbox = Policy::new(&SandboxConfig::default(), &work, &paths);
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let ctx = Ctx::new(work, paths, memory, rx, None, sandbox);
        let catalog = Catalog::build(&ToolsConfig { editor: "hashline".into(), ..Default::default() }, None).unwrap();
        Self { root, ctx, catalog }
    }

    fn file(&self, name: &str, content: &str) -> String {
        std::fs::write(self.ctx.cwd.join(name), content).unwrap();
        name.to_string()
    }

    fn content(&self, name: &str) -> String {
        std::fs::read_to_string(self.ctx.cwd.join(name)).unwrap()
    }

    /// Anchors for the file as it is now, the way read would show them.
    fn anchors(&self, name: &str) -> anchors::AnchorMap {
        let text = self.content(name);
        anchors::build(construct::tools::edit::hashline::fsio::split(&text).0, 2)
    }

    async fn edit(&self, path: &str, edits: Value) -> Result<(String, Value), ToolError> {
        self.call("edit", json!({ "path": path, "edits": edits })).await
    }

    async fn call(&self, name: &str, args: Value) -> Result<(String, Value), ToolError> {
        let o = self.catalog.call(&self.ctx, name, &args.to_string()).await?;
        Ok((o.text, o.data.unwrap_or(Value::Null)))
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

/// Every `LINE:HASH|content` row in a tool result must hold for the file now.
fn assert_rows_are_fresh(text: &str, map: &anchors::AnchorMap) {
    let mut seen = 0;
    for row in text.lines() {
        let Some((anchor, content)) = row.split_once('|') else { continue };
        let Some(a) = anchors::parse(anchor).filter(|_| anchor.contains(':')) else { continue };
        assert_eq!(a.hash, map.hashes[a.line - 1], "stale anchor at line {}: {row}", a.line);
        assert_eq!(content, map.lines[a.line - 1]);
        seen += 1;
    }
    assert!(seen > 0, "no anchored rows in {text:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_batch_applies_and_returns_fresh_anchors() {
    let f = Fixture::new("batch");
    let p = f.file("a.txt", "alpha\nbeta\ngamma\ndelta\n");
    let m = f.anchors(&p);
    let (text, data) = f
        .edit(
            &p,
            json!([
                { "op": "set", "anchor": m.anchor(2), "text": "BETA" },
                { "op": "insert_after", "anchor": m.anchor(3), "text": "gamma-2" },
            ]),
        )
        .await
        .unwrap();
    assert_eq!(f.content(&p), "alpha\nBETA\ngamma\ngamma-2\ndelta\n");
    assert_eq!((data["status"].as_str(), data["line_delta"].as_i64()), (Some("applied"), Some(1)));
    assert!(text.contains("5 lines (was 4)"), "{text}");
    assert_rows_are_fresh(&text, &f.anchors(&p));
}

#[tokio::test(flavor = "multi_thread")]
async fn read_rows_are_anchors_edit_accepts() {
    let f = Fixture::new("read");
    let p = f.file("a.rs", "fn main() {\n    let x = 1;\n\n    println!(\"{x}\");\n}\n");
    let (text, _) = f.call("read", json!({ "path": p })).await.unwrap();
    assert!(text.starts_with(&format!("1:{}|fn main() {{\n", f.anchors(&p).hashes[0])), "{text}");
    assert_rows_are_fresh(&text, &f.anchors(&p));
    let second = text.lines().nth(1).unwrap().split_once('|').unwrap().0.to_string();
    f.edit(&p, json!([{ "op": "set", "anchor": second, "text": "    let x = 2;" }])).await.unwrap();
    assert!(f.content(&p).contains("let x = 2;"));

    let (text, _) = f.call("read", json!({ "path": p, "offset": 2, "limit": 2 })).await.unwrap();
    assert!(text.starts_with("2:"), "{text}");
    assert!(text.contains("5 lines total; showing 2-3"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cut_short_line_has_no_anchor() {
    let f = Fixture::new("long");
    let p = f.file("a.txt", &format!("short\n{}\n", "x".repeat(3000)));
    let (text, _) = f.call("read", json!({ "path": p })).await.unwrap();
    let row = text.lines().nth(1).unwrap();
    assert!(row.starts_with("2|x"), "{row}");
    assert!(text.contains("line 2 is over 2000 chars"), "{text}");
}

#[tokio::test(flavor = "multi_thread")]
async fn one_stale_anchor_rejects_the_whole_batch() {
    let f = Fixture::new("stale");
    let p = f.file("a.txt", "one\ntwo\nthree\nfour\nfive\n");
    let m = f.anchors(&p);
    f.file(&p, "one\nTWO-CHANGED\nthree\nfour\nfive\n");
    let e = f
        .edit(
            &p,
            json!([
                { "op": "set", "anchor": m.anchor(1), "text": "ONE" },
                { "op": "set", "anchor": m.anchor(2), "text": "TWO" },
            ]),
        )
        .await
        .unwrap_err();
    assert_eq!(f.content(&p), "one\nTWO-CHANGED\nthree\nfour\nfive\n", "nothing applied");
    assert_eq!(e.code, "ANCHOR_MISMATCH");
    assert!(e.message.starts_with("REJECTED: 1 of 2 edit(s)"), "{}", e.message);
    assert!(e.message.contains("TWO-CHANGED"), "the window shows current content");
    assert_rows_are_fresh(&e.message, &f.anchors(&p));
    let data = e.data.unwrap();
    assert_eq!(data["status"], "rejected");
    assert_eq!(data["errors"][0]["code"], "ANCHOR_MISMATCH");
    assert_eq!(data["errors"][0]["index"], 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_right_hash_on_the_wrong_line_is_never_relocated() {
    let f = Fixture::new("relocate");
    let p = f.file("a.txt", "aaa\nbbb\nccc\nddd\n");
    let m = f.anchors(&p);
    let e =
        f.edit(&p, json!([{ "op": "set", "anchor": format!("2:{}", m.hashes[2]), "text": "X" }])).await.unwrap_err();
    assert_eq!(e.code, "ANCHOR_MISMATCH");
    assert!(e.message.contains("not at line 2"), "{}", e.message);
    assert_eq!(f.content(&p), "aaa\nbbb\nccc\nddd\n");

    // Two identical braces: occurrence 1's anchor on line 4 must fail.
    let p = f.file("b.txt", "if (a) {\n}\nif (b) {\n}\n");
    let m = f.anchors(&p);
    let e = f.edit(&p, json!([{ "op": "set", "anchor": format!("4:{}", m.hashes[1]), "text": "} // end" }])).await;
    assert_eq!(e.unwrap_err().code, "ANCHOR_MISMATCH");
    assert_eq!(f.content(&p), "if (a) {\n}\nif (b) {\n}\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn other_edit_shapes_get_a_redirect() {
    let f = Fixture::new("legacy");
    let p = f.file("a.txt", "x\n");
    for args in
        [json!({ "path": p, "old_string": "x", "new_string": "y" }), json!({ "path": p, "input": "PUT 1.=1:\nnew" })]
    {
        let e = f.call("edit", args).await.unwrap_err();
        assert_eq!(e.code, "legacy_input");
        assert!(e.message.contains("\"edits\""), "{}", e.message);
    }
    assert_eq!(f.content(&p), "x\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn missing_and_empty_files_point_at_write() {
    let f = Fixture::new("missing");
    let e = f.edit("nope.txt", json!([{ "op": "delete", "anchor": "1:aa" }])).await.unwrap_err();
    assert_eq!(e.code, "not_found");
    assert!(e.message.contains("write tool"));
    let p = f.file("empty.txt", "");
    let e = f.edit(&p, json!([{ "op": "delete", "anchor": "1:aa" }])).await.unwrap_err();
    assert_eq!(e.code, "empty_file");
    assert!(e.message.contains("write tool"));
    let e = f.edit(&f.file("a.txt", "x\n"), json!([])).await.unwrap_err();
    assert!(e.message.contains("non-empty"), "{}", e.message);
}

#[tokio::test(flavor = "multi_thread")]
async fn blank_lines_and_trailing_newlines_in_text() {
    let f = Fixture::new("blank");
    let p = f.file("a.txt", "a\n\n\nb\n");
    let m = f.anchors(&p);
    f.edit(
        &p,
        json!([{ "op": "set", "anchor": m.anchor(2), "text": "was-blank" }, { "op": "delete", "anchor": m.anchor(3) }]),
    )
    .await
    .unwrap();
    assert_eq!(f.content(&p), "a\nwas-blank\nb\n");

    let p = f.file("b.txt", "start\nend\n");
    let m = f.anchors(&p);
    f.edit(&p, json!([{ "op": "insert_after", "anchor": m.anchor(1), "text": "mid-1\nmid-2\n" }])).await.unwrap();
    assert_eq!(f.content(&p), "start\nmid-1\nmid-2\nend\n", "one trailing newline in text is a separator");
}

#[tokio::test(flavor = "multi_thread")]
async fn echoed_anchors_are_stripped_audibly() {
    let f = Fixture::new("echo");
    let p = f.file("a.txt", "keep\nreplace-me\n");
    let m = f.anchors(&p);
    let (text, data) = f
        .edit(&p, json!([{ "op": "set", "anchor": m.anchor(2), "text": format!("{}|replaced", m.anchor(2)) }]))
        .await
        .unwrap();
    assert_eq!(f.content(&p), "keep\nreplaced\n");
    assert_eq!(data["echo_stripped"], json!([0]));
    assert!(text.contains("note: removed echoed anchor prefix"), "{text}");

    let m = f.anchors(&p);
    let (text, data) = f.edit(&p, json!([{ "op": "set", "anchor": m.anchor(1), "text": "KEEP" }])).await.unwrap();
    assert!(data.get("echo_stripped").is_none());
    assert!(!text.contains("echoed anchor"));
}

#[tokio::test(flavor = "multi_thread")]
async fn identical_replacement_is_honestly_applied() {
    let f = Fixture::new("same");
    let p = f.file("a.txt", "same\n");
    let m = f.anchors(&p);
    let (text, _) = f.edit(&p, json!([{ "op": "set", "anchor": m.anchor(1), "text": "same" }])).await.unwrap();
    assert!(text.starts_with("ok: applied 1 edit"), "{text}");
    assert_eq!(f.content(&p), "same\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn crlf_files_stay_crlf() {
    let f = Fixture::new("crlf");
    let p = f.file("a.txt", "a\r\nb\r\n");
    let m = f.anchors(&p);
    f.edit(&p, json!([{ "op": "set", "anchor": m.anchor(2), "text": "B" }])).await.unwrap();
    assert_eq!(f.content(&p), "a\r\nB\r\n");
}

#[tokio::test(flavor = "multi_thread")]
async fn knobs_come_from_tools_hashline() {
    let mut cfg = ToolsConfig { editor: "hashline".into(), ..Default::default() };
    cfg.hashline.strip_echoes = false;
    cfg.hashline.window_radius = 1;
    let mut f = Fixture::new("knobs");
    f.catalog = Catalog::build(&cfg, None).unwrap();
    let p = f.file("a.txt", &(1..=20).map(|i| format!("l{i}\n")).collect::<String>());
    let m = f.anchors(&p);
    f.edit(&p, json!([{ "op": "set", "anchor": m.anchor(2), "text": format!("{}|kept", m.anchor(2)) }])).await.unwrap();
    assert!(f.content(&p).contains(&format!("{}|kept", m.anchor(2))), "strip_echoes = false writes the text verbatim");
    let e = f.edit(&p, json!([{ "op": "delete", "anchor": "10:zz" }])).await.unwrap_err();
    let window: Vec<&str> = e.message.lines().filter(|l| l.contains('|') && l.contains(':')).collect();
    assert_eq!(window.len(), 3, "window_radius = 1: {window:?}");
}
