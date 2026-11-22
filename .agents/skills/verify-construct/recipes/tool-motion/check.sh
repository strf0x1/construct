# The screen moved, and the tool really ran: its output is in the journal.
set -e
sec() { sed -n "/^--- snap $1 @/,/^---\$/p" "$EVIDENCE/transcript.txt" | grep -v '^--- snap'; }
strip() { sed 's/⟦[^⟧]*⟧//g'; }
# rows under the tool's header: from the `✓ bash` line to the activity line above the rule
body() { sec "$1" | strip | awk '/✓ bash/{h=NR} /^(─|━)(─|━)(─|━)/{r=NR} END{print (h && r) ? r-h-2 : -1}'; }
grep 'tool_end' "$EVIDENCE/journal.txt" | grep -q 'theta-out' || { echo "no tool_end with the command's output in the journal"; exit 1; }
sec busy | grep -q '⟦fg=#ebeeff⟧━' || { echo "no comet head on the rule while the tool ran"; exit 1; }
caught=""
for n in 1 2 3 4; do
  if sec "unfurl-$n" | strip | grep -q -e '╽' -e '┃'; then caught=$n; break; fi
done
[ -n "$caught" ] || { echo "no snap caught the gutter filling"; exit 1; }
echo "caught the gutter filling in snap unfurl-$caught"
full=$(body opened)
[ "$full" -ge 4 ] || { echo "the opened output has $full rows, expected the 4-line tail and its count"; exit 1; }
grew=""
for n in 1 2 3 4; do b=$(body "unfurl-$n"); if [ "$b" -ge 0 ] && [ "$b" -lt "$full" ]; then grew="unfurl-$n ($b of $full rows)"; break; fi; done
[ -n "$grew" ] || { echo "no snap caught the output opening: every snap had all $full rows"; exit 1; }
echo "caught the output opening in $grew"
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
echo "ok: comet while running, the output opened and its gutter filled, the reply dried behind a caret, idle rule plain, tool output in the journal"
