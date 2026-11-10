"""The build-from-a-spec tasks (`new-*`) can be passed and cannot be passed
by doing nothing: each task's reference answer scores every check, and the
bare fixture scores none."""

import shutil
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))
import tasklib

NEEDS = {"new-py-ledger": "python3", "new-js-csvtable": "node"}


class Greenfield(unittest.TestCase):
    def test_the_reference_passes_every_check_and_the_fixture_none(self):
        for name, tool in NEEDS.items():
            with self.subTest(name):
                if not shutil.which(tool):
                    self.skipTest(f"{name}: no {tool}")
                task = tasklib.load(name)
                with tempfile.TemporaryDirectory() as tmp:
                    work = Path(tmp) / "work"
                    tasklib.materialize(task, work, {})
                    bare = tasklib.check(task, work)
                    self.assertTrue(bare and not any(bare.values()), bare)
                    shutil.copytree(task.dir / "reference", work, dirs_exist_ok=True)
                    self.assertTrue(all(tasklib.check(task, work).values()))
                    self.assertEqual(tasklib.unintended(task, work), [])


if __name__ == "__main__":
    unittest.main()
