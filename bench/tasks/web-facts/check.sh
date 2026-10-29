#!/usr/bin/env bash
# One boolean per question, read from answers.txt (`N: value`, commas allowed
# in numbers). Runs in the work dir after the last turn. The expected values
# come from the vendors' own docs as of 2026-10-28 (docs/research/web-search-tooling.md,
# https://www.sqlite.org/limits.html, https://docs.kernel.org/userspace-api/landlock.html);
# if a vendor changes a limit, update expected/answers.txt and this table together.
exec "${BENCH_PYTHON:-python3}" - <<'PY'
import json, re
from pathlib import Path

WANT = {1: 100, 2: 9, 3: 1200, 4: 300, 5: 20, 6: 32766, 7: 5}
got = {}
p = Path("answers.txt")
for line in (p.read_text() if p.exists() else "").splitlines():
    m = re.match(r"\s*(\d+)\s*[:.)-]\s*(.*)$", line)
    if m:
        nums = re.findall(r"\d[\d,]*", m.group(2))
        got[int(m.group(1))] = int(nums[0].replace(",", "")) if nums else None
print(json.dumps({f"q{n}": got.get(n) == want for n, want in WANT.items()}))
PY
