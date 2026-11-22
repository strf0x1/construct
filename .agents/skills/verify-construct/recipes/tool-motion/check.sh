# The screen moved, and the tool really ran: its output is in the journal.
set -e
sec() { sed -n "/^--- snap $1 @/,/^---\$/p" "$EVIDENCE/transcript.txt" | grep -v '^--- snap'; }
strip() { sed 's/⟦[^⟧]*⟧//g'; }
grep 'tool_end' "$EVIDENCE/journal.txt" | grep -q 'gamma-out' || { echo "no tool_end with the command's output in the journal"; exit 1; }
sec busy | grep -q '⟦fg=#ebeeff⟧━' || { echo "no comet head on the rule while the tool ran"; exit 1; }
caught=""
for n in 1 2 3 4; do
  if sec "unfurl-$n" | strip | grep -q -e '╽' -e '┃'; then caught=$n; break; fi
done
[ -n "$caught" ] || { echo "no snap caught the gutter filling"; exit 1; }
echo "caught the unfurl in snap unfurl-$caught"
final=$(sec idle | strip)
echo "$final" | grep -q 'gamma-out' || { echo "the settled output lacks gamma-out"; exit 1; }
if sec idle | grep -q '━'; then echo "the comet is still on the rule when idle"; exit 1; fi
if echo "$final" | grep -q -e '╽' -e '┃'; then echo "the gutter never settled"; exit 1; fi
echo "ok: comet while running, the output unfurled, idle rule plain, tool output in the journal"
