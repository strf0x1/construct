#!/usr/bin/env python3
"""The search suite's verdict: what changes when the search tool has another
name, or is not there. Reads task results (bench.py task, arms grep / search /
none), merges the files of one model, and prints per model:

  adoption   of the times the model searched file contents, how often it used
             the tool and not a shell command: call-weighted (all calls) and
             "used the tool at least once" (per searching run)
  cost       requests, prompt tokens, wall time and dollars per run
  failures   tool calls that failed, over every tool

with a 95% bootstrap interval for every difference (resampling runs, 4000
times), so a gap inside the noise reads as one. Success rates come with a
Fisher exact test. Merged files are written beside the inputs so
`bench.py compare merged.json --a grep --b search` judges the same runs.

    python3 bench/search_report.py bench/results/task-A.json bench/results/task-B.json ...
"""

import argparse
import json
import sys
from collections import defaultdict
from pathlib import Path

import numpy as np

import compare

RESULTS = Path(__file__).resolve().parent / "results"
ARMS = ("grep", "search", "none")
B = 4000


def load(files):
    """{model: {arm: [run]}} over all files; runs the provider broke are left out and counted."""
    by = defaultdict(lambda: defaultdict(list))
    dropped = defaultdict(int)
    for f in files:
        d = json.loads(Path(f).read_text())
        for arm, runs in d["raw"].items():
            for r in runs:
                if r.get("invalid"):
                    dropped[d["model"]] += 1
                else:
                    by[d["model"]][arm].append(r)
    return by, dropped


def merge(files, out_dir=RESULTS):
    """One result file per model with every run, for `bench.py compare`."""
    by, _ = load(files)
    first = {}
    for f in files:
        d = json.loads(Path(f).read_text())
        first.setdefault(d["model"], d)
    paths = {}
    for model, arms in by.items():
        d = first[model]
        p = Path(out_dir) / f"search-merged-{model.replace('/', '_')}.json"
        p.write_text(json.dumps({"model": model, "rounds": sum(len(v) for v in arms.values()) // max(1, len(arms) * len(d["tasks"])), "tasks": d["tasks"], "arms": d.get("arms"), "raw": arms}))
        paths[model] = p
    return paths


def weighted(runs):
    t, s = sum(r["search"]["tool"] for r in runs), sum(r["search"]["shell"] for r in runs)
    return t / (t + s) if t + s else float("nan")


def used_tool(runs):
    searching = [r for r in runs if r["search"]["tool"] + r["search"]["shell"]]
    return sum(r["search"]["tool"] > 0 for r in searching) / len(searching) if searching else float("nan")


def failure_rate(runs):
    calls = sum(r["tool_failures"]["calls"] for r in runs)
    return sum(r["tool_failures"]["failed"] for r in runs) / calls if calls else float("nan")


def mean_of(key):
    return lambda runs: float(np.mean([r[key] or 0 for r in runs]))


def interval(fn, a, b, rng):
    """fn(a) - fn(b) with a 95% bootstrap interval, resampling runs in each arm."""
    ia = rng.integers(0, len(a), (B, len(a)))
    ib = rng.integers(0, len(b), (B, len(b)))
    diffs = np.array([fn([a[i] for i in x]) - fn([b[i] for i in y]) for x, y in zip(ia, ib)])
    diffs = diffs[~np.isnan(diffs)]
    point = fn(a) - fn(b)
    lo, hi = np.percentile(diffs, [2.5, 97.5]) if len(diffs) else (float("nan"),) * 2
    return point, lo, hi


def verdict(lo, hi):
    return "differs" if lo > 0 or hi < 0 else "no clear difference"


def report(by, dropped, out=sys.stdout):
    rng = np.random.default_rng(7)
    p = lambda *a: print(*a, file=out)
    for model, arms in sorted(by.items()):
        n = {a: len(arms[a]) for a in ARMS if a in arms}
        p(f"\n## {model}  ({', '.join(f'{a}: {k} runs' for a, k in n.items())}" + (f"; {dropped[model]} provider-broken runs left out" if dropped[model] else "") + ")\n")
        p("| arm | success | adoption (calls) | used the tool (runs) | requests/run | prompt tok/run | wall s/run | $/run | tool failures |")
        p("|---|---|---|---|---|---|---|---|---|")
        for a in ARMS:
            r = arms.get(a)
            if not r:
                continue
            ok = sum(x["success"] for x in r)
            adopt = "–" if a == "none" else f"{weighted(r):.2f}"
            used = "–" if a == "none" else f"{used_tool(r):.2f}"
            p(f"| {a} | {ok}/{len(r)} | {adopt} | {used} | {mean_of('requests')(r):.2f} | {mean_of('prompt_tokens')(r):.0f} | {mean_of('wall_s')(r):.1f} | {mean_of('cost_usd')(r):.4f} | {failure_rate(r):.1%} |")
        p("\nDifferences (95% bootstrap interval):\n")
        p("| question | difference | interval | |")
        p("|---|---|---|---|")
        pairs = [("name: grep vs search", "grep", "search", [("adoption (calls)", weighted), ("used the tool (runs)", used_tool)])]
        pairs += [(f"tool vs no tool: {x} vs none", x, "none", [(k, f) for k, f in (("requests/run", mean_of("requests")), ("prompt tok/run", mean_of("prompt_tokens")), ("$/run", mean_of("cost_usd")), ("wall s/run", mean_of("wall_s")))]) for x in ("grep", "search")]
        pairs += [("name: grep vs search", "grep", "search", [(k, f) for k, f in (("requests/run", mean_of("requests")), ("prompt tok/run", mean_of("prompt_tokens")), ("$/run", mean_of("cost_usd")), ("tool failures", failure_rate))])]
        for label, x, y, metrics in pairs:
            if x not in arms or y not in arms:
                continue
            for name, fn in metrics:
                d, lo, hi = interval(fn, arms[x], arms[y], rng)
                p(f"| {label}: {name} | {d:+.3g} | [{lo:+.3g}, {hi:+.3g}] | {verdict(lo, hi)} |")
        for x, y in (("grep", "none"), ("search", "none"), ("grep", "search")):
            if x in arms and y in arms:
                a, b = arms[x], arms[y]
                fa, fb = sum(r["success"] for r in a), sum(r["success"] for r in b)
                p(f"| success: {x} vs {y} | {fa / len(a) - fb / len(b):+.1%} | Fisher p={compare.fisher(fa, len(a), fb, len(b)):.2f} | |")
        # where the tool gets used: by task
        tasks = sorted({r["task"] for r in arms.get("grep", [])})
        p("\nAdoption by task (calls; `grep` arm / `search` arm):\n")
        p("| task | " + " | ".join(t.removeprefix("search-") for t in tasks) + " |")
        p("|---|" + "---|" * len(tasks))
        p("| grep / search | " + " | ".join(f"{weighted([r for r in arms['grep'] if r['task'] == t]):.2f} / {weighted([r for r in arms.get('search', []) if r['task'] == t]):.2f}" for t in tasks) + " |")


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("files", nargs="+")
    ap.add_argument("--merge", action="store_true", help="also write one merged result file per model")
    args = ap.parse_args()
    by, dropped = load(args.files)
    report(by, dropped)
    if args.merge:
        for model, path in merge(args.files).items():
            print(f"\nmerged {model}: {path}")


if __name__ == "__main__":
    main()
