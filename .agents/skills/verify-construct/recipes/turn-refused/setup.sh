#!/usr/bin/env bash
set -euo pipefail
cat > "$CHOME/config.toml" <<'TOML'
[router]
mode = "local"
[learn]
enabled = false
TOML
mkdir -p "$CHOME/skills/gone"
cat > "$CHOME/skills/gone/SKILL.md" <<'MD'
---
name: gone
description: A procedure that exists only long enough to be listed
---

1. Do the vanished thing.
MD
# The running turn's shell step touches `now`; delete SKILL.md then, while the
# queued /gone line is still in the worker's channel.
(
  for _ in $(seq 1 600); do
    if [ -e "$WORK/now" ]; then rm -f "$CHOME/skills/gone/SKILL.md"; exit 0; fi
    sleep 0.05
  done
) &