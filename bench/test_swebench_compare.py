"""A paired result uses the same selection, limits and immutable environments."""

import json
from pathlib import Path
import tempfile
import unittest

import swebench
import swebench_compare


class ComparisonTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        self.ids = ["repo__one-1", "repo__two-2"]
        self.sample = {"instances": [{"instance_id": task, "repo": repo} for task, repo in
                                     zip(self.ids, ("r/one", "r/two"))]}
        self.construct = self.arm(root / "construct", "construct", ["resolved", "unresolved"])
        self.mini = self.arm(root / "mini", "mini-swe-agent", ["unresolved", "resolved"])

    def arm(self, folder, harness, statuses):
        folder.mkdir()
        predictions = folder / "predictions.jsonl"
        predictions.write_text("frozen patch predictions\n")
        run = {"mode": "paid", "harness": harness, "manifest_sha256": "manifest", "model": "model",
               "upstream": "upstream", "limits": {"usd": "2"}, "reasoning_effort": "high", "workers": 2,
               "settled_usd": "0.25", "reserved_usd": "0",
               "predictions_sha256": swebench.sha256(predictions),
               "attempts": [{"instance_id": task, "image": {"id": f"sha-{task}"}} for task in self.ids]}
        swebench.write_json(folder / "run.json", run)
        swebench.write_json(folder / "evaluation.json", {"run_id": harness, "manifest_sha256": "manifest",
                                                         "predictions_sha256": run["predictions_sha256"]})
        result = folder / "result.json"
        swebench.write_json(result, {"run_id": harness, "manifest": {"instance_ids": self.ids},
                                     "resolved": statuses.count("resolved"), "total": len(self.ids),
                                     "instances": [{"instance_id": task, "status": status,
                                                     "infrastructure_failure": False}
                                                    for task, status in zip(self.ids, statuses)]})
        return folder, result

    def test_compares_pairs_and_spend(self):
        result = swebench_compare.compare(self.sample, *self.construct, *self.mini)
        self.assertEqual(result["outcomes"], {"construct_only": 1, "mini_only": 1})
        self.assertEqual(result["settled_usd"], "0.50")

    def test_refuses_different_images(self):
        manifest = self.mini[0] / "run.json"
        run = json.loads(manifest.read_text())
        run["attempts"][0]["image"]["id"] = "different"
        swebench.write_json(manifest, run)
        with self.assertRaisesRegex(ValueError, "different images"):
            swebench_compare.compare(self.sample, *self.construct, *self.mini)
