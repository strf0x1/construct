#!/usr/bin/env bash
# A teammate's change lands between turns: a two-line admin check holding a new copy of the line, as tall as the gap between copies above the line the agent will edit.
python3 - <<'PY'
from pathlib import Path
p = Path('access.py')
p.write_text(p.read_text().replace('    if user.banned:\n', '    if user.is_admin:\n        return None\n' + '    if user.banned:\n', 1))
PY
