"""Find where a session went wrong, and read only that part of it.

Three steps, each small enough to read whole: `triage` is one row per session
with counts of what looks wrong, `find` lists every occurrence of one kind as
a `session@seq` anchor, and `show` prints the few events around an anchor with
tool output cut to a byte budget. The signals are computed from `tool_start`
and `tool_end` rows alone, never from prose, so they mean the same thing for
every model.

    python3 bench/triage.py triage --since 7d
    python3 bench/triage.py find fail --label read
    python3 bench/triage.py show 20261101-225400-54b1@412

`--home` is a construct home or any directory of them (a bench run kept with
`--keep-homes`): every `projects/*/journal.sqlite` below it is read.
"""

import argparse
import json
import os
import sqlite3
from collections import Counter
from pathlib import Path

import journal

MUTATING = ("write", "edit")  # a successful one makes reading the same thing again fair
DENIED = "\n[sandbox:"  # the hint the shell tool adds to a non-zero exit the sandbox caused: that one is a failure
BIG = 8000  # bytes of one tool result above which it is a `big` finding
KINDS = {
    "fail": "a tool call that returned an error",
    "exit": "a shell command that ran and exited non-zero (a failing test is one): the command's answer, not a tool failure",
    "stuck": "a failure straight after the same tool's previous failure",
    "reread": "a read with the same arguments as an earlier one, nothing written in between",
    "repeat": "any other call repeated with the same arguments, nothing written in between",
    "big": f"a tool result over {BIG} bytes",
    "error": "an `error` event (the turn failed)",
    "note": "a `note` event (a provider condition construct recovered from)",
    "nudge": "a `nudge` event (an answer that was cut off, wrote its tool call as text, or was empty, and was asked to go on)",
}
# What `show` prints unless --all: the conversation and what the tools did.
SHOWN = ("user", "assistant", "nudge", "tool_start", "tool_end", "error", "note", "compacted", "turn_end", "model")


def load(home: Path, since: float = 0) -> list:
    """Every session below `home` active at or after `since` (unix seconds),
    oldest first: {id, project, model, rows: [(seq, type, at_ms, payload)]}."""
    out = []
    for f in sorted(home.rglob("projects/*/journal.sqlite")):
        db = sqlite3.connect(f"file:{f}?mode=ro", uri=True)
        try:
            rows = db.execute("SELECT session_id, seq, type, at_ms, payload FROM events ORDER BY seq").fetchall()
        finally:
            db.close()
        by = {}
        for sid, seq, ty, at, payload in rows:
            try:
                p = json.loads(payload)
            except ValueError:
                continue
            by.setdefault(sid, []).append((seq, ty, at, p))
        for sid, rs in by.items():
            if rs[-1][2] < since * 1000:
                continue
            model = ""
            for _, ty, _, p in rs:
                if ty in ("head", "request", "model"):
                    model = p.get("model") or model
            out.append({"id": sid, "project": f.parent.name, "model": model, "rows": rs})
    out.sort(key=lambda s: s["rows"][0][2])
    return out


def ids(home: Path, since: float = 0) -> list:
    """The ids of the sessions below `home` active at or after `since`: what a
    bench run records, so its result leads back to its events."""
    return [s["id"] for s in load(home, since)]


def keep(homes: dict, dest: Path) -> int:
    """Copy every journal of `homes` (name -> home) to `dest/<name>/projects/`,
    each as one file with its WAL folded in. Returns how many were copied."""
    n = 0
    for name, home in homes.items():
        for f in sorted(home.glob("projects/*/journal.sqlite")):
            out = dest / name / "projects" / f.parent.name / f.name
            out.parent.mkdir(parents=True, exist_ok=True)
            src, dst = sqlite3.connect(f"file:{f}?mode=ro", uri=True), sqlite3.connect(out)
            try:
                src.backup(dst)
            finally:
                src.close()
                dst.close()
            n += 1
    return n


def norm(args: str) -> str:
    """Tool arguments with key order and spacing removed, so equal calls compare equal."""
    try:
        return json.dumps(json.loads(args), sort_keys=True, separators=(",", ":"))
    except (ValueError, TypeError):
        return args or ""


def one_line(text, limit: int) -> str:
    text = " ".join(str(text or "").split())
    return text if len(text) <= limit else text[: limit - 1] + "…"


def findings(rows) -> list:
    """What looks wrong in one session: (kind, seq, label, detail), in order.
    `seq` is the `tool_end` row (or the event itself), which `show` centres on."""
    out, args, seen, last_failed = [], {}, set(), {}
    for seq, ty, _, p in rows:
        if ty == "tool_start":
            args[p.get("id")] = norm(p.get("args"))
        elif ty == "error":
            out.append(("error", seq, "error", one_line(p.get("message"), 160)))
        elif ty == "nudge":
            out.append(("nudge", seq, p.get("why", ""), ""))
        elif ty == "note":
            out.append(("note", seq, f"{p.get('class')}/{p.get('kind')}", one_line(p.get("detail"), 160)))
        elif ty == "tool_end" and p.get("name") != journal.ENABLE_TOOLS:
            name, ok, output = p.get("name", ""), p.get("ok", True), p.get("output") or ""
            call = args.get(p.get("id"), "")
            detail = f"{one_line(call, 110)} → {one_line(output, 110)}"
            failed = not ok and not (p.get("code") == "exit" and DENIED not in output)
            if failed:
                label = f"{name}:{'denied' if p.get('code') == 'exit' else p.get('code') or 'error'}"
                out.append(("fail", seq, label, detail))
                if last_failed.get(name):
                    out.append(("stuck", seq, label, detail))
            elif not ok:
                out.append(("exit", seq, name, detail))
            last_failed[name] = failed
            if (name, call) in seen:
                out.append(("reread" if name == "read" else "repeat", seq, name, detail))
            seen.add((name, call))
            if ok and name in MUTATING:
                seen.clear()
            if len(output.encode()) > BIG:
                out.append(("big", seq, name, f"{len(output.encode())} bytes · {one_line(call, 110)}"))
    return out


def served(rows) -> list:
    """(seq, model) for each model call whose response named the model that
    served it: under a router such as `openrouter/free` that differs per call."""
    return [(seq, p["served"]) for seq, ty, _, p in rows if ty == "usage" and p.get("served")]


def served_at(calls: list, seq: int) -> str:
    """The model whose answer led to the event at `seq`: the last call before it."""
    return next((m for at, m in reversed(calls) if at <= seq), "")


def summary(session) -> dict:
    """One session's denominators and its count of each kind of finding."""
    n = Counter(ty for _, ty, _, _ in session["rows"])
    usage = [p for _, ty, _, p in session["rows"] if ty == "usage"]
    ends = [p for _, ty, _, p in session["rows"] if ty == "tool_end" and p.get("name") != journal.ENABLE_TOOLS]
    first_write = next((i for i, p in enumerate(ends) if p.get("ok", True) and p.get("name") in MUTATING), None)
    return {
        "turns": n["turn_end"], "calls": n["request"], "tools": len(ends), "first_write": first_write,
        "prompt": sum(u.get("prompt", 0) for u in usage), "completion": sum(u.get("completion", 0) for u in usage),
        "out_bytes": sum(len((p.get("output") or "").encode()) for p in ends),
        "kinds": Counter(k for k, *_ in findings(session["rows"])),
    }


def triage(sessions, limit: int = 30) -> str:
    """The sessions with the most findings first, then every (kind, label) with
    its count: the arguments to pass `find` next."""
    cols = ("fail", "exit", "stuck", "reread", "repeat", "big", "error", "note", "nudge")
    rows, totals, by_model = [], Counter(), {}
    for s in sessions:
        m = summary(s)
        if not m["calls"] and not m["kinds"]:
            continue  # opened and closed: nothing to judge
        rows.append((sum(m["kinds"][k] for k in cols if k not in ("big", "note", "nudge")), s, m))
        totals.update((k, label) for k, _, label, _ in findings(s["rows"]))
        calls = served(s["rows"])
        for _, model in calls:
            by_model.setdefault(model, Counter())["calls"] += 1
        for k, seq, *_ in findings(s["rows"]):
            if model := served_at(calls, seq):
                by_model[model][k] += 1
    if not rows:
        return "no sessions in range\n"
    rows.sort(key=lambda r: -r[0])
    out = [f"{'session':<21} {'model':<28} turns calls tools  1stw " + " ".join(f"{c:>6}" for c in cols) + "  ptok_k ctok_k out_kb"]
    for _, s, m in rows[:limit]:
        first = "-" if m["first_write"] is None else m["first_write"]
        counts = " ".join(f"{m['kinds'][c] or '.':>6}" for c in cols)
        out.append(f"{s['id']:<21} {one_line(s['model'], 28):<28} {m['turns']:>5} {m['calls']:>5} {m['tools']:>5} {first:>5} {counts}  {m['prompt'] / 1000:>6.0f} {m['completion'] / 1000:>6.1f} {m['out_bytes'] / 1000:>6.0f}")
    if len(rows) > limit:
        out.append(f"… {len(rows) - limit} more sessions (--limit)")
    tools = sum(m["tools"] for _, _, m in rows)
    out.append(f"\n{len(rows)} sessions, {tools} tool calls. 1stw = tool calls before the first successful write or edit.")
    out += [f"  {kind:<7} {label:<34} {n:>5}" for (kind, label), n in sorted(totals.items(), key=lambda x: (x[0][0], -x[1]))]
    if len(by_model) > 1:
        out.append("\nby the model that served the call:")
        out += [f"  {one_line(m, 44):<44} {c['calls']:>5} calls  " + " ".join(f"{k} {c[k]}" for k in cols if c[k]) for m, c in sorted(by_model.items(), key=lambda x: -x[1]["calls"])]
    return "\n".join(out) + "\n"


def find(sessions, kind: str, label: str = "", limit: int = 40) -> str:
    """Every finding of `kind` whose label contains `label`, one anchor a line."""
    hits = [(s["id"], seq, lab, detail) for s in sessions for k, seq, lab, detail in findings(s["rows"]) if k == kind and label in lab]
    out = [f"{sid}@{seq}  {lab}  {detail}" for sid, seq, lab, detail in hits[:limit]]
    if len(hits) > limit:
        out.append(f"… {len(hits) - limit} more (--limit)")
    return "\n".join(out or [f"no `{kind}` findings"]) + "\n"


def clip(text: str, budget: int) -> str:
    """`text` cut to `budget` bytes by dropping its middle, which says how much went."""
    raw = (text or "").encode()
    if len(raw) <= budget:
        return text or ""
    half = budget // 2
    head, tail = raw[:half].decode(errors="ignore"), raw[len(raw) - half :].decode(errors="ignore")
    return f"{head}\n  …[{len(raw) - 2 * half} bytes cut]…\n{tail}"


def render(seq, ty, p, budget: int) -> str:
    """One event as a few lines: what was said, called or returned."""
    pad = lambda body: body.replace("\n", "\n" + " " * 8)
    if ty == "user":
        return f"{seq:>6}  USER {pad(clip(str(p.get('message', {}).get('content')), budget))}"
    if ty == "assistant":
        m = p.get("message", {})
        calls = ", ".join(c.get("function", {}).get("name", "?") for c in m.get("tool_calls") or [])
        think = f"\n(reasoning) {clip(m['reasoning'], budget // 2)}" if m.get("reasoning") else ""
        return f"{seq:>6}  ASSISTANT {pad(clip(m.get('content') or '', budget) + think)}" + (f"\n        [calls: {calls}]" if calls else "")
    if ty == "tool_start":
        return f"{seq:>6}  > {p.get('name')} {pad(clip(p.get('args') or '', budget))}"
    if ty == "tool_end":
        output = p.get("output") or ""
        state = "ok" if p.get("ok", True) else f"FAIL {p.get('code') or 'error'}"
        return f"{seq:>6}  < {p.get('name')} {state} {p.get('ms')}ms {len(output.encode())}B\n        {pad(clip(output, budget))}"
    rest = {k: v for k, v in p.items() if k != "event"}
    return f"{seq:>6}  {ty} {clip(json.dumps(rest), budget)}"


def show(sessions, anchor: str, around: int = 8, budget: int = 600, everything: bool = False) -> str:
    """The `around` events each side of `session@seq`, the anchor marked `*`."""
    sid, _, at = anchor.partition("@")
    found = [s for s in sessions if s["id"] == sid]
    if len(found) != 1:
        return f"{len(found)} sessions with id {sid}\n"
    rows = [r for r in found[0]["rows"] if everything or r[1] in SHOWN]
    centre = next((i for i, r in enumerate(rows) if r[0] >= int(at or 0)), len(rows) - 1)
    part = rows[max(0, centre - around) : centre + around + 1]
    out = [f"{sid} · {found[0]['project']} · {found[0]['model']} · events {part[0][0]}..{part[-1][0]} of {found[0]['rows'][-1][0]}"]
    calls = served(found[0]["rows"])
    for seq, ty, _, p in part:
        line = render(seq, ty, p, budget)
        if ty == "assistant" and (model := served_at(calls, seq)) and model != found[0]["model"]:
            line = line.replace("ASSISTANT", f"ASSISTANT [{model}]", 1)
        out.append("*" + line[1:] if seq == rows[centre][0] else line)
    return "\n".join(out) + "\n"


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--home", type=Path, default=Path(os.environ.get("CONSTRUCT_HOME") or Path.home() / ".construct"), help="a construct home, or a directory of them")
    ap.add_argument("--since", type=journal.parse_since, default=0, help="only sessions active this recently (7d, 12h); default all")
    ap.add_argument("--model", default="", help="only sessions whose model contains this")
    sub = ap.add_subparsers(dest="cmd", required=True)
    t = sub.add_parser("triage", help="one row per session, worst first, then counts by kind")
    t.add_argument("--limit", type=int, default=30)
    f = sub.add_parser("find", help="anchors for one kind of finding", epilog="\n".join(f"{k}: {v}" for k, v in KINDS.items()), formatter_class=argparse.RawDescriptionHelpFormatter)
    f.add_argument("kind", choices=list(KINDS))
    f.add_argument("--label", default="", help="only labels containing this (a tool name, `read:sandbox`)")
    f.add_argument("--limit", type=int, default=40)
    s = sub.add_parser("show", help="the events around one anchor")
    s.add_argument("anchor", help="session@seq, as `find` prints it")
    s.add_argument("--around", type=int, default=8, help="events each side (default 8)")
    s.add_argument("--bytes", type=int, default=600, help="budget for each message, argument or tool result (default 600)")
    s.add_argument("--all", action="store_true", help="every event type, not only the conversation and tools")
    a = ap.parse_args(argv)
    sessions = [x for x in load(a.home, a.since) if a.model in x["model"]]
    if a.cmd == "triage":
        print(triage(sessions, a.limit), end="")
    elif a.cmd == "find":
        print(find(sessions, a.kind, a.label, a.limit), end="")
    else:
        print(show(sessions, a.anchor, a.around, a.bytes, a.all), end="")


if __name__ == "__main__":
    main()
