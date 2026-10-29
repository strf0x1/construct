"""`bench.py compare`: two sides of a bench result, one verdict per metric.

A side is a harness (or arm) in a result file. Every numeric leaf of a run is
a metric; booleans are rates (and only booleans: a count of 0 or 1 is still a count). Runs that carry a "task" are compared per task, and pooled as `all/` when a file has
several tasks. `--only` matches the metric name without its task.

Verdicts:
  continuous  `same` when the median delta is inside the larger p10-p90
              spread, else `better`/`worse`; `inconclusive` under 3 runs a side.
  rates       Fisher's exact test: `better`/`worse` at p < 0.05, `same` when
              the rates are equal, else `inconclusive`.
  counts      (non-negative integers, mostly zero on at least one side: a
              median of 0) judged `same` by the spread rule are also judged
              as the rate of runs with a count > 0: 0 vs 7 of 10
              `edit.rewrites` is a difference even though the medians sit
              inside a 0-1 spread. A token count with one empty run is not.
Lower is better unless the metric's name says otherwise (HIGHER); counts with
no better direction (NEUTRAL) get `higher`/`lower`, never `worse`.
"""

import math
import re
import statistics

MIN_N = 3
# Raw fields that are logs or series, not metrics.
SKIP = {"findings", "rejection_codes", "failure_codes", "unintended", "turns", "records", "series", "rss", "stdout_tail", "stderr_tail", "errors", "check_error", "learned", "manifest", "tool_gaps_ms"}
# Higher is better for these; everything else (time, tokens, requests, errors,
# rejections, sizes) is lower-better. Boolean checks are higher-better unless
# their name is in LOWER_BOOL.
HIGHER = re.compile(r"(^|[._/])(success|success_rate|checks_passed|recoveries|cache_hit_rate|cached_tokens|cache_hit|cache_hit_after_cut|adoption)($|[._])|^checks\.")
# No better direction: a different editor may rightly take more or fewer calls,
# a compaction policy may rightly cut more or less often, cache writes are
# what turning caching on costs (cost_usd judges them), and a change's size
# is for the judge to weigh against the reference (docs/rust-eval.md).
# These get `higher`/`lower` instead of `better`/`worse`.
NEUTRAL = re.compile(r"(^|[._/])(tool_calls|reads|calls|applied|search\.tool|search\.shell|search\.find|n_tools|cache_write_tokens|cuts|cuts_elide|cuts_summarize|cuts_legacy|diff_lines|diff_files|test_lines|reference_lines|scope_ratio)$")
LOWER_BOOL = re.compile(r"(^|[._/])(ran_make_release|over_budget|timed_out|stopped|wrong_line|invalid)($|[._])")


def flatten(run, prefix=""):
    """Numeric leaves of one run as {dotted.name: number}. Lists of dicts are
    keyed by their "task" when they have one (sessions.variance.requests), so
    the same session lines up across sides, else by index; lists of scalars
    are skipped."""
    out = {}
    if isinstance(run, dict):
        items = run.items()
    elif isinstance(run, list) and run and all(isinstance(x, dict) for x in run):
        items = ((str(x.get("task") or i), x) for i, x in enumerate(run))
    else:
        return out
    for k, v in items:
        if k in SKIP:
            continue
        name = f"{prefix}{k}"
        if isinstance(v, (bool, int, float)):  # bools stay bools: they are rates
            out[name] = v
        elif isinstance(v, (dict, list)):
            out.update(flatten(v, name + "."))
    return out


def sides(data):
    """{side: {metric: [samples]}} from a result file's `raw`."""
    out = {}
    for side, raw in (data.get("raw") or {}).items():
        m = {}
        if isinstance(raw, dict):  # tools suite: {metric: [samples]}
            for k, v in raw.items():
                if isinstance(v, list) and all(isinstance(x, (int, float)) for x in v):
                    m[k] = list(v)
        else:
            tasks = {run.get("task") for run in raw if isinstance(run, dict)}
            for run in raw:
                task = run.get("task") if isinstance(run, dict) else None
                for k, v in flatten(run).items():
                    m.setdefault(f"{task}/{k}" if task else k, []).append(v)
                    # Runs of several tasks also pool, so a rate has every run behind it.
                    if task and len(tasks) > 1:
                        m.setdefault(f"all/{k}", []).append(v)
        out[side] = m
    return out


def higher_is_better(metric, binary):
    if binary:
        return not LOWER_BOOL.search(metric)
    return bool(HIGHER.search(metric))


def spread(xs):
    xs = sorted(xs)
    q = lambda p: xs[min(len(xs) - 1, int(round(p * (len(xs) - 1))))]
    return q(0.1), q(0.9)


def fisher(a_hits, a_n, b_hits, b_n):
    """Two-sided Fisher's exact test on a 2x2 table."""
    total, hits = a_n + b_n, a_hits + b_hits
    p = lambda k: math.comb(a_n, k) * math.comb(b_n, hits - k) / math.comb(total, hits)
    observed = p(a_hits)
    lo, hi = max(0, hits - b_n), min(a_n, hits)
    return min(1.0, sum(p(k) for k in range(lo, hi + 1) if p(k) <= observed * (1 + 1e-9)))


def verdict(metric, a, b):
    binary = all(isinstance(x, bool) for x in a + b)
    a, b = [float(x) for x in a], [float(x) for x in b]
    row = {"metric": metric, "n_a": len(a), "n_b": len(b), "binary": binary}
    up = higher_is_better(metric, binary)
    if binary:
        ra, rb = sum(a) / len(a), sum(b) / len(b)
        row.update(a=ra, b=rb, delta=rb - ra)
        if ra == rb:
            row["verdict"] = "same"
        elif min(len(a), len(b)) < MIN_N:
            row["verdict"] = "inconclusive"
        else:
            row["p"] = fisher(int(sum(a)), len(a), int(sum(b)), len(b))
            row["verdict"] = "inconclusive" if row["p"] >= 0.05 else ("better" if (rb > ra) == up else "worse")
        return row
    ma, mb = statistics.median(a), statistics.median(b)
    row.update(a=ma, b=mb, delta=mb - ma, a_spread=spread(a), b_spread=spread(b))
    if min(len(a), len(b)) < MIN_N:
        row["verdict"] = "inconclusive"
    else:
        width = max(row["a_spread"][1] - row["a_spread"][0], row["b_spread"][1] - row["b_spread"][0])
        if abs(mb - ma) <= width:
            row["verdict"] = "same"
        elif NEUTRAL.search(metric):
            row["verdict"] = "higher" if mb > ma else "lower"
        else:
            row["verdict"] = "better" if (mb > ma) == up else "worse"
        if row["verdict"] == "same" and is_count(a + b) and 0 in (ma, mb):
            nonzero(row, metric, a, b, up)
    return row


def is_count(xs):
    return all(x >= 0 and x == int(x) for x in xs)


def nonzero(row, metric, a, b, up):
    """Re-judge a `same` count by its rate of runs with a count > 0."""
    ha, hb = sum(x > 0 for x in a), sum(x > 0 for x in b)
    if ha * len(b) == hb * len(a):
        return
    row["nonzero"] = (ha / len(a), hb / len(b))
    row["p"] = fisher(ha, len(a), hb, len(b))
    more = hb / len(b) > ha / len(a)
    if row["p"] >= 0.05:
        row["verdict"] = "inconclusive"
    elif NEUTRAL.search(metric):
        row["verdict"] = "higher" if more else "lower"
    else:
        row["verdict"] = "better" if more == up else "worse"


def compare(a: dict, b: dict, only=None):
    """Rows for every metric both sides share, in a stable order."""
    rows = []
    for metric in sorted(set(a) & set(b)):
        if only and not re.search(only, metric.rpartition("/")[2]):
            continue
        if a[metric] and b[metric]:
            rows.append(verdict(metric, a[metric], b[metric]))
    return rows


def fmt(x):
    if isinstance(x, float):
        return f"{x:.3g}" if abs(x) < 1000 else f"{x:.0f}"
    return str(x)


def table(rows, a_label, b_label):
    lines = [f"| metric | {a_label} | {b_label} | delta | verdict |", "|---|---|---|---|---|"]
    for r in rows:
        if r["binary"]:
            cell = lambda side: f"{r[side]:.0%} (n={r['n_' + side]})"
        else:
            cell = lambda side: f"{fmt(r[side])} [{fmt(r[side + '_spread'][0])}–{fmt(r[side + '_spread'][1])}] (n={r['n_' + side]})"
        p = f" p={r['p']:.2f}" if "p" in r else ""
        if "nonzero" in r:
            p += f" (runs >0: {r['nonzero'][0]:.0%} vs {r['nonzero'][1]:.0%})"
        lines.append(f"| {r['metric']} | {cell('a')} | {cell('b')} | {fmt(r['delta'])} | {r['verdict']}{p} |")
    return "\n".join(lines)
