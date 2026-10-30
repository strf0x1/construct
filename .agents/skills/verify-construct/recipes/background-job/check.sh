# The completion reached the model as a tagged message with the tail, from the
# journal's own delivery, not as a user row; the user typed once.
set -e
last=$(ls "$EVIDENCE"/requests/*.json | tail -1)
grep -q 'status=\\"completed\\"' "$last" || { echo "the last request has no completed job"; exit 1; }
grep -q 'watched-done' "$last" || { echo "the job's output never reached the model"; exit 1; }
[ "$(grep -c '|turn_start|' "$EVIDENCE/journal.txt")" = 2 ] || { echo "expected two turns"; exit 1; }
[ "$(grep -c '|user|' "$EVIDENCE/journal.txt")" = 1 ] || { echo "a completion must not be a user row"; exit 1; }
grep -q '|jobs_delivered|.*"via":"turn"' "$EVIDENCE/journal.txt" || { echo "no delivery row"; exit 1; }
echo "ok: completion delivered once as jobs_delivered; two turns, one user row"
