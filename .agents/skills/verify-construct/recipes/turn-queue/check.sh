# Both turns ran, one after the other: two turn_start/end, two user rows, and
# the model was asked a third time with the queued "second" in the request.
set -e
j=$EVIDENCE/journal.txt
[ "$(grep -c '|turn_start|' "$j")" = 2 ] || { echo "expected two turns"; exit 1; }
[ "$(grep -c '|turn_end|' "$j")" = 2 ] || { echo "expected two turn_end rows"; exit 1; }
[ "$(grep -c '|user|' "$j")" = 2 ] || { echo "expected two user rows"; exit 1; }
[ "$(grep -c '|tool_end|' "$j")" = 1 ] || { echo "expected the sleep tool to end once"; exit 1; }
last=$(ls "$EVIDENCE"/requests/*.json | tail -1)
grep -q '"content":"second"' "$last" || { echo "the queued message never reached the model"; exit 1; }
idle=$(sed -n '/^--- snap idle @/,/^---$/p' "$EVIDENCE/transcript.txt")
if ! echo "$idle" | grep -q 'ask construct anything… (/help)'; then echo "not idle after both turns"; exit 1; fi
if echo "$idle" | grep -q 'queue a message'; then echo "still busy after both turns"; exit 1; fi
if echo "$idle" | grep -Eq '⠋|⠙|⠹|⠸|⠼|⠴|⠦|⠧|⠇|⠏'; then echo "spinner still on the idle screen"; exit 1; fi
echo "ok: two queued turns ran; the third request carried the queued text; idle placeholder back, no spinner"