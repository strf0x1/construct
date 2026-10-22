"""Compaction efficacy, from what a run left behind: construct's journal
(`compacted`, `usage`, `tool_start`, `error` events) and the endpoint's
request log (`window_rejected` when a prompt passed the task's `window`).

`metrics()` is one run's scorecard, stored as `compaction` on the run so
`bench.py compare` judges it like any other metric. Run as a script, it
prints one line per run of a result file, so a whole suite reads at a glance:

    python3 bench/compaction.py bench/results/task-....json [--side NAME]
"""

import json
import sqlite3
import sys
from pathlib import Path

OVERFLOW = ("context_length_exceeded", "maximum context length", "too long", "nothing is left to cut")


def events(home: Path, since: float) -> list:
    """(type, payload) of every journal row written at or after `since`
    (unix seconds), in seq order per project."""
    out = []
    for f in sorted((home / "projects").glob("*/journal.sqlite")):
        db = sqlite3.connect(f"file:{f}?mode=ro", uri=True)
        try:
            rows = db.execute("SELECT type, payload FROM events WHERE at_ms >= ? ORDER BY seq", (int(since * 1000),)).fetchall()
        finally:
            db.close()
        for ty, p in rows:
            try:
                out.append((ty, json.loads(p)))
            except ValueError:
                pass
    return out


def metrics(home: Path, since: float, recs: list, window: int) -> dict:
    ev = events(home, since)
    cuts = [p for t, p in ev if t == "compacted"]
    usage = [p for t, p in ev if t == "usage"]
    agent = [r for r in recs if not r.get("side") and r.get("n_tools")]
    ok = [r for r in agent if not r.get("window_rejected") and r.get("status", 200) == 200]
    prompts = [((r.get("usage") or {}).get("prompt_tokens") or 0) for r in ok]

    # The first usage after each cut: did the provider's cache survive it?
    after_cut, pending = [], False
    for t, p in ev:
        if t == "compacted":
            pending = True
        elif t == "usage" and pending:
            pending = False
            after_cut.append((p.get("cached") or 0) / max(1, p.get("prompt") or 0))

    # Calls the model made again after their result was elided: what a stub cost.
    calls, elided, reruns = {}, set(), 0
    for t, p in ev:
        if t == "assistant":
            for c in (p.get("message") or {}).get("tool_calls") or []:
                calls[c["id"]] = (c["function"]["name"], c["function"]["arguments"])
        elif t == "compacted":
            elided |= {calls[i] for i in ((p.get("cut") or {}).get("elide") or []) if i in calls}
        elif t == "tool_start" and (p.get("name"), p.get("args")) in elided:
            reruns += 1
            elided.discard((p.get("name"), p.get("args")))

    prompt_total = sum(u.get("prompt") or 0 for u in usage)
    summary_tokens = sum((c.get("summary_prompt") or 0) + (c.get("summary_completion") or 0) for c in cuts)
    errors = [p.get("message") or "" for t, p in ev if t == "error"]
    return {
        "cuts": len(cuts),
        "cuts_elide": sum(c.get("mode") == "elide" for c in cuts),
        "cuts_summarize": sum(c.get("mode") == "summarize" for c in cuts),
        # Journals from before the planner: `compacted` with no mode.
        "cuts_legacy": sum(not c.get("mode") for c in cuts),
        "cuts_overflow": sum(c.get("reason") == "overflow" for c in cuts),
        "cuts_unreached": sum(bool(c.get("mode")) and not c.get("reached") for c in cuts),
        "summary_fallbacks": sum(bool(c.get("fallback")) for c in cuts),
        "overflows": sum(bool(r.get("window_rejected")) for r in recs),
        "overflow_errors": sum(any(k in e.lower() for k in OVERFLOW) for e in errors),
        "peak_fill": max(prompts) / window if prompts else 0.0,
        "compact_ms": sum(c.get("ms") or 0 for c in cuts),
        "summary_tokens": summary_tokens,
        "summary_share": summary_tokens / prompt_total if prompt_total else 0.0,
        "cache_hit": sum(u.get("cached") or 0 for u in usage) / prompt_total if prompt_total else 0.0,
        "cache_hit_after_cut": sum(after_cut) / len(after_cut) if after_cut else 0.0,
        "stub_reruns": reruns,
    }


def line(m: dict) -> str:
    """The scorecard in one line, for the bench's progress output."""
    return (
        f"cuts={m['cuts']}(e{m['cuts_elide']}/s{m['cuts_summarize']}/l{m['cuts_legacy']}/o{m['cuts_overflow']}) "
        f"overflows={m['overflows']} fatal={m['overflow_errors']} peak={m['peak_fill']:.2f} "
        f"reruns={m['stub_reruns']} sum_tok={m['summary_tokens']} cache={m['cache_hit']:.2f}/{m['cache_hit_after_cut']:.2f}"
    )


def main(argv):
    if not argv:
        sys.exit(__doc__)
    data = json.loads(Path(argv[0]).read_text())
    only = argv[argv.index("--side") + 1] if "--side" in argv else None
    for side, runs in (data.get("raw") or {}).items():
        if only and side != only:
            continue
        for r in runs:
            ok = "PASS" if r.get("success") else "fail"
            failed = [k for k, v in (r.get("checks") or {}).items() if not v]
            c = r.get("compaction")
            print(
                f"{side:12} {r['task']:22} {ok} req={r['agent_requests']:3} tok={r['prompt_tokens'] // 1000:5}k "
                + (line(c) if c else "(no compaction metrics)")
                + (f" failed={failed}" if failed else "")
                + (" OVER_BUDGET" if r.get("over_budget") else "")
            )


if __name__ == "__main__":
    main(sys.argv[1:])
