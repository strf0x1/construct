# Both turns were journaled as interrupted, each with its own user row, and
# the idle screen armed quit (then quit, so the drive ended).
set -e
j=$EVIDENCE/journal.txt
[ "$(grep -c '|turn_start|' "$j")" = 2 ] || { echo "expected two turns"; exit 1; }
[ "$(grep -c '|turn_end|.*"interrupted":true' "$j")" = 2 ] || { echo "expected two interrupted turn_end rows: $(grep '|turn_end|' "$j")"; exit 1; }
[ "$(grep -c '|user|' "$j")" = 2 ] || { echo "expected two user rows"; exit 1; }
grep -q 'ask construct anything… (/help)' "$EVIDENCE/transcript.txt" || { echo "never idle after an interrupt"; exit 1; }
grep -q 'press ctrl+c again to quit' "$EVIDENCE/transcript.txt" || { echo "ctrl+c at idle never armed quit"; exit 1; }
echo "ok: esc and ctrl-c each interrupted a turn; idle ctrl+c armed quit and the second press quit"