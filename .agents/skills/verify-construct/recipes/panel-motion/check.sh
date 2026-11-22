# The panel arrived, moved, was answered and a tool failed as drawn, and the
# tools really ran that way: the read was allowed and returned the file, the
# shell failed.
set -e
sec() { sed -n "/^--- snap $1 @/,/^---\$/p" "$EVIDENCE/transcript.txt" | grep -v '^--- snap'; }
strip() { sed 's/⟦[^⟧]*⟧//g'; }
first() { # the first of the snaps $1-1 .. $1-4 whose text passes the shell test $2
  for n in 1 2 3 4; do if sec "$1-$n" | eval "$2"; then echo "$1-$n"; return 0; fi; done; return 1
}

grep 'tool_end' "$EVIDENCE/journal.txt" | grep -q 'alpha' || { echo "the read's tool_end lacks the file"; exit 1; }
grep 'tool_end' "$EVIDENCE/journal.txt" | grep -q '"ok":false' || { echo "no failed tool_end in the journal"; exit 1; }
grep -q '⚠ allowed once' "$EVIDENCE/transcript.txt" || { echo "the answer never reached the transcript"; exit 1; }

# arrival: the arms climb both sides and meet at the top middle, so an
# arriving panel's top edge has a gap of spaces between its rules; the
# settled one is whole
if sec settled | strip | grep 'PERMISSION' | grep -Eq '─ +─'; then echo "the settled panel's top edge has a gap"; exit 1; fi
s=$(first arrive "strip | grep 'PERMISSION' | grep -Eq '─ +─'") || { echo "no snap caught the border drawing itself"; exit 1; }
echo "arrival caught in $s"

# selection: the row moved to is swept white
s=$(first sel "grep 'Allow for this session' | grep -q 'fg=#ebeeff'") || { echo "no snap caught the selection sweep"; exit 1; }
echo "selection sweep caught in $s"

# ghost: after enter, the panel's edge is still drawn, washed green (the
# live panel's edge is orange to amber; green there is only the ghost)
s=$(first ghost "grep -q -e '⟦fg=#9ece6a⟧╰' -e '⟦fg=#9ece6a⟧╯' -e '⟦fg=#9ece6a⟧│'") \
  || { echo "no snap caught the answered panel dissolving"; exit 1; }
echo "ghost caught in $s"

# failure: the ✗ header is shifted off its two-space indent
s=$(first fail "strip | grep '✗ bash' | grep -vq '^  ✗ bash'") || { echo "no snap caught the failed header shaking"; exit 1; }
echo "shake caught in $s"

idle=$(sec idle | strip)
if echo "$idle" | grep -q 'PERMISSION'; then echo "the panel is still on screen when idle"; exit 1; fi
echo "$idle" | grep -q '^  ✗ bash' || { echo "the failed header did not settle at its indent"; exit 1; }
echo "ok: border drew itself, selection swept, the answered panel dissolved green, the failure shook; read allowed and shell failed in the journal"
