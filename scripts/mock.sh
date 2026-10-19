#!/usr/bin/env bash
# Start the scripted endpoint in the background: scripts/mock.sh <dir> <script.json> [extra serve args]
# Writes <dir>/port, <dir>/pid, logs requests to <dir>/log.jsonl.
set -euo pipefail
dir=$1; script=$2; shift 2
root=$(cd "$(dirname "$0")/.." && pwd)
mkdir -p "$dir"
[ -f "$dir/pid" ] && kill "$(cat "$dir/pid")" 2>/dev/null || true
rm -f "$dir/port" "$dir/log.jsonl"
"$root/target/debug/construct-dev" serve --script "$script" --log "$dir/log.jsonl" --port-file "$dir/port" "$@" >"$dir/serve.out" 2>&1 &
echo $! > "$dir/pid"
for _ in $(seq 100); do [ -s "$dir/port" ] && break; sleep 0.02; done
cat "$dir/port"
