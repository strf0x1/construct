#!/usr/bin/env -S python3 -u
"""Benchmark construct against Hermes Agent and opencode.

Every harness gets its own fresh home, its own recording endpoint
(`construct-dev serve`), a warm-up run, and then interleaved rounds. All
model traffic goes through that endpoint, so timings and token counts are
measured the same way, outside the harnesses.

  bench.py size
  bench.py frame     [--rounds N]                 first frame / ready, in a pty
  bench.py tools     [--rounds N] [--calls N]     tool-call latency (scripted model)
  bench.py memory    [--rounds N] [--steps N]     RSS over a long task (scripted model)
  bench.py task      [--rounds N] --model M       multi-turn real task via OpenRouter
  bench.py report                                  summarize results/*.json

Results land in bench/results/<suite>-<timestamp>.json.
"""

import argparse
import json
import os
import random
import re
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
RESULTS = ROOT / "bench" / "results"
DEV = ROOT / "target" / "debug" / "construct-dev"
CONSTRUCT = ROOT / "target" / "release" / "construct"
HERMES_VENV = Path.home() / ".local/share/mise/installs/pipx-hermes-agent/0.19.0/hermes-agent"
HERMES = HERMES_VENV / "bin" / "hermes"
OPENCODE = Path.home() / ".local/share/mise/installs/opencode/1.18.34/opencode"
KEY_FILE = Path.home() / ".config" / "construct-bench.key"
JEV_KEY_FILE = Path.home() / ".config" / "construct-bench-jev.key"


def read_key(path: Path):
    """A key file holds either the bare key or NAME=key (optionally `export`ed)."""
    t = path.read_text().strip()
    return re.sub(r"^(export\s+)?[A-Z][A-Z0-9_]*\s*=\s*", "", t).strip("'\"")


WORK = Path(os.environ.get("BENCH_WORK", tempfile.gettempdir())) / "construct-bench"


# ---------------------------------------------------------------- endpoint


class Endpoint:
    """A construct-dev serve process: scripted or forwarding upstream."""

    def __init__(self, dir: Path, script=None, upstream=None, chunk_ms=2):
        self.dir = dir
        dir.mkdir(parents=True, exist_ok=True)
        self.log = dir / "log.jsonl"
        if self.log.exists():
            self.log.unlink()
        port_file = dir / "port"
        if port_file.exists():
            port_file.unlink()
        args = [str(DEV), "serve", "--log", str(self.log), "--port-file", str(port_file), "--chunk-ms", str(chunk_ms)]
        if script is not None:
            sp = dir / "script.json"
            sp.write_text(json.dumps({"steps": script}))
            args += ["--script", str(sp)]
        if upstream:
            args += ["--upstream", upstream]
        env = dict(os.environ)
        if upstream:
            key = read_key(KEY_FILE) if KEY_FILE.exists() else os.environ.get("OPENROUTER_API_KEY")
            if not key:
                sys.exit(f"no OpenRouter key: put it in {KEY_FILE}")
            # Never forward some other secret to the upstream by mistake.
            if "openrouter.ai" in upstream and not key.startswith("sk-or-"):
                sys.exit(f"{KEY_FILE} doesn't hold an OpenRouter key (expected sk-or-...); not sending it upstream")
            if JEV_KEY_FILE.exists() and read_key(JEV_KEY_FILE) == key:
                sys.exit(f"{KEY_FILE} holds the same key as {JEV_KEY_FILE}; not sending it upstream")
            env["OPENROUTER_API_KEY"] = key
        self.proc = subprocess.Popen(args, stdout=subprocess.DEVNULL, stderr=open(dir / "serve.err", "w"), env=env)
        for _ in range(500):
            if port_file.exists() and port_file.read_text().strip():
                break
            time.sleep(0.01)
        self.port = int(port_file.read_text())
        self.url = f"http://127.0.0.1:{self.port}/v1"

    def records(self):
        if not self.log.exists():
            return []
        return [json.loads(l) for l in self.log.read_text().splitlines() if l.strip()]

    def close(self):
        self.proc.terminate()
        try:
            self.proc.wait(5)
        except subprocess.TimeoutExpired:
            self.proc.kill()


# ---------------------------------------------------------------- harnesses


class Harness:
    name = ""
    ready = ""

    def __init__(self, home: Path):
        self.home = home

    def env(self):
        return {}

    def configure(self, url, model):
        raise NotImplementedError

    def headless(self, prompt, cont=False):
        raise NotImplementedError

    def tui(self):
        raise NotImplementedError

    def full_env(self):
        e = dict(os.environ)
        # nothing from the user's own setup leaks in
        for k in list(e):
            if k.startswith(("OPENAI", "OPENROUTER", "ANTHROPIC", "TYPESAFE", "CONSTRUCT", "HERMES", "OPENCODE")):
                del e[k]
        e.update(self.env())
        return e


class Construct(Harness):
    name = "construct"
    ready = "ask construct"
    jev = False

    seed_skills = False

    def env(self):
        e = {"CONSTRUCT_HOME": str(self.home), "OPENAI_API_KEY": "via-proxy"}
        if self.jev and JEV_KEY_FILE.exists():
            e["TYPESAFE_API_KEY"] = read_key(JEV_KEY_FILE)
        else:
            e["CONSTRUCT_ROUTER"] = "local"
        return e

    def configure(self, url, model):
        self.home.mkdir(parents=True, exist_ok=True)
        (self.home / "config.toml").write_text(f'base_url = "{url}"\nmodel = "{model}"\n')
        skills = self.home / "skills"
        if self.seed_skills and not skills.exists():
            # A realistic existing library (bench/jev/library.json): the router
            # has something to choose from, or to wrongly suggest.
            lib = json.loads((ROOT / "bench" / "jev" / "library.json").read_text())
            for name, desc in lib["skills"].items():
                d = skills / name
                d.mkdir(parents=True)
                (d / "SKILL.md").write_text(f"---\nname: {name}\ndescription: {desc}\n---\n\n# {name}\n\n1. Follow the team runbook for this.\n")

    def headless(self, prompt, cont=False):
        return [str(CONSTRUCT), "run"] + (["-c"] if cont else []) + [prompt]

    def tui(self):
        return [str(CONSTRUCT)]


class ConstructJev(Construct):
    """construct with the TypeSafe key: Jev routes and gates learning."""

    name = "construct-jev"
    jev = True


class Hermes(Harness):
    name = "hermes"
    ready = "❯"

    def env(self):
        return {"HERMES_HOME": str(self.home)}

    def configure(self, url, model):
        self.home.mkdir(parents=True, exist_ok=True)
        (self.home / "config.yaml").write_text(
            f"model:\n  provider: custom\n  default: {model}\n  base_url: {url}\n  api_key: via-proxy\n"
            # same step budget as construct, so long tasks can finish
            "agent:\n  max_turns: 200\n"
        )

    def headless(self, prompt, cont=False):
        return [str(HERMES), "--yolo"] + (["-c"] if cont else []) + ["-z", prompt]

    def tui(self):
        return [str(HERMES)]


class Opencode(Harness):
    name = "opencode"
    ready = "Ask anything"

    def env(self):
        h = self.home
        return {
            "HOME": str(h),
            "XDG_CONFIG_HOME": str(h / "config"),
            "XDG_DATA_HOME": str(h / "data"),
            "XDG_CACHE_HOME": str(h / "cache"),
            "XDG_STATE_HOME": str(h / "state"),
        }

    def configure(self, url, model):
        d = self.home / "config" / "opencode"
        d.mkdir(parents=True, exist_ok=True)
        cfg = {
            "$schema": "https://opencode.ai/config.json",
            "provider": {
                "bench": {
                    "npm": "@ai-sdk/openai-compatible",
                    "name": "bench",
                    "options": {"baseURL": url, "apiKey": "via-proxy"},
                    "models": {model: {"name": model, "tool_call": True}},
                }
            },
            "model": f"bench/{model}",
            "small_model": f"bench/{model}",
            "autoupdate": False,
            "share": "disabled",
        }
        (d / "opencode.json").write_text(json.dumps(cfg, indent=1))

    def headless(self, prompt, cont=False):
        return [str(OPENCODE), "run", "--auto"] + (["-c"] if cont else []) + [prompt]

    def tui(self):
        return [str(OPENCODE)]


HARNESSES = {"construct": Construct, "hermes": Hermes, "opencode": Opencode}
# Variants, only run when named with --harness.
VARIANTS = {"construct-jev": ConstructJev}


# ---------------------------------------------------------------- helpers


def tree_rss_kb(pid):
    """RSS of a process and all its descendants, in KiB."""
    total = 0
    pids = [pid]
    seen = set()
    while pids:
        p = pids.pop()
        if p in seen:
            continue
        seen.add(p)
        try:
            for line in open(f"/proc/{p}/status"):
                if line.startswith("VmRSS:"):
                    total += int(line.split()[1])
                    break
            for t in os.listdir(f"/proc/{p}/task"):
                try:
                    pids += [int(c) for c in open(f"/proc/{p}/task/{t}/children").read().split()]
                except OSError:
                    pass
        except OSError:
            pass
    return total


def run(cmd, env, cwd, timeout=900, sample=False):
    """Run a command; optionally sample its process-tree RSS every 50ms."""
    t0 = time.perf_counter()
    # PWD must agree with cwd: opencode resolves the directory it works in
    # from $PWD, and an inherited one points the agent at another repo.
    env = {**env, "PWD": str(cwd)}
    # stdin must be closed: a harness that reads piped stdin as part of the
    # prompt waits for EOF, and an inherited open pipe never sends one.
    # PWD must match cwd: opencode takes its working directory (and which
    # AGENTS.md to load) from $PWD, not getcwd().
    env = dict(env, PWD=str(cwd))
    p = subprocess.Popen(cmd, env=env, cwd=cwd, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.PIPE, start_new_session=True)
    samples = []
    stop = threading.Event()

    def sampler():
        while not stop.is_set():
            rss = tree_rss_kb(p.pid)
            if rss:
                samples.append((time.perf_counter() - t0, rss))
            stop.wait(0.05)

    th = threading.Thread(target=sampler, daemon=True)
    if sample:
        th.start()
    try:
        out, err = p.communicate(timeout=timeout)
        timed_out = False
    except subprocess.TimeoutExpired:
        os.killpg(p.pid, signal.SIGKILL)
        out, err = p.communicate()
        timed_out = True
    stop.set()
    if sample:
        th.join()
    return {
        "rc": p.returncode,
        "timed_out": timed_out,
        "wall_s": time.perf_counter() - t0,
        "stdout": out.decode(errors="replace")[-4000:],
        "stderr": err.decode(errors="replace")[-4000:],
        "rss": samples,
    }


def wait_for_learners(timeout=300):
    """construct hands its learning pass to a detached process; let it finish
    so its traffic doesn't overlap the next run."""
    t0 = time.time()
    while time.time() - t0 < timeout:
        r = subprocess.run(["pgrep", "-f", f"{CONSTRUCT} learn"], capture_output=True)
        if r.returncode != 0:
            return
        time.sleep(0.5)


def du(path):
    path = Path(path)
    if not path.exists():
        return 0
    if path.is_file():
        return path.stat().st_size
    total = 0
    for root, _, files in os.walk(path):
        for f in files:
            fp = os.path.join(root, f)
            if not os.path.islink(fp):
                try:
                    total += os.stat(fp).st_size
                except OSError:
                    pass
    return total


def stats(xs):
    xs = sorted(x for x in xs if x is not None)
    if not xs:
        return None
    p = lambda q: xs[min(len(xs) - 1, int(round(q * (len(xs) - 1))))]
    return {"n": len(xs), "median": statistics.median(xs), "p10": p(0.1), "p90": p(0.9), "min": xs[0], "max": xs[-1], "mean": statistics.mean(xs)}


def save(suite, data):
    RESULTS.mkdir(parents=True, exist_ok=True)
    path = RESULTS / f"{suite}-{time.strftime('%Y%m%d-%H%M%S')}.json"
    path.write_text(json.dumps(data, indent=1))
    print(f"saved {path}")
    return path


def fresh_homes(names, tag):
    base = WORK / tag
    if base.exists():
        shutil.rmtree(base)
    return {n: {**HARNESSES, **VARIANTS}[n](base / n / "home") for n in names}


def order(names, r):
    xs = list(names)
    random.Random(r).shuffle(xs)
    return xs


STALLS = {}


def with_stall_retry(name, fn, retries=1):
    """opencode sometimes hangs at startup before contacting the model. Count
    that as a stall (reported) and retry; timed results only come from runs
    that reached the model."""
    for attempt in range(retries + 1):
        out = fn()
        if out is not None:
            return out
        STALLS[name] = STALLS.get(name, 0) + 1
        print(f"  {name}: stalled before any model request (attempt {attempt + 1})")
    return None


def tool_gaps(records):
    """Gap between a response that ended in tool calls and the next request:
    the harness's own overhead to run the tools and build the next call."""
    rs = [r for r in records if not r.get("side") and "t_arrive" in r]
    gaps = []
    for a, b in zip(rs, rs[1:]):
        if a.get("resp_tool_calls") and "t_end" in a:
            gaps.append(b["t_arrive"] - a["t_end"])
    return gaps


# ---------------------------------------------------------------- suites


def suite_size(args):
    out = {}
    out["construct"] = {"install": du(CONSTRUCT), "parts": {"binary": du(CONSTRUCT)}}
    py = Path(os.path.realpath(HERMES_VENV / "bin" / "python")).parent.parent
    out["hermes"] = {"install": du(HERMES_VENV) + du(py), "parts": {"venv": du(HERMES_VENV), "python": du(py)}}
    out["opencode"] = {"install": du(OPENCODE), "parts": {"binary": du(OPENCODE)}}
    # What a first run adds to a fresh home (bundled skills, npm packages...).
    homes = fresh_homes(HARNESSES, "size")
    ep = Endpoint(WORK / "size" / "ep", script=[])
    work = WORK / "size" / "work"
    work.mkdir(parents=True, exist_ok=True)
    for name, h in homes.items():
        h.configure(ep.url, "mock-model")
        r = run(h.headless("say hi"), h.full_env(), work, timeout=300)
        wait_for_learners()
        out[name]["fresh_home_after_first_run"] = du(h.home)
        out[name]["first_run_s"] = r["wall_s"]
        out[name]["first_run_rc"] = r["rc"]
    ep.close()
    for name, v in out.items():
        print(f"{name:10} install {v['install']/1e6:8.1f} MB   home after first run {v.get('fresh_home_after_first_run',0)/1e6:7.1f} MB")
    save("size", out)


def suite_frame(args):
    names = args.harness
    homes = fresh_homes(names, "frame")
    ep = Endpoint(WORK / "frame" / "ep", script=[])
    work = WORK / "frame" / "work"
    work.mkdir(parents=True, exist_ok=True)
    for h in homes.values():
        h.configure(ep.url, "mock-model")
    results = {n: [] for n in names}

    def once(h):
        env_args = []
        for k, v in h.env().items():
            env_args += ["--env", f"{k}={v}"]
        script = f'wait "{h.ready}" 30000\n'
        r = subprocess.run(
            [str(DEV), "drive", "--metrics", "--cols", "120", "--rows", "36", "--cwd", str(work)] + env_args + ["--"] + h.tui(),
            input=script.encode(),
            capture_output=True,
            timeout=60,
        )
        if r.returncode != 0:
            return {"error": r.stderr.decode()[-500:]}
        m = json.loads(r.stdout.decode().strip().splitlines()[-1])
        return {"first_output_ms": m["first_output_ms"], "first_frame_ms": m["first_frame_ms"], "ready_ms": m["waits"][0]["ms"]}

    for n in names:  # warm-up: first-run setup, disk cache
        once(homes[n])
        time.sleep(0.5)
    for r in range(args.rounds):
        for n in order(names, r):
            m = once(homes[n])
            results[n].append(m)
            print(f"round {r} {n:10} {m}")
            time.sleep(0.3)
    ep.close()
    summary = {n: {k: stats([x.get(k) for x in v]) for k in ("first_output_ms", "first_frame_ms", "ready_ms")} for n, v in results.items()}
    for n, s in summary.items():
        print(f"{n:10} first frame median {s['first_frame_ms']['median']:8.1f} ms   ready median {s['ready_ms']['median']:8.1f} ms")
    save("frame", {"rounds": args.rounds, "raw": results, "summary": summary})


def tool_script(calls):
    steps = []
    for i in range(calls):
        steps.append({"call": "read", "args": {"path": "data.txt"}})
        steps.append({"call": "shell", "args": {"command": "echo hi"}})
    steps.append({"text": "done"})
    return steps


def suite_tools(args):
    names = args.harness
    homes = fresh_homes(names, "tools")
    results = {n: {"read": [], "shell": [], "wall": [], "stalled": 0} for n in names}

    def once(name, h, r):
        ep = Endpoint(WORK / "tools" / name / f"ep{r}", script=tool_script(args.calls))
        h.configure(ep.url, "mock-model")
        work = WORK / "tools" / name / f"work{r}"
        work.mkdir(parents=True, exist_ok=True)
        (work / "data.txt").write_text("".join(f"line {i}: the quick brown fox\n" for i in range(60)))
        res = run(h.headless("go"), h.full_env(), work, timeout=180)
        wait_for_learners()
        recs = ep.records()
        ep.close()
        main = [x for x in recs if not x.get("side")]
        gaps = tool_gaps(recs)
        # steps alternate read, shell
        return res, gaps[0::2], gaps[1::2], len(main)

    for n in names:
        once(n, homes[n], "warm")
    for r in range(args.rounds):
        for n in order(names, r):
            res, rg, sg, nreq = once(n, homes[n], r)
            if nreq == 0:
                results[n]["stalled"] += 1
                print(f"round {r} {n:10} STALLED: no model request within {res['wall_s']:.0f}s (rc={res['rc']}, timed_out={res['timed_out']})")
                continue
            results[n]["read"] += rg
            results[n]["shell"] += sg
            results[n]["wall"].append(res["wall_s"])
            ok = nreq >= 2 * args.calls + 1  # extra = background learning requests
            med = lambda xs: f"{stats(xs)['median']:.1f}" if xs else "-"
            print(f"round {r} {n:10} rc={res['rc']} requests={nreq}{'' if ok else ' (UNEXPECTED)'} read med {med(rg)}ms shell med {med(sg)}ms wall {res['wall_s']:.1f}s")
    summary = {n: {k: (stats(v) if isinstance(v, list) else v) for k, v in d.items()} for n, d in results.items()}
    for n, s in summary.items():
        if s["read"] and s["shell"]:
            print(f"{n:10} read p50 {s['read']['median']:7.1f} p90 {s['read']['p90']:7.1f} | shell p50 {s['shell']['median']:7.1f} p90 {s['shell']['p90']:7.1f} ms  stalled runs {s['stalled']}")
        else:
            print(f"{n:10} no completed runs (stalled {s['stalled']})")
    save("tools", {"rounds": args.rounds, "calls": args.calls, "raw": results, "summary": summary})


def memory_script(steps, corpus):
    files = sorted(str(p.relative_to(corpus)) for p in corpus.rglob("*.ts") if p.stat().st_size > 4000)[: steps]
    out = []
    patterns = ["export", "function", "Effect", "import", "const", "async"]
    for i in range(steps):
        k = i % 3
        if k == 0:
            out.append({"call": "read", "args": {"path": files[i % len(files)]}, "delay_ms": 40})
        elif k == 1:
            out.append({"call": "grep", "args": {"pattern": patterns[i % len(patterns)]}, "delay_ms": 40})
        else:
            out.append({"call": "shell", "args": {"command": f"ls -la {Path(files[i % len(files)]).parent}"}, "delay_ms": 40})
    out.append({"text": "done"})
    return out


def suite_memory(args):
    names = args.harness
    homes = fresh_homes(names, "memory")
    corpus = ROOT / "research" / "src" / "opencode" / "packages" / "core"
    results = {n: [] for n in names}

    def once(name, h, r):
        work = WORK / "memory" / name / f"work{r}"
        if work.exists():
            shutil.rmtree(work)
        shutil.copytree(corpus, work, ignore=shutil.ignore_patterns("node_modules"))
        ep = Endpoint(WORK / "memory" / name / f"ep{r}", script=memory_script(args.steps, corpus))
        h.configure(ep.url, "mock-model")
        res = run(h.headless("explore this package"), h.full_env(), work, timeout=300, sample=True)
        wait_for_learners()
        recs = ep.records()
        ep.close()
        rss = [kb for _, kb in res["rss"]]
        main = [x for x in recs if not x.get("side")]
        if not main:
            return None
        return {
            "rc": res["rc"],
            "wall_s": res["wall_s"],
            "requests": len(main),
            "peak_mb": max(rss) / 1024 if rss else None,
            "mean_mb": statistics.mean(rss) / 1024 if rss else None,
            "last_req_kb": main[-1]["req_bytes"] / 1024 if main else None,
            "series": res["rss"][:: max(1, len(res["rss"]) // 200)],
        }

    for n in names:
        with_stall_retry(n, lambda: once(n, homes[n], "warm"))
        print(f"warm-up {n} done")
    for r in range(args.rounds):
        for n in order(names, r):
            m = with_stall_retry(n, lambda: once(n, homes[n], r))
            if m is None:
                continue
            results[n].append(m)
            print(f"round {r} {n:10} rc={m['rc']} requests={m['requests']} peak {m['peak_mb']:.0f} MB mean {m['mean_mb']:.0f} MB wall {m['wall_s']:.1f}s last request {m['last_req_kb']:.0f} KB")
    summary = {n: {k: stats([x[k] for x in v]) for k in ("peak_mb", "mean_mb", "wall_s", "last_req_kb")} for n, v in results.items()}
    save("memory", {"rounds": args.rounds, "steps": args.steps, "raw": results, "summary": summary, "stalls": STALLS})


# ---------------------------------------------------------------- real task

TASK_TURNS = [
    "This repo is a tiny Python library. `python3 -m unittest` fails. Find out why and fix the bug in the library (not the tests). Run the tests to confirm.",
    "Add a function `median(values)` to stats.py that returns the median of a non-empty list of numbers (average the two middle values for even lengths) and raises ValueError on an empty list. Add unit tests for it in test_stats.py and run the whole suite.",
    "Document both functions in README.md under a '## API' heading, with one short usage example each. Then tell me in one sentence what you changed across this session.",
]


def task_fixture(dest: Path):
    if dest.exists():
        shutil.rmtree(dest)
    dest.mkdir(parents=True)
    (dest / "stats.py").write_text(
        '"""Small statistics helpers."""\n\n\ndef mean(values):\n    """Arithmetic mean of a non-empty list."""\n    if not values:\n        raise ValueError("mean of empty list")\n    return sum(values) / (len(values) - 1)\n'
    )
    (dest / "test_stats.py").write_text(
        "import unittest\n\nfrom stats import mean\n\n\nclass MeanTest(unittest.TestCase):\n    def test_mean(self):\n        self.assertEqual(mean([1, 2, 3, 4]), 2.5)\n\n    def test_single(self):\n        self.assertEqual(mean([7]), 7)\n\n    def test_empty(self):\n        with self.assertRaises(ValueError):\n            mean([])\n\n\nif __name__ == \"__main__\":\n    unittest.main()\n"
    )
    (dest / "README.md").write_text("# stats\n\nSmall statistics helpers.\n")
    subprocess.run(["git", "init", "-q"], cwd=dest)
    subprocess.run(["git", "add", "-A"], cwd=dest)
    subprocess.run(["git", "-c", "user.name=bench", "-c", "user.email=bench@example.com", "commit", "-qm", "init"], cwd=dest)


def check_task(work: Path):
    """What a reviewer would check, scripted."""
    checks = {}
    r = subprocess.run([sys.executable, "-m", "unittest", "-q"], cwd=work, capture_output=True, timeout=60)
    checks["tests_pass"] = r.returncode == 0
    probe = (
        "import stats\n"
        "assert stats.mean([1,2,3,4]) == 2.5\n"
        "assert stats.median([3,1,2]) == 2\n"
        "assert stats.median([4,1,3,2]) == 2.5\n"
        "try:\n    stats.median([])\n    raise SystemExit(1)\nexcept ValueError:\n    pass\n"
    )
    r = subprocess.run([sys.executable, "-c", probe], cwd=work, capture_output=True, timeout=60)
    checks["behaviour"] = r.returncode == 0
    tests = (work / "test_stats.py").read_text() if (work / "test_stats.py").exists() else ""
    checks["median_tested"] = "median" in tests
    checks["tests_untouched_for_mean"] = "assertEqual(mean([1, 2, 3, 4]), 2.5)" in tests
    readme = (work / "README.md").read_text() if (work / "README.md").exists() else ""
    checks["readme_api"] = "## API" in readme and "median" in readme and "mean" in readme
    return checks


def suite_task(args):
    names = args.harness
    homes = fresh_homes(names, "task")
    results = {n: [] for n in names}

    def once(name, h, r):
        work = WORK / "task" / name / f"work{r}"
        task_fixture(work)
        ep = Endpoint(WORK / "task" / name / f"ep{r}", upstream=args.upstream)
        h.configure(ep.url, args.model)
        turns = []
        for i, prompt in enumerate(TASK_TURNS):
            for attempt in range(2):
                before = len(ep.records())
                res = run(h.headless(prompt, cont=i > 0), h.full_env(), work, timeout=args.timeout, sample=True)
                if len(ep.records()) > before:
                    break
                STALLS[name] = STALLS.get(name, 0) + 1
                print(f"  {name}: turn {i + 1} stalled before any model request (attempt {attempt + 1})")
            rss = [kb for _, kb in res["rss"]]
            turns.append({
                "rc": res["rc"],
                "timed_out": res["timed_out"],
                "wall_s": res["wall_s"],
                "peak_mb": max(rss) / 1024 if rss else None,
                "stdout_tail": res["stdout"][-600:],
                "stderr_tail": res["stderr"][-600:],
            })
        # the detached learning pass belongs to this run's token bill too
        t_learn0 = time.time()
        wait_for_learners()
        learn_s = time.time() - t_learn0
        recs = ep.records()
        ep.close()
        checks = check_task(work)
        main = [x for x in recs if not x.get("side")]
        first = next((x for x in recs if x.get("n_tools")), None)
        u = lambda x, k: (x.get("usage") or {}).get(k) or 0
        return {
            "turns": turns,
            "checks": checks,
            "success": all(checks.values()),
            "wall_s": sum(t["wall_s"] for t in turns),
            "requests": len(recs),
            "agent_requests": len(main),
            "errors": [x.get("error") for x in recs if x.get("error")][:5],
            "first_request": {
                "prompt_tokens": u(first, "prompt_tokens") if first else None,
                "bytes": first["req_bytes"] if first else None,
                "n_tools": first["n_tools"] if first else None,
                "system_chars": first["system_chars"] if first else None,
                "tools_chars": first["tools_chars"] if first else None,
            },
            "side_requests_before_first_agent": sum(1 for x in recs[: recs.index(first)] if first) if first else None,
            "prompt_tokens": sum(u(x, "prompt_tokens") for x in recs),
            "completion_tokens": sum(u(x, "completion_tokens") for x in recs),
            "tool_gaps_ms": tool_gaps(recs),
            "peak_mb": max((t["peak_mb"] or 0) for t in turns),
            "learn_wait_s": learn_s,
            "records": recs,
        }

    if not args.no_warmup:
        for n in names:
            m = once(n, homes[n], "warm")
            print(f"warm-up {n:10} success={m['success']} {m['checks']} requests={m['requests']} wall {m['wall_s']:.0f}s")
            time.sleep(args.pause)
    for r in range(args.rounds):
        for n in order(names, r):
            m = once(n, homes[n], r)
            results[n].append(m)
            print(
                f"round {r} {n:10} success={m['success']} {m['checks']} requests={m['requests']} tokens={m['prompt_tokens']}+{m['completion_tokens']} "
                f"first={m['first_request']['prompt_tokens']} wall {m['wall_s']:.0f}s peak {m['peak_mb']:.0f}MB"
            )
            time.sleep(args.pause)
    summary = {}
    for n, v in results.items():
        summary[n] = {
            "success_rate": sum(x["success"] for x in v) / len(v) if v else None,
            "checks_passed": stats([sum(x["checks"].values()) for x in v]),
            "wall_s": stats([x["wall_s"] for x in v]),
            "requests": stats([x["requests"] for x in v]),
            "prompt_tokens": stats([x["prompt_tokens"] for x in v]),
            "completion_tokens": stats([x["completion_tokens"] for x in v]),
            "first_request_tokens": stats([x["first_request"]["prompt_tokens"] for x in v]),
            "tool_gap_ms": stats([g for x in v for g in x["tool_gaps_ms"]]),
            "peak_mb": stats([x["peak_mb"] for x in v]),
        }
    save("task", {"model": args.model, "rounds": args.rounds, "turns": TASK_TURNS, "raw": results, "summary": summary, "stalls": STALLS, "jev": args.jev})


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("suite", choices=["size", "frame", "tools", "memory", "task"])
    ap.add_argument("--rounds", type=int, default=5)
    ap.add_argument("--calls", type=int, default=15)
    ap.add_argument("--steps", type=int, default=120)
    ap.add_argument("--model", default="mock-model")
    ap.add_argument("--upstream", default="https://openrouter.ai/api/v1")
    ap.add_argument("--timeout", type=int, default=900)
    ap.add_argument("--pause", type=float, default=2.0)
    ap.add_argument("--no-warmup", action="store_true")
    ap.add_argument("--harness", nargs="+", default=list(HARNESSES))
    ap.add_argument("--jev", action="store_true", help="give construct the TypeSafe key (Jev router)")
    ap.add_argument("--seed-skills", action="store_true", help="seed construct homes with bench/jev/library.json skills")
    args = ap.parse_args()
    Construct.jev = Construct.jev or args.jev
    Construct.seed_skills = args.seed_skills
    # One bench at a time: two runs share WORK and would corrupt each other.
    import fcntl
    WORK.mkdir(parents=True, exist_ok=True)
    lock = open(WORK / "bench.lock", "w")
    try:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
    except BlockingIOError:
        sys.exit(f"another bench is running (lock {WORK / 'bench.lock'}); stop it with bench/stop.sh")
    lock.write(str(os.getpid()))
    lock.flush()
    for b in (DEV, CONSTRUCT):
        if not b.exists():
            sys.exit(f"missing {b}: cargo build -p construct-dev && cargo build --release -p construct")
    {"size": suite_size, "frame": suite_frame, "tools": suite_tools, "memory": suite_memory, "task": suite_task}[args.suite](args)


if __name__ == "__main__":
    main()
