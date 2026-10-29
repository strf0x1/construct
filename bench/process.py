"""Process metrics from construct's journal: how a run got where it did.

Each tool call leaves a `tool_end` event in the journal (name, ok, code,
data, ms), read here as the old session log's `tool` record (journal.py) and
joined to the call's arguments by id. Everything below is
computed from those records and the usage entries, never from the prose the
model read, so it is the same for every editor.
"""

import json
import re
from pathlib import Path

import journal

# The content-search tool under either wire name (`[tools] search_name`).
SEARCH_TOOLS = {"grep", "search"}
# A shell command that searches file contents / finds files. A search program
# only counts at the start of a command (after ;, &&, ||, $(, a backtick or a
# line break, or behind xargs), never as a pipe filter (`pytest | grep FAIL`).
_START = r"(?:^|&&|\|\||;|\n|\$\(|`|\bxargs\s+|\bexec\s+)\s*(?:sudo\s+)?"
CONTENT_SEARCH = re.compile(_START + r"(?:rg|grep|egrep|fgrep|ag|ack|ugrep|git\s+grep)(?=\s|$)")
FILE_SEARCH = re.compile(_START + r"(?:find|fd|fdfind|locate)(?=\s|$)")


def tool_events(home: Path, since: float) -> list:
    """Tool records from sessions written after `since`, in order, each with
    the call's parsed arguments, its step (the model response that made it;
    calls made together share one) and that response's completion tokens,
    split between the calls."""
    out, step = [], 0
    for session in journal.sessions(home, since):
        calls, last_usage = {}, None
        for d in session:
            if d.get("type") == "usage":
                last_usage = d.get("usage") or {}
            m = d.get("message") or {}
            tcs = m.get("tool_calls") or []
            step += bool(tcs)
            for c in tcs:
                try:
                    args = json.loads(c["function"].get("arguments") or "{}")
                except ValueError:
                    args = {}
                share = ((last_usage or {}).get("completion_tokens") or 0) / len(tcs)
                calls[c["id"]] = (args if isinstance(args, dict) else {}, share, step)
            if d.get("type") == "tool":
                rec = d.get("data") or {}
                args, tokens, at = calls.get(rec.get("id"), ({}, 0, step))
                out.append({**rec, "args": args, "completion_tokens": tokens, "step": at})
    return out


def path_of(e) -> str:
    return str(e["args"].get("path") or (e.get("data") or {}).get("path") or "")


def metrics(events: list) -> dict:
    """Edit outcomes for one run.

    recoveries: a rejected edit followed, within the next two steps, by an
                applied edit of the same path.
    rereads:    a rejected edit followed by a read of the same path before the
                next edit of it.
    rewrites:   an applied edit that rewrites a line an earlier applied edit of
                the same path wrote: the trace of a wrong-line edit that was
                noticed and fixed (one never noticed fails the task's check).
                Only editors that report line regions (hashline) can show one,
                and positions are not shifted for edits in between.
    """
    edits = [i for i, e in enumerate(events) if e["name"] == "edit"]
    rejected = [i for i in edits if not events[i]["ok"]]
    recoveries = rereads = 0
    for i in rejected:
        p = path_of(events[i])
        step = events[i]["step"]
        if any(e["name"] == "edit" and e["ok"] and path_of(e) == p and e["step"] <= step + 2 for e in events[i + 1 :]):
            recoveries += 1
        for e in events[i + 1 :]:
            if path_of(e) != p:
                continue
            if e["name"] == "read":
                rereads += 1
                break
            if e["name"] == "edit":
                break
    rewrites, written = 0, {}
    for i in edits:
        regions = (events[i].get("data") or {}).get("regions") or []
        if not events[i]["ok"] or not regions:
            continue
        p = path_of(events[i])
        before = written.get(p, [])
        if any(r["old_start"] <= e and s <= r["old_end"] for r in regions for s, e in before):
            rewrites += 1
        written[p] = before + [(r["new_start"], r["new_end"]) for r in regions]
    codes = {}
    for i in rejected:
        codes[events[i].get("code") or "error"] = codes.get(events[i].get("code") or "error", 0) + 1
    edit_tokens = sum(events[i]["completion_tokens"] for i in edits)
    shells = [e for e in events if e["name"] == "shell"]
    boxed = [(e.get("data") or {}).get("sandbox") for e in shells]
    boxed = [b for b in boxed if b]
    return {
        **search_metrics(events),
        "tool_calls": len(events),
        "reads": sum(1 for e in events if e["name"] == "read"),
        # What the sandbox kept from shell calls (#64), from the tool's data.
        "shell": {
            "calls": len(shells),
            "blocked": len(boxed),
            "stops": sum(1 for b in boxed if b.get("stop")),
            "refused": sum(1 for b in boxed if b.get("refused")),
            "broad": sum(1 for b in boxed if b.get("broad")),
        },
        "edit": {
            "calls": len(edits),
            "applied": len(edits) - len(rejected),
            "rejected": len(rejected),
            "unrecovered": len(rejected) - recoveries,
            "recoveries": recoveries,
            "rewrites": rewrites,
            "rereads_after_rejection": rereads,
            "echo_stripped": sum(len((events[i].get("data") or {}).get("echo_stripped") or []) for i in edits),
            "completion_tokens_per_call": edit_tokens / len(edits) if edits else None,
        },
        # Informational: a run that never saw a code has no entry, so these
        # are left out of compare (see compare.SKIP).
        "rejection_codes": codes,
    }


def search_metrics(events: list) -> dict:
    """How the run searched, and how often its tools failed.

    search.tool       calls to the content-search tool, whatever it was named
    search.shell      shell commands that run a content-search program
    search.find       shell commands that look for files by name
    search.adoption   tool / (tool + shell); absent when the run never searched
    tool_failures     over every tool: calls, failed (ok = false) and the rate.
                      "no matches" is a result, not a failure; a refused
                      path, a bad regex or a timeout is.
    """
    tool = sum(1 for e in events if e["name"] in SEARCH_TOOLS)
    commands = [str(e["args"].get("command") or "") for e in events if e["name"] == "shell"]
    shell = sum(1 for c in commands if CONTENT_SEARCH.search(c))
    find = sum(1 for c in commands if FILE_SEARCH.search(c) and not CONTENT_SEARCH.search(c))
    failed = [e for e in events if not e["ok"]]
    codes = {}
    for e in failed:
        key = f"{e['name']}:{e.get('code') or 'error'}"
        codes[key] = codes.get(key, 0) + 1
    search = {"tool": tool, "shell": shell, "find": find, "tool_failed": sum(1 for e in failed if e["name"] in SEARCH_TOOLS)}
    if tool + shell:
        search["adoption"] = tool / (tool + shell)
    return {
        "search": search,
        "tool_failures": {"calls": len(events), "failed": len(failed), "rate": len(failed) / len(events) if events else 0.0},
        "failure_codes": codes,
    }
