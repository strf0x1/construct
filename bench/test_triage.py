"""Triage over a hand-built journal: each kind of finding is counted from the
tool rows, `find` gives anchors, and `show` prints only the window asked for."""

import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import triage
from test_journal import head, home_with, turn


def call(i, name, args, output="ok", ok=True, code=None):
    end = {"event": "tool_end", "id": f"c{i}", "name": name, "ok": ok, "output": output, "ms": 1, "kind": "plain"}
    return [{"event": "tool_start", "id": f"c{i}", "name": name, "args": args}, {**end, **({"code": code} if code else {})}]


def session():
    return (
        [head("a/one", "openrouter"), {"event": "user", "message": {"role": "user", "content": "fix it"}}]
        + call(1, "read", '{"path": "a.rs"}', "x" * 9000)  # seq 3, 4: big
        + call(2, "read", '{"path":"a.rs"}')  # 6: reread, whatever the spacing
        + call(3, "edit", '{"path":"a.rs"}', "Error: no match", ok=False, code="no_match")  # 8: fail
        + call(4, "edit", '{"path":"a.rs"}', "Error: no match", ok=False, code="no_match")  # 10: fail, stuck, repeat
        + call(5, "edit", '{"path":"a.rs","old":"y"}')  # 12: applied
        + call(6, "read", '{"path":"a.rs"}')  # 14: after a write, not a reread
        + [{"event": "error", "message": "boom"}]
        + turn("a/one")
    )


class Triage(unittest.TestCase):
    def setUp(self):
        self.sessions = triage.load(home_with(self, session(), [head("b/two", "openai")]))

    def test_findings_come_from_the_tool_rows(self):
        found = [(kind, seq, label) for kind, seq, label, _ in triage.findings(self.sessions[0]["rows"])]
        self.assertEqual(found, [
            ("big", 4, "read"), ("reread", 6, "read"), ("fail", 8, "edit:no_match"),
            ("fail", 10, "edit:no_match"), ("stuck", 10, "edit:no_match"), ("repeat", 10, "edit"), ("error", 15, "error"),
        ])

    def test_the_table_counts_them_and_skips_an_empty_session(self):
        m = triage.summary(self.sessions[0])
        self.assertEqual((m["turns"], m["calls"], m["tools"], m["first_write"]), (1, 1, 6, 4))
        text = triage.triage(self.sessions)
        self.assertIn("1 sessions, 6 tool calls", text)
        self.assertIn("fail    edit:no_match", text)
        self.assertNotIn("b/two", text)
        self.assertEqual(triage.triage([]), "no sessions in range\n")

    def test_find_gives_anchors_and_show_reads_only_around_one(self):
        lines = triage.find(self.sessions, "fail", "edit").splitlines()
        self.assertEqual(len(lines), 2)
        self.assertTrue(lines[0].startswith("s0@8  edit:no_match  "), lines[0])
        self.assertIn("Error: no match", lines[0])
        self.assertEqual(triage.find(self.sessions, "note"), "no `note` findings\n")
        text = triage.show(self.sessions, "s0@8", around=1, budget=100)
        self.assertIn("*    8  < edit FAIL no_match", text)
        self.assertIn("events 7..9 of", text)
        self.assertNotIn("fix it", text)
        big = triage.show(self.sessions, "s0@4", around=0, budget=100)
        self.assertIn("[8900 bytes cut]", big)
        self.assertLess(len(big), 400)

    def test_kept_journals_read_back_the_same(self):
        home = home_with(self, session())
        dest = home / "kept"
        self.assertEqual(triage.keep({"arm": home}, dest), 1)
        self.assertEqual(triage.ids(dest / "arm"), ["s0"])
        self.assertEqual(triage.ids(home, since=4e9), [])


if __name__ == "__main__":
    unittest.main()
