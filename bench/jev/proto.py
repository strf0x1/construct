#!/usr/bin/env python3
"""Try question designs for tool-group routing directly against Jev."""
import json, sys, time, concurrent.futures as cf, urllib.request
from pathlib import Path
KEY = (Path.home() / ".config/construct-bench-jev.key").read_text().strip()
lib = json.loads(Path(__file__).with_name("library.json").read_text())
cases = [json.loads(l) for l in Path(__file__).with_name("routes.jsonl").read_text().splitlines() if l.strip()]

def jev(state, questions):
    req = urllib.request.Request("https://api.typesafe.ai/v1/systemone", data=json.dumps({"model": "jev-latest", "state": state, "questions": questions}).encode(),
                                 headers={"Authorization": f"Bearer {KEY}", "Content-Type": "application/json"})
    t = time.time()
    v = json.load(urllib.request.urlopen(req, timeout=20))
    return v["answers"], (time.time() - t) * 1000

GROUPS = {
    "web": "the internet: web search, fetching URLs or online docs, checking current versions, CVEs or news",
    "memory": "saving a lasting preference or fact about the user or their machine (remember / forget / from now on)",
    "history": "transcripts of earlier conversations with the assistant (what we did or decided in a past session)",
    "skill-write": "editing the assistant's own saved skills library (create, update or delete a skill)",
}
for n, d in lib["mcp"].items():
    GROUPS[f"mcp:{n}"] = f"the {n} tools: {d}"

def variant_choice(c, thr):
    crit = {"none": "nothing beyond the local repository, files and shell commands"}
    crit.update(GROUPS)
    q = {"need": {"type": "choice", "instructions": "Besides the local files and a shell, what does the assistant need to access to handle `request`?", "criteria": crit}}
    st = {"request": c["request"]}
    if c.get("previous"): st["previous_reply"] = c["previous"]
    a, ms = jev(st, q)
    p = a["need"]["probabilities"]
    pred = sorted(k for k, v in p.items() if k != "none" and v >= thr)
    return pred, ms, {k: round(v, 2) for k, v in sorted(p.items(), key=lambda x: -x[1])[:3]}

def run(fn, thr):
    good = 0; lat = []; miss = []
    with cf.ThreadPoolExecutor(8) as ex:
        res = list(ex.map(lambda c: fn(c, thr), cases))
    for c, (pred, ms, info) in zip(cases, res):
        alts = [sorted(c["groups"])] + [sorted(a) for a in c.get("groups_ok", [])]
        ok = pred in alts
        good += ok; lat.append(ms)
        if not ok: miss.append((c["request"][:55], pred, c["groups"], info))
    print(f"{fn.__name__} thr={thr}: {good}/{len(cases)}  p50 {sorted(lat)[len(lat)//2]:.0f}ms")
    for m in miss: print("  ", m)


ACCESS = {"none": "nothing beyond the local repository, files and shell commands", "web": GROUPS["web"], "history": GROUPS["history"]}
ACCESS.update({k: v for k, v in GROUPS.items() if k.startswith("mcp:")})
INTENT = {
    "task": "asks the assistant to do or answer something (code, files, ops, questions)",
    "preference": "tells the assistant how to behave from now on, or shares a lasting fact about the user or their machine",
    "skill-edit": "asks to create, change or delete one of the assistant's saved skills",
}
def variant_split(c, thr):
    q = {"need": {"type": "choice", "instructions": "Besides the local files and a shell, what does the assistant need to access to handle `request`?", "criteria": ACCESS},
         "intent": {"type": "choice", "instructions": "What is `request` mainly doing?", "criteria": INTENT}}
    st = {"request": c["request"]}
    if c.get("previous"): st["previous_reply"] = c["previous"]
    a, ms = jev(st, q)
    p = a["need"]["probabilities"]; i = a["intent"]["probabilities"]
    pred = [k for k, v in p.items() if k != "none" and v >= thr]
    if i["preference"] >= thr: pred.append("memory")
    if i["skill-edit"] >= thr: pred.append("skill-write")
    return sorted(pred), ms, ({k: round(v, 2) for k, v in sorted(p.items(), key=lambda x: -x[1])[:2]}, {k: round(v, 2) for k, v in i.items()})

if __name__ == "__main__":
  fn = globals()[sys.argv[2]] if len(sys.argv) > 2 else variant_choice
  run(fn, float(sys.argv[1]) if len(sys.argv) > 1 else 0.35)
