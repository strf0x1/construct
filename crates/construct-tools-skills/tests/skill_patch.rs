//! Contract: patching a skill never goes through the editor tool.
//! `skill_manage patch` uses `tools::edit::replace::replace_once`, so a change
//! to the editor tool can't change how the skill library is maintained.

use construct_core::config::{MemoryConfig, Paths, SandboxConfig, ToolsConfig};
use construct_core::memory::Store;
use construct_core::sandbox::Policy;
use construct_core::tools::{Catalog, Ctx, Deck, ToolSources};
use construct_tools_skills::Skills;
use serde_json::json;

const SKILL: &str = "---\nname: crate-release\ndescription: Releasing a Rust crate\n---\n\n1. Run \u{201C}cargo package\u{201D}\n2. Tag it\n";

/// Patch a fresh skill; returns the tool result and the file.
async fn patch(old: &str, new: &str) -> (Result<String, String>, String) {
    let root = std::env::temp_dir().join(format!("construct-skill-patch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let paths = Paths { home: root.join("home") };
    let memory = Store::new(&paths, &MemoryConfig::default());
    let sandbox = Policy::new(&SandboxConfig::default(), &work, &paths);
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let ctx = Ctx::new(work, paths.clone(), memory, rx, sandbox);
    let sources = ToolSources::default().with(Skills);
    let catalog = Catalog::build_extended(&ToolsConfig::default(), None, &Deck::default(), &sources).unwrap();
    let call = |args: serde_json::Value| {
        let (catalog, ctx) = (&catalog, &ctx);
        async move { catalog.call(ctx, "skill_manage", &args.to_string()).await }
    };
    call(json!({ "action": "create", "name": "crate-release", "content": SKILL })).await.unwrap();
    let r = call(json!({ "action": "patch", "name": "crate-release", "old_string": old, "new_string": new }))
        .await
        .map(|o| o.text.split(" (global: ").next().unwrap().to_string())
        .map_err(|e| e.message);
    let file = std::fs::read_to_string(paths.skills_dir().join("crate-release/SKILL.md")).unwrap();
    let _ = std::fs::remove_dir_all(&root);
    (r, file)
}

#[tokio::test(flavor = "multi_thread")]
async fn skill_patching_uses_the_shared_replacement() {
    let cases = [
        // straight quotes match the file's curly ones
        ("Run \"cargo package\"", "Run `cargo package --locked`"),
        // ambiguous: refused, file untouched
        ("\n", "\n\n"),
        // missing: refused, file untouched
        ("3. Publish", "3. Publish it"),
    ];
    let (ok, file) = patch(cases[0].0, cases[0].1).await;
    assert_eq!(ok.unwrap(), "patched crate-release/SKILL.md");
    assert!(file.contains("1. Run `cargo package --locked`"), "{file}");
    let (err, file) = patch(cases[1].0, cases[1].1).await;
    assert!(err.unwrap_err().contains("matches"));
    assert!(file.contains("\u{201C}cargo package\u{201D}"));
}
