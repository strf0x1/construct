# The hub pinned old-pc, the code on screen is the one the peer printed, and
# the answer reached the peer: its read ran and returned the file.
set -e
scratch=$(dirname "$WORK")
grep -q '"id": "old-pc"' "$CHOME/hub/peers.json" && grep -q '"state": "pinned"' "$CHOME/hub/peers.json" \
  || { echo "old-pc is not pinned in the hub's registry"; cat "$CHOME/hub/peers.json"; exit 1; }
code=$(sed -n 's/.*code \([A-Z0-9-]*\) (it must match.*/\1/p' "$scratch/peer.log" | head -1)
[ -n "$code" ] && grep -q "$code" "$EVIDENCE/transcript.txt" \
  || { echo "the popup did not show the peer's code ($code)"; exit 1; }
j=$(ls "$scratch"/peer-home/projects/*/journal.sqlite)
sqlite3 "$j" "select payload from events where type = 'tool_end'" | grep -q '"ok":true.*secret value' \
  || { echo "the peer's read did not run"; sqlite3 "$j" "select type, payload from events"; exit 1; }
sqlite3 "$j" "select payload from events where type = 'access'" | grep -q '"decision":"once"' \
  || { echo "no access row with decision once on the peer"; exit 1; }
echo "ok: old-pc pinned; popup showed code $code; peer read ran after an answer of once"
