#!/usr/bin/env python3
"""Prepare a fixed SWE-bench subset, export patches, and bind official grades.

The task runner uses only public rows from agent/tasks.jsonl. The private
grader data and pinned upstream code stay on the host with the official v5
evaluator.
"""

import argparse
import hashlib
import json
import os
import re
import subprocess
import sys
import uuid
from pathlib import Path

MODEL = "deepseek/deepseek-v4.1-flash-1017"
EVALUATOR_REVISION = "02e7a74ffd0b707aab73d203fe87bdc7c76afc8e"
TASK_REVISION = "3d07b464b7b311a0cbfb5ed5b2d8a3b96f84a33d"
PUBLIC_FIELDS = {"instance_id", "repo", "base_commit", "problem_statement", "created_at"}


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def read_rows(path):
    text = Path(path).read_text()
    rows = json.loads(text) if text.lstrip().startswith("[") else [json.loads(l) for l in text.splitlines() if l.strip()]
    if not isinstance(rows, list) or any(not isinstance(r, dict) for r in rows):
        raise ValueError("expected a JSON array or JSONL of objects")
    ids = [r.get("instance_id") for r in rows]
    if any(not isinstance(i, str) or not re.fullmatch(r"[A-Za-z0-9_.-]+__[A-Za-z0-9_.-]+-\d+", i) for i in ids):
        raise ValueError("invalid instance_id")
    if len(ids) != len(set(ids)):
        raise ValueError("duplicate instance_id")
    return rows


def write_json(path, value):
    Path(path).write_text(json.dumps(value, indent=2) + "\n")


def write_rows(path, rows):
    Path(path).write_text("".join(json.dumps(r) + "\n" for r in rows))


def revision(value):
    if not re.fullmatch(r"[0-9a-f]{40}", value):
        raise ValueError("revision must be a full 40-character Git commit")
    return value


def prepare(dataset, ids, output, dataset_revision, model=MODEL, phase="pilot",
            evaluator_revision=EVALUATOR_REVISION, task_revision=TASK_REVISION,
            dataset_id="SWE-bench/SWE-bench_Verified"):
    for value in (dataset_revision, evaluator_revision, task_revision):
        revision(value)
    rows = {r["instance_id"]: r for r in read_rows(dataset)}
    if not ids or len(set(ids)) != len(ids) or set(ids) - rows.keys():
        raise ValueError("select distinct, known instance IDs explicitly")
    selected = [rows[i] for i in ids]
    public = []
    for row in selected:
        if not re.fullmatch(r"[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+", row.get("repo", "")):
            raise ValueError("invalid repository name")
        revision(row.get("base_commit", ""))
        if not isinstance(row.get("problem_statement"), str) or not row["problem_statement"].strip():
            raise ValueError("missing problem statement")
        public.append({k: row.get(k) for k in sorted(PUBLIC_FIELDS)})
    output = Path(output)
    output.mkdir(mode=0o700)
    (output / "agent").mkdir()
    (output / "grader").mkdir(mode=0o700)
    write_rows(output / "agent/tasks.jsonl", public)
    write_rows(output / "grader/dataset.jsonl", selected)
    write_json(output / "manifest.json", {
        "schema_version": 1, "suite": "swebench", "phase": phase, "model": model,
        "instance_ids": ids,
        "dataset": {"id": dataset_id, "revision": dataset_revision,
                    "source_sha256": sha256(dataset), "selected_sha256": sha256(output / "grader/dataset.jsonl"),
                    "public_sha256": sha256(output / "agent/tasks.jsonl")},
        "evaluator": {"url": "https://github.com/SWE-bench/SWE-bench", "revision": evaluator_revision},
        "tasks": {"url": "https://github.com/SWE-bench/swe-bench-tasks", "revision": task_revision},
    })


def load(manifest):
    manifest = Path(manifest).resolve()
    data = json.loads(manifest.read_text())
    if data.get("schema_version") != 1 or data.get("suite") != "swebench":
        raise ValueError("unsupported SWE-bench manifest")
    for relative, field in [("grader/dataset.jsonl", "selected_sha256"), ("agent/tasks.jsonl", "public_sha256")]:
        if sha256(manifest.parent / relative) != data["dataset"][field]:
            raise ValueError(f"{relative} changed after task selection")
    rows = read_rows(manifest.parent / "agent/tasks.jsonl")
    if [row["instance_id"] for row in rows] != data["instance_ids"]:
        raise ValueError("selected instance IDs do not match public task inputs")
    return data, rows


def git(work, *args):
    return subprocess.check_output(["git", "-C", str(work), "-c", "core.fsmonitor=false", *args], text=True).strip()


def patch(work, base):
    work = Path(work).resolve()
    if Path(git(work, "rev-parse", "--show-toplevel")).resolve() != work:
        raise ValueError(f"{work} must be the repository root")
    if git(work, "ls-files", "--unmerged"):
        raise ValueError(f"{work} contains unresolved conflicts")
    subprocess.run(["git", "-C", str(work), "merge-base", "--is-ancestor", base, "HEAD"], check=True, capture_output=True)
    # Diff from the task's base, so an agent's own commits are included too.
    command = ["git", "-C", str(work), "-c", "core.fsmonitor=false", "diff", "--binary", "--no-ext-diff", "--no-textconv"]
    diff = subprocess.check_output(command + [base, "--"], text=True)
    files = subprocess.check_output(["git", "-C", str(work), "-c", "core.fsmonitor=false",
                                     "ls-files", "--others", "--exclude-standard", "-z"], text=True)
    for file in filter(None, files.split("\0")):
        added = subprocess.run(command + ["--no-index", "--", "/dev/null", file], cwd=work, capture_output=True, text=True)
        if added.returncode not in (0, 1):
            raise ValueError(f"could not export added file {file}")
        diff += added.stdout
    return diff


def export(manifest, worktrees, output):
    data, rows = load(manifest)
    if set(worktrees) - set(data["instance_ids"]):
        raise ValueError("worktree mapping contains an unselected instance")
    patches = {row["instance_id"]: patch(worktrees[row["instance_id"]], row["base_commit"])
               for row in rows if worktrees.get(row["instance_id"])}
    write_predictions(data, patches, output)


def write_predictions(data, patches, output):
    predictions = [{"instance_id": instance, "model_name_or_path": data["model"],
                    "model_patch": patches.get(instance, "")} for instance in data["instance_ids"]]
    with Path(output).open("x") as stream:
        stream.write("".join(json.dumps(r) + "\n" for r in predictions))


def check_checkout(path, expected):
    if git(path, "rev-parse", "HEAD") != revision(expected):
        raise ValueError(f"{path} does not match its pinned revision")
    if git(path, "status", "--porcelain"):
        raise ValueError(f"{path} must be a clean pinned checkout")


# The task repo supersedes the dataset in upstream v5. Refuse a mismatched
# public task instead of grading a different base/prompt under a familiar id.
TASK_CHECK = """import json, sys
from pathlib import Path
from swebench.task.repo import load_task_repo
public_path, task_repo = sys.argv[1:3]
expected = [json.loads(s) for s in Path(public_path).read_text().splitlines()]
actual = load_task_repo(task_repo, [r['instance_id'] for r in expected])
for want, have in zip(expected, actual):
    for field in ('instance_id', 'repo', 'base_commit', 'problem_statement'):
        if want[field] != have[field]:
            raise ValueError('pinned task differs from selected dataset: ' + want['instance_id'] + ' / ' + field)
del sys.argv[1:3]
"""

ENTRYPOINT = TASK_CHECK + """
from swebench.cli.cli import main
main()
"""

PREBUILT_ENTRYPOINT = TASK_CHECK + """
from swebench.harness.utils import load_swebench_dataset
pinned = load_swebench_dataset(sys.argv[2])
for want, have in zip(pinned, actual, strict=True):
    if {k: v for k, v in want.items() if k != 'image'} != {k: v for k, v in have.items() if k != 'image'}:
        raise ValueError('prebuilt grader data differs from pinned task: ' + have['instance_id'])
from swebench.cli.cli import main
main()
"""


def use_prebuilt(rows, image_ids):
    if set(image_ids) != {row["instance_id"] for row in rows}:
        raise ValueError("prebuilt images must cover exactly the selected instances")
    pinned = []
    for row in rows:
        expected = image_ids[row["instance_id"]]
        if not re.fullmatch(r"sha256:[0-9a-f]{64}", expected) or row.get("image_assets"):
            raise ValueError("prebuilt grading requires immutable image IDs and tasks without external image assets")
        actual = subprocess.check_output(
            ["docker", "image", "inspect", "--format", "{{.Id}}", expected], text=True).strip()
        if actual != expected:
            raise ValueError(f"local image does not match pin: {row['instance_id']}")
        pinned.append({**row, "image": expected})
    return pinned


def plan(manifest, predictions, evaluator, tasks, output, arm, repeat, python=sys.executable, workers=1,
         prebuilt_images=None):
    data, _ = load(manifest)
    if not re.fullmatch(r"[A-Za-z0-9_-]+", arm) or repeat < 0:
        raise ValueError("arm must be a simple name and repeat must be nonnegative")
    if workers < 1:
        raise ValueError("workers must be positive")
    evaluator, tasks, predictions = [Path(p).resolve() for p in (evaluator, tasks, predictions)]
    check_checkout(evaluator, data["evaluator"]["revision"])
    check_checkout(tasks, data["tasks"]["revision"])
    rows = read_rows(predictions)
    if {r["instance_id"] for r in rows} != set(data["instance_ids"]):
        raise ValueError("predictions must contain every selected instance exactly once")
    if any(r.get("model_name_or_path") != data["model"] or not isinstance(r.get("model_patch"), str) for r in rows):
        raise ValueError("predictions have a different model or a non-text patch")
    run_id = f"construct-{arm}-r{repeat}-{uuid.uuid4().hex}"
    cwd = Path(output).resolve() / run_id
    cwd.mkdir(parents=True, mode=0o700)
    manifest = Path(manifest).resolve()
    dataset = manifest.parent / "grader/dataset.jsonl"
    entrypoint, task_args = ENTRYPOINT, ["--task-repo", str(tasks)]
    if prebuilt_images is not None:
        selected = use_prebuilt(read_rows(dataset), prebuilt_images)
        dataset = cwd / "prebuilt-dataset.jsonl"
        write_rows(dataset, selected)
        entrypoint, task_args = PREBUILT_ENTRYPOINT, []
    command = [str(python), "-c", entrypoint, str(manifest.parent / "agent/tasks.jsonl"), str(tasks),
               "eval", str(dataset), "-p", str(predictions), *task_args, "--run-id", run_id, "-j", str(workers)]
    for instance_id in data["instance_ids"]:
        command += ["-i", instance_id]
    destination = cwd / "evaluation.json"
    write_json(destination, {"run_id": run_id, "arm": arm, "repeat": repeat, "cwd": str(cwd),
                            "command": command, "evaluator_checkout": str(evaluator), "task_checkout": str(tasks),
                            "manifest": str(manifest), "manifest_sha256": sha256(manifest),
                            "predictions": str(predictions), "predictions_sha256": sha256(predictions),
                            "grader_dataset": str(dataset), "grader_dataset_sha256": sha256(dataset),
                            "prebuilt_images": prebuilt_images})
    return destination


def collect(evaluation):
    meta = json.loads(Path(evaluation).read_text())
    for field in ("manifest", "predictions"):
        if sha256(meta[field]) != meta[field + "_sha256"]:
            raise ValueError(f"{field} changed after evaluation was planned")
    if "grader_dataset" in meta and sha256(meta["grader_dataset"]) != meta["grader_dataset_sha256"]:
        raise ValueError("grader dataset changed after evaluation was planned")
    data, _ = load(meta["manifest"])
    root = Path(meta["cwd"]) / "logs/evaluation" / meta["run_id"]
    report = json.loads((root / "results.json").read_text())
    selected = set(data["instance_ids"])
    groups = {key: set(report.get(key, [])) for key in (
        "submitted_ids", "resolved_ids", "unresolved_ids", "error_ids", "empty_patch_ids", "infra_failure_ids")}
    if groups["submitted_ids"] != selected or any(group - selected for group in groups.values()):
        raise ValueError("official report does not match the selected instances")
    categories = [groups[s + "_ids"] for s in ("resolved", "unresolved", "error", "empty_patch")]
    if sum(map(len, categories)) != len(set().union(*categories)):
        raise ValueError("official report assigns conflicting outcomes")
    instances = []
    for instance_id in data["instance_ids"]:
        status = next((s for s in ("resolved", "unresolved", "error", "empty_patch") if instance_id in groups[s + "_ids"]), "missing_report")
        if status in ("resolved", "unresolved"):
            path = root / data["model"].replace("/", "__") / instance_id / "report.json"
            detail = json.loads(path.read_text())[instance_id]
            if detail.get("resolved") is not (status == "resolved"):
                raise ValueError(f"per-instance report disagrees with summary: {instance_id}")
        instances.append({"instance_id": instance_id, "status": status,
                          "infrastructure_failure": instance_id in groups["infra_failure_ids"],
                          "failure_reason": report.get("failure_reasons", {}).get(instance_id)})
    resolved = sum(r["status"] == "resolved" for r in instances)
    result = {"suite": "swebench", "run_id": meta["run_id"], "arm": meta["arm"], "repeat": meta["repeat"],
              "manifest": data, "total": len(instances), "resolved": resolved,
              "resolve_rate": resolved / len(instances), "instances": instances,
              "official_report": str(root / "results.json")}
    write_json(Path(meta["cwd"]) / "result.json", result)
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    p = commands.add_parser("prepare", help="freeze selected tasks and separate agent inputs from grading data")
    p.add_argument("--dataset", type=Path, required=True)
    p.add_argument("--dataset-revision", required=True)
    p.add_argument("--dataset-id", default="SWE-bench/SWE-bench_Verified")
    p.add_argument("--instance", action="append", required=True)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--model", default=MODEL)
    p.add_argument("--phase", choices=["pilot", "heldout"], default="pilot")
    p = commands.add_parser("export", help="export one official prediction per selected task, including empty attempts")
    p.add_argument("--manifest", type=Path, required=True)
    p.add_argument("--worktrees", type=Path, required=True, help="JSON mapping of instance IDs to completed repository roots")
    p.add_argument("--output", type=Path, required=True)
    p = commands.add_parser("grade", help="plan a unique official evaluation; --execute runs it locally")
    p.add_argument("--manifest", type=Path, required=True)
    p.add_argument("--predictions", type=Path, required=True)
    p.add_argument("--evaluator", type=Path, required=True)
    p.add_argument("--task-repo", type=Path, required=True)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--arm", required=True)
    p.add_argument("--repeat", type=int, default=0)
    p.add_argument("--workers", type=int, default=1)
    p.add_argument("--prebuilt-images", type=Path, help="exact image IDs already built from the pinned task repo")
    p.add_argument("--python", default=sys.executable)
    p.add_argument("--execute", action="store_true")
    p = commands.add_parser("collect", help="import official reports without changing the selected denominator")
    p.add_argument("evaluation", type=Path)
    args = parser.parse_args()
    try:
        if args.command == "prepare":
            prepare(args.dataset, args.instance, args.output, args.dataset_revision, args.model, args.phase,
                    dataset_id=args.dataset_id)
        elif args.command == "export":
            export(args.manifest, json.loads(args.worktrees.read_text()), args.output)
        elif args.command == "collect":
            print(json.dumps(collect(args.evaluation), indent=2))
        else:
            images = json.loads(args.prebuilt_images.read_text()) if args.prebuilt_images else None
            destination = plan(args.manifest, args.predictions, args.evaluator, args.task_repo,
                               args.output, args.arm, args.repeat, args.python, args.workers, images)
            print(destination)
            if args.execute:
                meta = json.loads(destination.read_text())
                env = {k: v for k, v in os.environ.items() if not k.endswith("_API_KEY")}
                env["PYTHONPATH"] = str(args.evaluator.resolve())
                subprocess.run(meta["command"], cwd=meta["cwd"], env=env, check=True)
                print(json.dumps(collect(destination), indent=2))
    except (ValueError, OSError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    main()
