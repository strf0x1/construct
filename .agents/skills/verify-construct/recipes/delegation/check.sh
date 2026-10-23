# The child's read succeeded after the user allowed it (its tool_end is ok),
# not just that the scripted child said so.
set -e
grep -q '|access|.*"decision":"once"' "$EVIDENCE/journal.txt" || { echo "no access row with decision once"; exit 1; }
row=$(grep '|tool_end|.*"name":"read"' "$EVIDENCE/journal.txt" | head -1)
case "$row" in
  *'"ok":true'*) echo "ok: the child's approved read succeeded" ;;
  *) echo "the child's approved read failed: $(echo "$row" | grep -o '"output":"[^"]*"')"; exit 1 ;;
esac
