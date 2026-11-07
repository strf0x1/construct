#!/usr/bin/env bash
set -euo pipefail
cat > "$CHOME/config.toml" <<'TOML'
[router]
mode = "local"
[learn]
enabled = false
TOML