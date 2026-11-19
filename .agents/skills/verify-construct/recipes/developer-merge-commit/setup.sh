#!/usr/bin/env bash
set -euo pipefail
cat > "$CHOME/config.toml" <<'EOF'
[router]
mode = "local"
[learn]
enabled = false
EOF
export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
git_work() { git -C "$WORK" -c core.hooksPath=/dev/null "$@"; }
git_work init -q -b main
git_work config user.name 'Construct Verification'
git_work config user.email 'verify@example.invalid'
printf 'base\n' > "$WORK/notes.txt"
git_work add notes.txt
git_work commit -qm baseline
git_work rev-parse HEAD > "$OUTSIDE/main"

git_work switch -qc contributor
printf 'contributor\n' > "$WORK/notes.txt"
git_work commit -qam contributor
git_work rev-parse HEAD > "$OUTSIDE/contributor"
git_work switch -q main
git_work worktree add -q -b integration "$WORK/integration"
git_integration() { git_work -C integration "$@"; }
printf 'integration\n' > "$WORK/integration/notes.txt"
git_integration commit -qam integration
git_integration rev-parse HEAD > "$OUTSIDE/first-parent"
status=0
git_integration merge --no-commit contributor || status=$?
test "$status" -eq 1
test "$(git_integration rev-parse MERGE_HEAD)" = "$(cat "$OUTSIDE/contributor")"
printf 'integration\ncontributor\n' > "$WORK/integration/notes.txt"
git_integration add notes.txt
