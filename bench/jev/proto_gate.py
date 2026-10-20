import json, concurrent.futures as cf
from pathlib import Path
from proto import jev
cases = [json.loads(l) for l in Path(__file__).with_name("gates.jsonl").read_text().splitlines() if l.strip()]
def run(c):
    q = {"preference": {"type": "choice", "instructions": "Across `user_messages`, what is the most lasting thing the user said?", "criteria": {
            "task": "only requests for this task, or corrections that apply to this task alone",
            "preference": "a rule for how the assistant should work in future sessions, or a durable fact about the user or their machine"}}}
    if c["errors"]:
        q["workaround"] = {"type": "noul", "instructions": "Do `tool_errors` show the assistant's own commands or approach failing in a way that needed a different technique (wrong flags, missing tools, environment quirks), as opposed to failures in the user's code that the task was about?"}
    if c["loaded"]:
        q["skill_gap"] = {"type": "noul", "instructions": "Do `user_messages` or `tool_errors` show that a procedure listed in `skills_loaded` was wrong, incomplete or outdated?"}
    a, ms = jev({"user_messages": c["users"], "tool_errors": c["errors"], "skills_loaded": c["loaded"]}, q)
    pref = a["preference"]["probabilities"]["preference"] >= 0.5
    sk = a.get("workaround", {}).get("noul", 0) >= 0.5 or a.get("skill_gap", {}).get("noul", 0) >= 0.5
    kind = {(1,1):"Both",(1,0):"Memory",(0,1):"Skills",(0,0):None}[(int(pref), int(sk))]
    return kind, ms, a
with cf.ThreadPoolExecutor(8) as ex: res = list(ex.map(run, cases))
good = 0
for c, (k, ms, a) in zip(cases, res):
    ok = k in [c["kind"]] + c.get("kind_ok", [])
    good += ok
    if not ok: print("  ", c["name"], k, "want", c["kind"], {q: round(v.get("noul", v.get("probabilities", {}).get("preference", -1)), 2) for q, v in a.items()})
print(f"gate {good}/{len(cases)}")
