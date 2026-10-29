//! Contract: grep's output does not depend on thread timing. It lists matches
//! in path order, counts every match even past the cap, honours `include`,
//! `.gitignore` and hidden files, and says so when it truncates.

use construct::config::{MemoryConfig, Paths, SandboxConfig, ToolsConfig};
use construct::memory::Store;
use construct::sandbox::Policy;
use construct::tools::{Catalog, Ctx};
use serde_json::{Value, json};
use std::path::PathBuf;

struct Fixture {
    root: PathBuf,
    ctx: Ctx,
    catalog: Catalog,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("construct-grep-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let work = root.join("work");
        std::fs::create_dir_all(&work).unwrap();
        let paths = Paths { home: root.join("home") };
        let memory = Store::new(&paths, &MemoryConfig::default());
        let sandbox = Policy::new(&SandboxConfig::default(), &work, &paths);
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let ctx = Ctx::new(work, paths, memory, rx, None, sandbox);
        let catalog = Catalog::build(&ToolsConfig::default(), None).unwrap();
        Self { root, ctx, catalog }
    }

    fn put(&self, rel: &str, text: &str) {
        let p = self.ctx.cwd.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    async fn grep(&self, args: Value) -> String {
        self.catalog.call(&self.ctx, "grep", &args.to_string()).await.map(|o| o.text).unwrap()
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn lists_matches_in_path_order_and_counts_past_the_cap() {
    let f = Fixture::new("order");
    // 60 files in 6 directories, 5 matching lines each: 300 matches, 200 shown.
    for d in 0..6 {
        for n in 0..10 {
            f.put(&format!("d{d}/f{n}.txt"), "needle\nother\nneedle\nneedle x\nneedle\nneedle\n");
        }
    }
    let first = f.grep(json!({ "pattern": "needle" })).await;
    let lines: Vec<&str> = first.lines().collect();
    assert_eq!(lines.len(), 201, "{first}");
    assert_eq!(lines[0], "d0/f0.txt:1: needle");
    assert_eq!(lines[1], "d0/f0.txt:3: needle");
    assert_eq!(lines[200], "… 300 matches in 60 files, showing 200. Narrow the pattern or path.");
    let paths: Vec<&str> = lines[..200].iter().map(|l| l.split(':').next().unwrap()).collect();
    assert!(paths.windows(2).all(|w| w[0] <= w[1]), "not in path order: {first}");
    for _ in 0..10 {
        assert_eq!(f.grep(json!({ "pattern": "needle" })).await, first, "output changed between runs");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn honours_include_gitignore_hidden_and_a_single_file() {
    let f = Fixture::new("filters");
    f.put("src/a.rs", "let needle = 1;\n");
    f.put("src/b.py", "needle = 2\n");
    f.put("docs/n.md", "NEEDLE\n");
    f.put("target/out.rs", "needle\n");
    f.put(".hidden/x.rs", "needle\n");
    f.put(".gitignore", "target/\n");
    assert_eq!(f.grep(json!({ "pattern": "needle", "include": "*.rs" })).await, "src/a.rs:1: let needle = 1;");
    assert_eq!(
        f.grep(json!({ "pattern": "needle" })).await,
        "src/a.rs:1: let needle = 1;\nsrc/b.py:1: needle = 2",
        "target/ is ignored and .hidden/ is skipped"
    );
    assert!(f.grep(json!({ "pattern": "needle", "ignore_case": true })).await.contains("docs/n.md:1: NEEDLE"));
    assert_eq!(f.grep(json!({ "pattern": "needle", "path": "src/b.py" })).await, "src/b.py:1: needle = 2");
    assert_eq!(f.grep(json!({ "pattern": "nothing here" })).await, "no matches");
}
