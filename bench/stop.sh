#!/usr/bin/env bash
# Stop a running bench and everything it started (harnesses, endpoints).
lock=${BENCH_WORK:-${TMPDIR:-/tmp}}/construct-bench/bench.lock
pid=$(cat "$lock" 2>/dev/null) || { echo "no bench lock"; exit 0; }
kill -0 "$pid" 2>/dev/null || { echo "bench $pid not running"; exit 0; }
# harnesses run in their own sessions; find every descendant first
all=$pid
frontier=$pid
while [ -n "$frontier" ]; do
  next=$(for p in $frontier; do cat /proc/$p/task/*/children 2>/dev/null; done)
  all="$all $next"; frontier=$next
done
kill $all 2>/dev/null
echo "stopped bench $pid and $(($(echo $all | wc -w) - 1)) descendants"
