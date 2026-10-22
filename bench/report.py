#!/usr/bin/env python3
"""Render the latest result of each bench suite as one markdown table.

Every number is shown, including the ones construct loses. Lower is better
everywhere except success rate.
"""

import json
import sys
from pathlib import Path

RESULTS = Path(__file__).resolve().parent / "results"
NAMES = ["construct", "hermes", "opencode"]


def latest(suite):
    files = sorted(RESULTS.glob(f"{suite}-*.json"))
    return (json.loads(files[-1].read_text()), files[-1].name) if files else (None, None)


def fmt(v, unit="", digits=1):
    if v is None:
        return "–"
    if isinstance(v, float):
        v = round(v, digits)
    return f"{v}{unit}"


def row(label, values, lower_better=True):
    present = [v for v in values if v is not None]
    best = (min(present) if lower_better else max(present)) if present else None
    cells = []
    for v in values:
        s = v if isinstance(v, str) else fmt(v)
        cells.append(f"**{s}**" if v is not None and v == best and len(present) > 1 else s)
    return f"| {label} | " + " | ".join(cells) + " |"


def med(s, key):
    x = (s or {}).get(key)
    return x["median"] if x else None


def main():
    out = ["| metric | " + " | ".join(NAMES) + " |", "|---|" + "---|" * len(NAMES)]
    sources = []

    d, f = latest("size")
    if d:
        sources.append(f)
        out.append(row("size on disk, install (MB)", [d[n]["install"] / 1e6 for n in NAMES]))
        out.append(row("fresh home after first run (MB)", [d[n].get("fresh_home_after_first_run", 0) / 1e6 for n in NAMES]))

    d, f = latest("frame")
    if d:
        sources.append(f)
        s = d["summary"]
        out.append(row(f"first frame, median of {d['rounds']} (ms)", [med(s[n], "first_frame_ms") for n in NAMES]))
        out.append(row("ready to type, median (ms)", [med(s[n], "ready_ms") for n in NAMES]))

    d, f = latest("tools")
    if d:
        sources.append(f)
        s = d["summary"]
        out.append(row("tool overhead: read, p50 (ms)", [med(s[n], "read") for n in NAMES]))
        out.append(row("tool overhead: read, p90 (ms)", [(s[n].get("read") or {}).get("p90") for n in NAMES]))
        out.append(row("tool overhead: shell, p50 (ms)", [med(s[n], "shell") for n in NAMES]))
        out.append(row("tool overhead: shell, p90 (ms)", [(s[n].get("shell") or {}).get("p90") for n in NAMES]))

    d, f = latest("memory")
    if d:
        sources.append(f)
        s = d["summary"]
        out.append(row(f"peak RSS, {d['steps']}-step task (MB)", [med(s[n], "peak_mb") for n in NAMES]))
        out.append(row("mean RSS, same task (MB)", [med(s[n], "mean_mb") for n in NAMES]))

    d, f = latest("task")
    if d:
        sources.append(f)
        s = d["summary"]
        out.append(row(f"real task success ({d['model']})", [s[n]["success_rate"] for n in NAMES], lower_better=False))
        out.append(row("checks passed (of 5), median", [med(s[n], "checks_passed") for n in NAMES], lower_better=False))
        out.append(row("first request prompt tokens", [med(s[n], "first_request_tokens") for n in NAMES]))
        out.append(row("total prompt tokens, 3 turns", [med(s[n], "prompt_tokens") for n in NAMES]))
        out.append(row("total completion tokens", [med(s[n], "completion_tokens") for n in NAMES]))
        pct = lambda v, k=100: None if v is None else v * k
        out.append(row("prompt cache hit rate (%)", [pct(med(s[n], "cache_hit_rate")) for n in NAMES], lower_better=False))
        out.append(row("cost per run (¢)", [pct(med(s[n], "cost_usd")) for n in NAMES]))
        out.append(row("model requests, 3 turns", [med(s[n], "requests") for n in NAMES]))
        out.append(row("wall time, 3 turns (s)", [med(s[n], "wall_s") for n in NAMES]))
        out.append(row("tool gap on real calls, p50 (ms)", [med(s[n], "tool_gap_ms") for n in NAMES]))
        out.append(row("peak RSS during task (MB)", [med(s[n], "peak_mb") for n in NAMES]))

    print("\n".join(out))
    print("\nsources: " + ", ".join(sources), file=sys.stderr)


if __name__ == "__main__":
    main()
