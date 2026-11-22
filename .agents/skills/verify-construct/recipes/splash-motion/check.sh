# The intro played over a first frame that waited on nothing: construct's own
# startup trace times the first frame, and the settled splash shows the
# router mode from the recipe's config.toml.
set -e
sec() { sed -n "/^--- snap $1 @/,/^---\$/p" "$EVIDENCE/transcript.txt" | grep -v '^--- snap'; }
strip() { sed 's/⟦[^⟧]*⟧//g'; }
first() { # the first of the snaps $1-1 .. $1-5 whose text passes the shell test $2
  for n in 1 2 3 4 5; do if sec "$1-$n" | eval "$2"; then echo "$1-$n"; return 0; fi; done; return 1
}

ff=$(grep 'first frame' "$CHOME/trace.log" | head -1 | grep -Eo '[0-9]+\.[0-9]+' | head -1)
[ -n "$ff" ] || { echo "no first-frame milestone in trace.log"; exit 1; }
awk -v ms="$ff" 'BEGIN { exit !(ms < 100) }' || { echo "first frame at ${ff}ms, over 100ms"; exit 1; }
echo "first frame at ${ff}ms (trace.log)"

# the block-letter title is drawn with ▄▀█, so "shaded" means ░▒▓ only
s=$(first s "strip | grep -q -e '░' -e '▒' -e '▓'") || { echo "no snap caught the title in shaded blocks"; exit 1; }
echo "title resolving caught in $s"
sec s-1 | strip | grep -q 'enter send' && { echo "the hints were already in at the first snap"; exit 1; }

settled=$(sec settled | strip)
echo "$settled" | grep -q '▄▀▀ ▄▀▄ █▄ █ ▄▀▀ ▀█▀ █▀▄ █ █ ▄▀▀ ▀█▀' || { echo "the settled title is not whole"; exit 1; }
echo "$settled" | grep -q 'enter send' || { echo "the hints never came in"; exit 1; }
echo "$settled" | grep -Eq 'router +local' || { echo "the splash does not show the configured router"; exit 1; }
if echo "$settled" | grep -q -e '░' -e '▒' -e '▓'; then echo "blocks left on the settled splash"; exit 1; fi
echo "ok: the title resolved out of blocks, the hints came in after it, first frame undelayed, router from config shown"
