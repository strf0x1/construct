"""construct's sessions, read from its journals with the stdlib sqlite3.

Every project has `$CONSTRUCT_HOME/projects/<id>/journal.sqlite`; a row's
payload is one `Event` (docs/adr/0008). `sessions()` turns each session back
into the rows the old `sessions/<id>.jsonl` files held (session, message,
usage, route, tool, compact), the same shape `construct sessions export`
writes, so the bench's readers work on either. Homes from before the journal
still have those files; they are read as they are.

Run as a script it reports what went wrong, from the journal alone:
`python3 bench/journal.py errors --since 7d [--provider X] [--model Y]`.
It prints a summary to review and paste into an issue yourself; nothing here
posts anywhere (issue #48).
"""

import argparse
import json
import os
import re
import sqlite3
import time
from collections import Counter, defaultdict
from pathlib import Path

ENABLE_TOOLS = "enable_tools"  # never had a tool record: it switches state, not the world


def legacy(rows):
    """Journal rows (type, at_ms, payload) of one session as old JSONL rows."""
    out, model = [], ""
    for ty, at, p in rows:
        if ty == "head":
            model = p.get("model", "")
            out.append({"type": "session", **{k: p.get(k) for k in ("id", "harness", "version", "cwd", "model", "manifest")}, "at": at})
        elif ty == "request":
            model = p.get("model", model)
        elif ty == "usage":
            usage = {
                "prompt_tokens": p.get("prompt", 0), "completion_tokens": p.get("completion", 0), "cached_tokens": p.get("cached", 0),
                "cache_write_tokens": p.get("cache_write", 0), "cost": p.get("cost"),
            }
            out.append({"type": "usage", "at": at, "model": model, "usage": usage})
        elif ty == "route":
            skills = []
            for s in p.get("skills") or []:
                name, _, score = s.rpartition(" ")
                try:
                    skills.append([name, float(score)])
                except ValueError:
                    skills.append([s, 0.0])
            data = {k: p.get(k) for k in ("groups", "source", "ms", "note")}
            out.append({"type": "route", "at": at, "data": {**data, "skills": skills}})
        elif ty == "tool_end" and p.get("name") != ENABLE_TOOLS:
            data = {k: p.get(k) for k in ("id", "name", "ok", "ms", "code", "data")}
            out.append({"type": "tool", "at": at, "data": data})
        elif ty == "compacted":
            out.append({"type": "compact", "at": at, "data": {"before": p.get("before"), "after": p.get("after")}})
        elif isinstance(p.get("message"), dict):  # an `error` event's message is a string
            out.append({"type": "message", "at": at, "message": p["message"]})
    return out


def raw_sessions(home: Path, since: float = 0):
    """Each journal session active at or after `since` (unix seconds) as its
    (type, at_ms, payload) rows in order, across every project."""
    for f in sorted((home / "projects").glob("*/journal.sqlite")):
        db = sqlite3.connect(f"file:{f}?mode=ro", uri=True)
        try:
            rows = db.execute("SELECT session_id, type, at_ms, payload FROM events ORDER BY seq").fetchall()
        finally:
            db.close()
        by = {}
        for sid, ty, at, payload in rows:
            try:
                p = json.loads(payload)
            except ValueError:
                continue
            by.setdefault(sid, []).append((ty, at, p))
        for rs in by.values():
            if rs[-1][1] >= since * 1000:
                yield rs


def sessions(home: Path, since: float = 0) -> list:
    """Each session active at or after `since` (unix seconds) as a list of old
    JSONL rows, oldest session first."""
    found = [(rs[0][1], legacy(rs)) for rs in raw_sessions(home, since)]
    # Sessions in different projects interleave by when they started.
    found.sort(key=lambda x: x[0])
    out = [rs for _, rs in found]
    for f in sorted((home / "sessions").glob("*.jsonl")):
        if f.stat().st_mtime >= since:
            out.append([json.loads(l) for l in f.read_text().splitlines() if l.strip()])
    return out


def errors(home: Path, since: float = 0, provider: str = "", model: str = "") -> dict:
    """What went wrong, grouped by (provider, model): turns and model calls as
    the denominators, and a counter each for `note` rows (summed by their
    `count`), `error` events and failed `tool_end`s. A note's rate is
    occurrences per 100 turns, so providers of different sizes compare.

    The provider is the named one (`[providers.x]`, ADR 0027) when the
    manifest or a `model` row names it, else the top-level endpoint's dialect
    (`openrouter`, `openai`), as journals from before named providers say."""
    groups = defaultdict(lambda: {"sessions": 0, "turns": 0, "calls": 0, "notes": Counter(), "errors": 0, "tools": Counter()})
    for rs in raw_sessions(home, since):
        head = rs[0][2]
        manifest = head.get("manifest") or {}
        # The top-level endpoint's label: its dialect. Only a manifest for it says.
        default = "unknown" if manifest.get("provider") else (manifest.get("dialect") or "unknown")
        label = manifest.get("provider") or default
        current = head.get("model", "")
        seen = set()
        for ty, at, p in rs:
            if at < since * 1000:
                continue
            if ty == "manifest" and not p.get("provider"):
                default = p.get("dialect") or default
            if ty in ("manifest", "model"):
                label = p.get("provider") or default
            if ty in ("request", "model"):
                current = p.get("model", current)
            if ty in ("model", "manifest"):
                continue  # they move the labels; the rows after them are counted
            g = groups[(label, current)]
            if (label, current) not in seen:
                seen.add((label, current))
                g["sessions"] += 1
            if ty == "turn_end":
                g["turns"] += 1
            elif ty == "request":
                g["calls"] += 1
            elif ty == "note":
                g["notes"][f"{p.get('class')}/{p.get('kind')}" + (f" ({p.get('detail')})" if p.get("kind") == "suppressed" else "")] += p.get("count", 1)
            elif ty == "error":
                g["errors"] += 1
            elif ty == "tool_end" and not p.get("ok", True):
                g["tools"][f"{p.get('name')}:{p.get('code') or 'error'}"] += 1
    return {k: v for k, v in sorted(groups.items()) if provider in k[0] and model in k[1]}


def report(found: dict) -> str:
    """`errors()` as text to read, with rates per 100 turns."""
    if not found:
        return "no sessions in range\n"
    out = []
    for (dialect, model), g in found.items():
        per = lambda n: f"{100 * n / g['turns']:.1f}/100 turns" if g["turns"] else "no turns"
        out.append(f"{dialect} · {model}: {g['sessions']} sessions, {g['turns']} turns, {g['calls']} model calls")
        rows = [(f"note {k}", n) for k, n in sorted(g["notes"].items())]
        rows += [("error", g["errors"])] if g["errors"] else []
        rows += [(f"tool {k}", n) for k, n in sorted(g["tools"].items())]
        out += [f"  {name:<44} {n:>5}  {per(n)}" for name, n in rows] or ["  nothing went wrong"]
    return "\n".join(out) + "\n"


def parse_since(text: str) -> float:
    """`7d`, `12h`, `30m`, `90s` or a bare number of days, as unix seconds ago."""
    m = re.fullmatch(r"(\d+(?:\.\d+)?)([dhms]?)", text)
    if not m:
        raise argparse.ArgumentTypeError(f"not a duration: {text} (try 7d, 12h)")
    return time.time() - float(m[1]) * {"d": 86400, "h": 3600, "m": 60, "s": 1, "": 86400}[m[2]]


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[1])
    sub = ap.add_subparsers(dest="cmd", required=True)
    e = sub.add_parser("errors", help="notes, errors and failed tools by provider and model")
    e.add_argument("--since", type=parse_since, default=parse_since("7d"), help="how far back (default 7d)")
    e.add_argument("--provider", default="", help="only this provider name or dialect (substring)")
    e.add_argument("--model", default="", help="only this model (substring)")
    e.add_argument("--home", type=Path, default=Path(os.environ.get("CONSTRUCT_HOME") or Path.home() / ".construct"))
    a = ap.parse_args(argv)
    print(report(errors(a.home, a.since, a.provider, a.model)), end="")


if __name__ == "__main__":
    main()
