# The beam intro played over an undelayed first frame without shaded blocks,
# and settled cleanly onto the centered heavy geometric logo with the router
# from config.toml.
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

# No shaded blocks on any frame of the intro or settled splash.
for n in s-1 s-2 s-3 s-4 s-5 settled; do
  if sec "$n" | strip | grep -q -e '░' -e '▒' -e '▓'; then
    echo "shaded blocks appeared in $n"; exit 1
  fi
done

# Beam leading edge caught on the logo strokes while hints are not yet in.
s=$(first s "grep -E '┏|┃|┗' | grep -q 'fg=#ebeeff'") || { echo "no snap caught the luminous beam tracing the logo"; exit 1; }
echo "beam trace caught in $s"
sec s-1 | strip | grep -q 'enter send' && { echo "the hints were already in at the first snap"; exit 1; }

settled=$(sec settled | strip)
echo "$settled" | grep -q '┏━━ ┏━┓ ┏┓┃ ┏━━ ━┳━ ┏━┓ ┃ ┃ ┏━━ ━┳━' || { echo "the settled title is not whole"; exit 1; }
echo "$settled" | grep -q '───────────' || { echo "the divider rule is missing"; exit 1; }
echo "$settled" | grep -q 'enter send' || { echo "the hints never came in"; exit 1; }
echo "$settled" | grep -Eq 'router +local' || { echo "the splash does not show the configured router"; exit 1; }
echo "ok: luminous beam traced the heavy logo without blocks, hints cascaded in after, first frame undelayed, router from config shown"
