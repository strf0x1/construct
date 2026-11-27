# Peer motion played cleanly and the hub/peer actions really happened.
set -e
sec() { sed -n "/^--- snap $1 @/,/^---\$/p" "$EVIDENCE/transcript.txt" | grep -v '^--- snap'; }
strip() { sed 's/⟦[^⟧]*⟧//g'; }
first() {
  for n in 1 2 3 4; do if sec "$1-$n" | eval "$2"; then echo "$1-$n"; return 0; fi; done; return 1
}

scratch=$(dirname "$WORK")
grep -q '"id": "old-pc"' "$CHOME/hub/peers.json" && grep -q '"state": "pinned"' "$CHOME/hub/peers.json" \
  || { echo "old-pc is not pinned in the hub's registry"; cat "$CHOME/hub/peers.json"; exit 1; }
code=$(sed -n 's/.*code \([A-Z0-9-]*\) (it must match.*/\1/p' "$scratch/peer.log" | head -1)
[ -n "$code" ] && grep -q "$code" "$EVIDENCE/transcript.txt" \
  || { echo "the popup did not show the peer's code ($code)"; exit 1; }
j=$(ls "$scratch"/peer-home/projects/*/journal.sqlite)
sqlite3 "$j" "select payload from events where type = 'tool_end'" | grep -q '"ok":true.*secret value' \
  || { echo "the peer's read did not run"; sqlite3 "$j" "select type, payload from events"; exit 1; }
sqlite3 "$j" "select payload from events where type = 'access'" | grep -q '"decision":"once"' \
  || { echo "no access row with decision once on the peer"; exit 1; }
grep -q '"model": "small"' "$CHOME/hub/peers.json" \
  || { echo "the hub did not keep old-pc's model override"; cat "$CHOME/hub/peers.json"; exit 1; }

# No shaded block noise in any arrival snap (beam reveal, not blocks).
for n in arrive-1 arrive-2 arrive-3 join-settled; do
  if sec "$n" | strip | grep -q -e '░' -e '▒' -e '▓'; then
    echo "shaded blocks appeared in $n"; exit 1
  fi
done

# Selection sweep: moving down to Deny sweeps it white.
s=$(first sel "grep 'Deny' | grep -q 'fg=#ebeeff'") || { echo "no snap caught the peer popup selection sweep"; exit 1; }
echo "selection sweep caught in $s"

# Confirm pop + green ghost after approving JOIN.
s=$(first ghost "grep -q -e '⟦fg=#9ece6a⟧╰' -e '⟦fg=#9ece6a⟧╯' -e '⟦fg=#9ece6a⟧│'") \
  || { echo "no snap caught the approved peer popup dissolving green"; exit 1; }
echo "green dissolve ghost caught in $s"

# Working peer in the tile uses the orbital indicator while motion is on.
sec ask-settled | strip | grep -Eq '(◐|◓|◑|◒) old-pc working' \
  || { echo "ask-settled did not show the orbital working glyph"; sec ask-settled | strip | head -3; exit 1; }

# Attach view scroll fade dims the top scrolled row compared to lower rows.
top_line=$(sec attach-settled | grep 'peer output line' | head -1)
echo "$top_line" | grep -q 'fg=#' || { echo "attach top row was not scroll-faded: $top_line"; exit 1; }

# Tile stays crisp while attach dissolves away on esc.
sec attach-close-1 | strip | grep -q 'old-pc online' \
  || { echo "peer tile was clobbered during attach close"; sec attach-close-1 | strip | head -3; exit 1; }

echo "ok: peer beam arrival, selection sweep, green dissolve ghost, orbital working tile, attach scroll fade, and real hub/peer state verified"
