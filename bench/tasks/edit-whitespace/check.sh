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

import subprocess
r = subprocess.run([sys.executable, "-c", "import calc; assert calc.legacy([1, None, 2]) == 3; assert calc.modern([1, 2]) == 3"], capture_output=True)
checks["still_works"] = r.returncode == 0
print(json.dumps(checks))
PY
