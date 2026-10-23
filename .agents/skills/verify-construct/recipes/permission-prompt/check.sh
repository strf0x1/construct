# The read went through: the model was handed the file's contents, and the
# decision is in the journal.
set -e
grep -l '"role":"tool"' "$EVIDENCE"/requests/*.json >/dev/null
grep -q 'alpha' "$(ls "$EVIDENCE"/requests/*.json | tail -1)" || { echo "the model never got the file's contents"; exit 1; }
grep -q '|access|.*"decision":"session"' "$EVIDENCE/journal.txt" || { echo "no access row with decision session"; exit 1; }
echo "ok: tool result carried the file; access row decision=session"
