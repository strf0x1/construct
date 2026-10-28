# /status and the status line show what the journal's usage rows add up to.
set -e
# transcript.txt is a styled snapshot; match on the text alone
plain=$(sed 's/⟦[^⟧]*⟧//g' "$EVIDENCE/transcript.txt")
rows=$(grep '|usage|' "$EVIDENCE/journal.txt" | sed 's/^[^{]*//')
n=$(echo "$rows" | jq -s 'length')
prompt=$(echo "$rows" | jq -s 'map(.prompt) | add')
grep -qE "requests +$n( |$)" <<<"$plain" || { echo "screen does not show $n requests"; exit 1; }
# human(): <1000 raw, else one decimal k
fmt=$(awk -v n="$prompt" 'BEGIN{ if (n<1000) print n; else if (n<100000) printf "%.1fk", n/1000; else printf "%dk", n/1000 }')
grep -qE "input +$fmt" <<<"$plain" || { echo "screen does not show input $fmt"; exit 1; }
sid=$(grep '|head|' "$EVIDENCE/journal.txt" | head -1 | sed 's/^[^{]*//' | jq -r .id)
grep -q "session   $sid" <<<"$plain" || { echo "/status does not show session $sid"; exit 1; }
echo "ok: /status shows session $sid, $n requests, input $fmt = sum of usage rows"
