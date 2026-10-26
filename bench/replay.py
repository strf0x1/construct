#!/usr/bin/env python3
"""Replay tasks (bench/tasks/rust-replay-*): a merged PR, redone by a model.

The fixture is this repo at the PR's parent (task.toml `git`), the prompt is
the PR's intent, and the score comes from the PR's own tests, which the model
never sees. docs/rust-eval.md says how to run them and judge the results.

    replay.py check            the scorer: run by check.sh in the work dir
    replay.py warm TASK ...    build each fixture once into a cached target
                               the work dirs are reflinked from; also proves
                               the fixture passes `cargo xtask check`
    replay.py oracle TASK ...  apply the real PR to a fresh fixture and score
                               it: every check must pass, or the task is wrong
    replay.py pairs A.json B.json --out DIR
                               blinded pairs of runs for the model judge, each
                               in both orders, and the key that unblinds them
    replay.py tally DIR        read the judge's answers (<pair>.out) and count
                               wins per criterion for A and B

task.toml `[replay]`:
    pr               the PR number, for people
    merge            the merge commit: hidden tests and the reference diff
    hidden           files copied from `merge` over the work dir for the
                     hidden tests, then put back
    reference_paths  pathspecs both diffs are measured over (default crates)
    [[replay.tests]] package, test (the test target), names: each must pass
                     (no names: the whole target, as one result)

The scorer prints {xtask_check, hidden_tests, conventions} plus one boolean per
hidden test, and writes the lint findings, the diff, and its size against the
reference PR to $BENCH_REPORT.
"""

import argparse
import json
import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import convention_lint  # noqa: E402
import tasklib  # noqa: E402

ROOT = tasklib.ROOT
DIFF_CAP = 400_000  # bytes of diff kept for the judge


def spec(task):
    r = task.spec["replay"]
    return {**r, "reference_paths": r.get("reference_paths", ["crates"])}


def run(cmd, cwd, env=None, timeout=None):
    # Timing limits in e2e tests are smoke limits; a loaded scorer machine
    # must not fail a model for them (CI sets the same).
    env = {"CONSTRUCT_E2E_SLOW": "1", **(env or {})}
    r = subprocess.run(cmd, cwd=cwd, env={**os.environ, **env}, capture_output=True, text=True, timeout=timeout)
    return r.returncode, r.stdout + r.stderr


def tail(text, n=60):
    return "\n".join(text.splitlines()[-n:])


def hidden_tests(task, work: Path):
    """Overlay the PR's test files, run each named test, put the files back.
    {name: passed}, and the output of any group that failed."""
    r = spec(task)
    saved = {}
    for path in r["hidden"]:
        p = work / path
        saved[path] = p.read_bytes() if p.exists() else None
        blob = subprocess.run(["git", "show", f"{r['merge']}:{path}"], cwd=ROOT, check=True, capture_output=True).stdout
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(blob)
    results, failed = {}, []
    try:
        for g in r["tests"]:
            names = g.get("names") or []
            rc, out = run(["cargo", "test", "-p", g["package"], "--test", g["test"], "--", *(["--exact", *names] if names else [])], work)
            for name in names:
                results[name] = bool(re.search(rf"^test {re.escape(name)} \.\.\. ok$", out, re.M))
            if not names:  # the whole target, as one result
                results[g["test"]] = rc == 0
            if rc != 0 or not all(results[n] for n in names or [g["test"]]):
                failed.append(tail(out))
    finally:
        for path, data in saved.items():
            p = work / path
            if data is None:
                p.unlink(missing_ok=True)
            else:
                p.write_bytes(data)
    return results, "\n\n".join(failed)


def diff_text(work: Path):
    """The change as one patch, new files included, for the judge."""
    out = subprocess.run(["git", "diff", "--no-color", "--no-ext-diff", "HEAD"], cwd=work, capture_output=True, text=True).stdout
    new = subprocess.run(["git", "ls-files", "--others", "--exclude-standard"], cwd=work, capture_output=True, text=True).stdout
    for path in new.splitlines():
        out += subprocess.run(["git", "diff", "--no-color", "--no-index", "/dev/null", path], cwd=work, capture_output=True, text=True).stdout
    return out[:DIFF_CAP] + ("\n[diff cut]\n" if len(out) > DIFF_CAP else "")


def size(diff):
    return diff["added"] + diff["removed"]


def score(task, work: Path):
    """(verdict, report) for the work dir as the model left it."""
    r = spec(task)
    rc, out = run(["cargo", "xtask", "check"], work)
    xtask_ok = rc == 0 and out.rstrip().endswith("ok")
    # clippy needs a build; after a passing check it is warm.
    lint = convention_lint.lint(work, "HEAD", clippy=xtask_ok)
    mine = convention_lint.lint(work, "HEAD", paths=r["reference_paths"])["diff"]
    ref = convention_lint.lint(ROOT, f"{r['merge']}^1", r["merge"], paths=r["reference_paths"])["diff"]
    hidden, hidden_out = hidden_tests(task, work)
    verdict = {
        "xtask_check": xtask_ok,
        "hidden_tests": bool(hidden) and all(hidden.values()),
        "conventions": lint["violations"] == 0,
        **{f"hidden.{n}": ok for n, ok in hidden.items()},
    }
    report = {
        "pr": r.get("pr"),
        "lint": {"counts": lint["counts"], "findings": lint["findings"]},
        "lint_violations": lint["violations"],
        "scope": {
            "diff_lines": size(lint["diff"]),
            "diff_files": lint["diff"]["files"],
            "test_lines": mine["test_added"],
            "reference_lines": size(ref),
            "scope_ratio": round(size(mine) / size(ref), 3) if size(ref) else None,
        },
        "xtask_tail": "" if xtask_ok else tail(out),
        "hidden_tail": hidden_out,
        "diff_text": diff_text(work),
    }
    return verdict, report


def cmd_check(_args):
    task = tasklib.load(os.environ["BENCH_TASK"])  # set by the task's check.sh
    verdict, report = score(task, Path.cwd())
    if os.environ.get("BENCH_REPORT"):
        Path(os.environ["BENCH_REPORT"]).write_text(json.dumps(report))
    print(json.dumps(verdict))


def cmd_warm(args):
    for name in args.tasks:
        task = tasklib.load(name)
        base = tasklib.WARM / name
        src = base / "src"
        tasklib.materialize(task, src, {}, warm=False)
        shutil.rmtree(base / "target", ignore_errors=True)
        print(f"{name}: cargo xtask check in {src}", flush=True)
        # In place, not via CARGO_TARGET_DIR: the e2e tests run <root>/target/debug/construct.
        rc, out = run(["cargo", "xtask", "check"], src)
        print(tail(out, 8))
        (src / "target").rename(base / "target")
        shutil.rmtree(src)
        if rc != 0:
            sys.exit(f"{name}: the fixture itself fails `cargo xtask check`")


def cmd_oracle(args):
    """The real PR must pass every check; with --baseline the untouched
    fixture must fail the hidden tests and pass `cargo xtask check`."""
    bad = []
    for name in args.tasks:
        task = tasklib.load(name)
        work = tasklib.WARM / name / "oracle"
        tasklib.materialize(task, work, {})
        r = spec(task)
        if args.baseline:
            verdict, report = score(task, work)
            print(f"{name} baseline: {json.dumps(verdict)}")
            if verdict["hidden_tests"] or not verdict["xtask_check"]:
                bad.append(f"{name} baseline")
            tasklib.materialize(task, work, {})
        patch = subprocess.run(["git", "diff", "--binary", f"{r['merge']}^1", r["merge"], "--", *r["reference_paths"]], cwd=ROOT, check=True, capture_output=True).stdout
        subprocess.run(["git", "apply", "--whitespace=nowarn"], cwd=work, input=patch, check=True)
        verdict, report = score(task, work)
        print(f"{name} oracle: {json.dumps(verdict)}")
        print(f"  scope {json.dumps(report['scope'])} lint {json.dumps({k: v for k, v in report['lint']['counts'].items() if v})}")
        for part in ("xtask_tail", "hidden_tail"):
            if report[part]:
                print(f"  {part}:\n{report[part]}")
        if not all(verdict.values()):
            bad.append(name)
        if not args.keep:
            shutil.rmtree(work)
    if bad:
        sys.exit(f"FAILED: {', '.join(bad)}")
    print("ok")


CRITERIA = ("idiom", "conventions", "scope", "honesty", "overall")
JUDGE = """You are reviewing two changes to construct, a Rust agent harness, made
by two different developers from the same request. You don't know who made
which. Judge only what is on the page: the request, each diff, and each
developer's own summary of their work. Hidden tests already decided whether
each change works; your job is how it is written.

Criteria (docs/rust-eval.md has the long form):
- idiom: idiomatic, simple Rust. Ownership and borrowing without needless
  clones; errors as values (`?`, `Result`), no panics on input; types that
  make wrong states hard; no dead code, no reinvented std.
- conventions: the repo's AGENTS.md rules: a `//!` paragraph on new modules;
  no `unwrap()` outside tests (a commented Mutex lock is fine); tool names
  referenced, never retyped; one concern per file; dependencies only via
  [workspace.dependencies]; tests at the highest tier that can observe the
  behaviour (e2e preferred); comments that say why, matching the code around.
- scope: does what was asked and nothing else. No drive-by refactors,
  unrelated files or speculative features; nothing asked for left out.
- honesty: does the summary match the diff? Penalize claims of tests or
  checks that the diff doesn't show, work described that isn't there, and
  changes made but not mentioned. A short, accurate summary beats a long one.
- overall: which change would you rather merge?

Answer with a few sentences per criterion, then end with exactly one JSON
object on its own line, each value "X", "Y" or "tie":
{"idiom": "...", "conventions": "...", "scope": "...", "honesty": "...", "overall": "..."}
"""


def runs(path: Path, side):
    raw = json.loads(path.read_text())["raw"]
    side = side or next(iter(raw))
    return {t: [r for r in raw[side] if r.get("task") == t] for t in {r.get("task") for r in raw[side]}}


def change(run):
    rep = run.get("report") or {}
    return f"```diff\n{rep.get('diff_text') or '(no diff)'}```\n\nTheir summary:\n\n> " + (run.get("final_message") or "(none)").replace("\n", "\n> ")


def cmd_pairs(args):
    import random

    a, b = runs(args.a, args.side), runs(args.b, args.side)
    rng = random.Random(args.seed)
    out = args.out
    out.mkdir(parents=True, exist_ok=True)
    key = {}
    for task in sorted(set(a) & set(b)):
        prompt = tasklib.load(task).turns[0]
        for i, (ra, rb) in enumerate(zip(a[task], b[task])):
            # Each pair twice, X and Y swapped, so a judge's taste for the
            # first answer cancels out. Pair ids are shuffled names.
            for order in ((ra, rb, "A", "B"), (rb, ra, "B", "A")):
                pid = f"p{rng.getrandbits(32):08x}"
                x, y, kx, ky = order
                (out / f"{pid}.md").write_text(f"{JUDGE}\n# The request\n\n{prompt}\n\n# Change X\n\n{change(x)}\n\n# Change Y\n\n{change(y)}\n")
                key[pid] = {"task": task, "round": i, "X": kx, "Y": ky}
    (out / "key.json").write_text(json.dumps({"a": str(args.a), "b": str(args.b), "pairs": key}, indent=1))
    print(f"{len(key)} prompts in {out}: give each <id>.md to the judge, save its answer as <id>.out, then `replay.py tally {out}`")


def cmd_tally(args):
    key = json.loads((args.dir / "key.json").read_text())
    wins = {c: {"A": 0, "B": 0, "tie": 0} for c in CRITERIA}
    missing = []
    for pid, k in key["pairs"].items():
        f = args.dir / f"{pid}.out"
        found = re.findall(r"\{[^{}]*\}", f.read_text()) if f.exists() else []
        if not found:
            missing.append(pid)
            continue
        v = json.loads(found[-1])
        for c in CRITERIA:
            pick = v.get(c)
            wins[c][k[pick] if pick in ("X", "Y") else "tie"] += 1
    print(f"A = {key['a']}\nB = {key['b']}\n{'criterion':12} {'A':>4} {'B':>4} {'tie':>4}")
    for c, w in wins.items():
        print(f"{c:12} {w['A']:>4} {w['B']:>4} {w['tie']:>4}")
    if missing:
        print(f"no answer yet for {len(missing)}: {', '.join(missing[:8])}")
    print(json.dumps({"wins": wins, "missing": missing}))


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("check")
    w = sub.add_parser("warm")
    w.add_argument("tasks", nargs="+")
    o = sub.add_parser("oracle")
    o.add_argument("tasks", nargs="+")
    o.add_argument("--baseline", action="store_true", help="also score the untouched fixture (it must fail the hidden tests)")
    o.add_argument("--keep", action="store_true", help="keep the oracle work dir")
    p = sub.add_parser("pairs")
    p.add_argument("a", type=Path)
    p.add_argument("b", type=Path)
    p.add_argument("--side", default=None, help="harness or arm in both files (default: the first)")
    p.add_argument("--out", type=Path, required=True)
    p.add_argument("--seed", type=int, default=None)
    t = sub.add_parser("tally")
    t.add_argument("dir", type=Path)
    args = ap.parse_args()
    {"check": cmd_check, "warm": cmd_warm, "oracle": cmd_oracle, "pairs": cmd_pairs, "tally": cmd_tally}[args.cmd](args)


if __name__ == "__main__":
    main()
