#!/usr/bin/env bash
# Drive construct's TUI through one recipe and keep the evidence.
#
#   drive.sh <recipe-dir> [cols] [rows]
#
# A recipe dir holds model.json (the scripted model's steps), drive.txt (the
# construct-dev drive script), optionally setup.sh (runs first, with WORK,
# OUTSIDE, CHOME and EVIDENCE set; write config or files there) and
# optionally serve.args (extra `serve` flags) and check.sh (runs after a passing
# drive with the same variables and
# the evidence collected; asserts what the system did, and exits non-zero to
# fail the run). The screen can look right while the tool call failed:
# check.sh is where that gets caught.
# {work} {outside} {home} {evidence} in model.json and drive.txt are replaced
# with those paths. The recipe dir's name is the feature id.
#
# Everything runs in a fresh scratch dir with its own CONSTRUCT_HOME and its
# own mock endpoint; nothing touches ~/.construct. Evidence lands in
# .verify/evidence/<run-id>/<feature-id>/ and survives cleanup:
#   transcript.txt  the drive output (snaps, metrics JSON)
#   requests.jsonl  one summary line per model request (model, tools, sizes)
#   requests/       each request body in full (req-0001.json, …): what the model was told
#   journal.txt     the session journal (seq | session | type | payload)
#   raw.log         every byte the TUI wrote, escape sequences included
#   *.png / *.svg   whatever the script wrote with `image {evidence}/…`
# Set VERIFY_NO_BUILD=1 to skip the cargo build, VERIFY_KEEP=1 to keep scratch.
set -euo pipefail
recipe=$(cd "${1:?usage: drive.sh <recipe-dir> [cols] [rows]}" && pwd)
cols=${2:-100}
rows=${3:-30}
root=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
feature=$(basename "$recipe")
run_id=${VERIFY_RUN_ID:-$(date +%Y%m%d-%H%M%S)}
export EVIDENCE="$root/.verify/evidence/$run_id/$feature"
scratch=$(mktemp -d "${TMPDIR:-/tmp}/verify-construct.XXXXXX")
export WORK="$scratch/work" OUTSIDE="$scratch/outside" CHOME="$scratch/home"
mkdir -p "$EVIDENCE" "$WORK" "$OUTSIDE" "$CHOME"

collect() {
  cp "$scratch/mock/log.jsonl" "$EVIDENCE/requests.jsonl" 2>/dev/null || true
  rm -rf "$EVIDENCE/requests" && cp -r "$scratch/requests" "$EVIDENCE/requests" 2>/dev/null || true
  for j in "$CHOME"/projects/*/journal.sqlite; do
    [ -f "$j" ] && sqlite3 "$j" "select seq, session_id, type, payload from events order by seq" >"$EVIDENCE/journal.txt"
  done
}

cleanup() {
  [ -f "$scratch/mock/pid" ] && kill "$(cat "$scratch/mock/pid")" 2>/dev/null || true
  collect
  [ "${VERIFY_KEEP:-}" = 1 ] && echo "scratch kept: $scratch" >&2 || rm -rf "$scratch"
}
trap cleanup EXIT

if [ "${VERIFY_NO_BUILD:-}" != 1 ]; then
  (cd "$root" && cargo build -q -p construct -p construct-dev)
fi

fill() {
  sed -e "s|{work}|$WORK|g" -e "s|{outside}|$OUTSIDE|g" -e "s|{home}|$CHOME|g" -e "s|{evidence}|$EVIDENCE|g" "$1"
}
fill "$recipe/model.json" >"$scratch/model.json"
fill "$recipe/drive.txt" >"$scratch/drive.txt"
[ -f "$recipe/setup.sh" ] && bash "$recipe/setup.sh"

mkdir -p "$scratch/requests"
# serve.args (optional): extra `construct-dev serve` flags, e.g. --chunk-ms to slow a stream down.
extra=(); [ -f "$recipe/serve.args" ] && read -r -a extra <"$recipe/serve.args"
port=$("$root/scripts/mock.sh" "$scratch/mock" "$scratch/model.json" --dump "$scratch/requests" ${extra[@]+"${extra[@]}"})
set +e
DRIVE_RAW_LOG="$EVIDENCE/raw.log" "$root/target/debug/construct-dev" drive --metrics --cols "$cols" --rows "$rows" --cwd "$WORK" \
  --script "$scratch/drive.txt" \
  --env CONSTRUCT_HOME="$CHOME" --env CONSTRUCT_BASE_URL="http://127.0.0.1:$port/v1" --env OPENAI_API_KEY=x \
  --env CONSTRUCT_MODEL= --env OPENROUTER_API_KEY= --env TYPESAFE_API_KEY= \
  -- "$root/target/debug/construct" >"$EVIDENCE/transcript.txt" 2>&1
status=$?
tail -n 40 "$EVIDENCE/transcript.txt"
if [ $status = 0 ] && [ -f "$recipe/check.sh" ]; then
  sleep 0.3 # let construct finish writing the journal
  collect
  echo "--- check.sh ---" >&2
  bash "$recipe/check.sh" 2>&1 | tee -a "$EVIDENCE/transcript.txt" >&2
  status=${PIPESTATUS[0]}
fi
set -e
echo "evidence: $EVIDENCE" >&2
[ $status = 0 ] && echo "PASS $feature" >&2 || echo "FAIL $feature (exit $status)" >&2
exit $status
