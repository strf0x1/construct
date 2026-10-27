# The screen showed the list; this checks what the harness did with it: the
# list was fetched into the cache, and the turn's manifest carries the window
# from the list, not the 131072 default.
set -e
ls "$CHOME"/models/*.json >/dev/null 2>&1 || { echo "no cached model list"; exit 1; }
grep -q 'mock/small' "$CHOME"/models/*.json || { echo "cache lacks mock/small"; exit 1; }
grep -q '|manifest|.*"context_window":32768' "$EVIDENCE/journal.txt" || { echo "manifest window is not 32768"; exit 1; }
echo "ok: list cached; manifest context_window 32768"
