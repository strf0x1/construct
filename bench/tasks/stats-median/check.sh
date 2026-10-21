#!/usr/bin/env bash
# What a reviewer would check, scripted. Runs in the work dir after the last
# turn and prints one JSON object of named booleans.
exec "${BENCH_PYTHON:-python3}" - <<'PY'
import json, subprocess, sys
from pathlib import Path

checks = {}
r = subprocess.run([sys.executable, "-m", "unittest", "-q"], capture_output=True, timeout=60)
checks["tests_pass"] = r.returncode == 0
probe = (
    "import stats\n"
    "assert stats.mean([1,2,3,4]) == 2.5\n"
    "assert stats.median([3,1,2]) == 2\n"
    "assert stats.median([4,1,3,2]) == 2.5\n"
    "try:\n    stats.median([])\n    raise SystemExit(1)\nexcept ValueError:\n    pass\n"
)
r = subprocess.run([sys.executable, "-c", probe], capture_output=True, timeout=60)
checks["behaviour"] = r.returncode == 0
tests = Path("test_stats.py").read_text() if Path("test_stats.py").exists() else ""
checks["median_tested"] = "median" in tests
checks["tests_untouched_for_mean"] = "assertEqual(mean([1, 2, 3, 4]), 2.5)" in tests
readme = Path("README.md").read_text() if Path("README.md").exists() else ""
checks["readme_api"] = "## API" in readme and "median" in readme and "mean" in readme
print(json.dumps(checks))
PY
