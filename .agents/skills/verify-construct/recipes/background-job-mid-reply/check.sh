# The job really ended mid-reply: its job_end row comes before the end of the
# turn that streamed the reply (else the screen proves nothing), and the reply
# is one block on screen with the notice after it.
set -e
j="$EVIDENCE/journal.txt"
end=$(grep -n '|job_end|' "$j" | head -1 | cut -d: -f1)
turn=$(grep -n '|turn_end|' "$j" | head -1 | cut -d: -f1)
[ -n "$end" ] && [ -n "$turn" ] || { echo "missing job_end or turn_end"; exit 1; }
[ "$end" -lt "$turn" ] || { echo "the job ended after the reply, not during it"; exit 1; }
s=$(mktemp)
sed -n '/^--- snap after /,/^---$/p' "$EVIDENCE/transcript.txt" >"$s"
reply=$(grep -n 'stays one line' "$s" | head -1 | cut -d: -f1)
note=$(grep -n 'job j1 completed' "$s" | head -1 | cut -d: -f1)
[ -n "$reply" ] && [ -n "$note" ] || { echo "reply or notice missing from the screen"; exit 1; }
[ "$note" -gt "$reply" ] || { echo "the notice is drawn above the end of the reply"; exit 1; }
echo "ok: job ended mid-reply; reply drawn whole, notice after it"
