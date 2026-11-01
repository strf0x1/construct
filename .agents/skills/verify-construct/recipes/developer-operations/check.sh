#!/usr/bin/env bash
set -euo pipefail
test "$(git -C "$WORK" log -1 --format=%s)" = 'Record approved change'
test "$(git -C "$WORK" show HEAD:readme.txt)" = 'approved content'
python3 - "$EVIDENCE" <<'PY'
import json
import pathlib
import sys

root = pathlib.Path(sys.argv[1])
events = [json.loads(line.split('|', 3)[3]) for line in (root / 'journal.txt').read_text().splitlines()]
decisions = [e for e in events if e.get('event') == 'operation']
assert len(decisions) == 1, decisions
assert decisions[0]['operation'] == 'commit' and decisions[0]['decision'] == 'once', decisions
ends = [e for e in events if e.get('event') == 'tool_end' and e.get('name') == 'developer']
assert len(ends) == 1 and ends[0]['ok'], ends
requests = [json.loads(p.read_text()) for p in sorted((root / 'requests').glob('req-*.json'))]
results = [m.get('content', '') for r in requests for m in r['messages'] if m['role'] == 'tool']
assert any('Committed ' in str(result) for result in results), results
print('ok: commit matches reviewed content; operation approval and result journaled')
PY
