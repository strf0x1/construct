#!/usr/bin/env bash
# Scores answers.txt: one check per probe, exact match after trimming, against
# the per-run values the bench passes as BENCH_VAR_<key> (never into the work
# dir). The fraction correct is written to $BENCH_REPORT as `recall`. No model
# judge.
exec "${BENCH_PYTHON:-python3}" - <<'PY'
import json
import os
import re
from pathlib import Path

# One key per probe; `bench/test_bench.py` keeps this list and task.toml's
# `probes` in sync, and fails if one grows without the other.
KEYS = ["kernel", "region", "train", "base", "staging", "signing", "flag", "channel"]


def norm(text):
    return text.strip().strip("`").strip()


answers = {}
p = Path("answers.txt")
if p.exists():
    for line in p.read_text(errors="replace").splitlines():
        m = re.fullmatch(r"([a-z_]+)\s*=\s*(.+)", line.strip().strip("`").strip())
        if m and m.group(1) in KEYS and norm(m.group(2)):
            answers[m.group(1)] = norm(m.group(2))  # a later line wins

want = {k: os.environ.get(f"BENCH_VAR_{k}") for k in KEYS}

checks = {f"probe_{k}": k in answers and want[k] is not None and answers[k] == want[k] for k in KEYS}
correct = sum(checks.values())
report = {"recall": correct / len(KEYS), "correct": correct, "probes": len(KEYS), "answers": answers}
if os.environ.get("BENCH_REPORT"):
    Path(os.environ["BENCH_REPORT"]).write_text(json.dumps(report))
print(json.dumps(checks))
PY
