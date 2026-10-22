#!/usr/bin/env bash
# Scores the work dir against the conventions the agent read first. The
# per-run values are read back from docs/CONVENTIONS.md, which the task
# fills from [vars].
exec "${BENCH_PYTHON:-python3}" - <<'PY'
import importlib, json, re, subprocess, sys
from pathlib import Path

sys.path.insert(0, ".")
conv = Path("docs/CONVENTIONS.md").read_text()
prefix = re.search(r"Error codes are `([A-Za-z0-9]+)-`", conv).group(1).upper()
suite = re.search(r"`# suite: ([^`]+)`", conv).group(1)
ticket = re.search(r"`## Unreleased \(([^)]+)\)`", conv).group(1)
FUNCS = {"stock": ["receive", "adjust", "write_off"], "picking": ["pick", "repick"],
         "transfers": ["transfer", "split_transfer"], "returns": ["accept_return"]}

checks = {}
r = subprocess.run([sys.executable, "-m", "unittest", "-q"], capture_output=True, timeout=120)
checks["tests_pass"] = r.returncode == 0
errors = importlib.import_module("inventory.errors")
codes, validated, valid_ok = {}, True, True
for mod, names in FUNCS.items():
    m = importlib.import_module(f"inventory.{mod}")
    for n in names:
        f = getattr(m, n)
        seen = set()
        for bad in (0, -2, "3", 1.5, True):
            try:
                f("A1", bad)
                validated = False
            except errors.InventoryError as e:
                seen.add(str(e.code))
            except Exception:
                validated = False
        codes[n] = seen
        try:
            f("A1", 2)
        except Exception:
            valid_ok = False
checks["all_validated"] = validated
checks["valid_still_works"] = valid_ok
flat = [c for s in codes.values() for c in s]
checks["codes_follow_prefix"] = bool(flat) and all(re.fullmatch(rf"{prefix}-\d{{3}}", c, re.I) for c in flat)
checks["codes_unique_per_function"] = all(len(s) >= 1 for s in codes.values()) and len({c for s in codes.values() for c in s}) >= len(codes)
checks["codes_not_reserved"] = bool(flat) and all(int(c[-3:]) < 900 for c in flat)
t = Path("tests/test_quantity.py")
checks["suite_marker"] = t.exists() and t.read_text().splitlines()[:1] == [f"# suite: {suite}"]
log = Path("CHANGELOG.md").read_text()
checks["changelog_heading"] = f"## Unreleased ({ticket})" in log
print(json.dumps(checks))
PY
