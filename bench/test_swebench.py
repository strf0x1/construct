"""Model-free checks of public inputs, patch export and official SWE-bench result binding."""

import json
import subprocess
import tempfile
import unittest
from pathlib import Path

import swebench as sb


def git(work, *args):
    return subprocess.check_output(["git", "-C", str(work), *args], text=True).strip()


class Bridge(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.work = self.root / "work"
        self.work.mkdir()
        git(self.work, "init", "-q")
        git(self.work, "config", "user.email", "test@example.com")
        git(self.work, "config", "user.name", "Test")
        (self.work / "code.py").write_text("result = False\n")
        git(self.work, "add", ".")
        git(self.work, "commit", "-qm", "base")
        self.rev = git(self.work, "rev-parse", "HEAD")
        self.rows = [
            {"instance_id": f"owner__repo-{n}", "repo": "owner/repo", "base_commit": self.rev,
             "problem_statement": f"Fix issue {n}", "created_at": f"2020-01-0{n}",
             "patch": "GOLD SECRET", "test_patch": "HIDDEN TEST", "hints_text": "HINT SECRET",
             "FAIL_TO_PASS": ["hidden_case"], "PASS_TO_PASS": ["regression_case"]}
            for n in (1, 2, 3)
        ]
        self.dataset = self.root / "dataset.jsonl"
        self.dataset.write_text("".join(json.dumps(r) + "\n" for r in self.rows))
        self.out = self.root / "prepared"

    def prepare(self, ids=None):
        sb.prepare(self.dataset, ids or [r["instance_id"] for r in self.rows], self.out,
                   "a" * 40, evaluator_revision=self.rev, task_revision=self.rev)
        return self.out / "manifest.json"

    def test_only_public_fields_reach_agent_inputs_and_selection_order_is_stable(self):
        manifest = self.prepare(["owner__repo-2", "owner__repo-1"])
        public = (self.out / "agent" / "tasks.jsonl").read_text()
        self.assertNotIn("SECRET", public)
        self.assertNotIn("hidden_case", public)
        self.assertNotIn("HIDDEN TEST", public)
        rows = [json.loads(l) for l in public.splitlines()]
        self.assertEqual([r["instance_id"] for r in rows], ["owner__repo-2", "owner__repo-1"])
        self.assertEqual(set(rows[0]), sb.PUBLIC_FIELDS)
        self.assertEqual(json.loads(manifest.read_text())["instance_ids"], [r["instance_id"] for r in rows])
        self.assertEqual((self.out / "grader").stat().st_mode & 0o077, 0)

    def test_missing_duplicate_or_unsafe_ids_and_floating_revisions_fail_before_writing(self):
        for ids in [["missing"], ["owner__repo-1", "owner__repo-1"], ["../escape"]]:
            with self.subTest(ids=ids), self.assertRaises(ValueError):
                self.prepare(ids)
            self.assertFalse(self.out.exists())
        with self.assertRaisesRegex(ValueError, "revision"):
            sb.prepare(self.dataset, ["owner__repo-1"], self.out, "main")
        self.rows.append(self.rows[0])
        self.dataset.write_text(json.dumps(self.rows))
        with self.assertRaisesRegex(ValueError, "duplicate"):
            self.prepare()

    def test_patch_export_includes_committed_changes_added_files_and_empty_attempts(self):
        manifest = self.prepare()
        (self.work / "code.py").write_text("result = True\n")
        git(self.work, "commit", "-qam", "fix")
        (self.work / "new file.py").write_text("answer = 42\n")
        target = self.root / "predictions.jsonl"
        sb.export(manifest, {"owner__repo-1": str(self.work)}, target)
        rows = [json.loads(l) for l in target.read_text().splitlines()]
        self.assertEqual(len(rows), 3, "missing attempts stay in the denominator")
        self.assertIn("+result = True", rows[0]["model_patch"])
        self.assertIn("+answer = 42", rows[0]["model_patch"])
        self.assertEqual(rows[1]["model_patch"], "")
        self.assertEqual(set(rows[0]), {"instance_id", "model_name_or_path", "model_patch"})
        git(self.work, "reset", "--hard", self.rev)
        (self.work / "new file.py").unlink()
        patch = self.root / "export.patch"
        patch.write_text(rows[0]["model_patch"])
        git(self.work, "apply", str(patch))
        self.assertEqual((self.work / "new file.py").read_text(), "answer = 42\n")

    def test_export_refuses_wrong_roots_unknown_ids_and_changed_selected_data(self):
        manifest = self.prepare()
        with self.assertRaises(ValueError):
            sb.export(manifest, {"unexpected": str(self.work)}, self.root / "a.jsonl")
        subdir = self.work / "sub"
        subdir.mkdir()
        with self.assertRaisesRegex(ValueError, "root"):
            sb.export(manifest, {"owner__repo-1": str(subdir)}, self.root / "b.jsonl")
        (self.out / "grader" / "dataset.jsonl").write_text("[]")
        with self.assertRaisesRegex(ValueError, "changed"):
            sb.export(manifest, {}, self.root / "c.jsonl")

    def test_evaluations_have_distinct_ids_bound_pins_and_all_selected_instances(self):
        manifest = self.prepare()
        predictions = self.root / "predictions.jsonl"
        sb.export(manifest, {}, predictions)
        plans = [sb.plan(manifest, predictions, self.work, self.work, self.root / "evaluations", "nolearn", 0)
                 for _ in range(2)]
        a, b = [json.loads(p.read_text()) for p in plans]
        self.assertNotEqual(a["run_id"], b["run_id"])
        self.assertNotEqual(a["cwd"], b["cwd"])
        self.assertEqual(a["command"].count("-i"), 3)
        self.assertIn(str(self.work.resolve()), a["command"])
        self.assertEqual(a["predictions_sha256"], sb.sha256(predictions))
        (self.work / "code.py").write_text("dirty\n")
        with self.assertRaisesRegex(ValueError, "clean"):
            sb.plan(manifest, predictions, self.work, self.work, self.root / "evaluations", "learn", 0)

    def test_collect_keeps_infrastructure_failures_in_the_original_denominator(self):
        manifest = self.prepare()
        predictions = self.root / "predictions.jsonl"
        sb.export(manifest, {}, predictions)
        # Reports for nonempty predictions: their contents belong to the evaluator.
        rows = [json.loads(l) for l in predictions.read_text().splitlines()]
        for row in rows:
            row["model_patch"] = "patch"
        predictions.write_text("".join(json.dumps(r) + "\n" for r in rows))
        plan = sb.plan(manifest, predictions, self.work, self.work, self.root / "evals", "learn", 1)
        meta = json.loads(plan.read_text())
        logs = Path(meta["cwd"]) / "logs/evaluation" / meta["run_id"]
        logs.mkdir(parents=True)
        (logs / "results.json").write_text(json.dumps({
            "submitted_ids": [r["instance_id"] for r in rows],
            "resolved_ids": ["owner__repo-1"], "unresolved_ids": ["owner__repo-2"],
            "error_ids": ["owner__repo-3"], "empty_patch_ids": [],
            "infra_failure_ids": ["owner__repo-3"], "failure_reasons": {"owner__repo-3": "container failed"},
        }))
        model = rows[0]["model_name_or_path"].replace("/", "__")
        for row, resolved in zip(rows[:2], [True, False]):
            directory = logs / model / row["instance_id"]
            directory.mkdir(parents=True)
            (directory / "report.json").write_text(json.dumps({row["instance_id"]: {"resolved": resolved}}))
        result = sb.collect(plan)
        self.assertEqual((result["total"], result["resolved"], result["resolve_rate"]), (3, 1, 1 / 3))
        self.assertTrue(result["instances"][2]["infrastructure_failure"])
        predictions.write_text("tampered")
        with self.assertRaisesRegex(ValueError, "changed"):
            sb.collect(plan)


if __name__ == "__main__":
    unittest.main()
