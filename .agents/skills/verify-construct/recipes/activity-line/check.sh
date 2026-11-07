# The starting line carries no elapsed timer; the running ones do, and the
# writing line carries the chars/s readout. Shimmer splits glyphs across style
# runs, so strip the markers before matching.
set -e
strip() { sed 's/⟦[^⟧]*⟧//g'; }
sec() { sed -n "/^--- snap $1 @/,/^---\$/p" "$EVIDENCE/transcript.txt" | strip; }
start=""
for n in 1 2 3 4 5; do
  line=$(sec "starting-$n" | grep -v '^--- snap' | grep 'starting  ' | head -1)
  [ -n "$line" ] && { start=$line; echo "caught in snap starting-$n"; break; }
done
[ -n "$start" ] || { echo "no snap caught the starting frame"; exit 1; }
if echo "$start" | grep -Eq '[0-9]+\.[0-9]s'; then echo "starting line has a timer: $start"; exit 1; fi
if ! echo "$start" | grep -q 'esc to interrupt'; then echo "starting line lacks the interrupt hint: $start"; exit 1; fi
tool=$(sec tool | grep 'running shell' | grep -v '^--- snap' | head -1)
if ! echo "$tool" | grep -Eq '[0-9]+\.[0-9]s'; then echo "the tool line has no timer: $tool"; exit 1; fi
think=$(sec thinking | grep 'thinking' | grep -v '^--- snap' | head -1)
if ! echo "$think" | grep -Eq '[0-9]+\.[0-9]s'; then echo "the thinking line has no timer: $think"; exit 1; fi
write=$(sec writing | grep 'writing' | grep -v '^--- snap' | head -1)
if ! echo "$write" | grep -Eq '[0-9]+\.[0-9]s'; then echo "the writing line has no timer: $write"; exit 1; fi
if ! echo "$write" | grep -q 'chars/s'; then echo "the writing line has no chars/s: $write"; exit 1; fi
grep -q 'ask construct anything… (/help)' "$EVIDENCE/transcript.txt" || { echo "never idle at the end"; exit 1; }
echo "ok: starting has no timer; tool, thinking and writing keep the timer; writing shows chars/s"
echo "starting: $start"
echo "writing:  $write"