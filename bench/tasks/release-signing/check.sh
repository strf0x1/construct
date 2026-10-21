#!/usr/bin/env bash
# Runs in the work dir after the last turn; prints one JSON object of named booleans.
exec "${BENCH_PYTHON:-python3}" - <<'PY'
import json, subprocess
from pathlib import Path

r = subprocess.run(["./tools/verify-release"], capture_output=True, text=True, timeout=30)
print(json.dumps({
    "verify_pass": r.returncode == 0 and "PASS" in r.stdout,
    "version_1_4_0": Path("VERSION").read_text().strip() == "1.4.0",
    "history_kept": "## 1.3.2" in Path("CHANGELOG.md").read_text(),
}))
PY
