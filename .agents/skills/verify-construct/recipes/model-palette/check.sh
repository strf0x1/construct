# The screen showed the row; this checks what the harness did with the pick:
# the session started on `mock` (the manifest is written once, at start), and
# the one request sent after the pick went out as mock/small.
set -e
grep -q 'mock/small' "$CHOME"/models/*.json || { echo "cache lacks mock/small"; exit 1; }
grep -q '|manifest|.*"model":"mock"' "$EVIDENCE/journal.txt" || { echo "session did not start on mock"; exit 1; }
n=$(grep -c . "$EVIDENCE/requests.jsonl")
[ "$n" = 1 ] || { echo "expected one model request, got $n"; exit 1; }
grep -q '"model":"mock/small"' "$EVIDENCE/requests.jsonl" || { echo "request not sent to mock/small"; head -c 300 "$EVIDENCE/requests.jsonl"; exit 1; }
echo "ok: started on mock; the picked model mock/small answered the one request"
