# The interrupt did not cancel the child (its completion is after the parent's
# interrupted turn_end and was delivered); quitting did (j2 is cancelled and
# nothing woke on it); the summary reached the model in the wake request.
set -e
j=$EVIDENCE/journal.txt
row() { grep -n "$1" "$j" | head -1 | cut -d: -f1; }
cut=$(row '|turn_end|.*"interrupted":true')
end1=$(grep -n '|job_end|.*"id":"j1".*"status":"completed"\|job_end|.*"status":"completed".*"id":"j1"' "$j" | head -1 | cut -d: -f1)
[ -n "$cut" ] || { echo "the parent's turn was never interrupted"; exit 1; }
[ -n "$end1" ] || { echo "j1 did not complete"; exit 1; }
[ "$end1" -gt "$cut" ] || { echo "j1 ended before the interrupt, so it proves nothing"; exit 1; }
grep -q '|job_end|.*"id":"j2".*"status":"cancelled"\|job_end|.*"status":"cancelled".*"id":"j2"' "$j" || { echo "quitting did not journal j2 as cancelled"; exit 1; }
[ "$(grep -c '|jobs_delivered|' "$j")" = 1 ] || { echo "expected one delivery (j1); a cancel must not wake"; exit 1; }
grep -q 'scout-done: found nothing' "$EVIDENCE/requests/$(ls "$EVIDENCE/requests" | grep -v '^$' | sed -n 5p)" || { echo "the child's summary did not reach the model"; exit 1; }
echo "ok: interrupt left the child running; its summary woke the parent; quitting cancelled the second child"
