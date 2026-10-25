# /status and the status line show what the journal's usage rows add up to.
set -e
rows=$(grep '|usage|' "$EVIDENCE/journal.txt" | sed 's/^[^{]*//')
n=$(echo "$rows" | jq -s 'length')
prompt=$(echo "$rows" | jq -s 'map(.prompt) | add')
grep -q "requests  $n " "$EVIDENCE/transcript.txt" || { echo "screen does not show $n requests"; exit 1; }
# human(): <1000 raw, else one decimal k
fmt=$(awk -v n="$prompt" 'BEGIN{ if (n<1000) print n; else if (n<100000) printf "%.1fk", n/1000; else printf "%dk", n/1000 }')
grep -q "input $fmt" "$EVIDENCE/transcript.txt" || { echo "screen does not show input $fmt"; exit 1; }
sid=$(grep '|head|' "$EVIDENCE/journal.txt" | head -1 | sed 's/^[^{]*//' | jq -r .id)
grep -q "session   $sid" "$EVIDENCE/transcript.txt" || { echo "/status does not show session $sid"; exit 1; }
echo "ok: /status shows session $sid, $n requests, input $fmt = sum of usage rows"
