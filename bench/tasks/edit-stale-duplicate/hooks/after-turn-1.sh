#!/usr/bin/env bash
# A teammate's change lands between turns: a three-line admin check holding a new copy of the line above the line the agent will edit.
python3 - <<'PY'
from pathlib import Path
p = Path('access.py')
p.write_text(p.read_text().replace('    if user.banned:\n', '    # admins skip the audit\n    if user.is_admin:\n        return None\n' + '    if user.banned:\n', 1))
PY
