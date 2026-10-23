#!/usr/bin/env bash
# Is this checkout worth driving? Read-only. Prints OK or what is wrong.
set -uo pipefail
root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
bad=0
for b in construct construct-dev; do
  [ -x "$root/target/debug/$b" ] || { echo "missing target/debug/$b: cargo build -p construct -p construct-dev"; bad=1; }
done
[ $bad = 0 ] || exit 1
built=$(CONSTRUCT_HOME=$(mktemp -d) "$root/target/debug/construct" manifest | sed -n 's/.*"git_sha": "\(.*\)".*/\1/p')
head=$(git -C "$root" rev-parse HEAD)
[ "$built" = "$head" ] || { echo "target/debug/construct was built at ${built:-?}, HEAD is $head: rebuild"; bad=1; }
newer=$(find "$root/crates" -name '*.rs' -newer "$root/target/debug/construct" | head -3)
[ -z "$newer" ] || { echo "sources changed since the last build (e.g. $newer): rebuild"; bad=1; }
for t in sqlite3 rsvg-convert; do command -v $t >/dev/null || echo "note: no $t (journal.txt / PNG images will be missing)"; done
[ $bad = 0 ] && echo OK
exit $bad
