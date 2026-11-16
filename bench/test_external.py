"""An external replay against a repository made here: the fixture is the base
tree without history, setup's leftovers stay out of git, and the hidden tests
come from the merge commit and are put back after scoring."""

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

sys.path.insert(0, str(Path(__file__).parent))
import external
import tasklib


def upstream(root: Path) -> dict:
    """A repo with a bug at `base` and its fix plus a test at `merge`."""
    repo = root / "up"
    repo.mkdir()
    git = lambda *a: subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@example.com", *a], cwd=repo, check=True, capture_output=True, text=True).stdout.strip()
    git("init", "-q")
    (repo / "lib.py").write_text("def double(x):\n    return x + 1\n")
    (repo / "test_lib.py").write_text("import lib\nassert lib.double(1) == 2\n")
    git("add", "-A")
    git("commit", "-qm", "base")
    base = git("rev-parse", "HEAD")
    (repo / "lib.py").write_text("def double(x):\n    return x * 2\n")
    (repo / "test_lib.py").write_text("import lib\nassert lib.double(3) == 6\n")
    git("commit", "-qam", "fix")
    return {"url": str(repo), "base": base, "merge": git("rev-parse", "HEAD"), "hidden": ["test_lib.py"], "test": f"{sys.executable} test_lib.py", "setup": "mkdir deps && touch deps/x", "exclude": ["deps"]}


class External(unittest.TestCase):
    def setUp(self):
        tmp = tempfile.TemporaryDirectory()
        self.addCleanup(tmp.cleanup)
        self.root = Path(tmp.name)
        p = patch.object(external, "REPOS", self.root / "repos")
        p.start()
        self.addCleanup(p.stop)
        self.task = tasklib.Task("ext", self.root, {"external": upstream(self.root)})

    def test_the_fixture_is_the_base_tree_and_setup_stays_out_of_git(self):
        work = self.root / "work"
        tasklib.materialize(self.task, work, {})
        self.assertIn("x + 1", (work / "lib.py").read_text())
        self.assertTrue((work / "deps" / "x").exists())
        self.assertEqual(tasklib.changed_paths(work), [])
        log = subprocess.run(["git", "log", "--oneline"], cwd=work, capture_output=True, text=True).stdout
        self.assertEqual(len(log.splitlines()), 1, "no upstream history to read ahead in")

    def test_hidden_tests_fail_until_the_fix_and_are_put_back(self):
        work = self.root / "work"
        tasklib.materialize(self.task, work, {})
        verdict, report = external.score(self.task, work)
        self.assertEqual(verdict, {"hidden_tests": False, "changed_something": False})
        self.assertIn("AssertionError", report["hidden_tail"])
        self.assertIn("double(1)", (work / "test_lib.py").read_text(), "the model's own test file is back")
        (work / "lib.py").write_text("def double(x):\n    return 2 * x\n")
        verdict, report = external.score(self.task, work)
        self.assertEqual(verdict, {"hidden_tests": True, "changed_something": True})
        self.assertEqual(report["diff_lines"], 2)


if __name__ == "__main__":
    unittest.main()
