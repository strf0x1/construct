//! Contract: no file tool reads, lists, searches or writes through a link
//! that leaves the granted roots, and an approved outside path still works.
//! The farm is a workspace full of links to a sibling directory (a plain
//! link, a chain, a directory link, `..` after a link); every path goes
//! through every tool under both editors. Each call must be refused or leave
//! the outside tree exactly as it was.

use construct::config::{MemoryConfig, Paths, SandboxConfig, ToolsConfig};
use construct::memory::Store;
use construct::sandbox::{Policy, open};
use construct::tools::{Catalog, Ctx};
use serde_json::{Value, json};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

struct Farm {
    root: PathBuf,
    ctx: Ctx,
    catalog: Catalog,
}

impl Farm {
    fn new(name: &str, editor: &str) -> Self {
        let root = std::env::temp_dir().join(format!("construct-farm-{name}-{editor}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let root = {
            std::fs::create_dir_all(&root).unwrap();
            root.canonicalize().unwrap()
        };
        let (work, outside) = (root.join("work"), root.join("outside"));
        std::fs::create_dir_all(work.join("sub")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("secret.txt"), "secret\n").unwrap();
        std::fs::write(work.join("ok.txt"), "fine\n").unwrap();
        std::fs::write(work.join("sub/in.txt"), "inside\n").unwrap();
        symlink("../outside/secret.txt", work.join("file")).unwrap();
        symlink("../outside", work.join("out")).unwrap();
        symlink("chain2", work.join("chain1")).unwrap();
        symlink("file", work.join("chain2")).unwrap();
        symlink("sub", work.join("lnk_in")).unwrap();
        let paths = Paths { home: root.join("home") };
        let memory = Store::new(&paths, &MemoryConfig::default());
        let sandbox = Policy::new(&SandboxConfig::default(), &work, &paths);
        let (_tx, rx) = tokio::sync::watch::channel(false);
        let ctx = Ctx::new(work, paths, memory, rx, None, sandbox);
        let catalog = Catalog::build(&ToolsConfig { editor: editor.into(), ..Default::default() }, None).unwrap();
        Self { root, ctx, catalog }
    }

    fn work(&self) -> PathBuf {
        self.root.join("work")
    }
    fn outside(&self) -> PathBuf {
        self.root.join("outside")
    }

    async fn call(&self, name: &str, args: Value) -> Result<String, String> {
        self.catalog.call(&self.ctx, name, &args.to_string()).await.map(|o| o.text).map_err(|e| e.message)
    }

    /// The outside tree as a list of (name, content).
    fn outside_state(&self) -> Vec<(String, String)> {
        let mut v: Vec<_> = std::fs::read_dir(self.outside())
            .unwrap()
            .flatten()
            .map(|e| (e.file_name().to_string_lossy().into(), std::fs::read_to_string(e.path()).unwrap_or_default()))
            .collect();
        v.sort();
        v
    }
}

impl Drop for Farm {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn paths(f: &Farm) -> Vec<String> {
    let w = f.work();
    let w = w.display();
    let o = f.outside();
    [
        "file".to_string(),
        "chain1".into(),
        "out/secret.txt".into(),
        "out".into(),
        "out/new.txt".into(),
        "out/sub/new.txt".into(),
        "file/x".into(),
        // `..` after a link lands in the root's parent, then back out to the farm's sibling.
        "out/../outside/secret.txt".into(),
        format!("{w}/out/../outside/secret.txt"),
        format!("{w}/lnk_in/../out/secret.txt"),
        o.join("secret.txt").display().to_string(),
        format!("{w}/../outside/secret.txt"),
    ]
    .into()
}

fn edits(editor: &str, path: &str) -> Value {
    if editor == "hashline" {
        json!({ "path": path, "edits": [{ "op": "set", "anchor": "1:aa", "text": "pwned" }] })
    } else {
        json!({ "path": path, "old_string": "secret", "new_string": "pwned" })
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn nothing_leaves_the_workspace_through_a_link() {
    for editor in ["replace", "hashline"] {
        let f = Farm::new("farm", editor);
        let before = f.outside_state();
        for p in paths(&f) {
            let calls = [
                ("read", json!({ "path": p })),
                ("write", json!({ "path": p, "content": "pwned\n" })),
                ("edit", edits(editor, &p)),
                ("grep", json!({ "pattern": "secret", "path": p })),
                ("glob", json!({ "pattern": "*", "path": p })),
            ];
            for (tool, args) in calls {
                if let Ok(out) = f.call(tool, args).await {
                    assert!(!out.contains("secret"), "{editor} {tool} {p} leaked: {out}");
                }
                assert_eq!(f.outside_state(), before, "{editor} {tool} {p} changed the outside tree");
            }
        }
        // Searching the workspace itself never follows the farm's links out.
        let out = f.call("grep", json!({ "pattern": "secret" })).await.unwrap_or_default();
        assert!(!out.contains("secret"), "{editor} grep over the workspace: {out}");
        let out = f.call("glob", json!({ "pattern": "**/*.txt" })).await.unwrap();
        assert!(out.contains("ok.txt") && !out.contains("secret"), "{editor} glob: {out}");
        // The listing shows the links, but does not open them.
        let out = f.call("read", json!({ "path": f.work().display().to_string() })).await.unwrap();
        assert!(out.contains("ok.txt"), "{out}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_link_that_stays_inside_still_works() {
    for editor in ["replace", "hashline"] {
        let f = Farm::new("inside", editor);
        let out = f.call("read", json!({ "path": "lnk_in/in.txt" })).await.unwrap();
        assert!(out.contains("inside"), "{out}");
        f.call("write", json!({ "path": "lnk_in/new.txt", "content": "n\n" })).await.unwrap();
        assert_eq!(std::fs::read_to_string(f.work().join("sub/new.txt")).unwrap(), "n\n");
        let out = f.call("read", json!({ "path": "lnk_in" })).await.unwrap();
        assert!(out.contains("in.txt"), "{out}");
    }
}

/// The race, as a stress: one thread keeps replacing a file with a link to a
/// secret outside and back, atomically, while tools read and write it. Each
/// result is whichever the swap allowed, never the outside content.
#[tokio::test(flavor = "multi_thread")]
async fn a_file_flipping_to_a_link_is_never_followed() {
    let f = Farm::new("flip", "replace");
    let (swap, target) = (f.work().join("swap"), f.outside().join("secret.txt"));
    std::fs::write(&swap, "plain\n").unwrap();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let flipper = {
        let (stop, swap, target, tmp) = (stop.clone(), swap.clone(), target.clone(), f.work().join("swap.tmp"));
        std::thread::spawn(move || {
            let mut link = true;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let _ = std::fs::remove_file(&tmp);
                if link {
                    symlink(&target, &tmp).unwrap();
                } else {
                    std::fs::write(&tmp, "plain\n").unwrap();
                }
                std::fs::rename(&tmp, &swap).unwrap();
                link = !link;
            }
        })
    };
    for _ in 0..1500 {
        if let Ok(out) = f.call("read", json!({ "path": "swap" })).await {
            assert!(!out.contains("secret"), "read followed the link: {out}");
        }
        let _ = f.call("write", json!({ "path": "swap", "content": "w\n" })).await;
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    flipper.join().unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "secret\n");
}

fn granted(f: &Farm, read: &[&Path], write: &[&Path]) -> Policy {
    let to = |v: &[&Path]| v.iter().map(|p| p.to_path_buf()).collect::<Vec<_>>();
    f.ctx.policy().widened(&to(read), &to(write))
}

#[test]
fn an_approved_outside_path_works_but_only_as_far_as_the_grant() {
    let f = Farm::new("grant", "replace");
    let secret = f.outside().join("secret.txt");
    // The folder was approved: its files read and write, links out of it do not.
    let policy = granted(&f, &[&f.outside()], &[&f.outside()]);
    assert_eq!(open::read_file(&policy, &secret).unwrap(), b"secret\n");
    open::write_file(&policy, &f.outside().join("deep/er/new.txt"), b"n").unwrap();
    open::replace_file(&policy, &secret, b"changed\n", 0o600).unwrap();
    assert_eq!(std::fs::read_to_string(&secret).unwrap(), "changed\n");
    symlink(f.root.join("work/ok.txt"), f.outside().join("back")).unwrap();
    assert_eq!(open::read_file(&policy, &f.outside().join("back")).unwrap(), b"fine\n");
    // Only the one file was approved: it opens, its neighbours do not.
    std::fs::write(&secret, "secret\n").unwrap();
    std::fs::write(f.outside().join("other.txt"), "o").unwrap();
    let policy = granted(&f, &[&secret], &[]);
    assert_eq!(open::read_file(&policy, &secret).unwrap(), b"secret\n");
    let err = open::read_file(&policy, &f.outside().join("other.txt")).unwrap_err();
    assert!(err.contains("blocked by the sandbox"), "{err}");
    let err = open::write_file(&policy, &secret, b"x").unwrap_err();
    assert!(err.contains("blocked by the sandbox"), "{err}");
}

#[test]
fn replace_keeps_the_mode_and_leaves_no_temp_file() {
    use std::os::unix::fs::PermissionsExt;
    let f = Farm::new("replace", "replace");
    let policy = f.ctx.policy();
    let p = f.work().join("ok.txt");
    open::replace_file(&policy, &p, b"new\n", 0o750).unwrap();
    assert_eq!(std::fs::read_to_string(&p).unwrap(), "new\n");
    assert_eq!(std::fs::metadata(&p).unwrap().permissions().mode() & 0o777, 0o750);
    assert!(!std::fs::read_dir(f.work()).unwrap().flatten().any(|e| e.file_name().to_string_lossy().ends_with(".tmp")));
}
