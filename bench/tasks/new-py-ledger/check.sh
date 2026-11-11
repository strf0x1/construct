#!/usr/bin/env bash
# Hidden tests against the CLI the model built, one verdict per group, plus
# whether its own test suite exists and passes.
here=$(cd "$(dirname "$0")" && pwd)
exec "${BENCH_PYTHON:-python3}" "$here/hidden/check.py"
