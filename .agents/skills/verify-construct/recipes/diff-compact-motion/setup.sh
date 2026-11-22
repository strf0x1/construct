#!/usr/bin/env bash
# A file to edit, and three ~10 KB files whose reads fill a 16k window (the
# same setup as crates/construct-dev/tests/compaction.rs), so the turn compacts.
set -euo pipefail
cat > "$WORK/router.rs" <<'RS'
fn groups(mentions_url: bool, asks_search: bool) -> Vec<Group> {
    if mentions_url { vec![Web] } else { vec![] }
}
RS
for i in 0 1 2; do
  for l in $(seq -w 0 199); do echo "file $i line $l: some ordinary content here"; done > "$WORK/big$i.txt"
done
cat > "$CHOME/config.toml" <<'TOML'
context_window = 16384
[router]
mode = "local"
[learn]
enabled = false
TOML
