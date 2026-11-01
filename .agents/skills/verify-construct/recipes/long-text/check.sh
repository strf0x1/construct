# Both ends of what was shown are on screen: the path's start and its file name
# in the panel, and the whole long cell in the table. Rows are read back with
# the frame and padding stripped, joined without spaces, since a wrap lands anywhere.
set -e
plain=$(sed 's/⟦[^⟧]*⟧//g' "$EVIDENCE/transcript.txt")
section() { awk -v s="--- snap $1" '/^--- snap /{on=index($0,s)==1} on' <<<"$plain"; }
path=nested/directories/that/go/on/and/on/until/the/path/is/wider/than/the/screen/a.txt
panel=$(section prompt | grep '▸\|^ *│ \{3,\}[^ ]' | tr -d '│▸ \n')
grep -q "$path" <<<"$panel" || { echo "the panel does not show the whole path: $panel"; exit 1; }
cell=$(section table | grep '│' | sed 's/^[^│]*│//' | tr -d ' \n')
want=Thecargoxtaskcheckcommandrunsthearchgateoneverychangeandfailswhenacountgrows
grep -q "$want" <<<"$cell" || { echo "the table cell is cut: $cell"; exit 1; }
if section table | grep -q '│.*…'; then echo "a table row still ends in …"; exit 1; fi
echo "ok: the whole path and the whole table cell are on screen"
