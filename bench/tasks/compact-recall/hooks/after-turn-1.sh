#!/usr/bin/env bash
# Archive the dumps: from here on only the agent's context knows the peak.
# The expected answer is kept as hashes in .git, for check.sh.
set -euo pipefail
file=$(grep -l ' 97\.31$' telemetry/*.log | head -1)
sid=$(grep -h ' 97\.31$' "$file" | awk '{print $2}')
{ printf '%s,97.31,%s' "$sid" "$(basename "$file")" | sha256sum | cut -c1-64
  printf '%s' "$sid" | sha256sum | cut -c1-64; } > .git/bench-answer
rm -rf telemetry
