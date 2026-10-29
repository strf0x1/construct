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


def home_with(*sessions):
    """A home whose one project journal holds `sessions`: lists of events."""
    home = Path(tempfile.mkdtemp())
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
        home = home_with([head("a/one", "openai")] + turn("a/one") + [{"event": "model", "model": "a/two", "context_window": 1}] + turn("a/two") + [note("truncated", 1)])
        found = journal.errors(home)
        self.assertEqual(found[("openai", "a/one")]["turns"], 1)
        self.assertEqual(dict(found[("openai", "a/two")]["notes"]), {"provider/truncated": 1})

    def test_filters_and_the_window(self):
        home = home_with([head("a/one", "openrouter")] + turn("a/one"), [head("b/two", "openai")] + turn("b/two"))
        self.assertEqual(list(journal.errors(home, provider="openai")), [("openai", "b/two")])
        self.assertEqual(list(journal.errors(home, model="a/")), [("openrouter", "a/one")])
        self.assertEqual(journal.errors(home, since=time.time() + 60), {})
        self.assertEqual(journal.report({}), "no sessions in range\n")

    def test_suppressed_rows_name_the_kind_they_fold(self):
        home = home_with([head("m", "openai")] + turn() + [note("suppressed", 40, "bad_chunk")])
        self.assertEqual(dict(journal.errors(home)[("openai", "m")]["notes"]), {"provider/suppressed (bad_chunk)": 40})

    def test_since_is_a_duration_ago(self):
        self.assertAlmostEqual(journal.parse_since("2h"), time.time() - 7200, delta=5)
        self.assertAlmostEqual(journal.parse_since("7"), time.time() - 7 * 86400, delta=5)
        with self.assertRaises(Exception):
            journal.parse_since("soon")

    def test_the_old_session_view_still_skips_notes(self):
        home = home_with([head("m", "openai"), note("retry", 1)] + turn())
        types = [r["type"] for r in journal.sessions(home)[0]]
        self.assertEqual(types, ["session"])


if __name__ == "__main__":
    unittest.main()
