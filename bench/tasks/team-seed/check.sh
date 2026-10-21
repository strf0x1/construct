#!/usr/bin/env bash
# Runs in the work dir after the session; prints one JSON object of named booleans.
exec "${BENCH_PYTHON:-python3}" - <<'PY'
import json, os, subprocess, sys
from pathlib import Path

fn = os.environ["BENCH_SESSION"]
r = subprocess.run([sys.executable, "-m", "unittest", "-q"], capture_output=True, timeout=60)
print(json.dumps({
    "tests_pass": r.returncode == 0,
    f"{fn}_tested": fn in Path("test_stats.py").read_text(),
    "fixtures_built": Path(".fixtures/golden.json").exists(),
}))
PY
