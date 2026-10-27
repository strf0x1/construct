# The screen showed the row and the notice; this checks what the harness did
# with the pick: the session started on `mock` (head row), the switch journaled
# a `model` row for mock/small with its 32768 window, and the one request went
# out as mock/small.
set -e
grep -q 'mock/small' "$CHOME"/models/*.json || { echo "cache lacks mock/small"; exit 1; }
head=$(grep '|head|' "$EVIDENCE/journal.txt" | head -1 | sed 's/^[^{]*//')
[ "$(echo "$head" | jq -r .model)" = mock ] || { echo "session did not start on mock: $head"; exit 1; }
row=$(grep '|model|' "$EVIDENCE/journal.txt" | sed 's/^[^{]*//')
[ "$(echo "$row" | jq -r .model)" = mock/small ] || { echo "no model row for mock/small: $row"; exit 1; }
[ "$(echo "$row" | jq -r .context_window)" = 32768 ] || { echo "model row lacks the 32768 window: $row"; exit 1; }
n=$(grep -c . "$EVIDENCE/requests.jsonl")
[ "$n" = 1 ] || { echo "expected one model request, got $n"; exit 1; }
grep -q '"model":"mock/small"' "$EVIDENCE/requests.jsonl" || { echo "request not sent to mock/small"; head -c 300 "$EVIDENCE/requests.jsonl"; exit 1; }
echo "ok: started on mock; model row mock/small 32768; the one request went to mock/small"
