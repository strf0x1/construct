#!/usr/bin/env bash
set -euo pipefail
cat > "$CHOME/config.toml" <<'TOML'
model = "model-a"
[router]
mode = "local"
[learn]
enabled = false
TOML
printf 'original A tool output\n' > "$WORK/note.txt"
