#!/usr/bin/env bash
set -euo pipefail
cat > "$CHOME/config.toml" <<'EOF'
[router]
mode = "local"
[learn]
enabled = false
EOF
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
git -C "$WORK" init -q -b feature
git -C "$WORK" config user.name 'Construct Verification'
git -C "$WORK" config user.email 'verify@example.invalid'
printf 'before\n' > "$WORK/readme.txt"
git -C "$WORK" add readme.txt
git -C "$WORK" -c core.hooksPath=/dev/null -c commit.gpgsign=false commit -qm 'baseline'
printf 'approved content\n' > "$WORK/readme.txt"
git -C "$WORK" add readme.txt
