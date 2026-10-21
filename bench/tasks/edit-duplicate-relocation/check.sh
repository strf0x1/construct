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

t = text("access.py")
body = t.split("def audit")[0]
checks["final_return_changed"] = "    audit(user)\n    return user.id\n" in body
checks["admin_added"] = "    if user.is_admin:\n        return None\n    if user.banned:" in body
# Silent relocation: an edit meant for the final return landed on a branch's
# return. Its text keeps the final line's indent, so look at the branches.
checks["no_wrong_line"] = "    if user.banned:\n        return None\n" in body and (
    "if user.is_admin" not in body or "    if user.is_admin:\n        return None\n" in body
)
print(json.dumps(checks))
PY
