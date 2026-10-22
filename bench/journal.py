"""construct's sessions, read from its journals with the stdlib sqlite3.

Every project has `$CONSTRUCT_HOME/projects/<id>/journal.sqlite`; a row's
payload is one `Event` (docs/adr/0008). `sessions()` turns each session back
into the rows the old `sessions/<id>.jsonl` files held (session, message,
usage, route, tool, compact), the same shape `construct sessions export`
writes, so the bench's readers work on either. Homes from before the journal
still have those files; they are read as they are.
"""

import json
import sqlite3
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


def sessions(home: Path, since: float = 0) -> list:
    """Each session active at or after `since` (unix seconds) as a list of old
    JSONL rows, oldest session first."""
    found = []
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
                found.append((rs[0][1], legacy(rs)))
    # Sessions in different projects interleave by when they started.
    found.sort(key=lambda x: x[0])
    out = [rs for _, rs in found]
    for f in sorted((home / "sessions").glob("*.jsonl")):
        if f.stat().st_mtime >= since:
            out.append([json.loads(l) for l in f.read_text().splitlines() if l.strip()])
    return out
