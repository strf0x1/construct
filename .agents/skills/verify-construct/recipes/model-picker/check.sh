# The model got the id the picker chose, and the journal says what the switch
# resolved to (the screen's notice proves neither).
set -e
req=$(ls "$EVIDENCE"/requests/req-*.json | tail -1)
[ "$(jq -r .model "$req")" = "anthropic/claude-opus-5.5" ] || { echo "request model is $(jq -r .model "$req")"; exit 1; }
row=$(grep '|model|' "$EVIDENCE/journal.txt" | sed 's/^[^{]*//')
[ "$(echo "$row" | jq -r .context_window)" = 1000000 ] || { echo "no model row with the 1M window: $row"; exit 1; }
[ "$(echo "$row" | jq -r .max_tokens)" = 32000 ] || { echo "model row lacks max_tokens 32000: $row"; exit 1; }
head=$(grep '|head|' "$EVIDENCE/journal.txt" | head -1 | sed 's/^[^{]*//')
[ "$(echo "$head" | jq -r .manifest.context_window)" = 200000 ] || { echo "head row should hold haiku's 200k: $head"; exit 1; }
echo "ok: request model opus; model row 1M/32000; head row manifest 200000"
