# The copy must have reached the pty as OSC 52 and mouse capture must be off.
set -e
raw="$EVIDENCE/raw.log"
[ -s "$raw" ] || { echo "no raw pty log (DRIVE_RAW_LOG)"; exit 1; }
grep -aq $'\x1b\\[?1007h' "$raw" || { echo "alternate scroll not enabled"; exit 1; }
! grep -aq $'\x1b\\[?1000h' "$raw" || { echo "mouse capture is on"; exit 1; }
grep -aq $'\x1b\\]52;c;' "$raw" || { echo "no OSC 52"; exit 1; }
echo "ok: alt-scroll on, capture off, OSC 52 sent"
