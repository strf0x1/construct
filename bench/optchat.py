"""`optchat`'s own work in a run, from what it left behind: the journal's
`memory` events (ADR 0043), and the endpoint's request log for how full the
view got.

`metrics()` is one run's scorecard, stored as `optchat` on the run beside
`compaction`, so `bench.py compare` judges it like any other metric.

There is no `leaves_verbatim`: the journal records only the leaves the summary
model wrote (ADR 0043 decision 8). A short entry's leaf is the entry's own
text, derived from the log and never journaled, so an event cannot tell the
two apart.
"""

from compaction import events


def metrics(home, since, recs, window) -> dict:
    ev = events(home, since)
    fits = [p for t, p in ev if t == "memory"]
    nodes = [n for f in fits for group in (f.get("leaves"), f.get("merges")) for n in (group or [])]
    # The agent's own requests: a tool-less call is a summary, not a step.
    agent = [r for r in recs if not r.get("side") and r.get("n_tools")]
    ok = [r for r in agent if not r.get("window_rejected") and r.get("status", 200) == 200]
    prompts = [((r.get("usage") or {}).get("prompt_tokens") or 0) for r in ok]
    fit_ms = [f.get("ms") or 0 for f in fits]
    return {
        "leaves": sum((n.get("l") or 0) == 0 for n in nodes),
        "parents": sum((n.get("l") or 0) > 0 for n in nodes),
        "fallbacks": sum(bool(n.get("fallback")) for n in nodes),
        "summary_tokens": sum((f.get("summary_prompt") or 0) + (f.get("summary_completion") or 0) for f in fits),
        "fit_ms": sum(fit_ms),
        "fit_ms_max": max(fit_ms, default=0),
        # `zoom` is the one tool the strategy offers (ADR 0043 decision 9).
        "zooms": sum(t == "tool_start" and p.get("name") == "zoom" for t, p in ev),
        "peak_fill": max(prompts) / window if prompts and window else 0.0,
    }


def line(m: dict) -> str:
    """The scorecard in one line, for the bench's progress output."""
    return (
        f"leaves={m['leaves']} parents={m['parents']} fallbacks={m['fallbacks']} "
        f"sum_tok={m['summary_tokens']} fit={m['fit_ms']}ms(max {m['fit_ms_max']}) "
        f"zooms={m['zooms']} peak={m['peak_fill']:.2f}"
    )
