#!/usr/bin/env bash
# kill-matching.sh REGEX — kill processes whose command line matches REGEX,
# never this script or its ancestors (pkill -f would match the calling shell).
re=$1
skip=" $$ "; p=$$
while [ "$p" -gt 1 ]; do p=$(awk '{print $4}' /proc/$p/stat 2>/dev/null) || break; skip="$skip$p "; done
for d in /proc/[0-9]*; do
  pid=${d#/proc/}
  case "$skip" in *" $pid "*) continue;; esac
  cmd=$(tr '\0' ' ' < $d/cmdline 2>/dev/null) || continue
  [ -n "$cmd" ] && [[ $cmd =~ $re ]] && { echo "kill $pid ${cmd:0:90}"; kill "$pid" 2>/dev/null; }
done
