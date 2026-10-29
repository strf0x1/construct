//! The arch gate on small trees: each case yields exactly the expected lines.

use super::edges::sources;
use super::*;
use std::path::PathBuf;

struct Tree(PathBuf);

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A crate with `lib`, `a`, `b`, `c` (`a` uses `b`) and a table allowing just that.
fn tree(name: &str) -> Tree {
    let dir = std::env::temp_dir().join(format!("arch-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for d in ["docs", "crates/construct/src/tools"] {
        std::fs::create_dir_all(dir.join(d)).unwrap();
    }
    let t = Tree(dir);
    write(&t, "crates/construct/src/lib.rs", "//! l\npub mod a;\npub mod b;\npub mod c;\n");
    write(&t, "crates/construct/src/a.rs", "//! a\nuse crate::b::X;\n");
    write(&t, "crates/construct/src/b.rs", "//! b\n");
    write(&t, "crates/construct/src/c.rs", "//! c\n");
    table(&t, "");
    t
}

fn write(t: &Tree, path: &str, text: &str) {
    let p = t.0.join(path);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, text).unwrap();
}

/// The base table, with `extra` appended to `[modules.a]`.
fn table(t: &Tree, extra: &str) {
    let text = format!("[modules.lib]\n[modules.b]\n[modules.c]\n[modules.a]\nmay_use = [\"b\"]\n{extra}\n");
    write(t, "docs/architecture.toml", &text);
}

fn check(t: &Tree) -> Vec<String> {
    boundaries(&t.0, &sources(&t.0))
}

#[test]
fn the_base_tree_is_clean() {
    assert_eq!(check(&tree("clean")), Vec::<String>::new());
}

#[test]
fn a_forbidden_edge_gives_its_reason() {
    let t = tree("forbidden");
    write(&t, "crates/construct/src/a.rs", "//! a\nuse crate::b::X;\n\nfn f() { crate::c::go() }\n");
    table(&t, "must_not_use = [{ module = \"c\", reason = \"c is for the UI only\" }]");
    assert_eq!(
        check(&t),
        ["crates/construct/src/a.rs:4: a must not use c (docs/architecture.toml): c is for the UI only"]
    );
}

#[test]
fn a_new_edge_says_what_to_add() {
    let t = tree("new");
    write(&t, "crates/construct/src/c.rs", "//! c\nuse crate::b::Y;\n");
    assert_eq!(
        check(&t),
        [
            "crates/construct/src/c.rs:2: c uses b, which docs/architecture.toml does not list; add \"b\" to may_use under [modules.c] so the new dependency shows in the diff"
        ]
    );
}

#[test]
fn every_branch_of_a_nested_group_is_an_edge() {
    let t = tree("group");
    write(&t, "crates/construct/src/a.rs", "//! a\nuse crate::{\n    b::{X, y::Z},\n    c::{self, W},\n};\n");
    assert_eq!(
        check(&t),
        [
            "crates/construct/src/a.rs:4: a uses c, which docs/architecture.toml does not list; add \"c\" to may_use under [modules.a] so the new dependency shows in the diff"
        ]
    );
}

#[test]
fn an_inline_path_is_an_edge() {
    let t = tree("inline");
    write(&t, "crates/construct/src/a.rs", "//! a\nuse crate::b::X;\nfn f() -> u8 {\n    crate::c::f()\n}\n");
    assert_eq!(
        check(&t),
        [
            "crates/construct/src/a.rs:4: a uses c, which docs/architecture.toml does not list; add \"c\" to may_use under [modules.a] so the new dependency shows in the diff"
        ]
    );
}

#[test]
fn super_climbing_to_the_root_is_an_edge() {
    let t = tree("super");
    write(&t, "crates/construct/src/b/mod.rs", "//! b\nmod deep;\n");
    write(
        &t,
        "crates/construct/src/b/deep.rs",
        "//! d\nuse super::Own;\nmod m {\n    use super::super::super::c::X;\n}\n",
    );
    std::fs::remove_file(t.0.join("crates/construct/src/b.rs")).unwrap();
    assert_eq!(
        check(&t),
        [
            "crates/construct/src/b/deep.rs:4: b uses c, which docs/architecture.toml does not list; add \"c\" to may_use under [modules.b] so the new dependency shows in the diff"
        ]
    );
}

#[test]
fn comments_strings_and_tests_are_not_edges() {
    let t = tree("prose");
    let text = r###"//! a: see crate::c::f
use crate::b::X;
/* crate::c::f /* nested crate::c */ */
const S: &str = "crate::c::f";
const R: &str = r#"crate::c::f "quoted""#;
const C: char = '"';
fn f<'a>(x: &'a str) -> &'a str { x } // crate::c
macro_rules! m { () => { $crate::c::f() } }
#[cfg(test)]
mod tests {
    use crate::c::f;
}
"###;
    write(&t, "crates/construct/src/a.rs", text);
    assert_eq!(check(&t), Vec::<String>::new());
}

#[test]
fn an_unknown_module_needs_an_entry() {
    let t = tree("unknown");
    write(&t, "crates/construct/src/d/mod.rs", "//! d\nuse crate::{b::X, c::Y};\n");
    assert_eq!(
        check(&t),
        [
            "crates/construct/src/d/mod.rs: module `d` has no entry in docs/architecture.toml; add `[modules.d]` with `may_use = [\"b\", \"c\"]`"
        ]
    );
}

#[test]
fn a_stale_edge_and_a_gone_module_are_deleted() {
    let t = tree("stale");
    table(&t, "[modules.gone]\n");
    write(&t, "crates/construct/src/c.rs", "//! c\nuse crate::b::X;\n");
    let text = std::fs::read_to_string(t.0.join("docs/architecture.toml")).unwrap();
    write(&t, "docs/architecture.toml", &text.replace("[modules.c]", "[modules.c]\nmay_use = [\"a\", \"b\"]"));
    assert_eq!(
        check(&t),
        [
            "docs/architecture.toml: [modules.c] may_use lists \"a\", but c no longer uses a; delete \"a\" from that list",
            "docs/architecture.toml: [modules.gone] names a module that no longer exists under crates/construct/src; delete that entry",
        ]
    );
}

#[test]
fn a_deny_rule_without_a_reason_fails() {
    let t = tree("reason");
    write(&t, "docs/architecture.toml", &{
        let base = "[modules.lib]\n[modules.c]\n[modules.a]\nmay_use = [\"b\"]\nmust_not_use = [{ module = \"c\" }]\n";
        format!("{base}[modules.b]\nused_only_by = {{ modules = [\"a\"], reason = \" \" }}\n")
    });
    assert_eq!(
        check(&t),
        [
            "docs/architecture.toml: [modules.a] must_not_use \"c\" has no reason; every deny rule says why it exists",
            "docs/architecture.toml: [modules.b] used_only_by has no reason; every deny rule says why it exists",
        ]
    );
}

#[test]
fn only_the_named_modules_may_use_a_guarded_one() {
    let t = tree("only");
    write(&t, "crates/construct/src/c.rs", "//! c\nuse crate::b::X;\n");
    let text = "[modules.lib]\n[modules.a]\nmay_use = [\"b\"]\n[modules.c]\nmay_use = [\"b\"]\n";
    write(
        &t,
        "docs/architecture.toml",
        &format!("{text}[modules.b]\nused_only_by = {{ modules = [\"a\"], reason = \"b is a's\" }}\n"),
    );
    assert_eq!(check(&t), ["crates/construct/src/c.rs:2: only a may use b (docs/architecture.toml), not c: b is a's"]);
}

#[test]
fn a_scanner_that_finds_nothing_fails() {
    let t = tree("empty");
    write(&t, "crates/construct/src/a.rs", "//! a\n");
    write(&t, "docs/architecture.toml", "[modules.lib]\n[modules.a]\n[modules.b]\n[modules.c]\n");
    assert_eq!(
        check(&t),
        [
            "crates/construct/src: found no edge between modules; the arch scanner is broken (or the tree is empty), so nothing was checked"
        ]
    );
}

/// A tree for the ratchets: one tool named `shell`, `a` spelling it once, and
/// `big.rs` with `n` code lines and a test module that does not count.
fn ratchets(name: &str, n: usize, baseline: &str) -> Vec<String> {
    let t = tree(name);
    write(&t, "crates/construct/src/tools/shell.rs", "//! s\nimpl Shell {\n    pub const NAME: &str = \"shell\";\n}\n");
    write(
        &t,
        "crates/construct/src/a.rs",
        "//! a\nconst S: &str = \"shell\";\n#[cfg(test)]\nconst T: &str = \"shell\";\n",
    );
    let tests = "#[cfg(test)]\nmod tests {\n".to_string() + &"    // t\n".repeat(50) + "}\n";
    write(&t, "crates/construct/src/big.rs", &("//! big\n".to_string() + &"fn f() {}\n".repeat(n - 1) + &tests));
    write(&t, "docs/ratchets.toml", baseline);
    ratchet::problems(&t.0, &sources(&t.0))
}

#[test]
fn a_ratchet_that_rises_fails() {
    let base = "wire_name_literals = 0\n[lines]\n\"crates/construct/src/big.rs\" = 301\n";
    assert_eq!(
        ratchets("rise", 305, base),
        [
            "docs/ratchets.toml: crates/construct/src/big.rs grew from 301 to 305 non-test lines; split it by concern instead of adding to it",
            "docs/ratchets.toml: wire_name_literals grew from 0 to 1: crates/construct/src/a.rs:2 \"shell\"; use the tool's `NAME` const instead (AGENTS.md convention 5)",
        ]
    );
    assert_eq!(
        ratchets("new-long", 302, "wire_name_literals = 1\n"),
        [
            "crates/construct/src/big.rs: 302 non-test lines, over the 300-line soft limit; split it by concern, or if it is one concern add `\"crates/construct/src/big.rs\" = 302` under [lines] in docs/ratchets.toml"
        ]
    );
}

#[test]
fn a_ratchet_that_falls_asks_for_the_lower_baseline() {
    let base = "wire_name_literals = 3\n[lines]\n\"crates/construct/src/big.rs\" = 320\n";
    assert_eq!(
        ratchets("fall", 310, base),
        [
            "docs/ratchets.toml: crates/construct/src/big.rs shrank from 320 to 310 non-test lines; lower the baseline to 310",
            "docs/ratchets.toml: wire_name_literals dropped from 3 to 1; lower the baseline to 1",
        ]
    );
    assert_eq!(
        ratchets("under", 300, "wire_name_literals = 1\n[lines]\n\"crates/construct/src/big.rs\" = 301\n"),
        [
            "docs/ratchets.toml: crates/construct/src/big.rs is no longer over the 300-line soft limit (or is gone); delete its line"
        ]
    );
}
