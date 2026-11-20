"""The work map links issues, stages them and places them on modules as the page expects."""

import importlib.util
from pathlib import Path
import unittest

SCRIPT = Path(__file__).resolve().parents[1] / "scripts/workmap/workmap.py"
spec = importlib.util.spec_from_file_location("workmap", SCRIPT)
workmap = importlib.util.module_from_spec(spec)
spec.loader.exec_module(workmap)

SLUG = "strf0x1/construct"
ARCH = """
[crates.construct-core.modules.agent]
may_use = ["journal"]
[crates.construct-core.modules.journal]
may_use = []
[crates.construct-tui.modules.app]
may_use = []
"""
FILES = {
    "docs/architecture.toml": ARCH,
    "docs/adr/0008-durable-journal.md": "# 0008 — Durable journal\n\nStatus: accepted. Implements #2.\n",
    "docs/adr/template.md": "# NNNN — Title\n",
    "docs/feature-map/README.md": "# map\n",
    "docs/feature-map/status-line.md": "---\nid: status-line\nstatus: verified\nverified_at: abc1234\n"
    "depends: [crates/construct-tui/src/status.rs, crates/construct-core/src/journal]\n---\n# Status line\n",
}


class FakeTree:
    ref = "origin/main"

    def show(self, path):
        return FILES[path]

    def ls(self, path):
        return sorted(p for p in FILES if p.startswith(path + "/"))

    def sha(self):
        return "abc1234"


def issue(n, title, body="", state="OPEN", reason=None, labels=(), blocked_by=(), subs=()):
    return {
        "number": n, "title": title, "body": body, "state": state, "stateReason": reason,
        "createdAt": "2026-11-01T00:00:00Z", "closedAt": None, "url": f"https://github.com/{SLUG}/issues/{n}",
        "labels": {"nodes": [{"name": x} for x in labels]},
        "blockedBy": {"nodes": [{"number": x} for x in blocked_by]},
        "subIssues": {"nodes": [{"number": x} for x in subs]},
        "closedByPullRequestsReferences": {"nodes": []},
    }


class WorkMapTest(unittest.TestCase):
    def setUp(self):
        raw = [
            issue(1, "journal: crash recovery", "Umbrella for the journal work.", subs=[3]),
            issue(2, "journal: group commit", "Measure first.", labels=["experiment"]),
            issue(3, "recovery: replay-safe calls", "Start after #2 lands. Unrelated: needs strf0x1/other#2.",
                  labels=["question"]),
            issue(4, "Dropped idea", state="CLOSED", reason="NOT_PLANNED"),
            issue(5, "agent: follow-up", "Follow-up to #3, see PR #2 for code.", blocked_by=[1]),
        ]
        self.g = workmap.build(FakeTree(), raw, SLUG)
        self.by = {i["n"]: i for i in self.g["issues"]}

    def links(self):
        return {(e["from"], e["to"], e["rel"], e["kind"]) for e in self.g["edges"]}

    def test_links_native_and_inferred(self):
        self.assertEqual(self.links(), {
            (3, 1, "part", "native"),
            (5, 1, "after", "native"),
            (3, 2, "after", "inferred"),  # "Start after #2"; the other repo's #2 is skipped
            (5, 3, "after", "inferred"),  # "PR #2" is a pull request, not a link
        })
        why = next(e["why"] for e in self.g["edges"] if (e["from"], e["to"]) == (3, 2))
        self.assertEqual(why, "Start after #2")

    def test_stages_and_unblocks(self):
        stages = {n: i["stage"] for n, i in self.by.items()}
        self.assertEqual(stages, {1: "shape", 2: "experiment", 3: "waiting", 4: "dropped", 5: "waiting"})
        self.assertEqual(self.by[2]["unblocks"], 2)  # #3, and #5 through it
        self.assertEqual(self.by[5]["open_blockers"], [1, 3])

    def test_areas_modules_adrs_features(self):
        self.assertEqual(self.by[2]["areas"], ["journal"])
        self.assertIn("agent", self.by[5]["areas"])
        mods = {m["id"]: m for m in self.g["modules"]}
        self.assertEqual(mods["tui"]["submodules"], ["app"])
        self.assertIn(["agent", "journal"], self.g["module_edges"])
        self.assertTrue({"bench", "dev", "workspace"} <= mods.keys())
        self.assertEqual([(a["id"], a["implements"]) for a in self.g["adrs"]], [("0008", [2])])
        self.assertEqual(self.by[2]["adrs"], ["0008"])
        self.assertEqual(self.g["features"][0]["areas"], ["journal", "tui"])


if __name__ == "__main__":
    unittest.main()
