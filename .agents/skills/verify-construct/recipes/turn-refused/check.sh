# Exactly one turn ran (the prompt, two requests); the /gone line produced an
# error and no turn, and the TUI is idle rather than stuck busy.
set -e
j=$EVIDENCE/journal.txt
[ "$(grep -c '|turn_start|' "$j")" = 1 ] || { echo "expected one turn, saw $(grep -c '|turn_start|' "$j")"; exit 1; }
[ "$(grep -c '|turn_end|' "$j")" = 1 ] || { echo "expected one turn_end"; exit 1; }
grep -q '|error|.*no skill named' "$j" || { echo "no error row for the vanished skill: $(grep '|error|' "$j")"; exit 1; }
! grep -q '|invoke|' "$j" || { echo "the vanished skill was journaled as invoked"; exit 1; }
! grep -q 'SHOULD-NOT-RUN' "$EVIDENCE/transcript.txt" "$j" "$EVIDENCE"/requests/*.json 2>/dev/null \
  || { echo "a second turn ran for the refused skill"; exit 1; }
[ "$(ls "$EVIDENCE"/requests/*.json | wc -l)" = 2 ] || { echo "expected two model requests (tool call, then its reply)"; exit 1; }
grep -q 'ask construct anything… (/help)' "$EVIDENCE/transcript.txt" || { echo "the TUI never returned to idle"; exit 1; }
echo "ok: the vanished skill errored without a turn and the TUI returned to idle"