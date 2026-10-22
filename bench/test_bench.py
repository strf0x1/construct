"""Model-free tests of the bench's own machinery: tasks, scorers, compare.

    python3 -m unittest discover -s bench -p 'test_*.py'     (part of `cargo xtask check`)
"""

import json
import os
import random
import shutil
import sqlite3
import subprocess
import tempfile
import unittest
from pathlib import Path

import compare as cmp
import journal
import process
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


def copy_expected(task):
    return lambda w, v: shutil.copytree(task.dir / "expected", w, dirs_exist_ok=True)


# A task with expected/ is solved by those files.
for _name in tasklib.names():
    if (tasklib.TASKS / _name / "expected").exists():
        SOLUTIONS.setdefault(_name, copy_expected(tasklib.load(_name)))


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

    def test_every_task_with_expected_files_has_a_solution(self):
        for name in tasklib.names():
            if (tasklib.TASKS / name / "expected").exists():
                self.assertIn(name, SOLUTIONS)

    def test_the_stale_reference_hook_changes_the_file_between_turns(self):
        task, vars, work = self.materialize("edit-stale-reference")
        before = (work / "config.py").read_text()
        tasklib.after_turn(task, 1, work)
        after = (work / "config.py").read_text()
        self.assertNotEqual(before, after)
        self.assertIn('"jitter": True', after)
        # the teammate's change alone is not a solution, and the expected file keeps it
        self.assertFalse(all(tasklib.check(task, work).values()))
        self.assertIn('"jitter": True', (task.dir / "expected" / "config.py").read_text())

    def test_stale_duplicate_hooks_add_a_copy_above_the_target(self):
        for name in ("edit-stale-duplicate", "edit-stale-duplicate-gap"):
            with self.subTest(name):
                task, _, work = self.materialize(name)
                p = work / "access.py"
                self.assertEqual(p.read_text().count("return None"), 2)
                tasklib.after_turn(task, 1, work)
                hooked = p.read_text()
                self.assertEqual(hooked.count("return None"), 3)
                # The right edit on the hooked file is exactly the expected file.
                right = hooked.replace("    audit(user)\n    return None", "    audit(user)\n    return user.id")
                self.assertEqual(right, (task.dir / "expected" / "access.py").read_text())
                # Where a stale rank-2 anchor lands: the banned branch's return,
                # set to the text meant for the final line (its indent too).
                for wrong in ("    return user.id", "        return user.id"):
                    p.write_text(hooked.replace("    if user.banned:\n        return None", "    if user.banned:\n" + wrong))
                    c = tasklib.check(task, work)
                    self.assertFalse(c["no_wrong_line"], wrong)
                    self.assertTrue(c["teammate_change_kept"])

    def test_wrong_line_edits_are_caught(self):
        task, _, work = self.materialize("edit-duplicate-relocation")
        # Silent relocation: the second edit lands on the banned branch's return.
        (work / "access.py").write_text(
            (task.dir / "expected" / "access.py").read_text()
            .replace("    audit(user)\n    return user.id", "    audit(user)\n    return None")
            .replace("    if user.banned:\n        return None", "    if user.banned:\n    return user.id")
        )
        c = tasklib.check(task, work)
        self.assertFalse(c["exact"])
        self.assertFalse(c["no_wrong_line"])
        task, _, work = self.materialize("edit-duplicate-lines")
        (work / "settings.toml").write_text((work / "settings.toml").read_text().replace("enabled = false", "enabled = true", 1))
        self.assertFalse(tasklib.check(task, work)["no_wrong_line"])

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


class Process(unittest.TestCase):
    def test_edit_outcomes_from_a_session_log(self):
        home = Path(tempfile.mkdtemp())
        (home / "sessions").mkdir()

        def call(id, name, args):
            return {"id": id, "type": "function", "function": {"name": name, "arguments": json.dumps(args)}}

        lines = [
            {"type": "session"},
            {"type": "usage", "usage": {"completion_tokens": 40}},
            {"type": "message", "message": {"role": "assistant", "tool_calls": [call("a", "read", {"path": "f.py"})]}},
            {"type": "tool", "data": {"id": "a", "name": "read", "ok": True, "code": None, "data": None}},
            {"type": "usage", "usage": {"completion_tokens": 100}},
            {"type": "message", "message": {"role": "assistant", "tool_calls": [call("b", "edit", {"path": "f.py"}), call("c", "edit", {"path": "g.py"})]}},
            {"type": "tool", "data": {"id": "b", "name": "edit", "ok": False, "code": "ANCHOR_MISMATCH", "data": {"status": "rejected"}}},
            {"type": "tool", "data": {"id": "c", "name": "edit", "ok": False, "code": "not_found", "data": None}},
            {"type": "usage", "usage": {"completion_tokens": 30}},
            {"type": "message", "message": {"role": "assistant", "tool_calls": [call("d", "read", {"path": "f.py"})]}},
            {"type": "tool", "data": {"id": "d", "name": "read", "ok": True}},
            {"type": "usage", "usage": {"completion_tokens": 60}},
            {"type": "message", "message": {"role": "assistant", "tool_calls": [call("e", "edit", {"path": "f.py"})]}},
            {"type": "tool", "data": {"id": "e", "name": "edit", "ok": True, "data": {"status": "applied", "echo_stripped": [0]}}},
        ]
        (home / "sessions" / "s.jsonl").write_text("\n".join(json.dumps(l) for l in lines) + "\n")
        m = process.metrics(process.tool_events(home, 0))
        self.assertEqual(m["tool_calls"], 5)
        self.assertEqual(m["reads"], 2)
        self.assertEqual(m["rejection_codes"], {"ANCHOR_MISMATCH": 1, "not_found": 1})
        e = m["edit"]
        self.assertEqual((e["calls"], e["applied"], e["rejected"]), (3, 1, 2))
        # f.py: rejected, re-read, then applied two steps later; g.py never recovers
        self.assertEqual((e["recoveries"], e["unrecovered"], e["rereads_after_rejection"]), (1, 1, 1))
        self.assertEqual(e["echo_stripped"], 1)
        self.assertEqual(e["completion_tokens_per_call"], (50 + 50 + 60) / 3)
        self.assertEqual(e["rewrites"], 0)

    def test_edit_outcomes_from_the_journal_match_the_session_log(self):
        # The same run as above, as construct now records it: one event per
        # row in the project's journal (stream deltas and all the rest).
        home = Path(tempfile.mkdtemp())
        f = home / "projects" / "p-0000" / "journal.sqlite"
        f.parent.mkdir(parents=True)
        db = sqlite3.connect(f)
        db.execute("CREATE TABLE events (seq INTEGER PRIMARY KEY, session_id TEXT, type TEXT, v INTEGER, at_ms INTEGER, payload TEXT)")

        def call(id, name, args):
            return {"id": id, "type": "function", "function": {"name": name, "arguments": json.dumps(args)}}

        def end(id, name, ok, code=None, data=None):
            return {"event": "tool_end", "id": id, "name": name, "ok": ok, "output": "", "ms": 1, "kind": "plain", "code": code, "data": data}

        def step(tokens, *calls):
            return [{"event": "request", "model": "m", "bytes": 1, "n_tools": 1, "tools": []}, {"event": "usage", "prompt": 1, "completion": tokens, "cached": 0},
                    {"event": "assistant", "message": {"role": "assistant", "tool_calls": list(calls)}}]

        events = [{"event": "head", "id": "s", "harness": "construct", "version": "0", "cwd": "/w", "model": "m", "manifest": {}},
                  {"event": "route", "groups": [], "skills": ["release 0.83"], "source": "local", "ms": 0, "note": ""}, {"event": "from_a_newer_build"}]
        events += step(40, call("a", "read", {"path": "f.py"})) + [end("a", "read", True)]
        events += step(100, call("b", "edit", {"path": "f.py"}), call("c", "edit", {"path": "g.py"}))
        events += [end("b", "edit", False, "ANCHOR_MISMATCH", {"status": "rejected"}), end("c", "edit", False, "not_found")]
        events += [{"event": "tool_result", "id": i, "message": {"role": "tool", "content": "x", "tool_call_id": i}} for i in "bc"]
        events += step(30, call("d", "read", {"path": "f.py"})) + [end("d", "read", True)]
        events += step(60, call("e", "edit", {"path": "f.py"}), call("z", "enable_tools", {"groups": ["web"]}))
        events += [end("z", "enable_tools", True), end("e", "edit", True, None, {"status": "applied", "echo_stripped": [0]})]
        for e in events:
            db.execute("INSERT INTO events (session_id, type, v, at_ms, payload) VALUES ('s', ?, 1, 5000, ?)", (e["event"], json.dumps(e)))
        db.commit()
        db.close()
        m = process.metrics(process.tool_events(home, 0))
        self.assertEqual(m["tool_calls"], 5)
        self.assertEqual(m["rejection_codes"], {"ANCHOR_MISMATCH": 1, "not_found": 1})
        e = m["edit"]
        self.assertEqual((e["calls"], e["applied"], e["rejected"]), (3, 1, 2))
        self.assertEqual((e["recoveries"], e["unrecovered"], e["rereads_after_rejection"]), (1, 1, 1))
        self.assertEqual(e["echo_stripped"], 1)
        # enable_tools shares the last step but was never a tool record.
        self.assertEqual(e["completion_tokens_per_call"], (50 + 50 + 30) / 3)
        self.assertEqual(process.tool_events(home, 6), [], "nothing active after `since`")
        session = journal.sessions(home)[0]
        self.assertEqual(session[0]["type"], "session")
        self.assertEqual(next(r for r in session if r["type"] == "route")["data"]["skills"], [["release", 0.83]])

    def test_a_fixed_wrong_line_edit_is_a_rewrite(self):
        # The hash-only shape from issue #26: a stale anchor sets line 6 (the
        # wrong copy), the model notices, puts line 6 back and sets line 8.
        def edit(id, *regions):
            rs = [{"index": i, "kind": "set", "old_start": s, "old_end": e, "new_start": s, "new_end": e} for i, (s, e) in enumerate(regions)]
            return {"id": id, "name": "edit", "ok": True, "args": {"path": "access.py"}, "step": 0, "completion_tokens": 0,
                    "data": {"path": "access.py", "status": "applied", "regions": rs}}

        self.assertEqual(process.metrics([edit("a", (6, 6)), edit("b", (6, 6), (8, 8))])["edit"]["rewrites"], 1)
        self.assertEqual(process.metrics([edit("a", (6, 6)), edit("b", (8, 8))])["edit"]["rewrites"], 0)
        # replace records no regions, so it never counts
        plain = {**edit("a", (1, 1)), "data": None}
        self.assertEqual(process.metrics([plain, plain])["edit"]["rewrites"], 0)


class Arms(unittest.TestCase):
    def test_an_arm_is_construct_with_a_tools_overlay(self):
        import bench

        name, body = bench.parse_arm('hashonly:editor="hashline", hashline={anchor="hash"}')
        self.assertEqual(name, "hashonly")
        bench.ARMS[name] = body
        try:
            h = bench.make_harness(name, Path(tempfile.mkdtemp()) / "home")
            h.learn = False
            h.configure("http://127.0.0.1:9/v1", "m")
            cfg = (h.home / "config.toml").read_text()
            import tomllib

            parsed = tomllib.loads(cfg)
            self.assertEqual(parsed["tools"], {"editor": "hashline", "hashline": {"anchor": "hash"}})
            self.assertEqual(parsed["learn"], {"enabled": False})
            # construct itself reads it that way
            exe = next((p for p in (bench.ROOT / "target" / "debug" / "construct", bench.CONSTRUCT) if p.exists()), None)
            if exe is None:
                self.skipTest("no construct binary built")
            r = subprocess.run([str(exe), "manifest"], env={**h.full_env(), "PATH": os.environ.get("PATH", "")}, capture_output=True, text=True, timeout=60)
            m = json.loads(r.stdout)
            self.assertEqual((m["editor"], m["hashline"]["anchor"]), ("hashline", "hash"))
        finally:
            bench.ARMS.pop(name, None)

    def test_endpoint_records_skip_a_half_written_line(self):
        import bench

        ep = bench.Endpoint.__new__(bench.Endpoint)
        ep.log = Path(tempfile.mkdtemp()) / "log.jsonl"
        ep.log.write_text('{"i": 0}\n{"i": 1, "usage": {"prom')
        self.assertEqual(ep.records(), [{"i": 0}])

    def test_bad_arms_are_refused(self):
        import bench

        for spec in ["noname", "construct:editor=\"x\"", "a:editor=hashline"]:
            with self.subTest(spec), self.assertRaises(SystemExit):
                bench.parse_arm(spec)


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

    def test_mostly_zero_counts_are_judged_as_a_rate(self):
        # Issue #26's run: 0 vs 7 of 10 runs with a rewrite, medians inside a 0-1 spread.
        a, b = [0] * 10, [1] * 6 + [2] + [0] * 3
        row = cmp.verdict("edit.rewrites", a, b)
        self.assertEqual((row["verdict"], row["nonzero"]), ("worse", (0.0, 0.7)))
        self.assertLess(row["p"], 0.01)
        self.assertEqual(cmp.verdict("edit.rewrites", b, a)["verdict"], "better")
        self.assertEqual(cmp.verdict("edit.rewrites", [0] * 10, [1] * 2 + [0] * 8)["verdict"], "inconclusive")
        self.assertEqual(cmp.verdict("edit.rewrites", [0, 1, 0, 1], [1, 0, 1, 0])["verdict"], "same")
        self.assertEqual(cmp.verdict("edit.calls", [0, 1, 1, 1, 1, 1], [0] * 6)["verdict"], "lower")
        self.assertEqual(cmp.verdict("wall_s", [0.0, 1.5, 0.5], [0.5, 1.0, 0.0])["verdict"], "same")
        self.assertIn("runs >0: 0% vs 70%", cmp.table([row], "a", "b"))

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
        self.assertEqual(
            s["construct"],
            {
                "a/success": [True], "a/requests": [3], "a/checks.ok": [True],
                "b/success": [False], "b/requests": [5], "b/checks.ok": [False],
                "all/success": [True, False], "all/requests": [3, 5], "all/checks.ok": [True, False],
            },
        )
        rows = cmp.compare(s["construct"], s["construct"], only="^success$")
        self.assertEqual([r["metric"] for r in rows], ["a/success", "all/success", "b/success"])
        self.assertEqual(s["tools-shape"], {"read": [1.0, 2.0]})
        self.assertEqual(s["loop-shape"], {"sessions.0.requests": [4], "sessions.variance.requests": [6]})


if __name__ == "__main__":
    unittest.main()
