# The model got the skill's body and the request, and the journal says which
# skill the user ran (the scripted reply proves neither).
set -e
req=$(ls "$EVIDENCE"/requests/req-*.json | tail -1)
grep -q 'Find-the-entry-point' "$req" || { echo "SKILL.md body not in the request"; exit 1; }
grep -q 'do-the-thing' "$req" || { echo "the typed request is not in the request"; exit 1; }
grep -q '|invoke|.*"skill":"create-verification"' "$EVIDENCE/journal.txt" || { echo "no invoke row"; exit 1; }
echo "ok: request carried SKILL.md and do-the-thing; invoke row for create-verification"
