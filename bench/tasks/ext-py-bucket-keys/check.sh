#!/usr/bin/env bash
# The scorer for an external replay: the PR's hidden tests, in bench/external.py.
here=$(cd "$(dirname "$0")" && pwd)
BENCH_TASK=$(basename "$here") exec "${BENCH_PYTHON:-python3}" "$here/../../external.py" check
