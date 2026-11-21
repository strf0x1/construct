"""Unit tests for the #248 measurement harness (no binaries needed)."""

import sqlite3
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import group_commit as gc


class Script(unittest.TestCase):
    def test_each_turn_is_one_reply_of_n_parallel_calls_then_an_answer(self):
        s = gc.script_for(4, 2)
        self.assertEqual(len(s["steps"]), 4)
        self.assertEqual(s["steps"][0]["calls"], [
            {"call": "read", "args": {"path": "data.txt"}} for _ in range(4)])
        self.assertEqual(s["steps"][1], {"text": "done"})
        self.assertEqual(s["steps"][2], s["steps"][0])
        self.assertEqual(len(s["side"]), 8)

    def test_one_call_still_uses_the_calls_form(self):
        self.assertEqual(len(gc.script_for(1, 1)["steps"][0]["calls"]), 1)


class Measure(unittest.TestCase):
    def test_summary_reports_median_and_p95(self):
        self.assertEqual(gc.summarize([10, 20, 30, 40]), {"p50": 25, "p95": 40})
        self.assertEqual(gc.summarize([]), {"p50": None, "p95": None})

    def test_counts_sums_row_types_for_one_session_across_projects(self):
        with tempfile.TemporaryDirectory() as tmp:
            home = Path(tmp)
            for project, sid, types in [("a", "s1", ["user", "tool_end", "tool_end"]),
                                        ("b", "s1", ["user"]),
                                        ("c", "s2", ["user"])]:
                d = home / "projects" / project
                d.mkdir(parents=True)
                db = sqlite3.connect(d / "journal.sqlite")
                db.execute("CREATE TABLE events (seq INTEGER PRIMARY KEY, session_id TEXT, type TEXT)")
                for i, ty in enumerate(types):
                    db.execute("INSERT INTO events VALUES (?, ?, ?)", (i, sid, ty))
                db.commit()
                db.close()
            self.assertEqual(gc.counts(home, "s1"), {"user": 2, "tool_end": 2})

    def test_fsyncs_counts_successful_syscalls_only(self):
        with tempfile.TemporaryDirectory() as tmp:
            trace = Path(tmp) / "trace.log"
            trace.write_text(
                "123  fsync(4) = 0\n"
                "123  fdatasync(5) = 0\n"
                "123  fsync(6) = -1 errno=5 (Input/output error)\n"
                "123  write(4, \"x\", 1) = 1\n")
            acp = gc.Acp.__new__(gc.Acp)
            acp.trace = trace
            self.assertEqual(acp.fsyncs(), 2)

    def test_no_trace_means_no_direct_count(self):
        acp = gc.Acp.__new__(gc.Acp)
        acp.trace = None
        self.assertIsNone(acp.fsyncs())

    def test_fsync_cost_is_measurable(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.assertGreater(gc.fsync_cost_ms(Path(tmp), count=10), 0)


if __name__ == "__main__":
    unittest.main()
