#!/usr/bin/env bash
# Runs in the work dir; prints one JSON object of named booleans. The feature
# checks are hidden/<session>.py, outside the work dir. The lesson checks
# (fixtures_ok, changelog_ok, decimal_ok) are also what the simulated user
# corrects after the first turn: see `bench.py curriculum`.
here="$(cd "$(dirname "$0")" && pwd)"
HERE="$here" exec "${BENCH_PYTHON:-python3}" - <<'PY'
import json, os, re, subprocess, sys
from pathlib import Path

here = Path(os.environ["HERE"])
fn = os.environ["BENCH_SESSION"]
work = Path.cwd()


def run(cmd, env=None):
    e = {**os.environ, **(env or {})}
    try:
        return subprocess.run(cmd, cwd=work, env=e, capture_output=True, text=True, timeout=120).returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False


def root_file(path):
    root = subprocess.run(["git", "rev-list", "--max-parents=0", "HEAD"], cwd=work, capture_output=True, text=True).stdout.split()
    if not root:
        return None
    r = subprocess.run(["git", "show", f"{root[-1]}:{path}"], cwd=work, capture_output=True, text=True)
    return r.stdout if r.returncode == 0 else None


def tests_changed(globs):
    for p in [q for g in globs for q in work.glob(g)]:
        if p.read_text(errors="ignore") != (root_file(str(p.relative_to(work))) or ""):
            return True
    return False


feature = run([sys.executable, str(here / "hidden" / f"h_{fn}.py")], {"PYTHONPATH": f"{work / 'src'}:{work}:{here / 'hidden'}"})
if fn == "units":
    out = {
        "feature_ok": feature,
        "suite_ok": run([sys.executable, "-m", "unittest", "-q"]),
        "tested": tests_changed(["test_*.py"]),
        # tally's lessons don't apply here: a changelog or Decimal is lore leaking across repos.
        "no_changelog": not (work / "CHANGELOG.md").exists(),
        "floats_kept": "Decimal" not in (work / "units.py").read_text(errors="ignore"),
    }
else:
    want = re.search(r'== "([0-9a-f]{16})"', root_file("tests/test_ledger.py") or "")
    golden = work / ".fixtures" / "golden.json"
    try:
        fixtures = bool(want) and json.loads(golden.read_text()).get("seed_check") == want.group(1)
    except (OSError, ValueError):
        fixtures = False
    log = (work / "CHANGELOG.md").read_text(errors="ignore") if (work / "CHANGELOG.md").exists() else ""
    unreleased = log.split("## Unreleased", 1)[1].split("\n## ", 1)[0] if "## Unreleased" in log else ""
    src = "\n".join(p.read_text(errors="ignore") for p in (work / "src").rglob("*.py"))
    out = {
        "feature_ok": feature,
        "suite_ok": run([str(work / "tools" / "test")]),
        "tested": tests_changed(["tests/*.py"]),
        "fixtures_ok": fixtures,
        "changelog_ok": bool(re.search(r"^\s*[-*] \S", unreleased, re.M)),
        "decimal_ok": not re.search(r"\bfloat\(", src),
    }
print(json.dumps(out))
PY
