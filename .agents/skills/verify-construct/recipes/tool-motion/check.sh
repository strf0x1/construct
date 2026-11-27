# The screen moved, the live tail streamed with upward darkening and collapsed
# into a compact preview, and the tool really ran: its output is in the journal.
set -e
sec() { sed -n "/^--- snap $1 @/,/^---\$/p" "$EVIDENCE/transcript.txt" | grep -v '^--- snap'; }
strip() { sed 's/⟦[^⟧]*⟧//g'; }
# rows under the bash header (spinning or finished) down to the activity line above the rule
body() { sec "$1" | strip | awk '/bash for w/{h=NR} /^(─|━)(─|━)(─|━)/{r=NR} END{print (h && r) ? r-h-2 : -1}'; }
grep 'tool_end' "$EVIDENCE/journal.txt" | grep -q 'theta-out' || { echo "no tool_end with the command's output in the journal"; exit 1; }
sec busy | grep -q '⟦fg=#ebeeff⟧━' || { echo "no comet head on the rule while the tool ran"; exit 1; }

# Live streaming tail: visible while running, with multiple lines and brighter newest line
live_rows=$(body busy)
[ "$live_rows" -ge 4 ] || { echo "expected live tail while running, got $live_rows rows"; exit 1; }
sec busy | strip | grep -q 'zeta-out' || { echo "live tail did not show zeta-out while running"; exit 1; }
sec busy | grep -E 'zeta-out|eta-out|theta-out' | grep -qv '⟦fg=#464c66⟧' || { echo "newest live tail line was not brighter than FAINT"; exit 1; }
echo "caught live tail while running ($live_rows rows)"

# Smooth collapse on finish: settled preview is 3 rows (2 lines + `… 6 more lines`), fewer than the 5-row live tail
settled_rows=$(body settled)
[ "$settled_rows" -eq 3 ] || { echo "settled preview has $settled_rows rows, expected 3 (2 lines + count)"; exit 1; }
sec settled | strip | grep -q '… 6 more lines (ctrl+o)' || { echo "settled preview missing collapsed line count"; exit 1; }
collapsed=""
for n in 1 2 3; do
  b=$(body "settle-$n")
  if [ "$b" -gt "$settled_rows" ]; then collapsed="settle-$n ($b -> $settled_rows rows)"; break; fi
done
[ -n "$collapsed" ] || { echo "no snap caught the live tail collapsing: every snap was already $settled_rows rows"; exit 1; }
echo "caught live tail collapsing in $collapsed"

# Reply drying bright behind the blinking caret
inked=""
for n in 1 2 3 4; do
  line=$(sec "stream-$n" | grep '▍' || true)
  if [ -n "$line" ] && echo "$line" | grep -Eq 'fg=#[d-f][0-9a-f][d-f][0-9a-f][e-f][0-9a-f]⟧[^ ▍⟦]'; then inked="stream-$n"; break; fi
done
[ -n "$inked" ] || { echo "no snap caught the reply's tail bright behind the caret"; exit 1; }
echo "caught the reply drying behind the caret in $inked"

final=$(sec idle | strip)
echo "$final" | grep -q 'theta-out' || { echo "the settled output lacks theta-out"; exit 1; }
if sec idle | grep -q '━'; then echo "the comet is still on the rule when idle"; exit 1; fi
if echo "$final" | grep -q -e '╽' -e '┃'; then echo "the gutter never settled"; exit 1; fi
if echo "$final" | grep -q '▍'; then echo "the caret stayed after the reply"; exit 1; fi
echo "ok: comet while running, live tail streamed and collapsed to compact preview, reply dried behind caret, idle rule plain, tool output in journal"
