#!/usr/bin/env bash
set -euo pipefail
python3 - <<'PY'
import glob, json, os, sqlite3
from pathlib import Path
requests = [json.loads(p.read_text()) for p in sorted((Path(os.environ['EVIDENCE'])/'requests').glob('req-*.json'))]
assert len(requests) == 5, len(requests)
a, b = requests[-2:]
assert a['model'] == 'model-a' and b['model'] == 'model-b'
assert 'original A tool output' in json.dumps(a)
assert 'First B prompt' not in json.dumps(a)
assert 'First A prompt' not in json.dumps(b)
assert 'First B prompt' in json.dumps(b)
for req in requests:
    pending = set()
    for m in req['messages']:
        if m['role'] == 'tool':
            assert m['tool_call_id'] in pending
            pending.remove(m['tool_call_id'])
        else:
            assert not pending
            pending.update(c['id'] for c in m.get('tool_calls', []))
    assert not pending
db = sqlite3.connect(glob.glob(os.environ['CHOME']+'/projects/*/journal.sqlite')[0])
rows = [(sid, typ, json.loads(p)) for sid, typ, p in db.execute('select session_id,type,payload from events order by seq')]
heads = [sid for sid,t,p in rows if t == 'head']
assert len(heads) == 2
for sid,title in zip(heads, ['Session A','Session B']):
    assert [p['title'] for s,t,p in rows if s == sid and t == 'session_name'] == [title]
    assert len([1 for s,t,p in rows if s == sid and t == 'submitted']) == 2
    assert len([1 for s,t,p in rows if s == sid and t == 'usage']) == (3 if title == 'Session A' else 2)
print('ok: two session IDs, durable names and original prompts; next requests preserve models, paired tools and isolated histories')
PY
