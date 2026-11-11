#!/usr/bin/env bash
# Hidden tests against the library and CLI the model built, one verdict per
# group, plus whether its own test suite exists and passes.
here=$(cd "$(dirname "$0")" && pwd)
exec node "$here/hidden/check.mjs"
