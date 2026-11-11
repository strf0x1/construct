"""Replay a merged pull request of someone else's repository as a bench task.

construct's own replays (`replay.py`) only ever show a model this codebase, in
Rust. An `[external]` task takes any public repository: the fixture is its
tree at the PR's parent commit, without history, and the score is the PR's own
test files, which the model never sees, laid over the model's work dir and
run. Nothing of the repository is vendored: a bare mirror is cloned once into
`$BENCH_CACHE/repos` and both the fixture and the hidden tests are read from
it by commit.

    [external]
    url = "https://github.com/owner/name"
    base = "<the PR's parent commit>"
    merge = "<the merge commit>"
    hidden = ["tests/test_x.py"]     # taken from `merge` when scoring
    test = "python3 -m unittest tests.test_x"   # exit 0 = the hidden tests pass
    setup = "npm ci"                 # optional: run in the work dir once, with network
    exclude = ["node_modules"]       # optional: what setup leaves, kept out of git

    python3 bench/external.py oracle [task ...]   # base must fail, merge must pass
"""

import argparse
import json
import os
import subprocess
import sys
import tempfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import tasklib  # noqa: E402

REPOS = Path(os.environ.get("BENCH_CACHE", Path.home() / ".cache" / "construct-bench")) / "repos"
DIFF_CAP = 200_000  # bytes of diff kept in the report


def mirror(spec: dict) -> Path:
    """The bare clone of the task's repository, fetched if it lacks a commit."""
    path = REPOS / spec["url"].rstrip("/").removesuffix(".git").split("/", 3)[-1].replace("/", "--")
    if not path.exists():
        path.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(["git", "clone", "-q", "--bare", spec["url"], str(path)], check=True)
    has = lambda rev: subprocess.run(["git", "cat-file", "-e", f"{rev}^{{commit}}"], cwd=path, capture_output=True).returncode == 0
    if not all(has(spec[k]) for k in ("base", "merge")):
        subprocess.run(["git", "fetch", "-q", "origin", spec["base"], spec["merge"]], cwd=path, check=True)
    return path


def export(spec: dict, dest: Path, rev: str = "base"):
    """The repository's tree at the task's `base` (or `merge`), without history."""
    dest.mkdir(parents=True)
    archive = subprocess.run(["git", "archive", "--format=tar", spec[rev]], cwd=mirror(spec), check=True, capture_output=True).stdout
    subprocess.run(["tar", "-x", "-C", str(dest)], input=archive, check=True)


def setup(spec: dict, work: Path):
    """Install what the project's tests need, inside the work dir, and keep it
    out of git so it is neither committed nor counted as the model's change."""
    if spec.get("exclude") and (work / ".git").exists():
        with open(work / ".git" / "info" / "exclude", "a") as f:
            f.write("".join(f"/{p}\n" for p in spec["exclude"]))
    if spec.get("setup"):
        r = subprocess.run(["bash", "-c", spec["setup"]], cwd=work, capture_output=True, text=True, errors="replace")
        if r.returncode != 0:
            sys.exit(f"setup failed in {work}: {(r.stdout + r.stderr)[-800:]}")


def hidden_tests(spec: dict, work: Path, timeout: int = 600):
    """Lay the PR's test files over the work dir, run `test`, put the files
    back. (passed, the end of the output)."""
    repo, saved = mirror(spec), {}
    for path in spec["hidden"]:
        p = work / path
        saved[path] = p.read_bytes() if p.exists() else None
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(subprocess.run(["git", "show", f"{spec['merge']}:{path}"], cwd=repo, check=True, capture_output=True).stdout)
    try:
        # No bytecode cache: a .pyc is trusted by its source's size and mtime to
        # the second, so a same-length edit scored within a second ran stale code.
        env = {**os.environ, "PYTHONDONTWRITEBYTECODE": "1"}
        r = subprocess.run(["bash", "-c", spec["test"]], cwd=work, capture_output=True, text=True, errors="replace", timeout=timeout, env=env)
        ok, out = r.returncode == 0, r.stdout + r.stderr
    except subprocess.TimeoutExpired:
        ok, out = False, f"`{spec['test']}` ran past {timeout}s"
    finally:
        for path, data in saved.items():
            if data is None:
                (work / path).unlink(missing_ok=True)
            else:
                (work / path).write_bytes(data)
    return ok, "\n".join(out.splitlines()[-40:])


def diff(work: Path) -> str:
    """The model's change as one patch, new files included."""
    git = lambda *a: subprocess.run(["git", *a], cwd=work, capture_output=True, text=True, errors="replace").stdout
    out = git("diff", "--no-color", "--no-ext-diff", "HEAD")
    for path in git("ls-files", "--others", "--exclude-standard").splitlines():
        out += git("diff", "--no-color", "--no-index", "/dev/null", path)
    return out


def score(task, work: Path):
    """(verdict, report) for the work dir as the model left it."""
    spec = task.spec["external"]
    ok, out = hidden_tests(spec, work)
    patch = diff(work)
    lines = [l for l in patch.splitlines() if l[:1] in "+-" and l[:3] not in ("+++", "---")]
    report = {"url": spec["url"], "pr": spec.get("pr"), "diff_lines": len(lines), "hidden_tail": "" if ok else out, "diff_text": patch[:DIFF_CAP]}
    return {"hidden_tests": ok, "changed_something": bool(lines)}, report


def cmd_check(_args):
    task = tasklib.load(os.environ["BENCH_TASK"])  # set by the task's check.sh
    verdict, report = score(task, Path.cwd())
    if os.environ.get("BENCH_REPORT"):
        Path(os.environ["BENCH_REPORT"]).write_text(json.dumps(report))
    print(json.dumps(verdict))


def cmd_oracle(args):
    """Prove each task: the hidden tests fail on the untouched base and pass
    on the merged tree. Exit 1 if any task is not proven."""
    names = args.tasks or [n for n in tasklib.names() if "external" in tasklib.load(n).spec]
    bad = 0
    for name in names:
        spec = tasklib.load(name).spec["external"]
        got = {}
        for rev in ("base", "merge"):
            with tempfile.TemporaryDirectory() as tmp:
                work = Path(tmp) / "work"
                export(spec, work, rev)
                setup(spec, work)
                got[rev], out = hidden_tests(spec, work)
                if got[rev] != (rev == "merge") and args.verbose:
                    print(out)
        proven = not got["base"] and got["merge"]
        bad += not proven
        print(f"{'ok  ' if proven else 'BAD '} {name}: base {'passes' if got['base'] else 'fails'}, merge {'passes' if got['merge'] else 'fails'}")
    sys.exit(1 if bad else 0)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("check", help="score the current directory (run by a task's check.sh)").set_defaults(fn=cmd_check)
    o = sub.add_parser("oracle", help="prove tasks: base fails the hidden tests, merge passes")
    o.add_argument("tasks", nargs="*", help="default: every [external] task")
    o.add_argument("-v", "--verbose", action="store_true", help="print the test output of a side that went the wrong way")
    o.set_defaults(fn=cmd_oracle)
    a = ap.parse_args()
    a.fn(a)


if __name__ == "__main__":
    main()
