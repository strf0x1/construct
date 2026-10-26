"""convention_lint.py on small git repos: each rule fires on what a change
adds, and never on what the tree already had.

    python3 -m unittest discover -s bench -p 'test_*.py'     (part of `cargo xtask check`)
"""

import subprocess
import tempfile
import unittest
from pathlib import Path

import convention_lint as cl

TOOL = 'pub struct Read;\nimpl Read {\n    pub const NAME: &str = "read";\n}\npub struct Query;\nimpl Query {\n    pub const NAME: &str = "journal_query";\n}\n'
MANIFEST = '[package]\nname = "construct"\n\n[dependencies]\nanyhow.workspace = true\nlibc = "0.2"\n'
ROOT_MANIFEST = '[workspace]\nmembers = ["crates/*"]\n\n[workspace.dependencies]\nanyhow = "1"\n'


class Repo:
    def __init__(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = Path(self.tmp.name)
        self.git("init", "-q")
        self.write("crates/construct/src/tools/read.rs", TOOL)
        self.write("crates/construct/Cargo.toml", MANIFEST)
        self.write("Cargo.toml", ROOT_MANIFEST)
        self.write("crates/construct/src/old.rs", "fn old() { x.unwrap(); }\n")
        self.commit()

    def git(self, *a):
        subprocess.run(["git", *a], cwd=self.dir, check=True, capture_output=True)

    def write(self, path, text):
        p = self.dir / path
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)

    def commit(self):
        self.git("add", "-A")
        self.git("-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-qm", "c")

    def lint(self, **kw):
        return cl.lint(self.dir, **kw)

    def rules(self, **kw):
        return sorted((f["rule"], f["file"], f["line"]) for f in self.lint(**kw)["findings"])


class Rules(unittest.TestCase):
    def setUp(self):
        self.r = Repo()

    def tearDown(self):
        self.r.tmp.cleanup()

    def test_a_clean_change_has_no_findings(self):
        self.r.write("crates/construct/src/new.rs", "//! What it is for.\n\npub fn f() -> Option<u8> {\n    None\n}\n")
        out = self.r.lint()
        self.assertEqual(out["findings"], [])
        self.assertEqual(out["violations"], 0)
        self.assertEqual(out["diff"], {"files": 1, "added": 5, "removed": 0, "test_added": 0})

    def test_unwrap_outside_tests_only(self):
        self.r.write(
            "crates/construct/src/new.rs",
            "//! Doc.\n"
            "fn a() { x.unwrap(); }\n"  # 2: flagged
            "fn b() { let s = \"don't .unwrap() me\"; } // nor .unwrap() here\n"
            "// the lock cannot be poisoned: nothing panics while holding it\n"
            "fn c() { m.lock().unwrap(); }\n"  # 5: allowed, comment above
            "fn d() { m.lock().unwrap(); }\n"  # 6: flagged, no comment
            "#[cfg(test)]\n"
            "mod tests {\n"
            "    fn t() { x.unwrap(); }\n"
            "}\n"
            "fn e() { y.unwrap(); }\n",  # 11: flagged, after the test module
        )
        self.r.write("crates/construct/tests/contract.rs", "fn t() { x.unwrap(); }\n")
        self.r.write("crates/construct/src/journal/tests.rs", "fn t() { x.unwrap(); }\n")
        got = [(f, l) for rule, f, l in self.r.rules() if rule == "unwrap"]
        self.assertEqual(got, [("crates/construct/src/new.rs", 2), ("crates/construct/src/new.rs", 6), ("crates/construct/src/new.rs", 11)])

    def test_old_debt_is_not_the_changes_fault(self):
        p = self.r.dir / "crates/construct/src/old.rs"
        p.write_text(p.read_text() + "fn newer() {}\n")
        self.assertEqual(self.r.rules(), [])

    def test_wire_names_outside_tools(self):
        self.r.write(
            "crates/construct/src/agent.rs",
            "//! Doc.\n"
            'fn a(n: &str) -> bool { n == "read" }\n'  # 2: matching on a tool name
            'fn b() { let what = if w { "write" } else { "read" }; }\n'  # 3: a verb, not a match
            'fn c() { let t = "journal_query"; }\n'  # 4: distinctive: always a wire name
            'fn d(n: &str) { match n { "read" => {} _ => {} } }\n',  # 5: a match arm
        )
        self.r.write("crates/construct/src/tools/other.rs", '//! Doc.\nfn x(n: &str) -> bool { n == "read" }\n')
        self.r.write("crates/construct-dev/tests/e2e.rs", 'fn t() { call("journal_query"); }\n')
        got = [l for rule, f, l in self.r.rules() if rule == "wire-name"]
        self.assertEqual(got, [2, 4, 5])

    def test_new_module_needs_a_doc_paragraph(self):
        self.r.write("crates/construct/src/nodoc.rs", "use std::fs;\n")
        self.r.write("crates/construct/src/attr.rs", "#![allow(dead_code)]\n//! Doc after an inner attribute.\n")
        self.r.write("crates/construct/src/main.rs", "fn main() {}\n")
        self.r.write("crates/construct/tests/t.rs", "fn t() {}\n")
        self.assertEqual(self.r.rules(), [("module-doc", "crates/construct/src/nodoc.rs", 1)])

    def test_crossing_the_ceiling(self):
        self.r.write("crates/construct/src/grow.rs", "//! Doc.\n" + "fn f() {}\n" * 299)
        self.r.write("crates/construct/src/big.rs", "//! Doc.\n" + "fn f() {}\n" * 400)
        self.r.commit()
        for f in ("grow.rs", "big.rs", "tui/view.rs"):
            p = self.r.dir / "crates/construct/src" / f
            p.parent.mkdir(parents=True, exist_ok=True)
            p.write_text((p.read_text() if p.exists() else "//! Doc.\n") + "fn g() {}\n" * 310)
        # big.rs was already past it; view.rs may run longer
        self.assertEqual(self.r.rules(), [("file-length", "crates/construct/src/grow.rs", None)])

    def test_dependencies(self):
        p = self.r.dir / "crates/construct/Cargo.toml"
        p.write_text(p.read_text().replace('libc = "0.2"', 'libc = "0.3"') + 'regex = "1"\nserde.workspace = true\n[dev-dependencies]\ntempfile = { version = "3" }\n')
        root = self.r.dir / "Cargo.toml"
        root.write_text(root.read_text() + 'serde = "1"\n')
        got = sorted((f["rule"], f["message"].split("`")[1]) for f in self.r.lint()["findings"])
        # a version bump is not a new dependency
        self.assertEqual(got, [("dependency", "regex"), ("dependency", "tempfile"), ("workspace-dependency", "serde")])
        self.assertEqual(self.r.lint()["violations"], 2)

    def test_snapshot_edits(self):
        self.r.write("crates/construct-dev/tests/snapshots/s__a.snap", "old\n")
        self.r.write("Cargo.lock", "# lock\n")
        self.r.commit()
        self.r.write("crates/construct-dev/tests/snapshots/s__a.snap", "new\n")
        self.r.write("Cargo.lock", "# lock\nmore\n")
        self.r.write("crates/construct-dev/tests/snapshots/s__new.snap", "a new test's golden\n")
        out = self.r.lint()
        self.assertEqual([f["rule"] for f in out["findings"]], ["snap"])
        # generated files count as changed files, not changed lines
        self.assertEqual(out["diff"], {"files": 3, "added": 0, "removed": 0, "test_added": 0})

    def test_between_two_revisions(self):
        self.r.write("crates/construct/src/a.rs", "//! Doc.\nfn a() { x.unwrap(); }\n")
        self.r.commit()
        self.r.write("crates/construct/src/b.rs", "uncommitted, not linted\n")
        out = self.r.lint(base="HEAD~1", head="HEAD")
        self.assertEqual([(f["rule"], f["line"]) for f in out["findings"]], [("unwrap", 2)])
        self.assertEqual(out["head"], "HEAD")


class Parsing(unittest.TestCase):
    def test_parse_diff_numbers_added_lines_in_the_new_file(self):
        diff = (
            "diff --git a/x.rs b/x.rs\n--- a/x.rs\n+++ b/x.rs\n"
            "@@ -2,0 +3,2 @@\n+one\n+two\n@@ -9 +11 @@\n-old\n+new\n"
            "diff --git a/y.rs b/y.rs\nnew file mode 100644\n--- /dev/null\n+++ b/y.rs\n@@ -0,0 +1 @@\n+y\n"
        )
        got = cl.parse_diff(diff)
        self.assertEqual(got["x.rs"], {"status": "M", "added": {3: "one", 4: "two", 11: "new"}, "removed": 1})
        self.assertEqual(got["y.rs"]["status"], "A")

    def test_test_lines_cover_the_cfg_test_module_only(self):
        text = "fn a() {}\n#[cfg(test)]\nmod tests {\n    fn s() { let b = \"}\"; }\n}\nfn z() {}\n#[cfg(test)]\nmod more;\nfn y() {}\n"
        self.assertEqual(cl.test_lines(text), {2, 3, 4, 5, 7, 8})


if __name__ == "__main__":
    unittest.main()
