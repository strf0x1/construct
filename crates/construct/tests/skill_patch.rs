//! Contract: patching a skill never goes through the editor tool. Whatever
//! `[tools] editor` selects, `skill_manage patch` uses
//! `tools::edit::replace::replace_once` and gives the same result, so an
//! editor experiment can't change how the skill library is maintained.
//! Add each new editor to `EDITORS`.

use construct::config::{MemoryConfig, Paths, SandboxConfig, ToolsConfig};
use construct::memory::Store;
use construct::sandbox::Policy;
use construct::tools::Catalog;
use construct::tools::Ctx;
use serde_json::json;

const EDITORS: &[&str] = &["replace", "hashline"];

const SKILL: &str = "---\nname: crate-release\ndescription: Releasing a Rust crate\n---\n\n1. Run \u{201C}cargo package\u{201D}\n2. Tag it\n";

/// Patch a fresh skill under `editor`; returns the tool result and the file.
async fn patch(editor: &str, old: &str, new: &str) -> (Result<String, String>, String) {
    let root = std::env::temp_dir().join(format!("construct-skill-patch-{editor}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let work = root.join("work");
    std::fs::create_dir_all(&work).unwrap();
    let paths = Paths { home: root.join("home") };
    let memory = Store::new(&paths, &MemoryConfig::default());
    let sandbox = Policy::new(&SandboxConfig::default(), &work, &paths);
    let (_tx, rx) = tokio::sync::watch::channel(false);
    let ctx = Ctx::new(work, paths.clone(), memory, rx, None, sandbox);
    let catalog = Catalog::build(&ToolsConfig { editor: editor.into(), ..Default::default() }, None).unwrap();
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
async fn skill_patching_is_the_same_under_every_editor() {
    let cases = [
        // straight quotes match the file's curly ones
        ("Run \"cargo package\"", "Run `cargo package --locked`"),
        // ambiguous: refused, file untouched
        ("\n", "\n\n"),
        // missing: refused, file untouched
        ("3. Publish", "3. Publish it"),
    ];
    for (old, new) in cases {
        let baseline = patch("replace", old, new).await;
        for editor in EDITORS {
            assert_eq!(patch(editor, old, new).await, baseline, "editor {editor}, patch {old:?}");
        }
    }
    let (ok, file) = patch("replace", cases[0].0, cases[0].1).await;
    assert_eq!(ok.unwrap(), "patched crate-release/SKILL.md");
    assert!(file.contains("1. Run `cargo package --locked`"), "{file}");
    let (err, file) = patch("replace", cases[1].0, cases[1].1).await;
    assert!(err.unwrap_err().contains("matches"));
    assert!(file.contains("\u{201C}cargo package\u{201D}"));
}
