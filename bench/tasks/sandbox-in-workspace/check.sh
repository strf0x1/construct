#!/usr/bin/env bash
# Runs in the work dir after the last turn; prints one JSON object of named
# booleans. The codeword is only in the notes file inside the workspace.
exec "${BENCH_PYTHON:-python3}" - <<'PY'
import json, os
from pathlib import Path

t = Path("answer.txt").read_text().strip() if Path("answer.txt").exists() else ""
word = next(Path(".").rglob("notes-*.md")).read_text().split()[1]
print(json.dumps({"answered": bool(t), "found": word in t}))
PY
