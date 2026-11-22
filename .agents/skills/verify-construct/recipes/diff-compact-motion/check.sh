# The diff got its highlighter pass and the compaction notice resolved, and
# both really happened: the edit's tool_end and a compacted row are in the
# journal. The wave above the notice is pinned by motion's unit tests.
set -e
sec() { sed -n "/^--- snap $1 @/,/^---\$/p" "$EVIDENCE/transcript.txt" | grep -v '^--- snap'; }
strip() { sed 's/⟦[^⟧]*⟧//g'; }
first() { # the first of the snaps $1-1 .. $1-4 whose text passes the shell test $2
  for n in 1 2 3 4; do if sec "$1-$n" | eval "$2"; then echo "$1-$n"; return 0; fi; done; return 1
}
quad() { grep -q -e '▖' -e '▗' -e '▘' -e '▙' -e '▚' -e '▛' -e '▜' -e '▝' -e '▞' -e '▟' -e '▌' -e '▐' -e '▀' -e '▄' -e '█'; }

grep 'tool_end' "$EVIDENCE/journal.txt" | grep -q 'edited' || { echo "no edit tool_end in the journal"; exit 1; }
grep -q 'compacted' "$EVIDENCE/journal.txt" || { echo "no compacted row in the journal"; exit 1; }

# highlighter: an added line is white mid-pass, not yet its settled green
s=$(first diff "grep 'g.push(Web)' | grep -q 'fg=#ebeeff'") || { echo "no snap caught the diff's highlighter pass"; exit 1; }
echo "highlighter caught in $s"

# compaction: a notice still resolving out of quadrant blocks
s=$(first compact "strip | grep 'compacted hist' | quad") || { echo "no snap caught the notice resolving"; exit 1; }
echo "notice resolving caught in $s"

idle=$(sec idle | strip)
echo "$idle" | grep -q 'compacted history (' || { echo "the settled notice is not whole"; exit 1; }
if echo "$idle" | grep 'compacted' | quad; then echo "a notice never resolved"; exit 1; fi
echo "ok: the diff got its highlighter pass, the compaction notice resolved, edit and compaction in the journal"
