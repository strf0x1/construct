"""Foreman-style strict nouls (one per group) vs our two-choice design."""
import json, sys, concurrent.futures as cf
from pathlib import Path
from proto import jev, GROUPS
STRICT = {k: f"Does `request` explicitly require {d}? Answer no if it can be done with only the local files and a shell, or if the evidence is insufficient." for k, d in GROUPS.items()}
STRICT["memory"] = "Does `request` explicitly tell the assistant how to behave in future sessions, or share a lasting fact about the user or their machine? Answer no for ordinary task requests."
STRICT["skill-write"] = "Does `request` explicitly ask to create, change or delete one of the assistant's saved skills? Answer no otherwise."
def run(c):
    st = {"request": c["request"]}
    if c.get("previous"): st["previous_reply"] = c["previous"]
    a, ms = jev(st, {k: {"type": "noul", "instructions": q} for k, q in STRICT.items()})
    return sorted(k for k in STRICT if a[k]["noul"] >= 0.5), {k: round(a[k]["noul"], 2) for k in STRICT if a[k]["noul"] >= 0.3}
for f in ["routes.jsonl", "routes_holdout.jsonl"]:
    cases = [json.loads(l) for l in Path(f).read_text().splitlines() if l.strip()]
    with cf.ThreadPoolExecutor(8) as ex: res = list(ex.map(run, cases))
    good = fp = fn = 0
    for c, (pred, info) in zip(cases, res):
        alts = [sorted(c["groups"])] + [sorted(a) for a in c.get("groups_ok", [])]
        if pred in alts: good += 1
        else:
            fp += len(set(pred) - set(c["groups"])); fn += len(set(c["groups"]) - set(pred))
            print("  ", c["request"][:50], pred, "want", c["groups"], info)
    print(f"{f}: strict nouls {good}/{len(cases)} fp {fp} fn {fn}")
