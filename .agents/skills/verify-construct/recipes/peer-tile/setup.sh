# A real hub in the TUI's home with its pairing window open, and a real peer
# (its own home, work dir and scripted model) waiting for approval as old-pc.
# The peer's model reads a file outside its work dir, so it asks the hub.
set -e
root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
bin=$root/target/debug/construct
scratch=$(dirname "$WORK")
mkdir -p "$scratch/peer-home" "$scratch/peer-work"
echo "secret value" >"$OUTSIDE/secret.txt"
port=$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1",0)); print(s.getsockname()[1])')
CONSTRUCT_HOME=$CHOME "$bin" hub --listen "127.0.0.1:$port" >"$scratch/hub.log" 2>&1 &
echo $! >"$scratch/hub.pid"
for _ in $(seq 100); do
  CONSTRUCT_HOME=$CHOME "$bin" hub status --json 2>/dev/null | grep -q '"running": true' && break
  sleep 0.05
done
CONSTRUCT_HOME=$CHOME "$bin" hub pair --for 10m >/dev/null
key=$(CONSTRUCT_HOME=$CHOME "$bin" hub key)
printf '{"steps":[{"call":"read","args":{"path":"%s"}},{"text":"peer finished"}]}\n' "$OUTSIDE/secret.txt" >"$scratch/peer-model.json"
pport=$("$root/scripts/mock.sh" "$scratch/peer-mock" "$scratch/peer-model.json")
printf '[router]\nmode = "local"\n[learn]\nenabled = false\n' >"$scratch/peer-home/config.toml"
(cd "$scratch/peer-work" && CONSTRUCT_HOME="$scratch/peer-home" CONSTRUCT_BASE_URL="http://127.0.0.1:$pport/v1" \
  OPENAI_API_KEY=x CONSTRUCT_MODEL= OPENROUTER_API_KEY= TYPESAFE_API_KEY= \
  "$bin" peer --url "ws://127.0.0.1:$port" --hub-key "$key" --id old-pc >"$scratch/peer.log" 2>&1 &
  echo $! >"$scratch/peer.pid")
for _ in $(seq 200); do grep -q "waiting for approval" "$scratch/peer.log" 2>/dev/null && break; sleep 0.05; done
grep -q "waiting for approval" "$scratch/peer.log" || { cat "$scratch/peer.log" "$scratch/hub.log"; exit 1; }
