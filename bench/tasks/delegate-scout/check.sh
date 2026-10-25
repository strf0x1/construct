#!/usr/bin/env bash
# Runs in the work dir after the last turn; prints one JSON object of named
# booleans. The codeword exists only in the scout's definition and the model
# can't derive it, so its presence means the answer came back from the child.
exec "${BENCH_PYTHON:-python3}" - <<'PY'
import json
from pathlib import Path

t = Path("answer.txt").read_text().strip() if Path("answer.txt").exists() else ""
print(json.dumps({"codeword": "7f3a9c1e" in t, "uname": "Linux" in t}))
PY
