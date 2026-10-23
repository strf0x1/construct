# The token total on screen is the sum of the journal's usage rows.
set -e
want=$(grep '|usage|' "$EVIDENCE/journal.txt" | sed 's/^[^{]*//' | jq -s 'map(.prompt + .completion) | add')
grep -q "$want tok" "$EVIDENCE/transcript.txt" || { echo "screen does not show $want tok"; exit 1; }
echo "ok: status shows $want tok = sum of usage rows"
