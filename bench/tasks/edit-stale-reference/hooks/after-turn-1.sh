#!/usr/bin/env bash
# A teammate's change lands between turns: two keys inserted above the line the agent will edit.
python3 - <<'PY'
from pathlib import Path
p = Path('config.py')
p.write_text(p.read_text().replace('    "retries": 3,\n', '    "retries": 3,\n    "jitter": True,\n    "max_backoff_s": 60,\n', 1))
PY
