"""Compaction at several windows, without a model (#95 item 1, tier 0).

Replays a fixed tool sequence on the `compact-long-fix` fixture through
`construct run --json` against the scripted endpoint (`construct-dev serve
--window W`): the tool results are real, the model's replies are scripted, so
the only thing that changes between windows is what the planner does. Reads
the cuts back from the journal (`compacted` rows) and the endpoint's request
log, and prints one row per (window, arm).

    python3 bench/window_sweep.py [--windows 4096 8192 16384 32768 131072]
        [--script issue|long] [--arm NAME:'toml lines' ...] [--json out.json]

An arm is a `config.toml` overlay, e.g.
    --arm k4:'[compaction]\\nkeep_results = 4'
The `base` arm (no overlay) always runs.
"""

import argparse
import json
import os
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
FIXTURE = ROOT / "bench/tasks/compact-long-fix/fixture"
PROMPT = (ROOT / "bench/tasks/compact-long-fix/task.toml").read_text().split('"')[1]
SHOP = sorted(p.name for p in (FIXTURE / "shop").glob("*.py") if p.name != "__init__.py")


def script(kind: str) -> dict:
    """`issue`: the recording in #95 (a test run, then a read of each module).
    `long`: that, a grep, and every module read again, so 32k cuts too.
    `paged`: `long` with every read in 60-line pages, the way a model pages a
    file read cuts short, so each arm sees the same content whatever its cap."""
    if kind == "paged":
        reads = [{"call": "read", "args": {"path": f"shop/{f}", "offset": o, "limit": 60}}
                 for f in SHOP for o in range(1, len((FIXTURE / "shop" / f).read_text().splitlines()) + 1, 60)]
        steps = [{"call": "shell", "args": {"command": "python3 -m unittest 2>&1 | tail -40"}}, *reads,
                 {"call": "grep", "args": {"pattern": "def ", "path": "shop"}}, *reads, {"text": "done"}]
        return {"steps": steps, "side": [f"Summary {i}: ran the tests; read shop modules." for i in range(1, 80)]}
    steps = [{"call": "shell", "args": {"command": "python3 -m unittest 2>&1 | tail -40"}}]
    steps += [{"call": "read", "args": {"path": f"shop/{f}"}} for f in SHOP]
    if kind == "long":
        steps.append({"call": "grep", "args": {"pattern": "def ", "path": "shop"}})
        steps += [{"call": "read", "args": {"path": f"shop/{f}"}} for f in SHOP]
    steps.append({"text": "done"})
    side = [f"Summary {i}: ran the tests; read shop modules; no fixes yet." for i in range(1, 40)]
    return {"steps": steps, "side": side}


def journal(home: Path) -> list:
    out = []
    for f in sorted((home / "projects").glob("*/journal.sqlite")):
        db = sqlite3.connect(f"file:{f}?mode=ro", uri=True)
        try:
            out += [(t, json.loads(p)) for t, p in db.execute("SELECT type, payload FROM events ORDER BY seq")]
        finally:
            db.close()
    return out


def run_one(window: int, overlay: str, steps: dict, tmp: Path) -> dict:
    work, home, ep = tmp / "w", tmp / "h", tmp / "ep"
    shutil.copytree(FIXTURE, work)
    home.mkdir()
    ep.mkdir()
    (home / "config.toml").write_text(f"context_window = {window}\n[learn]\nenabled = false\n" + overlay + "\n")
    (tmp / "s.json").write_text(json.dumps(steps))
    port_file = ep / "port"
    serve = subprocess.Popen(
        [str(ROOT / "target/debug/construct-dev"), "serve", "--script", str(tmp / "s.json"),
         "--log", str(ep / "log.jsonl"), "--port-file", str(port_file), "--window", str(window)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    try:
        for _ in range(200):
            if port_file.exists() and port_file.read_text().strip():
                break
            time.sleep(0.02)
        env = {k: v for k, v in os.environ.items() if not k.endswith("_API_KEY")}
        env |= {"CONSTRUCT_HOME": str(home), "CONSTRUCT_ROUTER": "local", "OPENAI_API_KEY": "x",
                "CONSTRUCT_BASE_URL": f"http://127.0.0.1:{port_file.read_text().strip()}/v1"}
        t0 = time.time()
        p = subprocess.run([str(ROOT / "target/debug/construct"), "run", "--json", PROMPT],
                           cwd=work, env=env, capture_output=True, text=True, timeout=300)
        wall = time.time() - t0
    finally:
        serve.kill()
        serve.wait()
    recs = [json.loads(l) for l in (ep / "log.jsonl").read_text().splitlines() if l.strip()]
    agent = [r for r in recs if not r.get("side")]
    ev = journal(home)
    cuts = [c for t, c in ev if t == "compacted"]
    errors = [c.get("message", "") for t, c in ev if t == "error"]
    prompts = [(r.get("usage") or {}).get("prompt_tokens") or 0 for r in agent if not r.get("window_rejected")]
    budget = cuts[0]["budget"] if cuts else None
    # Requests between consecutive cuts: the thrash measure (small = thrash).
    return {
        "exit": p.returncode,
        "finished": any(t == "turn_end" for t, _ in ev) and not errors,
        "requests": len(agent),
        "side": len(recs) - len(agent),
        "rejected": sum(bool(r.get("window_rejected")) for r in recs),
        "cuts": len(cuts),
        "elide": sum(c.get("mode") == "elide" for c in cuts),
        "summarize": sum(c.get("mode") == "summarize" for c in cuts),
        "overflow_cuts": sum(c.get("reason") == "overflow" for c in cuts),
        "unreached": sum(not c.get("reached") for c in cuts),
        "cuts_per_request": len(cuts) / max(1, len(agent)),
        "budget": budget,
        "peak_fill": max(prompts) / window if prompts else 0,
        "prompt_tokens": sum(prompts),
        "first_prompt": prompts[0] if prompts else 0,
        # What the tools handed the model: less here means a cap hid content.
        "result_bytes": sum(len(c.get("output") or "") for t, c in ev if t == "tool_end"),
        "cut_tokens": [(c.get("tokens_before"), c.get("tokens_after")) for c in cuts],
        "errors": errors[:3],
        "wall_s": round(wall, 2),
    }


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--windows", type=int, nargs="+", default=[4096, 8192, 16384, 32768, 131072])
    ap.add_argument("--script", choices=["issue", "long", "paged"], default="long")
    ap.add_argument("--arm", action="append", default=[], metavar="NAME:TOML")
    ap.add_argument("--json", type=Path)
    a = ap.parse_args()
    arms = [("base", "")] + [(s.split(":", 1)[0], s.split(":", 1)[1].replace("\\n", "\n")) for s in a.arm]
    steps = script(a.script)
    rows = []
    print(f"{'arm':8} {'window':>7} {'budget':>7} {'1st':>5} {'req':>4} {'cuts':>4} {'e/s/o':>7} {'miss':>4} "
          f"{'cut/req':>7} {'peak':>5} {'prompt':>7} {'res kB':>6} {'rej':>3} ok")
    for name, overlay in arms:
        for w in a.windows:
            with tempfile.TemporaryDirectory() as d:
                r = run_one(w, overlay, steps, Path(d))
            r |= {"arm": name, "window": w}
            rows.append(r)
            print(f"{name:8} {w:>7} {r['budget'] or '-':>7} {r['first_prompt']:>5} {r['requests']:>4} {r['cuts']:>4} "
                  f"{r['elide']}/{r['summarize']}/{r['overflow_cuts']:<3} {r['unreached']:>4} {r['cuts_per_request']:>7.2f} "
                  f"{r['peak_fill']:>5.2f} {r['prompt_tokens']:>7} {r['result_bytes'] // 1000:>6} {r['rejected']:>3} "
                  f"{'yes' if r['finished'] else 'NO ' + '; '.join(e[:80] for e in r['errors'])}")
    if a.json:
        a.json.write_text(json.dumps({"script": a.script, "commit": subprocess.run(
            ["git", "rev-parse", "--short", "HEAD"], cwd=ROOT, capture_output=True, text=True).stdout.strip(),
            "rows": rows}, indent=1))


if __name__ == "__main__":
    sys.exit(main())
