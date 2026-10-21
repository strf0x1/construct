#!/usr/bin/env bash
# Runs in the work dir after the last turn; prints one JSON object of named
# booleans. `exact` compares every file in expected/ byte for byte, so a
# wrong-line edit, lost whitespace or a mangled character all fail it.
exec "${BENCH_PYTHON:-python3}" - "$(dirname "$0")/expected" <<'PY'
import json, sys
from pathlib import Path

expected = Path(sys.argv[1])
def text(p):
    return Path(p).read_text(encoding="utf-8") if Path(p).exists() else ""
checks = {"exact": all(Path(f.relative_to(expected)).exists() and Path(f.relative_to(expected)).read_bytes() == f.read_bytes() for f in expected.rglob("*") if f.is_file())}

t = text("config.py")
checks["timeout_45"] = '"timeout_s": 45' in t
checks["teammate_change_kept"] = '"jitter": True' in t and '"max_backoff_s": 60' in t
print(json.dumps(checks))
PY
