#!/usr/bin/env bash
# Scores the work dir: the remembered answer (against hashes the turn-1 hook
# left in .git), and the work of turns 2 and 3.
exec "${BENCH_PYTHON:-python3}" - <<'PY'
import hashlib, json, subprocess, sys
from pathlib import Path

sha = lambda s: hashlib.sha256(s.encode()).hexdigest()
checks = {}
want = Path(".git/bench-answer").read_text().split() if Path(".git/bench-answer").exists() else ["", ""]
a = Path("ANSWER.txt")
line = a.read_text().strip().splitlines()[0].replace(" ", "").strip("`") if a.exists() and a.read_text().strip() else ""
parts = line.split(",")
checks["answer_exact"] = sha(line) == want[0]
checks["answer_sensor"] = bool(parts[0]) and sha(parts[0]) == want[1]
checks["answer_reading"] = len(parts) > 1 and parts[1] == "97.31"
r = subprocess.run([sys.executable, "-m", "unittest", "-q"], capture_output=True, timeout=120)
checks["tests_pass"] = r.returncode == 0
api = Path("docs/API.md").read_text() if Path("docs/API.md").exists() else ""
checks["api_doc"] = all(n in api for n in ("round_cents", "split", "bulk_discount", "rate_for"))
tm = Path("tests/test_money.py").read_text() if Path("tests/test_money.py").exists() else ""
checks["split7_test"] = "7" in tm and tm.count("def test_") >= 4
print(json.dumps(checks))
PY
