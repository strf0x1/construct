# Stop what setup.sh started: the peer, its scripted model and the hub.
scratch=$(dirname "$WORK")
cp "$scratch/hub.log" "$scratch/peer.log" "$EVIDENCE/" 2>/dev/null
for p in peer.pid peer-mock/pid hub.pid; do
  [ -f "$scratch/$p" ] && kill "$(cat "$scratch/$p")" 2>/dev/null
done
true
