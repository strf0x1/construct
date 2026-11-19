//! The arch gate on small trees: each case yields exactly the expected lines.

use super::edges::krates;
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
    write(&t, "crates/construct/Cargo.toml", "[package]\nname = \"construct\"\n");
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

/// The base table, with `extra` appended to `[crates.construct.modules.a]`.
fn table(t: &Tree, extra: &str) {
    let text = format!(
        "[crates.construct]\n[crates.construct.modules.lib]\n[crates.construct.modules.b]\n[crates.construct.modules.c]\n[crates.construct.modules.a]\nmay_use = [\"b\"]\n{extra}\n"
    );
    write(t, "docs/architecture.toml", &text);
}

fn check(t: &Tree) -> Vec<String> {
    boundaries(&t.0, &krates(&t.0))
}

/// A second crate, `other`, with `lib` and `a`; its table entry is appended.
fn other(t: &Tree, a: &str, entry: &str) {
    write(t, "crates/other/Cargo.toml", "[package]\nname = \"other\"\n");
    write(t, "crates/other/src/lib.rs", "//! l\npub mod a;\n");
    write(t, "crates/other/src/a.rs", a);
    let text = std::fs::read_to_string(t.0.join("docs/architecture.toml")).unwrap();
    write(
        t,
        "docs/architecture.toml",
        &format!("{text}[crates.other]\n{entry}\n[crates.other.modules.lib]\n[crates.other.modules.a]\n"),
    );
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
            "crates/construct/src/c.rs:2: c uses b, which docs/architecture.toml does not list; add \"b\" to may_use under [crates.construct.modules.c] so the new dependency shows in the diff"
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
            "crates/construct/src/a.rs:4: a uses c, which docs/architecture.toml does not list; add \"c\" to may_use under [crates.construct.modules.a] so the new dependency shows in the diff"
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
            "crates/construct/src/a.rs:4: a uses c, which docs/architecture.toml does not list; add \"c\" to may_use under [crates.construct.modules.a] so the new dependency shows in the diff"
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
            "crates/construct/src/b/deep.rs:4: b uses c, which docs/architecture.toml does not list; add \"c\" to may_use under [crates.construct.modules.b] so the new dependency shows in the diff"
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
            "crates/construct/src/d/mod.rs: module `d` has no entry in docs/architecture.toml; add `[crates.construct.modules.d]` with `may_use = [\"b\", \"c\"]`"
        ]
    );
}

#[test]
fn a_stale_edge_and_a_gone_module_are_deleted() {
    let t = tree("stale");
    table(&t, "[crates.construct.modules.gone]\n");
    write(&t, "crates/construct/src/c.rs", "//! c\nuse crate::b::X;\n");
    let text = std::fs::read_to_string(t.0.join("docs/architecture.toml")).unwrap();
    write(
        &t,
        "docs/architecture.toml",
        &text.replace("[crates.construct.modules.c]", "[crates.construct.modules.c]\nmay_use = [\"a\", \"b\"]"),
    );
    assert_eq!(
        check(&t),
        [
            "docs/architecture.toml: [crates.construct.modules.c] may_use lists \"a\", but c no longer uses a; delete \"a\" from that list",
            "docs/architecture.toml: [crates.construct.modules.gone] names a module that no longer exists under crates/construct/src; delete that entry",
        ]
    );
}

#[test]
fn a_deny_rule_without_a_reason_fails() {
    let t = tree("reason");
    write(&t, "docs/architecture.toml", &{
        let base = "[crates.construct]\n[crates.construct.modules.lib]\n[crates.construct.modules.c]\n[crates.construct.modules.a]\nmay_use = [\"b\"]\nmust_not_use = [{ module = \"c\" }]\n";
        format!("{base}[crates.construct.modules.b]\nused_only_by = {{ modules = [\"a\"], reason = \" \" }}\n")
    });
    assert_eq!(
        check(&t),
        [
            "docs/architecture.toml: [crates.construct.modules.a] must_not_use \"c\" has no reason; every deny rule says why it exists",
            "docs/architecture.toml: [crates.construct.modules.b] used_only_by has no reason; every deny rule says why it exists",
        ]
    );
}

#[test]
fn only_the_named_modules_may_use_a_guarded_one() {
    let t = tree("only");
    write(&t, "crates/construct/src/c.rs", "//! c\nuse crate::b::X;\n");
    let text = "[crates.construct]\n[crates.construct.modules.lib]\n[crates.construct.modules.a]\nmay_use = [\"b\"]\n[crates.construct.modules.c]\nmay_use = [\"b\"]\n";
    write(
        &t,
        "docs/architecture.toml",
        &format!("{text}[crates.construct.modules.b]\nused_only_by = {{ modules = [\"a\"], reason = \"b is a's\" }}\n"),
    );
    assert_eq!(check(&t), ["crates/construct/src/c.rs:2: only a may use b (docs/architecture.toml), not c: b is a's"]);
}

#[test]
fn a_scanner_that_finds_nothing_fails() {
    let t = tree("empty");
    write(&t, "crates/construct/src/a.rs", "//! a\n");
    write(
        &t,
        "docs/architecture.toml",
        "[crates.construct]\n[crates.construct.modules.lib]\n[crates.construct.modules.a]\n[crates.construct.modules.b]\n[crates.construct.modules.c]\n",
    );
    assert_eq!(
        check(&t),
        [
            "crates/*/src: found no edge between modules or crates; the arch scanner is broken (or the tree is empty), so nothing was checked"
        ]
    );
}

#[test]
fn the_own_lib_name_is_a_module_edge_not_a_crate_edge() {
    let t = tree("ownname");
    // main.rs is its own crate root: `construct::b` names the lib's module,
    // and so does the same path from a bin-only module (construct-dev's shape).
    write(&t, "crates/construct/src/main.rs", "//! m\nuse construct::b::X;\nmod binmod;\n");
    write(&t, "crates/construct/src/binmod.rs", "//! bin\nuse construct::c::Y;\n");
    let text = std::fs::read_to_string(t.0.join("docs/architecture.toml")).unwrap();
    write(
        &t,
        "docs/architecture.toml",
        &format!(
            "{text}[crates.construct.modules.main]\nmay_use = [\"b\"]\n[crates.construct.modules.binmod]\nmay_use = [\"c\"]\n"
        ),
    );
    assert_eq!(check(&t), Vec::<String>::new());
}

#[test]
fn an_unlisted_cross_crate_edge_says_what_to_add() {
    let t = tree("cross-new");
    other(&t, "//! a\nuse construct::b::X;\n", "");
    assert_eq!(
        check(&t),
        [
            "crates/other/src/a.rs:2: crate other uses crate construct, which docs/architecture.toml does not list; add \"construct\" to may_use_crates under [crates.other] so the new dependency shows in the diff"
        ]
    );
}

#[test]
fn a_listed_cross_crate_edge_that_is_gone_fails() {
    let t = tree("cross-stale");
    other(&t, "//! a\n", "may_use_crates = [\"construct\"]");
    assert_eq!(
        check(&t),
        [
            "docs/architecture.toml: [crates.other] may_use_crates lists \"construct\", but other no longer uses construct; delete \"construct\" from that list"
        ]
    );
}

#[test]
fn a_listed_cross_crate_edge_is_clean() {
    let t = tree("cross-ok");
    other(&t, "//! a\nuse construct::{b::X, c::Y};\n", "may_use_crates = [\"construct\"]");
    assert_eq!(check(&t), Vec::<String>::new());
}

#[test]
fn a_crate_deny_rule_gives_its_reason() {
    let t = tree("cross-deny");
    other(
        &t,
        "//! a\nuse construct::b::X;\n",
        "must_not_use_crates = [{ crate = \"construct\", reason = \"the dev tools never link the agent\" }]",
    );
    assert_eq!(
        check(&t),
        [
            "crates/other/src/a.rs:2: other must not use construct (docs/architecture.toml): the dev tools never link the agent"
        ]
    );
}

#[test]
fn a_module_scoped_crate_deny_gives_its_reason() {
    let t = tree("cross-deny-module");
    other(
        &t,
        "//! a\nuse construct::b::X;\n",
        "may_use_crates = [\"construct\"]\nmust_not_use_crates = [{ crate = \"construct\", module = \"b\", reason = \"no secrets on the UI path\" }]",
    );
    assert_eq!(
        check(&t),
        [
            "crates/other/src/a.rs:2: other must not use construct::b (docs/architecture.toml): no secrets on the UI path"
        ]
    );
}

#[test]
fn a_module_scoped_deny_leaves_other_modules_free() {
    let t = tree("cross-deny-module-ok");
    other(
        &t,
        "//! a\nuse construct::c::Y;\n",
        "may_use_crates = [\"construct\"]\nmust_not_use_crates = [{ crate = \"construct\", module = \"b\", reason = \"no secrets on the UI path\" }]",
    );
    assert_eq!(check(&t), Vec::<String>::new());
}

#[test]
fn a_grouped_use_trips_a_module_scoped_deny_once() {
    let t = tree("cross-deny-module-group");
    other(
        &t,
        "//! a\nuse construct::{b::X, c::Y};\n",
        "may_use_crates = [\"construct\"]\nmust_not_use_crates = [{ crate = \"construct\", module = \"b\", reason = \"no secrets on the UI path\" }]",
    );
    assert_eq!(
        check(&t),
        [
            "crates/other/src/a.rs:2: other must not use construct::b (docs/architecture.toml): no secrets on the UI path"
        ]
    );
}

#[test]
fn a_crate_deny_on_a_grouped_use_still_fires_once() {
    let t = tree("cross-deny-group");
    other(
        &t,
        "//! a\nuse construct::{b::X, c::Y};\n",
        "must_not_use_crates = [{ crate = \"construct\", reason = \"the dev tools never link the agent\" }]",
    );
    assert_eq!(
        check(&t),
        [
            "crates/other/src/a.rs:2: other must not use construct (docs/architecture.toml): the dev tools never link the agent"
        ]
    );
}

#[test]
fn a_stale_module_in_a_crate_deny_fails() {
    let t = tree("cross-deny-module-stale");
    other(&t, "//! a\n", "must_not_use_crates = [{ crate = \"construct\", module = \"ghost\", reason = \"gone\" }]");
    assert_eq!(
        check(&t),
        [
            "docs/architecture.toml: [crates.other] must_not_use_crates \"construct\" module \"ghost\" names no module of construct; delete it"
        ]
    );
}

#[test]
fn a_module_scoped_deny_without_a_reason_fails() {
    let t = tree("cross-deny-module-reason");
    other(&t, "//! a\n", "must_not_use_crates = [{ crate = \"construct\", module = \"b\" }]");
    assert_eq!(
        check(&t),
        [
            "docs/architecture.toml: [crates.other] must_not_use_crates \"construct::b\" has no reason; every deny rule says why it exists"
        ]
    );
}

#[test]
fn only_the_named_crates_may_use_a_guarded_one() {
    let t = tree("cross-only");
    other(&t, "//! a\nuse construct::b::X;\n", "may_use_crates = [\"construct\"]");
    let text = std::fs::read_to_string(t.0.join("docs/architecture.toml")).unwrap();
    write(
        &t,
        "docs/architecture.toml",
        &text.replace(
            "[crates.construct]\n",
            "[crates.construct]\nused_only_by = { crates = [\"construct-bin\"], reason = \"only the binary chooses the agent\" }\n",
        ),
    );
    assert_eq!(
        check(&t),
        [
            "crates/other/src/a.rs:2: only construct-bin may use construct (docs/architecture.toml), not other: only the binary chooses the agent"
        ]
    );
}

#[test]
fn a_crate_without_an_entry_and_a_gone_crate_entry_fail() {
    let t = tree("cross-entries");
    other(&t, "//! a\n", "");
    let text = std::fs::read_to_string(t.0.join("docs/architecture.toml")).unwrap();
    let text = text.split("[crates.other]").next().unwrap_or_default().to_string();
    write(&t, "docs/architecture.toml", &format!("{text}[crates.gone]\n"));
    assert_eq!(
        check(&t),
        [
            "crates/other/src: crate `other` has no entry in docs/architecture.toml; add `[crates.other]`",
            "crates/other/src/a.rs: module `a` has no entry in docs/architecture.toml; add `[crates.other.modules.a]` with `may_use = []`",
            "crates/other/src/lib.rs: module `lib` has no entry in docs/architecture.toml; add `[crates.other.modules.lib]` with `may_use = []`",
            "docs/architecture.toml: [crates.gone] names a crate that no longer exists under crates/; delete that entry",
        ]
    );
}

#[test]
fn a_crate_deny_rule_without_a_reason_or_target_fails() {
    let t = tree("cross-reason");
    other(&t, "//! a\n", "must_not_use_crates = [{ crate = \"construct\" }, { crate = \"ghost\", reason = \"gone\" }]");
    assert_eq!(
        check(&t),
        [
            "docs/architecture.toml: [crates.other] must_not_use_crates \"construct\" has no reason; every deny rule says why it exists",
            "docs/architecture.toml: [crates.other] must_not_use_crates \"ghost\" names no workspace crate; delete it",
        ]
    );
}

/// A tree for the ratchets: `big.rs` with `n` code lines and a test module
/// that does not count.
fn ratchets(name: &str, n: usize, baseline: &str) -> Vec<String> {
    let t = tree(name);
    let tests = "#[cfg(test)]\nmod tests {\n".to_string() + &"    // t\n".repeat(50) + "}\n";
    write(&t, "crates/construct/src/big.rs", &("//! big\n".to_string() + &"fn f() {}\n".repeat(n - 1) + &tests));
    write(&t, "docs/ratchets.toml", baseline);
    ratchet::problems(&t.0)
}

#[test]
fn a_ratchet_that_rises_fails() {
    assert_eq!(
        ratchets("rise", 305, "[lines]\n\"crates/construct/src/big.rs\" = 301\n"),
        [
            "docs/ratchets.toml: crates/construct/src/big.rs grew from 301 to 305 non-test lines; split it by concern instead of adding to it"
        ]
    );
    assert_eq!(
        ratchets("new-long", 302, "[lines]\n"),
        [
            "crates/construct/src/big.rs: 302 non-test lines, over the 300-line soft limit; split it by concern, or if it is one concern add `\"crates/construct/src/big.rs\" = 302` under [lines] in docs/ratchets.toml"
        ]
    );
}

#[test]
fn a_ratchet_that_falls_asks_for_the_lower_baseline() {
    assert_eq!(
        ratchets("fall", 310, "[lines]\n\"crates/construct/src/big.rs\" = 320\n"),
        [
            "docs/ratchets.toml: crates/construct/src/big.rs shrank from 320 to 310 non-test lines; lower the baseline to 310"
        ]
    );
    assert_eq!(
        ratchets("under", 300, "[lines]\n\"crates/construct/src/big.rs\" = 301\n"),
        [
            "docs/ratchets.toml: crates/construct/src/big.rs is no longer over the 300-line soft limit (or is gone); delete its line"
        ]
    );
}
