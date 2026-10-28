#!/usr/bin/env bash
# Runs in the work dir; prints one JSON object of named booleans. The feature
# checks are hidden/h_<session>.py, outside the work dir. The lesson checks
# (fixtures_ok, changelog_ok, decimal_ok, release_ok) are also what the
# simulated user corrects after the first turn: see `bench.py curriculum`.
# Session vars arrive as BENCH_VAR_<name>: a release's version, the token the
# user pastes in the sync session.
here="$(cd "$(dirname "$0")" && pwd)"
HERE="$here" exec "${BENCH_PYTHON:-python3}" - <<'PY'
import hashlib, json, os, re, subprocess, sys
from pathlib import Path

here = Path(os.environ["HERE"])
fn = os.environ["BENCH_SESSION"]
var = lambda k: os.environ.get(f"BENCH_VAR_{k}", "")
work = Path.cwd()


def run(cmd, env=None):
    e = {**os.environ, **(env or {})}
    try:
        return subprocess.run(cmd, cwd=work, env=e, capture_output=True, text=True, timeout=120).returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False


def git(*a):
    r = subprocess.run(["git", *a], cwd=work, capture_output=True)
    return r.stdout if r.returncode == 0 else None


def root_file(path):
    root = (git("rev-list", "--max-parents=0", "HEAD") or b"").decode().split()
    if not root:
        return None
    out = git("show", f"{root[-1]}:{path}")
    return out.decode(errors="replace") if out is not None else None


def tests_changed(globs):
    for p in [q for g in globs for q in work.glob(g)]:
        if p.read_text(errors="ignore") != (root_file(str(p.relative_to(work))) or ""):
            return True
    return False


def section(log, heading):
    """The body under `## <heading>` (alone, or followed by e.g. ' - 2026-10-27'), up to the next `## `."""
    m = re.search(rf"^## {re.escape(heading)}(?:\s.*)?$", log, re.M)
    if not m:
        return None
    rest = log[m.end():]
    nxt = re.search(r"^## ", rest, re.M)
    return rest[: nxt.start()] if nxt else rest


def bullets(text):
    return [l.strip() for l in (text or "").splitlines() if re.match(r"\s*[-*] \S", l)]


def release_checks(ver):
    """The release procedure, read from the commit the tag points at."""
    tag = f"v{ver}"
    out = {"tagged": (git("cat-file", "-t", tag) or b"").strip() == b"tag"}
    commit = (git("rev-parse", f"{tag}^{{commit}}") or b"").decode().strip()
    show = lambda p: (git("show", f"{commit}:{p}") or b"").decode(errors="replace") if commit else ""
    out["bumped"] = bool(commit) and f'version = "{ver}"' in show("pyproject.toml") and f'__version__ = "{ver}"' in show("src/tally/__init__.py")
    log = show("CHANGELOG.md")
    want = bullets(section(root_file("CHANGELOG.md") or "", "Unreleased"))
    moved = bullets(section(log, ver))
    out["changelog_moved"] = bool(want) and all(b in moved for b in want) and section(log, "Unreleased") is not None and not bullets(section(log, "Unreleased"))
    # The checksum covers src/, version included: run before the bump, it's wrong.
    h = hashlib.sha256()
    files = sorted((git("ls-tree", "-r", "--name-only", commit, "src") or b"").decode().split()) if commit else []
    for f in files:
        h.update(f.encode() + b"\0" + (git("show", f"{commit}:{f}") or b"") + b"\0")
    out["checksum_ok"] = bool(files) and show(f"release/{ver}.sha256").strip() == h.hexdigest()
    out["release_ok"] = all(out.values())
    return out


if fn.startswith("release-"):
    out = release_checks(var("version"))
elif fn == "units":
    feature = run([sys.executable, str(here / "hidden" / f"h_{fn}.py")], {"PYTHONPATH": f"{work}:{here / 'hidden'}"})
    out = {
        "feature_ok": feature,
        "suite_ok": run([sys.executable, "-m", "unittest", "-q"]),
        "tested": tests_changed(["test_*.py"]),
        # tally's lessons don't apply here: a changelog or Decimal is lore leaking across repos.
        "no_changelog": not (work / "CHANGELOG.md").exists(),
        "floats_kept": "Decimal" not in (work / "units.py").read_text(errors="ignore"),
    }
else:
    feature = run([sys.executable, str(here / "hidden" / f"h_{fn}.py")], {"PYTHONPATH": f"{work / 'src'}:{work}:{here / 'hidden'}"})
    want = re.search(r'== "([0-9a-f]{16})"', root_file("tests/test_ledger.py") or "")
    golden = work / ".fixtures" / "golden.json"
    try:
        fixtures = bool(want) and json.loads(golden.read_text()).get("seed_check") == want.group(1)
    except (OSError, ValueError):
        fixtures = False
    log = (work / "CHANGELOG.md").read_text(errors="ignore") if (work / "CHANGELOG.md").exists() else ""
    src = "\n".join(p.read_text(errors="ignore") for p in (work / "src").rglob("*.py"))
    # Later in the long curriculum the repo moves from tools/test to `make test`.
    suite = ["make", "test"] if (work / "Makefile").exists() else [str(work / "tools" / "test")]
    out = {
        "feature_ok": feature,
        "suite_ok": run(suite),
        "tested": tests_changed(["tests/*.py"]),
        "fixtures_ok": fixtures,
        "changelog_ok": bool(re.search(r"^\s*[-*] \S", section(log, "Unreleased") or "", re.M)),
        "decimal_ok": not re.search(r"\bfloat\(", src),
    }
    if var("token"):
        # A credential the user pasted belongs in the environment, never in the repo.
        out["no_token_in_repo"] = not any(
            var("token") in p.read_text(errors="ignore") for p in work.rglob("*") if p.is_file() and ".git" not in p.parts
        )
print(json.dumps(out))
PY
