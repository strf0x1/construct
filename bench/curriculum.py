#!/usr/bin/env python3
"""Read `bench.py curriculum` results: one table per arm of what each session
cost the user and the agent, then the summary that answers "did learning
pay": after the first session, how often the user had to correct a lesson
again, how often a known landmine went off, failed tool calls, requests and
dollars (the learner's included), and in the other repo, tally lore leaking.

    python3 bench/curriculum.py results/curriculum-*.json [...]

Files of the same model are merged (rounds split across processes); each
model prints on its own.
"""

import json
import statistics
import sys
from pathlib import Path


def mean(xs):
    xs = [x for x in xs if x is not None]
    return statistics.mean(xs) if xs else None


def f(x, nd=2):
    if x is None:
        return "-"
    return f"{x:.{nd}f}" if isinstance(x, float) else str(x)


def later(rounds):
    """Sessions after the first in the learning repo: where a lesson can pay off."""
    return [s for r in rounds for i, s in enumerate(r["sessions"]) if i > 0 and s["repo"] == "fixture"]


def summary(rounds):
    tally = later(rounds)
    other = [s for r in rounds for s in r["sessions"] if s["repo"] == "other"]
    allx = [s for r in rounds for s in r["sessions"]]
    cost = lambda s: (s.get("cost_usd") or 0) + (s.get("learn_cost_usd") or 0)
    return {
        "n_later": len(tally),
        "corrections": mean([len(s["corrections"]) for s in tally]),
        "first_try": mean([float(not s["corrections"]) for s in tally]),
        "pass": mean([float(s["pass"]) for s in tally]),
        "seed_hits": mean([s["landmines"]["seed"] for s in tally]),
        "runner_hits": mean([s["landmines"]["runner"] for s in tally]),
        "tool_errors": mean([s["tool_errors"] for s in tally]),
        "requests": mean([s["requests"] for s in tally]),
        "learn_requests": mean([s["learn_requests"] for s in allx]),
        "usd_per_session": mean([cost(s) for s in allx]),
        "learn_usd_share": (sum(s.get("learn_cost_usd") or 0 for s in allx) / max(sum(cost(s) for s in allx), 1e-9)),
        "other_pass": mean([float(s["pass"]) for s in other]),
        "other_lore_calls": mean([s["lore_calls"] for s in other]),
        "other_errors": mean([s["tool_errors"] for s in other]),
        "system_chars_last": mean([r["sessions"][-1].get("system_chars") for r in rounds]),
    }


def per_session(rounds):
    n = max(len(r["sessions"]) for r in rounds)
    rows = []
    for i in range(n):
        xs = [r["sessions"][i] for r in rounds if i < len(r["sessions"])]
        rows.append({
            "s": f"s{i} {xs[0]['task']}",
            "pass": mean([float(x["pass"]) for x in xs]),
            "corr": mean([len(x["corrections"]) for x in xs]),
            "seed": mean([x["landmines"]["seed"] for x in xs]),
            "runner": mean([x["landmines"]["runner"] for x in xs]),
            "err": mean([x["tool_errors"] for x in xs]),
            "req": mean([x["requests"] for x in xs]),
            "learn": mean([x["learn_requests"] for x in xs]),
            "sys": mean([x.get("system_chars") for x in xs]),
            "lore": mean([x["lore_calls"] for x in xs]),
        })
    return rows


def merged(paths):
    """Result files of the same model (e.g. rounds split across processes) as one."""
    by = {}
    for p in paths:
        d = json.loads(Path(p).read_text())
        m = by.setdefault(d["model"], {"model": d["model"], "rounds": 0, "raw": {}, "files": []})
        m["rounds"] += d["rounds"]
        m["files"].append(str(p))
        for arm, rounds in d["raw"].items():
            m["raw"].setdefault(arm, []).extend(rounds)
    return list(by.values())


def main(paths):
    for d in merged(paths):
        p = ", ".join(d["files"])
        print(f"\n== {p}  model={d['model']} rounds={d['rounds']}")
        for arm, rounds in d["raw"].items():
            print(f"\n-- {arm}")
            rows = per_session(rounds)
            cols = list(rows[0])
            print("  ".join(f"{c:>13}" if c == "s" else f"{c:>6}" for c in cols))
            for row in rows:
                print("  ".join(f"{row[c]:>13}" if c == "s" else f"{f(row[c], 1):>6}" for c in cols))
        print("\n-- summary (s1+ in tally unless noted; lower is better except pass/first_try)")
        sums = {arm: summary(rounds) for arm, rounds in d["raw"].items()}
        keys = list(next(iter(sums.values())))
        print(f"{'':20}" + "".join(f"{a:>13}" for a in sums))
        for k in keys:
            print(f"{k:20}" + "".join(f"{f(sums[a][k], 3 if 'usd' in k else 2):>13}" for a in sums))


if __name__ == "__main__":
    main(sys.argv[1:])
