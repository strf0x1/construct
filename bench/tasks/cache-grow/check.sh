#!/usr/bin/env bash
# The work, not the cache: the fix holds, the docstring is there, and the
# background run's status reached results.txt. The cache is read from the
# journal by `bench/journal.py cache`.
exec "${BENCH_PYTHON:-python3}" - <<'PY'
import json, subprocess, sys
from pathlib import Path

checks = {}
r = subprocess.run([sys.executable, "-m", "unittest", "-q"], capture_output=True, timeout=60)
checks["tests_pass"] = r.returncode == 0
src = Path("stats.py").read_text() if Path("stats.py").exists() else ""
checks["fixed"] = "len(values) - 1" not in src
checks["docstring_example"] = ">>>" in src or "Example" in src or "example" in src.split("def mean", 1)[-1][:400]
res = Path("results.txt").read_text() if Path("results.txt").exists() else ""
checks["results"] = "OK" in res
print(json.dumps(checks))
PY
