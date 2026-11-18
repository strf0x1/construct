#!/usr/bin/env python3
"""Compare graded Construct and mini-SWE-agent attempts on a frozen sample."""

import argparse
from collections import defaultdict, Counter
from decimal import Decimal
import json
from pathlib import Path
import random

import swebench


def load_arm(root, result_file, harness):
    run = json.loads((root / "run.json").read_text())
    result = json.loads(result_file.read_text())
    evaluation = json.loads(result_file.with_name("evaluation.json").read_text())
    if (run["harness"] != harness or run["mode"] != "paid"
            or evaluation["manifest_sha256"] != run["manifest_sha256"]
            or evaluation["predictions_sha256"] != run["predictions_sha256"]
            or swebench.sha256(root / "predictions.jsonl") != run["predictions_sha256"]
            or result["run_id"] != evaluation["run_id"]):
        raise ValueError(f"{harness}: run, inputs and official grading do not match")
    attempts = {a["instance_id"]: a for a in run["attempts"]}
    grades = {row["instance_id"]: row for row in result["instances"]}
    if set(attempts) != set(grades) or len(attempts) != result["total"]:
        raise ValueError(f"{harness}: results omit selected instances")
    return run, result, attempts, grades


def bootstrapped_delta(rows):
    """Percentile interval from resampling repositories with replacement."""
    groups = defaultdict(list)
    for row in rows:
        groups[row["repo"]].append(int(row["construct"]["resolved"]) - int(row["mini-swe-agent"]["resolved"]))
    groups = list(groups.values())
    rng = random.Random(236)
    draws = []
    for _ in range(10_000):
        sample = rng.choices(groups, k=len(groups))
        draws.append(sum(map(sum, sample)) / sum(map(len, sample)))
    draws.sort()
    return [draws[249], draws[9749]]


def compare(sample, construct_root, construct_grade, mini_root, mini_grade):
    c, c_result, c_attempts, c_grades = load_arm(construct_root, construct_grade, "construct")
    m, m_result, m_attempts, m_grades = load_arm(mini_root, mini_grade, "mini-swe-agent")
    for key in ("manifest_sha256", "model", "upstream", "limits", "reasoning_effort", "workers"):
        if c[key] != m[key]:
            raise ValueError(f"arms have different {key}")
    selected = [row["instance_id"] for row in sample["instances"]]
    if selected != c_result["manifest"]["instance_ids"] or selected != m_result["manifest"]["instance_ids"]:
        raise ValueError("official grades do not match the frozen sample")
    rows = []
    for task in sample["instances"]:
        instance = task["instance_id"]
        pair = {**task}
        for arm, attempts, grades in (("construct", c_attempts, c_grades), ("mini-swe-agent", m_attempts, m_grades)):
            attempt, grade = attempts[instance], grades[instance]
            pair[arm] = {"status": grade["status"], "resolved": grade["status"] == "resolved",
                         "infrastructure_failure": grade["infrastructure_failure"],
                         "requests": attempt.get("requests", 0), "tokens": attempt.get("tokens", 0),
                         "budget_stop": attempt.get("budget_stop") or attempt.get("not_started"),
                         "runner_error": attempt.get("runner_error"), "settled_usd": attempt.get("usd", "0")}
        if c_attempts[instance]["image"]["id"] != m_attempts[instance]["image"]["id"]:
            raise ValueError(f"{instance}: arms used different images")
        c_solved, m_solved = pair["construct"]["resolved"], pair["mini-swe-agent"]["resolved"]
        pair["outcome"] = {(True, True): "both_resolved", (False, False): "neither_resolved",
                           (True, False): "construct_only", (False, True): "mini_only"}[c_solved, m_solved]
        rows.append(pair)
    spend = sum((Decimal(run["settled_usd"]) for run in (c, m)), start=Decimal(0))
    reserved = sum((Decimal(run["reserved_usd"]) for run in (c, m)), start=Decimal(0))
    return {"model": c["model"], "limits_per_arm": c["limits"],
            "grade_run_ids": {"construct": c_result["run_id"], "mini-swe-agent": m_result["run_id"]},
            "outcomes": dict(Counter(row["outcome"] for row in rows)), "total": len(rows),
            "construct_resolved": c_result["resolved"], "mini_resolved": m_result["resolved"],
            "construct_minus_mini_repo_bootstrap_95pct": bootstrapped_delta(rows),
            "settled_usd": str(spend), "reserved_usd": str(reserved), "tasks": rows}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("sample", "construct-run", "construct-grade", "mini-run", "mini-grade", "output"):
        parser.add_argument(f"--{name}", type=Path, required=True)
    args = parser.parse_args()
    try:
        result = compare(json.loads(args.sample.read_text()), args.construct_run, args.construct_grade,
                         args.mini_run, args.mini_grade)
        with args.output.open("x") as stream:
            json.dump(result, stream, indent=2)
            stream.write("\n")
    except (OSError, ValueError) as error:
        parser.exit(1, f"{error}\n")
    print(args.output)


if __name__ == "__main__":
    main()
