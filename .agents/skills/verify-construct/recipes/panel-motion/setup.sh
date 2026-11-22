#!/usr/bin/env bash
set -euo pipefail
echo "alpha" > "$OUTSIDE/a.txt"
cat > "$CHOME/config.toml" <<'TOML'
[router]
mode = "local"
[learn]
enabled = false
TOML
