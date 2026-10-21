"""Model-free tests of the bench's own machinery: tasks, scorers, compare.

    python3 -m unittest discover -s bench -p 'test_*.py'     (part of `cargo xtask check`)
"""

import random
import subprocess
import tempfile
import unittest
from pathlib import Path

import compare as cmp
import tasklib


def sh(cmd, cwd):
    subprocess.run(cmd, cwd=cwd, shell=True, check=True, capture_output=True)


SOLUTIONS = {
    # What a correct agent leaves behind, per task.
    "stats-median": lambda w, v: (
        (w / "stats.py").write_text(
            (w / "stats.py").read_text().replace("(len(values) - 1)", "len(values)")
            + "\n\ndef median(values):\n    if not values:\n        raise ValueError('median of empty list')\n"
            "    s = sorted(values)\n    m = len(s) // 2\n    return s[m] if len(s) % 2 else (s[m - 1] + s[m]) / 2\n"
        ),
        (w / "test_stats.py").write_text(
            (w / "test_stats.py").read_text().replace("from stats import mean", "from stats import mean, median")
            .replace("\n\nif __name__", "\n    def test_median(self):\n        self.assertEqual(median([3, 1, 2]), 2)\n\n\nif __name__")
        ),
        (w / "README.md").write_text("# stats\n\n## API\n\n`mean([1, 2])` and `median([1, 2, 3])`.\n"),
    ),
    "release-signing": lambda w, v: sh(
        f"printf '1.4.0\\n' > VERSION && sed -i 's/1.3.2/1.4.0/' stats/meta.toml && ./tools/stamp --sign-with {v['key']} 1.4.0", w
    ),
    "team-seed": lambda w, v: (
        sh(f"./tools/gen-fixtures --seed {v['seed']}", w),
        (w / "stats.py").write_text((w / "stats.py").read_text() + "\n\ndef median(values):\n    return sorted(values)[len(values) // 2]\n"),
        (w / "test_stats.py").write_text((w / "test_stats.py").read_text().replace("\n\nif __name__", "\n    def test_median(self):\n        from stats import median\n        self.assertEqual(median([1, 2, 3]), 2)\n\n\nif __name__")),
    ),
}
CHECK_ENV = {"team-seed": {"BENCH_SESSION": "median"}}


class Tasks(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp())

    def materialize(self, name):
        task = tasklib.load(name)
        vars = tasklib.make_vars(task, random.Random(7))
        work = self.tmp / name
        tasklib.materialize(task, work, vars)
        return task, vars, work

    def test_every_task_loads_and_fills_its_vars(self):
        for name in tasklib.names():
            with self.subTest(name):
                task, vars, work = self.materialize(name)
                self.assertTrue(task.turns or task.spec.get("sessions"), "a task needs turns or sessions")
                self.assertTrue((task.dir / "check.sh").exists())
                left = [p for p in work.rglob("*") if p.is_file() and ".git" not in p.parts and "{{" in p.read_text(errors="ignore")]
                self.assertEqual(left, [], "every {{var}} is filled")
                self.assertEqual(tasklib.changed_paths(work), [], "the fixture is committed")

    def test_var_templates(self):
        task = tasklib.load("team-seed")
        v = tasklib.make_vars(task, random.Random(1))
        self.assertRegex(v["seed"], r"^[0-9a-f]{12}$")
        import hashlib

        self.assertEqual(v["seed_check"], hashlib.sha256(v["seed"].encode()).hexdigest()[:16])
        self.assertRegex(tasklib.make_vars(tasklib.load("release-signing"), random.Random(1))["key"], r"^ledger-[0-9a-f]{4}$")

    def test_scorers_fail_the_fixture_and_pass_a_solution(self):
        for name, solve in SOLUTIONS.items():
            with self.subTest(name):
                task, vars, work = self.materialize(name)
                before = tasklib.check(task, work, CHECK_ENV.get(name))
                self.assertFalse(all(before.values()), f"untouched fixture must fail: {before}")
                solve(work, vars)
                after = tasklib.check(task, work, CHECK_ENV.get(name))
                self.assertTrue(after and all(after.values()), f"solution must pass: {after}")
                self.assertEqual(tasklib.unintended(task, work), [], "a solution changes only expected paths")

    def test_unintended_changes(self):
        task, _, work = self.materialize("stats-median")
        (work / "stats.py").write_text("changed\n")  # expected
        (work / "notes.txt").write_text("stray\n")  # not expected
        (work / "__pycache__").mkdir()
        (work / "__pycache__" / "stats.cpython-314.pyc").write_bytes(b"\0")  # ignored
        sh("git mv README.md README.rst", work)  # rename: both sides count
        self.assertEqual(tasklib.unintended(task, work), ["README.rst", "notes.txt"])

    def test_a_broken_scorer_is_a_failed_check(self):
        task, _, work = self.materialize("team-seed")
        self.assertEqual(tasklib.check(task, work), {"check_ran": False})  # BENCH_SESSION missing

    def test_budget(self):
        class Ep:
            recs = []

            def records(self):
                return self.recs

        task = tasklib.Task("t", Path("."), {"budget": {"max_requests": 2, "max_prompt_tokens": 100}})
        ep = Ep()
        b = tasklib.Budget(task, ep)
        ep.recs = [{"usage": {"prompt_tokens": 40}}] * 2
        self.assertIsNone(b())
        ep.recs = [{"usage": {"prompt_tokens": 40}}] * 3
        self.assertIn("requests", b())
        ep.recs = [{"usage": {"prompt_tokens": 60}}] * 2
        self.assertIn("prompt tokens", b())
        self.assertIsNone(tasklib.Budget(task, ep, since=1)())


class Compare(unittest.TestCase):
    def test_fisher(self):
        self.assertAlmostEqual(cmp.fisher(0, 3, 3, 3), 0.1)
        self.assertAlmostEqual(cmp.fisher(0, 6, 6, 6), 0.002164, places=5)
        self.assertAlmostEqual(cmp.fisher(2, 4, 2, 4), 1.0)

    def test_continuous_verdicts(self):
        tight_a, tight_b = [10, 11, 10, 11, 10], [20, 21, 20, 21, 20]
        self.assertEqual(cmp.verdict("wall_s", tight_a, tight_b)["verdict"], "worse")
        self.assertEqual(cmp.verdict("wall_s", tight_b, tight_a)["verdict"], "better")
        self.assertEqual(cmp.verdict("wall_s", [10, 30, 20], [12, 31, 19])["verdict"], "same")
        self.assertEqual(cmp.verdict("wall_s", [10, 11], [20, 21])["verdict"], "inconclusive")
        self.assertEqual(cmp.verdict("checks_passed", tight_a, tight_b)["verdict"], "better")
        # a count that happens to be 0 or 1 is a count, not a rate
        self.assertFalse(cmp.verdict("unintended_changes", [0, 1, 0], [1, 1, 1])["binary"])

    def test_rate_verdicts(self):
        self.assertEqual(cmp.verdict("success", [False] * 6, [True] * 6)["verdict"], "better")
        self.assertEqual(cmp.verdict("success", [False] * 3, [True] * 3)["verdict"], "inconclusive")
        self.assertEqual(cmp.verdict("success", [True] * 3, [True] * 3)["verdict"], "same")
        self.assertEqual(cmp.verdict("over_budget", [False] * 6, [True] * 6)["verdict"], "worse")
        self.assertEqual(cmp.verdict("checks.verify_pass", [True] * 6, [False] * 6)["verdict"], "worse")

    def test_sides_flatten_runs_per_task(self):
        data = {
            "raw": {
                "construct": [
                    {"task": "a", "success": True, "requests": 3, "checks": {"ok": True}, "records": [{"x": 1}], "turns": [{"wall_s": 1}]},
                    {"task": "b", "success": False, "requests": 5, "checks": {"ok": False}},
                ],
                "tools-shape": {"read": [1.0, 2.0], "stalled": 0},
                "loop-shape": [{"sessions": [{"requests": 4}, {"task": "variance", "requests": 6}], "learned": {"skills": []}}],
            }
        }
        s = cmp.sides(data)
        self.assertEqual(s["construct"], {"a/success": [True], "a/requests": [3], "a/checks.ok": [True], "b/success": [False], "b/requests": [5], "b/checks.ok": [False]})
        self.assertEqual(s["tools-shape"], {"read": [1.0, 2.0]})
        self.assertEqual(s["loop-shape"], {"sessions.0.requests": [4], "sessions.variance.requests": [6]})


if __name__ == "__main__":
    unittest.main()
