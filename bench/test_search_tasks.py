"""Model-free tests of the search suite (bench/tasks/search-*): each scorer
passes the answer a careful agent would leave, fails the answers a careless
one would, and fails an untouched fixture. A scorer that is wrong would read
as a model failure in the bench, so it is checked here first.

    python3 -m unittest discover -s bench -p 'test_*.py'     (part of `cargo xtask check`)
"""

import json
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

import process
import tasklib


def expected(task):
    """The answer check.sh embeds (computed from the fixture by search_fixtures.py)."""
    m = re.search(r"EXPECTED = json\.loads\((.*)\)\n", (task.dir / "check.sh").read_text())
    return json.loads(eval(m.group(1)))  # a Python string literal written by repr()


def tracked(work):
    return subprocess.run(["git", "ls-files"], cwd=work, capture_output=True, text=True, check=True).stdout.split()


def rewrite(work, pattern, repl, skip=("CHANGELOG.md",)):
    for p in tracked(work):
        f = work / p
        if p in skip or not p.endswith((".py", ".md")):
            continue
        text = f.read_text()
        new = re.sub(pattern, repl, text)
        if new != text:
            f.write_text(new)


class SearchTasks(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)

    def work(self, name):
        task = tasklib.load(name)
        work = Path(self.tmp.name) / name
        tasklib.materialize(task, work, {}, warm=False)
        return task, work

    def check(self, task, work):
        return tasklib.check(task, work)

    def passes(self, name, solve, wrongs):
        """`solve` leaves the right answer; each of `wrongs` a believable wrong one."""
        task, work = self.work(name)
        untouched = self.check(task, work)
        self.assertFalse(all(untouched.values()), f"{name}: an untouched fixture passed: {untouched}")
        solve(task, work)
        right = self.check(task, work)
        self.assertTrue(right and all(right.values()), f"{name}: the right answer failed: {right}")
        for i, wrong in enumerate(wrongs):
            task, work = self.work(name)
            for p in list(work.glob("answer.txt")) + list(work.glob("callers.txt")):
                p.unlink()
            wrong(task, work)
            got = self.check(task, work)
            self.assertFalse(all(got.values()), f"{name}: wrong answer #{i} passed: {got}")

    def test_definition(self):
        w = lambda text: lambda t, d: (d / "answer.txt").write_text(text)
        self.passes(
            "search-definition",
            lambda t, d: (d / "answer.txt").write_text(expected(t)["answer"] + "\n"),
            [w("vendor/ledgerkit/batch.py:1"), w("pkg/billing/ledger/batches.py:6"), w("build/lib/pkg/billing/ledger/batch.py:14"), w("pkg/billing/ledger/batch.py:1")],
        )

    def test_callers(self):
        callers = lambda t: expected(t)["callers"]
        put = lambda lines: lambda t, d: (d / "callers.txt").write_text("\n".join(lines) + "\n")
        self.passes(
            "search-callers",
            lambda t, d: put(callers(t))(t, d),
            [
                lambda t, d: put(callers(t)[1:])(t, d),  # misses one
                lambda t, d: put(callers(t) + ["dist/web/pay.js"])(t, d),  # generated output
                lambda t, d: put(callers(t) + ["web/client/refund.js"])(t, d),  # the v2 sibling
                lambda t, d: put(callers(t) + ["pkg/orders/notes.py"])(t, d),  # a comment
            ],
        )

    def test_config(self):
        put = lambda text: lambda t, d: (d / "answer.txt").write_text(text)
        self.passes(
            "search-config",
            lambda t, d: put("config/overrides.d/20-tune.toml 15000\n")(t, d),
            [put("config/production.toml 12000"), put("config/overrides.d/30-staging-only.toml 60000"), put("config/overrides.d/20-tune.toml 12000"), put("config/base.toml 5000")],
        )

    def test_count(self):
        want = lambda t: expected(t)
        put = lambda text: lambda t, d: (d / "answer.txt").write_text(text)
        self.passes(
            "search-count",
            lambda t, d: put(f"{want(t)['count']} {want(t)['top']}\n")(t, d),
            [
                lambda t, d: put(f"{want(t)['count'] + 20} {want(t)['top']}")(t, d),  # counted build/
                lambda t, d: put(f"{want(t)['count'] + 4} {want(t)['top']}")(t, d),  # counted near-miss tags
                lambda t, d: put(f"{want(t)['count']} pkg/billing/invoice_run.py")(t, d),  # wrong file
            ],
        )

    def test_signature(self):
        want = lambda t: expected(t)["functions"]
        put = lambda lines: lambda t, d: (d / "answer.txt").write_text("\n".join(lines) + "\n")
        self.passes(
            "search-signature",
            lambda t, d: put(want(t))(t, d),
            [
                lambda t, d: put(want(t)[1:])(t, d),
                lambda t, d: put(want(t) + ["pkg/reports/parse_row.py:parse_row"])(t, d),  # three named parameters
                lambda t, d: put(want(t) + ["pkg/search/parse_query.py:parse_flags"])(t, d),  # a lambda
            ],
        )

    def test_rename(self):
        def solve(t, d):
            rewrite(d, r"\bfetch_user_record\b", "load_user_record")

        def naive_sed(t, d):  # what `sed -i s/fetch_user_record/load_user_record/g` does
            rewrite(d, r"fetch_user_record", "load_user_record", skip=())

        def forgets_docs(t, d):
            rewrite(d, r"\bfetch_user_record\b", "load_user_record")
            (d / "docs/guide.md").write_text("Call `fetch_user_record(user_id)`.\n")

        def only_definition(t, d):
            store = d / "pkg/users/store.py"
            store.write_text(store.read_text().replace("def fetch_user_record(", "def load_user_record("))

        task, work = self.work("search-rename")
        self.assertFalse(all(self.check(task, work).values()))
        solve(task, work)
        right = self.check(task, work)
        self.assertTrue(all(right.values()), right)
        for wrong in (naive_sed, forgets_docs, only_definition):
            task, work = self.work("search-rename")
            wrong(task, work)
            got = self.check(task, work)
            self.assertFalse(all(got.values()), f"{wrong.__name__} passed: {got}")
        # the sed trap in particular trips the "different function" checks, not just the tests
        task, work = self.work("search-rename")
        naive_sed(task, work)
        got = self.check(task, work)
        self.assertFalse(got["plural_untouched"] and got["prefetch_untouched"], got)

    def test_no_fixture_file_is_hidden_from_git(self):
        """A fixture's own .gitignore (build/, dist/, vendor/: the decoys) also hides those
        files from this repo, where they must be added with `git add -f`. Without them a
        fresh checkout gets easier tasks, and only a checkout shows it."""
        root = Path(tasklib.TASKS).parent.parent
        probe = subprocess.run(["git", "rev-parse", "--is-inside-work-tree"], cwd=root, capture_output=True, text=True)
        if probe.returncode != 0:
            self.skipTest("not a git checkout")
        # (A wildcard pathspec matches nothing here, so list everything and filter.)
        listing = subprocess.run(["git", "ls-files", "--others", "--ignored", "--exclude-standard"], cwd=root, capture_output=True, text=True, check=True).stdout.split()
        hidden = [p for p in listing if re.match(r"bench/tasks/search-[^/]+/fixture/", p)]
        self.assertEqual(hidden, [], "ignored by git; run `git add -f` on them")

    def test_the_committed_fixtures_are_what_the_generator_makes(self):
        """Stale fixtures would score against answers the tree no longer has."""
        import search_fixtures

        out = Path(self.tmp.name) / "regen"
        real, search_fixtures.TASKS = search_fixtures.TASKS, out
        try:
            for f in (search_fixtures.definition, search_fixtures.callers, search_fixtures.config, search_fixtures.rename, search_fixtures.count, search_fixtures.signature, *__import__('search_fixtures_hard').ALL):
                f()
        finally:
            search_fixtures.TASKS = real
        names = sorted(p.name for p in out.iterdir())
        self.assertEqual(names, sorted(p.name for p in tasklib.TASKS.glob("search-*")))
        for name in names:
            mine = {str(p.relative_to(out / name)): p.read_bytes() for p in (out / name).rglob("*") if p.is_file()}
            theirs = {str(p.relative_to(tasklib.TASKS / name)): p.read_bytes() for p in (tasklib.TASKS / name).rglob("*") if p.is_file()}
            self.assertEqual(sorted(mine), sorted(theirs), name)
            self.assertEqual([k for k in mine if mine[k] != theirs[k]], [], f"{name}: run python3 bench/search_fixtures.py")


class HardTasks(SearchTasks):
    """The same checks for the tier whose trees are built per run."""

    def test_definition(self):  # inherited tests belong to the first tier
        self.skipTest("first tier")

    test_callers = test_config = test_count = test_signature = test_rename = test_the_committed_fixtures_are_what_the_generator_makes = test_definition

    def lines(self, name, key="routes"):
        return lambda t: expected(t)[key]

    def put(self, lines):
        return lambda t, d: (d / "answer.txt").write_text("\n".join(lines) + "\n")

    def test_trace(self):
        want = self.lines("search-hard-trace")
        self.passes(
            "search-hard-trace",
            lambda t, d: self.put(want(t))(t, d),
            [
                lambda t, d: self.put(want(t)[:1])(t, d),  # misses the route that reaches it through cancel
                lambda t, d: self.put(want(t) + ["POST /v1/refunds"])(t, d),  # the v1 action is a different one
                lambda t, d: self.put(want(t) + ["POST /v2/refunds/preview"])(t, d),  # the dry run writes nothing
            ],
        )

    def test_unused(self):
        want = self.lines("search-hard-unused", "unused")
        self.passes(
            "search-hard-unused",
            lambda t, d: self.put(want(t))(t, d),
            [
                lambda t, d: self.put(want(t)[1:])(t, d),
                lambda t, d: self.put(want(t) + ["legacy_swap_keys"])(t, d),  # used by other unused code, which counts
                lambda t, d: self.put(want(t) + ["legacy_strip_bom"])(t, d),  # named in a comment, which counts
            ],
        )

    def test_env(self):
        want = self.lines("search-hard-env", "vars")
        self.passes(
            "search-hard-env",
            lambda t, d: self.put(want(t))(t, d),
            [
                lambda t, d: self.put([v for v in want(t) if v != "SENTRY_DSN"])(t, d),  # the multi-line call
                lambda t, d: self.put([v for v in want(t) if v != "WORKER_COUNT"])(t, d),  # behind a helper
                lambda t, d: self.put(want(t) + ["NOT_OS_VAR"])(t, d),  # not the process environment
                lambda t, d: self.put(want(t) + ["PYTHONHASHSEED"])(t, d),  # a write
            ],
        )

    def test_errorcode(self):
        right = lambda t: expected(t)["answer"]
        put = lambda text: lambda t, d: (d / "answer.txt").write_text(text)
        self.passes(
            "search-hard-errorcode",
            lambda t, d: put(right(t))(t, d),
            [put("pkg/sync/limits.py:check_sync_quota"), put("pkg/exports/limits.py:check_export_quota"), put("pkg/errors/codes.py:ERR_QUOTA_EXCEEDED")],
        )

    def test_rename(self):
        def solve(t, d):
            rewrite(d, r"\bcompute_rate\b", "quote_rate")
            for p in ("config/handlers.json",):
                f = d / p
                f.write_text(re.sub(r"\bcompute_rate\b", "quote_rate", f.read_text()))

        def sed_everything(t, d):  # also renames compute_rate_card
            rewrite(d, r"compute_rate", "quote_rate")
            f = d / "config/handlers.json"
            f.write_text(f.read_text().replace("compute_rate", "quote_rate"))

        def forgets_config(t, d):
            rewrite(d, r"\bcompute_rate\b", "quote_rate")

        def forgets_strings(t, d):  # identifiers only: leaves "module:compute_rate", getattr(..., "compute_rate")
            rewrite(d, r"(?<![\"':.])\bcompute_rate\b(?![\"'])", "quote_rate")

        def only_definition(t, d):
            f = d / "pkg/pricing/rates.py"
            f.write_text(f.read_text().replace("def compute_rate(", "def quote_rate("))

        task, work = self.work("search-hard-rename")
        self.assertFalse(all(self.check(task, work).values()))
        solve(task, work)
        right = self.check(task, work)
        self.assertTrue(all(right.values()), right)
        for wrong in (sed_everything, forgets_config, forgets_strings, only_definition):
            task, work = self.work("search-hard-rename")
            wrong(task, work)
            got = self.check(task, work)
            self.assertFalse(all(got.values()), f"{wrong.__name__} passed: {got}")

    def test_trees_are_deterministic_and_big(self):
        import search_fixtures_hard as h

        for make in (h.trace_tree, h.unused_tree, h.env_tree, h.rename_tree, h.errorcode_tree):
            a, b = make(), make()
            self.assertEqual(a[0].files, b[0].files, make.__name__)
            self.assertEqual(a[1], b[1])
            self.assertGreater(len(a[0].files), 1400, make.__name__)


class SearchMetrics(unittest.TestCase):
    def ev(self, name, command=None, ok=True, code=None):
        return {"name": name, "ok": ok, "code": code, "args": {"command": command} if command else {}}

    def test_adoption_counts_searching_not_piping(self):
        events = [
            self.ev("grep"),
            self.ev("search"),
            self.ev("shell", "rg -n retry src/"),
            self.ev("shell", "cd pkg && grep -rn legacy_charge ."),
            self.ev("shell", "git grep -n foo"),
            self.ev("shell", "find . -name '*.py' | xargs grep -l foo"),
            self.ev("shell", "python3 -m unittest 2>&1 | grep FAIL"),  # a filter, not a search
            self.ev("shell", "find . -name '*.toml'"),
            self.ev("shell", "ls -la"),
            self.ev("read"),
        ]
        m = process.search_metrics(events)
        self.assertEqual(m["search"]["tool"], 2)
        self.assertEqual(m["search"]["shell"], 4)
        self.assertEqual(m["search"]["find"], 1, "only a find that is not also a content search")
        self.assertAlmostEqual(m["search"]["adoption"], 2 / 6)

    def test_failures_are_over_every_tool_and_no_matches_is_not_one(self):
        events = [self.ev("grep"), self.ev("grep", ok=False, code="error"), self.ev("read", ok=False, code="not_found"), self.ev("edit")]
        m = process.search_metrics(events)
        self.assertEqual(m["tool_failures"], {"calls": 4, "failed": 2, "rate": 0.5})
        self.assertEqual(m["search"]["tool_failed"], 1)
        self.assertEqual(m["failure_codes"], {"grep:error": 1, "read:not_found": 1})

    def test_a_run_that_never_searched_has_no_adoption(self):
        m = process.search_metrics([self.ev("shell", "ls"), self.ev("read")])
        self.assertNotIn("adoption", m["search"])
        self.assertEqual(process.search_metrics([])["tool_failures"]["rate"], 0.0)


if __name__ == "__main__":
    unittest.main()
