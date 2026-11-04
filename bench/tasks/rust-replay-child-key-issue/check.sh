#!/usr/bin/env bash
# The scorer for a replay task: hidden tests, `cargo xtask check` and the
# convention lint, in bench/replay.py.
here=$(cd "$(dirname "$0")" && pwd)
BENCH_TASK=$(basename "$here") exec "${BENCH_PYTHON:-python3}" "$here/../../replay.py" check
