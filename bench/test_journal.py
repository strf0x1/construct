"""The `errors` report over a hand-built journal: grouped by provider and
model, rates per 100 turns, notes summed by their count (issue #48)."""

import json
import sqlite3
import sys
import tempfile
import time
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import journal


def home_with(test, *sessions):
    """A home whose one project journal holds `sessions`: lists of events.
    Removed when `test` ends."""
    tmp = tempfile.TemporaryDirectory()
    test.addCleanup(tmp.cleanup)
    home = Path(tmp.name)
    f = home / "projects" / "p" / "journal.sqlite"
    f.parent.mkdir(parents=True)
    db = sqlite3.connect(f)
    db.execute("CREATE TABLE events (seq INTEGER PRIMARY KEY, session_id TEXT, type TEXT, v INTEGER, at_ms INTEGER, payload TEXT)")
    for i, events in enumerate(sessions):
        for e in events:
            db.execute("INSERT INTO events (session_id, type, v, at_ms, payload) VALUES (?, ?, 1, ?, ?)", (f"s{i}", e["event"], int(time.time() * 1000), json.dumps(e)))
    db.commit()
    db.close()
    return home


def head(model, dialect):
    return {"event": "head", "id": "s", "harness": "construct", "version": "0", "cwd": "/w", "model": model, "manifest": {"dialect": dialect}}


def turn(model="m"):
    return [{"event": "request", "model": model, "bytes": 1, "n_tools": 0, "tools": []}, {"event": "turn_end", "steps": 1, "prompt": 1, "completion": 1, "ms": 1, "interrupted": False}]


def note(kind, count, detail="", cls="provider"):
    return {"event": "note", "class": cls, "kind": kind, "detail": detail, "count": count}


class Errors(unittest.TestCase):
    def test_groups_by_provider_and_model_with_rates_per_100_turns(self):
        home = home_with(
            self,
            [head("a/one", "openrouter")] + turn("a/one") * 4 + [note("retry", 2, "status 503"), note("bad_chunk", 200), {"event": "error", "message": "boom"}],
            [head("a/one", "openrouter")] + turn("a/one") * 4 + [note("retry", 1)],
            [head("b/two", "openai")] + turn("b/two") * 10 + [{"event": "tool_end", "id": "x", "name": "shell", "ok": False, "output": "", "ms": 1, "kind": "plain", "code": "timeout"}],
        )
        found = journal.errors(home)
        self.assertEqual(sorted(found), [("openai", "b/two"), ("openrouter", "a/one")])
        a = found[("openrouter", "a/one")]
        self.assertEqual((a["sessions"], a["turns"], a["calls"], a["errors"]), (2, 8, 8, 1))
        self.assertEqual(dict(a["notes"]), {"provider/retry": 3, "provider/bad_chunk": 200})
        self.assertEqual(dict(found[("openai", "b/two")]["tools"]), {"shell:timeout": 1})
        text = journal.report(found)
        self.assertIn("openrouter · a/one: 2 sessions, 8 turns, 8 model calls", text)
        self.assertIn("2500.0/100 turns", text)  # 200 bad chunks over 8 turns
        self.assertIn("10.0/100 turns", text)  # one timeout over 10 turns

    def test_a_model_switch_moves_later_rows_to_the_new_model(self):
        home = home_with(self, [head("a/one", "openai")] + turn("a/one") + [{"event": "model", "model": "a/two", "context_window": 1}] + turn("a/two") + [note("truncated", 1)])
        found = journal.errors(home)
        self.assertEqual(found[("openai", "a/one")]["turns"], 1)
        self.assertEqual(dict(found[("openai", "a/two")]["notes"]), {"provider/truncated": 1})

    def test_a_named_provider_labels_its_rows_and_default_keeps_the_dialect(self):
        start = {"event": "head", "id": "s", "harness": "construct", "version": "0", "cwd": "/w", "model": "local/m", "manifest": {"dialect": "openai", "provider": "local"}}
        back = {"event": "model", "model": "anthropic/x", "context_window": 1}
        again = {"event": "manifest", "dialect": "openrouter", "model": "anthropic/x"}
        home = home_with(self, [start] + turn("local/m") + [note("retry", 1), back, again] + turn("anthropic/x"))
        found = journal.errors(home)
        self.assertEqual(dict(found[("local", "local/m")]["notes"]), {"provider/retry": 1})
        self.assertEqual(found[("openrouter", "anthropic/x")]["turns"], 1)
        self.assertEqual(list(journal.errors(home, provider="local")), [("local", "local/m")])
        self.assertEqual(sorted(found), [("local", "local/m"), ("openrouter", "anthropic/x")], "no group for a switch alone")

    def test_filters_and_the_window(self):
        home = home_with(self, [head("a/one", "openrouter")] + turn("a/one"), [head("b/two", "openai")] + turn("b/two"))
        self.assertEqual(list(journal.errors(home, provider="openai")), [("openai", "b/two")])
        self.assertEqual(list(journal.errors(home, model="a/")), [("openrouter", "a/one")])
        self.assertEqual(journal.errors(home, since=time.time() + 60), {})
        self.assertEqual(journal.report({}), "no sessions in range\n")

    def test_suppressed_rows_name_the_kind_they_fold(self):
        home = home_with(self, [head("m", "openai")] + turn() + [note("suppressed", 40, "bad_chunk")])
        self.assertEqual(dict(journal.errors(home)[("openai", "m")]["notes"]), {"provider/suppressed (bad_chunk)": 40})

    def test_since_is_a_duration_ago(self):
        self.assertAlmostEqual(journal.parse_since("2h"), time.time() - 7200, delta=5)
        self.assertAlmostEqual(journal.parse_since("7"), time.time() - 7 * 86400, delta=5)
        with self.assertRaises(Exception):
            journal.parse_since("soon")

    def test_the_old_session_view_still_skips_notes(self):
        home = home_with(self, [head("m", "openai"), note("retry", 1)] + turn())
        types = [r["type"] for r in journal.sessions(home)[0]]
        self.assertEqual(types, ["session"])


class Cache(unittest.TestCase):
    def usage(self, prompt, cached=0):
        return {"event": "usage", "prompt": prompt, "cached": cached, "completion": 1}

    def test_weighted_share_first_drop_and_session_boundaries(self):
        home = home_with(self,
            [head("m", "openai"), self.usage(100), self.usage(100, 90), self.usage(200, 20)],
            [head("m", "openai"), self.usage(100, 5)])
        m = journal.cache_metrics(home)
        self.assertAlmostEqual(m["cache_hit_rate"], 115 / 500)
        self.assertEqual(m["drops"], 1)
        first, second = m["series"]
        self.assertEqual([c["cache_hit_rate"] for c in first["calls"]], [0, .9, .1])
        self.assertEqual(first["first_drop"], 3)
        self.assertAlmostEqual(first["cache_hit_rate"], 110 / 400)
        self.assertIsNone(second["first_drop"])
        self.assertIn("call 3 m: 10.0% (20/200) DROP", journal.cache_report(m))

    def test_model_switch_zero_prompt_and_equal_share_do_not_mark_drops(self):
        events = [head("a", "openai"), self.usage(100, 90), self.usage(200, 180),
                  {"event": "request", "model": "b"}, self.usage(100, 5),
                  self.usage(0), self.usage(100), self.usage(100, 80), self.usage(100, 40)]
        m = journal.cache_metrics(home_with(self, events))
        calls = m["series"][0]["calls"]
        self.assertEqual(m["drops"], 1)
        self.assertEqual(m["series"][0]["first_drop"], 7)
        self.assertIsNone(calls[3]["cache_hit_rate"])
        self.assertEqual(calls[2]["model"], "b")

    def test_a_drop_is_a_prefix_not_reused_not_a_falling_share(self):
        # Call 2 adds much new text: its share falls, but the prefix was reused.
        # Calls 3 and 4 both miss; the old share rule saw only the first.
        events = [head("m", "openai"), self.usage(100, 90), self.usage(400, 95),
                  self.usage(500, 0), self.usage(600, 0), self.usage(700, 590)]
        m = journal.cache_metrics(home_with(self, events))
        self.assertEqual([c["drop"] for c in m["series"][0]["calls"]], [False, False, True, True, False])
        self.assertEqual(m["series"][0]["first_drop"], 3)

    def test_since_excludes_old_usage_but_keeps_session_call_numbers(self):
        home = home_with(self, [head("m", "openai"), self.usage(100, 90), self.usage(100, 10)])
        db = sqlite3.connect(next(home.glob("projects/*/journal.sqlite")))
        db.execute("UPDATE events SET at_ms = seq * 1000")
        db.commit()
        db.close()
        m = journal.cache_metrics(home, since=2.5)
        self.assertEqual(m["prompt_tokens"], 100)
        self.assertEqual(m["series"][0]["calls"][0]["index"], 2)
        self.assertEqual(m["drops"], 0)

    def test_empty_and_missing_usage(self):
        m = journal.cache_metrics(home_with(self, [head("m", "openai")]))
        self.assertIsNone(m["cache_hit_rate"])
        self.assertEqual(m["series"], [])
        m = journal.cache_metrics(home_with(self, [head("m", "openai"), {"event": "usage", "prompt": 100}]))
        self.assertEqual(m["cache_hit_rate"], 0)

    def test_legacy_sessions_and_json_cli(self):
        import contextlib
        import io

        home = home_with(self)
        folder = home / "sessions"
        folder.mkdir()
        rows = [{"type": "session", "id": "old", "model": "m"},
                {"type": "usage", "at": int(time.time() * 1000),
                 "usage": {"prompt_tokens": 20, "cached_tokens": 10}}]
        (folder / "old.jsonl").write_text("\n".join(json.dumps(r) for r in rows))
        out = io.StringIO()
        with contextlib.redirect_stdout(out):
            journal.main(["cache", "--home", str(home), "--json"])
        m = json.loads(out.getvalue())
        self.assertEqual(m["cache_hit_rate"], .5)
        self.assertEqual(m["series"][0]["session"], "old")


if __name__ == "__main__":
    unittest.main()
