"""The replay tasks without cargo: each names real hidden tests the fixture
lacks, and the judge's pairs unblind to the right side.

    python3 -m unittest discover -s bench -p 'test_*.py'     (part of `cargo xtask check`)
    python3 bench/replay.py oracle --baseline rust-replay-…   (the slow proof: cargo, minutes)
"""

import json
import subprocess
import tempfile
import unittest
from pathlib import Path

import replay
import tasklib

TASKS = [n for n in tasklib.names() if n.startswith("rust-replay-")]


def show(rev, path):
    r = subprocess.run(["git", "show", f"{rev}:{path}"], cwd=tasklib.ROOT, capture_output=True, text=True)
    return r.stdout if r.returncode == 0 else None


class Tasks(unittest.TestCase):
    def test_there_are_five(self):
        self.assertEqual(len(TASKS), 5, TASKS)

    def test_fixture_is_the_merges_parent_and_its_hidden_tests_are_new(self):
        for name in TASKS:
            with self.subTest(name):
                task = tasklib.load(name)
                r = replay.spec(task)
                parent = subprocess.run(["git", "rev-parse", f"{r['merge']}^1"], cwd=tasklib.ROOT, capture_output=True, text=True, check=True).stdout.strip()
                self.assertEqual(task.git, parent, "the fixture is the tree the PR started from")
                for path in r["hidden"]:
                    self.assertIsNotNone(show(r["merge"], path), path)
                for g in r["tests"]:
                    files = [p for p in r["hidden"] if p.endswith(f"tests/{g['test']}.rs")]
                    self.assertEqual(len(files), 1, f"{g['test']} is one of the hidden files")
                    after, before = show(r["merge"], files[0]), show(task.git, files[0]) or ""
                    for n in g.get("names") or []:
                        self.assertIn(f"fn {n}(", after)
                        self.assertNotIn(f"fn {n}(", before, "the model must not find the test in its fixture")
                    if not g.get("names"):
                        self.assertNotEqual(after, before, "a whole-target group is one the PR changed")

    def test_prompt_asks_for_a_summary_and_the_check(self):
        for name in TASKS:
            with self.subTest(name):
                p = tasklib.load(name).turns[0]
                self.assertIn("cargo xtask check", p)
                self.assertIn("summary", p)


def result(side, runs):
    return {"raw": {side: runs}}


def run(task, diff, summary):
    return {"task": task, "report": {"diff_text": diff}, "final_message": summary}


class Judge(unittest.TestCase):
    def test_pairs_are_blind_both_ways_and_tally_unblinds(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            task = TASKS[0]
            a, b = tmp / "a.json", tmp / "b.json"
            a.write_text(json.dumps(result("construct", [run(task, "+model-a-line\n", "A did it")])))
            b.write_text(json.dumps(result("construct", [run(task, "+model-b-line\n", "B did it")])))
            out = tmp / "pairs"
            replay.cmd_pairs(type("Args", (), {"a": a, "b": b, "side": None, "out": out, "seed": 3})())
            key = json.loads((out / "key.json").read_text())["pairs"]
            self.assertEqual(len(key), 2)
            for pid, k in key.items():
                text = (out / f"{pid}.md").read_text()
                self.assertNotIn("a.json", text)
                x_is_a = text.index("model-a-line") < text.index("model-b-line")
                self.assertEqual(k["X"], "A" if x_is_a else "B")
                # The judge prefers A's change every time, by its blinded letter.
                pick = "X" if x_is_a else "Y"
                (out / f"{pid}.out").write_text('reasons...\n{"idiom": "%s", "conventions": "tie", "scope": "%s", "honesty": "%s", "overall": "%s"}\n' % ((pick,) * 4))
            import contextlib
            import io

            buf = io.StringIO()
            with contextlib.redirect_stdout(buf):
                replay.cmd_tally(type("Args", (), {"dir": out})())
            wins = json.loads(buf.getvalue().strip().splitlines()[-1])["wins"]
            self.assertEqual(wins["overall"], {"A": 2, "B": 0, "tie": 0})
            self.assertEqual(wins["conventions"], {"A": 0, "B": 0, "tie": 2})


if __name__ == "__main__":
    unittest.main()
