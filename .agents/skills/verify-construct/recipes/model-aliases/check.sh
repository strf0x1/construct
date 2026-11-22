# The picker picked the canonical id, the request and journal agree, and the
# aliases the details showed came from the cached list.
set -e
req=$(ls "$EVIDENCE"/requests/req-*.json | tail -1)
[ "$(jq -r .model "$req")" = "mock/small" ] || { echo "request model is $(jq -r .model "$req")"; exit 1; }
row=$(grep '|model|' "$EVIDENCE/journal.txt" | sed 's/^[^{]*//')
[ "$(echo "$row" | jq -r .model)" = "mock/small" ] || { echo "no model row for mock/small: $row"; exit 1; }
cache=$(ls "$CHOME"/models/*.json)
jq -e '.models[] | select(.id == "mock/small") | .aliases | index("compact")' "$cache" >/dev/null \
  || { echo "cached list lacks mock/small alias compact"; exit 1; }
echo "ok: alias search picked mock/small; request, journal and cached aliases agree"
