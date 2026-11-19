#!/usr/bin/env bash
set -euo pipefail
repo="$WORK/integration"
main=$(cat "$OUTSIDE/main")
contributor=$(cat "$OUTSIDE/contributor")
first_parent=$(cat "$OUTSIDE/first-parent")
test "$(git -C "$repo" log -1 --format=%P)" = "$first_parent $contributor"
test "$(git -C "$repo" show HEAD:notes.txt)" = $'integration\ncontributor'
test "$(git -C "$WORK" rev-parse main)" = "$main"
test "$(git -C "$WORK" rev-parse contributor)" = "$contributor"
if git -C "$repo" rev-parse --verify --quiet MERGE_HEAD >/dev/null; then
  echo "merge state still present" >&2
  exit 1
fi
python3 - "$EVIDENCE/journal.txt" <<'PY'
import json
from pathlib import Path
import sys

events = [json.loads(row.split("|", 3)[3]) for row in Path(sys.argv[1]).read_text().splitlines()]
assert any(e.get("event") == "operation" and e["decision"] == "once" for e in events)
assert any(e.get("event") == "tool_end" and e.get("name") == "developer" and e["ok"] for e in events)
PY
echo "ok: approved merge retained both parents and left source refs intact"
