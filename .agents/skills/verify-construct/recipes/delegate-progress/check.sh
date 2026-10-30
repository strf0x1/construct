# What the dock and the peek panel showed came from the children's real rows:
# the shell commands on screen are tool_start rows of the child's own session
# (not the parent's) that ran ok, the peek showed the finished one with its
# check mark, the background child's job is tied to the delegate call it was
# filed under, and the Child wrapper is never journaled.
set -e
j=$EVIDENCE/journal.txt
t=$EVIDENCE/screens.txt  # the snaps, styles stripped
sed 's/⟦[^⟧]*⟧//g' "$EVIDENCE/transcript.txt" >"$t"
parent=$(grep '|user|.*"send the scout"' "$j" | head -1 | cut -d'|' -f2)
[ -n "$parent" ] || { echo "no parent session in the journal"; exit 1; }
for n in 4 6; do
  grep -q "shell sleep $n" "$t" || { echo "the screen never showed the child's sleep $n"; exit 1; }
  row=$(grep "|tool_start|.*sleep $n" "$j" | grep -v "^[0-9]*|$parent|" | head -1)
  [ -n "$row" ] || { echo "no child tool_start for sleep $n under a session other than the parent's"; exit 1; }
  id=$(echo "$row" | grep -o '"id":"[^"]*"' | head -1)
  grep "|tool_end|" "$j" | grep -F "$id" | grep -q '"ok":true' || { echo "the child's sleep $n did not end ok"; exit 1; }
done
grep -q "✓ shell sleep 4" "$t" || { echo "the peek panel never showed the finished tool"; exit 1; }
grep -q "esc close" "$t" || { echo "the peek panel never opened"; exit 1; }
grep -q "· found" "$t" || { echo "the screen never showed the child's words"; exit 1; }
grep -q '|assistant|.*found the notes and am checking them now' "$j" || { echo "the child never said what the screen showed"; exit 1; }
call=$(grep '|job_start|' "$j" | grep -o '"call_id":"[^"]*"' | head -1)
grep "|tool_start|" "$j" | grep '"name":"delegate"' | grep -q "\"id\":${call#*:}" || { echo "job_start does not name the delegate call"; exit 1; }
grep -q '|job_end|.*"status":"completed"' "$j" || { echo "the background child did not complete"; exit 1; }
! grep -q '|child|' "$j" || { echo "the Child wrapper was journaled"; exit 1; }
# The third child was cancelled through its job, and nobody was woken for it.
grep -q '|job_end|.*"id":"j2".*"status":"cancelled"\|job_end|.*"status":"cancelled".*"id":"j2"' "$j" || { echo "j2 was not journaled as cancelled"; exit 1; }
grep -q '|tool_start|.*sleep 20' "$j" || { echo "the third child never ran its command"; exit 1; }
[ "$(grep -c '|jobs_delivered|' "$j")" = 1 ] || { echo "expected one delivery (j1); a cancel must not wake"; exit 1; }
echo "ok: the dock and peek for both delegate calls came from the children's own journaled rows; the job is tied to its call; ctrl+x then y cancelled the third without waking anyone"
