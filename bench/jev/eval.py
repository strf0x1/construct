#!/usr/bin/env python3
"""Score Jev against the local router on labelled routing and gate cases.

    python3 bench/jev/eval.py [--rounds N] [--only jev|local]

Builds a fresh CONSTRUCT_HOME with the skill library and MCP descriptions in
library.json, then feeds routes.jsonl / gates.jsonl through the hidden
`construct route` / `construct gate` commands. The TypeSafe key is read from
~/.config/construct-bench-jev.key and passed only to construct's environment.
"""
import argparse, json, os, statistics, subprocess, sys, tempfile, time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
BIN = ROOT / "target/release/construct"
KEY = Path.home() / ".config/construct-bench-jev.key"


def home():
    h = Path(tempfile.mkdtemp(prefix="construct-jev-"))
    lib = json.loads((HERE / "library.json").read_text())
    for name, desc in lib["skills"].items():
        d = h / "skills" / name
        d.mkdir(parents=True)
        (d / "SKILL.md").write_text(f"---\nname: {name}\ndescription: {desc}\n---\n\n# {name}\n\n1. ...\n")
    cfg = "".join(f'[mcp.{n}]\ncommand = "false"\ndescription = "{d}"\n\n' for n, d in lib["mcp"].items())
    (h / "config.toml").write_text(cfg)
    return h


def run(cmd, mode, h, lines):
    env = {k: v for k, v in os.environ.items() if k not in ("TYPESAFE_API_KEY",)}
    env.update(CONSTRUCT_HOME=str(h), CONSTRUCT_ROUTER=mode)
    if mode == "jev":
        env["TYPESAFE_API_KEY"] = KEY.read_text().strip()
    inp = "".join(json.dumps(l) + "\n" for l in lines)
    out = subprocess.run([str(BIN), cmd], input=inp, capture_output=True, text=True, env=env, timeout=600)
    if out.returncode:
        sys.exit(f"construct {cmd} failed: {out.stderr[-500:]}")
    return [json.loads(l) for l in out.stdout.splitlines() if l.strip()]


def ok(pred, want, alts):
    return pred in [want] + (alts or [])


def score_routes(cases, res):
    rows, g_ok, s_ok, fp, fn = [], 0, 0, 0, 0
    for c, r in zip(cases, res):
        pg = sorted(g for g in r["groups"] if g != "skills")
        ps = [s[0] for s in r["skills"]][:1]
        wg, ws = sorted(c["groups"]), c["skills"]
        gk = ok(pg, wg, [sorted(a) for a in c.get("groups_ok", [])])
        sk = ok(ps, ws, c.get("skills_ok"))
        if not gk:
            fp += len(set(pg) - set(wg))
            fn += len(set(wg) - set(pg))
        g_ok += gk
        s_ok += sk
        rows.append((gk, sk, c["request"], pg, wg, [f"{n} {p:.2f}" for n, p in r["skills"]], ws, r.get("note", ""), r["ms"]))
    return g_ok, s_ok, fp, fn, rows


def score_gates(cases, res):
    good, rows = 0, []
    for c, r in zip(cases, res):
        k = ok(r["kind"], c["kind"], c.get("kind_ok"))
        good += k
        rows.append((k, c["name"], r["kind"], c["kind"], r["note"], r["ms"]))
    return good, rows


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--rounds", type=int, default=3)
    ap.add_argument("--only")
    ap.add_argument("--set", default="", help="'holdout' to score the held-out cases")
    ap.add_argument("--out", default=str(HERE / "results" / time.strftime("eval-%Y%m%d-%H%M%S.json")))
    a = ap.parse_args()
    routes = [json.loads(l) for l in (HERE / f"routes{'_' + a.set if a.set else ''}.jsonl").read_text().splitlines() if l.strip()]
    gates = [json.loads(l) for l in (HERE / f"gates{'_' + a.set if a.set else ''}.jsonl").read_text().splitlines() if l.strip()]
    h = home()
    modes = [a.only] if a.only else ["local", "jev"]
    report = {}
    for mode in modes:
        rounds = a.rounds if mode == "jev" else 1
        rres, gres = [], []
        for i in range(rounds):
            rres.append(run("route", mode, h, [{"request": c["request"], "previous": c.get("previous", "")} for c in routes]))
            gres.append(run("gate", mode, h, gates))
        # Accuracy from round 1; latency over all rounds (round 1 includes the cold connection).
        g_ok, s_ok, fp, fn, rrows = score_routes(routes, rres[0])
        gate_ok, grows = score_gates(gates, gres[0])
        lat = [r["ms"] for rr in rres for r in rr]
        glat = [r["ms"] for gg in gres for r in gg]
        stable = None
        if rounds > 1:
            sig = lambda rr: [(tuple(r["groups"]), tuple(s[0] for s in r["skills"][:1])) for r in rr]
            stable = sum(all(sig(rr)[i] == sig(rres[0])[i] for rr in rres) for i in range(len(routes)))
        report[mode] = dict(
            groups_correct=g_ok, skills_correct=s_ok, n=len(routes), group_fp=fp, group_fn=fn,
            gate_correct=gate_ok, gates=len(gates), stable=stable,
            route_ms=dict(p50=statistics.median(lat), p95=sorted(lat)[int(len(lat) * 0.95) - 1], max=max(lat), first=rres[0][0]["ms"]),
            gate_ms=dict(p50=statistics.median(glat), max=max(glat)),
            routes=rrows, gate_rows=grows, raw_routes=rres, raw_gates=gres,
        )
        m = report[mode]
        print(f"\n== {mode}: groups {g_ok}/{len(routes)} (fp {fp}, fn {fn})  skills {s_ok}/{len(routes)}  gate {gate_ok}/{len(gates)}"
              f"  route ms p50 {m['route_ms']['p50']} p95 {m['route_ms']['p95']} first {m['route_ms']['first']}"
              + (f"  stable {stable}/{len(routes)}" if stable is not None else ""))
        for gk, sk, req, pg, wg, ps, ws, note, ms in rrows:
            if not (gk and sk):
                print(f"  {'G' if not gk else ' '}{'S' if not sk else ' '} {req[:60]!r:64} groups {pg} want {wg}  skills {ps} want {ws}")
                if note:
                    print(f"       {note}")
        for k, name, got, want, note, ms in grows:
            if not k:
                print(f"  gate {name!r:36} got {got} want {want}  {note}")
    Path(a.out).parent.mkdir(exist_ok=True)
    Path(a.out).write_text(json.dumps(report, indent=1))
    print(f"\nwrote {a.out}")


if __name__ == "__main__":
    main()
