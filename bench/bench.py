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
import hashlib
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
def resolve_bin(name: str, env_var: str):
    """Find a competitor binary without pinning a version or a machine: an
    explicit override first, then $PATH (mise/pipx put a shim there), then
    `mise which`. Returns None if it isn't installed — preflight() reports that
    with an install hint rather than failing deep inside a run."""
    override = os.environ.get(env_var)
    if override:
        return Path(override)
    found = shutil.which(name)
    if found:
        return Path(found)
    try:
        r = subprocess.run(["mise", "which", name], capture_output=True, text=True, timeout=10)
        if r.returncode == 0 and r.stdout.strip():
            return Path(r.stdout.strip())
    except Exception:
        pass
    return None


# Resolved lazily-ish at import; may be None when the tool isn't installed.
HERMES = resolve_bin("hermes", "BENCH_HERMES")
OPENCODE = resolve_bin("opencode", "BENCH_OPENCODE")
# Install dir (for the `size` suite only): the real path mise points at, not
# the shim. Best-effort — None when hermes isn't installed.
try:
    _hv = subprocess.run(["mise", "which", "hermes"], capture_output=True, text=True, timeout=10)
    HERMES_VENV = Path(_hv.stdout.strip()).parent.parent if _hv.returncode == 0 and _hv.stdout.strip() else None
except Exception:
    HERMES_VENV = None

# How to get each competitor, shown by preflight() when one is missing.
INSTALL_HINTS = {
    "hermes": "mise use -g pipx:hermes-agent   (or set BENCH_HERMES=/path/to/hermes)",
    "opencode": "mise use -g opencode           (or set BENCH_OPENCODE=/path/to/opencode)",
}
KEY_FILE = Path.home() / ".config" / "construct-bench.key"
JEV_KEY_FILE = Path.home() / ".config" / "construct-bench-jev.key"


def read_key(path: Path):
    """A key file holds either the bare key or NAME=key (optionally `export`ed)."""
    t = path.read_text().strip()
    return re.sub(r"^(export\s+)?[A-Z][A-Z0-9_]*\s*=\s*", "", t).strip("'\"")


WORK = Path(os.environ.get("BENCH_WORK", tempfile.gettempdir())) / "construct-bench"


# ---------------------------------------------------------------- sandbox
# construct sandboxes itself (Landlock, src/sandbox.rs). The competitor
# harnesses don't, so the bench confines them with bwrap instead — and it does
# so inside their adapters (see Sandboxed), not at the call site, so there is
# no way to launch hermes or opencode unsandboxed by forgetting a wrapper.
# An un-contained agent on a real model searches the disk: one read the bench
# key files. The policy mirrors src/sandbox.rs: writable only inside WORK
# (every bench home and work dir lives there), system paths and toolchains
# read-only, a private /tmp and PID namespace, and no reach to the real $HOME,
# the key files, the construct repo or other processes. Network stays shared so
# the harness can still reach the proxy.
BWRAP = shutil.which("bwrap")
_SYS_RO = ["/usr", "/bin", "/sbin", "/lib", "/lib64", "/etc", "/opt", "/nix", "/run/current-system", "/sys"]
_HOME_RO = [".local/share/mise", ".config/mise", ".local/bin", ".rustup", ".nvm", ".pyenv", ".bun/bin", "go/bin"]
_HOME_RW = [".cargo", ".local/state/mise", ".cache", ".npm", ".bun/install", "go/pkg"]


def bwrap_prefix(cwd: Path) -> list:
    """The bwrap argv that a competitor command is launched behind."""
    if not BWRAP:
        if os.environ.get("BENCH_ALLOW_UNSANDBOXED") == "1":
            print("!!! BENCH_ALLOW_UNSANDBOXED=1: launching a competitor harness WITHOUT a sandbox", file=sys.stderr)
            return []
        sys.exit(
            "bwrap not found: hermes/opencode must be sandboxed before a real-model run. "
            "Install bubblewrap, or (never for a real model) set BENCH_ALLOW_UNSANDBOXED=1."
        )
    home = Path.home()
    argv = [BWRAP, "--die-with-parent", "--unshare-pid", "--proc", "/proc", "--dev", "/dev", "--tmpfs", "/tmp"]
    for p in _SYS_RO:
        argv += ["--ro-bind-try", p, p]
    for rel in _HOME_RO:
        argv += ["--ro-bind-try", str(home / rel), str(home / rel)]
    for rel in _HOME_RW:
        argv += ["--bind-try", str(home / rel), str(home / rel)]
    argv += ["--bind", str(WORK), str(WORK)]
    argv += ["--setenv", "TMPDIR", "/tmp", "--chdir", str(cwd), "--"]
    return argv


class Sandboxed:
    """Mixin: every command a harness hands back is already wrapped in bwrap.
    Order matters — put it before Harness in the bases."""

    def wrap(self, cmd: list, cwd) -> list:
        if cwd is None:
            raise ValueError(f"{self.name}: a sandboxed harness needs cwd to confine the run")
        return bwrap_prefix(Path(cwd)) + cmd


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

    def headless(self, prompt, cont=False, cwd=None):
        raise NotImplementedError

    def tui(self, cwd=None):
        raise NotImplementedError

    def wrap(self, cmd: list, cwd) -> list:
        """No sandbox by default. construct confines itself; the Sandboxed
        mixin overrides this for harnesses that don't."""
        return cmd

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

    def headless(self, prompt, cont=False, cwd=None):
        return [str(CONSTRUCT), "run"] + (["-c"] if cont else []) + [prompt]

    def tui(self, cwd=None):
        return [str(CONSTRUCT)]


class ConstructJev(Construct):
    """construct with the TypeSafe key: Jev routes and gates learning."""

    name = "construct-jev"
    jev = True


class Hermes(Sandboxed, Harness):
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

    def headless(self, prompt, cont=False, cwd=None):
        return self.wrap([str(HERMES), "--yolo"] + (["-c"] if cont else []) + ["-z", prompt], cwd)

    def tui(self, cwd=None):
        return self.wrap([str(HERMES)], cwd)


class Opencode(Sandboxed, Harness):
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

    def headless(self, prompt, cont=False, cwd=None):
        return self.wrap([str(OPENCODE), "run", "--auto"] + (["-c"] if cont else []) + [prompt], cwd)

    def tui(self, cwd=None):
        return self.wrap([str(OPENCODE)], cwd)


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
    if HERMES_VENV:
        py = Path(os.path.realpath(HERMES_VENV / "bin" / "python")).parent.parent
        out["hermes"] = {"install": du(HERMES_VENV) + du(py), "parts": {"venv": du(HERMES_VENV), "python": du(py)}}
    if OPENCODE:
        out["opencode"] = {"install": du(OPENCODE), "parts": {"binary": du(OPENCODE)}}
    # What a first run adds to a fresh home (bundled skills, npm packages...).
    homes = fresh_homes(HARNESSES, "size")
    ep = Endpoint(WORK / "size" / "ep", script=[])
    work = WORK / "size" / "work"
    work.mkdir(parents=True, exist_ok=True)
    for name, h in homes.items():
        h.configure(ep.url, "mock-model")
        r = run(h.headless("say hi", cwd=work), h.full_env(), work, timeout=300)
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
            [str(DEV), "drive", "--metrics", "--cols", "120", "--rows", "36", "--cwd", str(work)] + env_args + ["--"] + h.tui(cwd=work),
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
        res = run(h.headless("go", cwd=work), h.full_env(), work, timeout=180)
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
        res = run(h.headless("explore this package", cwd=work), h.full_env(), work, timeout=300, sample=True)
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


def session_facts(home: Path, since: float):
    """Routes and tool calls from construct sessions written after `since`.
    (run() keeps only the tail of stdout, so don't parse events from it.)"""
    routes, calls = [], []
    for f in sorted((home / "sessions").glob("*.jsonl")):
        if f.stat().st_mtime < since:
            continue
        for line in f.read_text().splitlines():
            d = json.loads(line)
            if d.get("type") == "route":
                routes.append(d["data"])
            for c in (d.get("message") or {}).get("tool_calls") or []:
                calls.append({"name": c["function"]["name"], "args": c["function"]["arguments"]})
    return routes, calls


def assert_no_keys(root: Path):
    """Stop the bench if a run's logs contain one of the bench keys: it means
    an agent read a key file or the environment and sent it upstream."""
    keys = [read_key(f) for f in (KEY_FILE, JEV_KEY_FILE) if f.exists()]
    keys = [k for k in keys if len(k) > 12]
    for dp, _, files in os.walk(root):
        for n in files:
            p = Path(dp) / n
            try:
                t = p.read_text(errors="ignore")
            except OSError:
                continue
            if any(k in t for k in keys):
                sys.exit(f"KEY LEAK: a bench key appears in {p}; stop, redact it and rotate the key")


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
                res = run(h.headless(prompt, cont=i > 0, cwd=work), h.full_env(), work, timeout=args.timeout, sample=True)
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
        assert_no_keys(WORK / "task" / name)
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



# ---------------------------------------------------------------- routing
# A task that only succeeds with knowledge from one skill in a library of 15:
# the signing key id and the landmine (`make release` wipes the changelog) are
# written nowhere in the repo. Measures whether the router puts the right skill
# in front of the model when the request doesn't use the skill's words.
# The key id is random per run and lives only in that run's skill and stamp
# script: agents do search the disk (one found this file), and sibling runs'
# homes sit next to each other.

ROUTING_PROMPT = "Get version 1.4.0 out to customers."
RELEASE_SKILL_T = """---
name: stats-release
description: Ship a new version of the stats package to customers - version bump in VERSION and stats/meta.toml, signed changelog stamp, release verification. Not for publishing docs.
---

# Releasing stats

1. Write the new version to `VERSION` and to `version = "..."` in `stats/meta.toml`.
2. Run `./tools/stamp --sign-with {key} <version>`. The key id is `{key}`; nothing else is accepted.
3. Run `./tools/verify-release`; it must print PASS.

Pitfall: `make release` is deprecated and empties CHANGELOG.md. Don't run it.
"""


def routing_fixture(dest: Path, key: str):
    if dest.exists():
        shutil.rmtree(dest)
    (dest / "stats").mkdir(parents=True)
    (dest / "tools").mkdir()
    (dest / "VERSION").write_text("1.3.2\n")
    (dest / "stats" / "meta.toml").write_text('name = "stats"\nversion = "1.3.2"\n')
    (dest / "stats" / "__init__.py").write_text('"""Small statistics helpers."""\n')
    (dest / "CHANGELOG.md").write_text("# Changelog\n\n## 1.3.2 (signed ledger-2025, retired)\n- Fix mean of one value.\n")
    (dest / "README.md").write_text("# stats\n\nSmall statistics helpers. Releases are signed.\n")
    (dest / "Makefile").write_text("release:\n\t@: > CHANGELOG.md\n\t@echo released $$(cat VERSION)\n")
    stamp = dest / "tools" / "stamp"
    stamp.write_text(
        "#!/usr/bin/env bash\nset -e\n"
        'if [ "$1" != "--sign-with" ] || [ -z "$2" ] || [ -z "$3" ]; then echo "usage: stamp --sign-with <key-id> <version>" >&2; exit 2; fi\n'
        'if [ "$2" != "{key}" ]; then echo "error: unknown signing key id \'$2\' (ask the release owner)" >&2; exit 3; fi\n'
        'tmp=$(mktemp); { echo "# Changelog"; echo; echo "## $3 (signed $2)"; echo "- Release $3."; tail -n +3 CHANGELOG.md; } > "$tmp"; mv "$tmp" CHANGELOG.md\n'
        'echo "stamped $3"\n'
    )
    verify = dest / "tools" / "verify-release"
    verify.write_text(
        "#!/usr/bin/env bash\n"
        'v=$(cat VERSION); m=$(sed -n \'s/^version = "\\(.*\\)"/\\1/p\' stats/meta.toml)\n'
        'top=$(grep -m1 "^## " CHANGELOG.md || true)\n'
        'fail=0\n'
        '[ "$v" = "$m" ] || { echo "FAIL: VERSION $v != meta.toml $m"; fail=1; }\n'
        'case "$top" in "## $v (signed {key})") ;; *) echo "FAIL: top changelog entry is not a signed $v entry"; fail=1;; esac\n'
        'grep -q "^## 1.3.2" CHANGELOG.md || { echo "FAIL: changelog history missing"; fail=1; }\n'
        '[ $fail = 0 ] && echo PASS || exit 1\n'
    )
    for f in (stamp, verify):
        f.write_text(f.read_text().replace("{key}", key))
        f.chmod(0o755)
    subprocess.run(["git", "init", "-q"], cwd=dest)
    subprocess.run(["git", "add", "-A"], cwd=dest)
    subprocess.run(["git", "-c", "user.name=bench", "-c", "user.email=bench@example.com", "commit", "-qm", "init"], cwd=dest)


def release_skill(key: str):
    return RELEASE_SKILL_T.replace("{key}", key)


def check_routing(work: Path):
    r = subprocess.run(["./tools/verify-release"], cwd=work, capture_output=True, text=True, timeout=30)
    return {
        "verify_pass": r.returncode == 0 and "PASS" in r.stdout,
        "version_1_4_0": (work / "VERSION").read_text().strip() == "1.4.0",
        "history_kept": "## 1.3.2" in (work / "CHANGELOG.md").read_text(),
    }


def suite_routing(args):
    names = args.harness
    lib = json.loads((ROOT / "bench" / "jev" / "library.json").read_text())
    results = {n: [] for n in names}

    def once(name, r):
        base = WORK / "routing" / name / f"r{r}"
        if base.exists():
            shutil.rmtree(base)
        h = {**HARNESSES, **VARIANTS}[name](base / "home")
        work = base / "work"
        key = f"ledger-{random.randrange(16**4):04x}"
        routing_fixture(work, key)
        skills = h.home / "skills"
        for sname, desc in lib["skills"].items():
            (skills / sname).mkdir(parents=True)
            (skills / sname / "SKILL.md").write_text(f"---\nname: {sname}\ndescription: {desc}\n---\n\n# {sname}\n\n1. Follow the team runbook for this.\n")
        (skills / "stats-release").mkdir()
        (skills / "stats-release" / "SKILL.md").write_text(release_skill(key))
        ep = Endpoint(base / "ep", upstream=args.upstream)
        h.configure(ep.url, args.model)
        cmd = [str(CONSTRUCT), "run", "--json", "--no-learn", ROUTING_PROMPT]
        t0 = time.time()
        res = run(cmd, h.full_env(), work, timeout=args.timeout)
        routes, calls = session_facts(h.home, t0)
        recs = ep.records()
        ep.close()
        assert_no_keys(base)
        route = routes[0] if routes else {}
        loaded = [c for c in calls if c["name"] == "skill"]
        ran_make = any("make release" in c["args"] for c in calls)
        checks = check_routing(work)
        u = lambda x, k: (x.get("usage") or {}).get(k) or 0
        return {
            "checks": checks,
            "success": all(checks.values()),
            "suggested": [s[0] for s in route.get("skills", [])] if route else None,
            "route_ms": route.get("ms"),
            "route_source": route.get("source"),
            "skill_loaded": bool(loaded),
            "ran_make_release": ran_make,
            "requests": len(recs),
            "prompt_tokens": sum(u(x, "prompt_tokens") for x in recs),
            "completion_tokens": sum(u(x, "completion_tokens") for x in recs),
            "wall_s": res["wall_s"],
        }

    if not args.no_warmup:
        for n in names:
            m = once(n, "warm")
            print(f"warm-up {n:14} success={m['success']} suggested={m['suggested']} loaded={m['skill_loaded']} requests={m['requests']}")
    for r in range(args.rounds):
        for n in order(names, r):
            m = once(n, r)
            results[n].append(m)
            print(
                f"round {r} {n:14} success={m['success']} {m['checks']} suggested={m['suggested']} loaded={m['skill_loaded']} "
                f"make_release={m['ran_make_release']} requests={m['requests']} tokens={m['prompt_tokens']} wall {m['wall_s']:.0f}s route {m['route_ms']}ms"
            )
            time.sleep(args.pause)
    summary = {
        n: {
            "success_rate": sum(x["success"] for x in v) / len(v) if v else None,
            "suggested_right": sum("stats-release" in (x["suggested"] or []) for x in v),
            "skill_loaded": sum(x["skill_loaded"] for x in v),
            "ran_make_release": sum(x["ran_make_release"] for x in v),
            "requests": stats([x["requests"] for x in v]),
            "prompt_tokens": stats([x["prompt_tokens"] for x in v]),
            "wall_s": stats([x["wall_s"] for x in v]),
        }
        for n, v in results.items()
    }
    save("routing", {"model": args.model, "rounds": args.rounds, "prompt": ROUTING_PROMPT, "raw": results, "summary": summary})


# ---------------------------------------------------------------- loop
# The learning loop end to end, on a landmine nothing in the repo explains:
# the tests read fixtures generated with the team seed (`./tools/gen-fixtures
# --seed <n>`), and the seed is written nowhere: only the user knows it. A
# model can find the script, but only learning gets it the seed. The seed is
# random per round and the sandbox keeps rounds from reading each other.
# Sessions 1 and 2 are different tasks where the user has to say so; session
# 3 is a third task with no hint. A control runs session 3 in a fresh home.

LOOP_HINT = "The tests need ./tools/gen-fixtures --seed {seed} run first (that's the team seed); that's how this repo works."
LOOP_TASKS = [
    ("median", "Add a median(values) function to stats.py with a unit test, and make sure the whole test suite passes."),
    ("mode", "Add a mode(values) function to stats.py (most common value, ValueError on empty) with a unit test, and make sure the whole test suite passes."),
    ("variance", "Add a variance(values) function to stats.py (population variance, ValueError on empty) with a unit test, and make sure the whole test suite passes."),
]


def loop_fixture(dest: Path, seed: str, remote: str = "git@example.com:team/stats.git"):
    if dest.exists():
        shutil.rmtree(dest)
    (dest / "tools").mkdir(parents=True)
    (dest / "stats.py").write_text('"""Small statistics helpers."""\n\n\ndef mean(values):\n    if not values:\n        raise ValueError("mean of empty list")\n    return sum(values) / len(values)\n')
    (dest / "test_stats.py").write_text(
        "import json\nimport unittest\nfrom pathlib import Path\n\nfrom stats import mean\n\n"
        "GOLDEN = json.loads((Path(__file__).parent / '.fixtures' / 'golden.json').read_text())\n"
        f"assert GOLDEN.get('seed_check') == '{hashlib.sha256(seed.encode()).hexdigest()[:16]}', 'fixtures were generated with the wrong seed'\n\n\n"
        "class MeanTest(unittest.TestCase):\n    def test_golden(self):\n        for case in GOLDEN['mean']:\n            self.assertAlmostEqual(mean(case['in']), case['out'])\n\n\n"
        "if __name__ == '__main__':\n    unittest.main()\n"
    )
    gen = dest / "tools" / "gen-fixtures"
    gen.write_text(
        "#!/usr/bin/env bash\nset -e\ncd \"$(dirname \"$0\")/..\"\n"
        'if [ "$1" != "--seed" ] || [ -z "$2" ]; then echo "usage: gen-fixtures --seed <team seed>" >&2; exit 2; fi\n'
        "mkdir -p .fixtures\n"
        'check=$(printf %s "$2" | sha256sum | cut -c1-16)\n'
        'echo "{\\"seed_check\\": \\"$check\\", \\"mean\\": [{\\"in\\": [1, 2, 3, 4], \\"out\\": 2.5}]}" > .fixtures/golden.json\n'
        "echo 'fixtures written'\n"
    )
    gen.chmod(0o755)
    (dest / "README.md").write_text("# stats\n\nSmall statistics helpers.\n")
    (dest / ".gitignore").write_text(".fixtures/\n")
    subprocess.run(["git", "init", "-q"], cwd=dest)
    # Every session of a round is the same repo (same remote), so they share a project kit.
    subprocess.run(["git", "remote", "add", "origin", remote], cwd=dest)
    subprocess.run(["git", "add", "-A"], cwd=dest)
    subprocess.run(["git", "-c", "user.name=bench", "-c", "user.email=bench@example.com", "commit", "-qm", "init"], cwd=dest)


def check_loop(work: Path, fn: str):
    r = subprocess.run([sys.executable, "-m", "unittest", "-q"], cwd=work, capture_output=True, timeout=60)
    tests = (work / "test_stats.py").read_text()
    return {"tests_pass": r.returncode == 0, f"{fn}_tested": fn in tests, "fixtures_built": (work / ".fixtures" / "golden.json").exists()}


def landmine_hits(home: Path, since: float):
    """Shell results where the fixture landmine went off (missing, unseeded or
    wrongly seeded fixtures). Only command output counts: reading
    test_stats.py or tools/gen-fixtures shows the same strings as source."""
    failed = re.compile(r"FileNotFoundError.*golden\.json|golden\.json.*No such file|AssertionError: fixtures were generated with the wrong seed|^usage: gen-fixtures", re.M)
    hits = 0
    for f in sorted((home / "sessions").glob("*.jsonl")):
        if f.stat().st_mtime < since:
            continue
        shell_ids = set()
        for line in f.read_text().splitlines():
            m = json.loads(line).get("message") or {}
            for t in m.get("tool_calls") or []:
                if t.get("function", {}).get("name") == "shell":
                    shell_ids.add(t.get("id"))
            if m.get("role") == "tool" and m.get("tool_call_id") in shell_ids and failed.search(str(m.get("content") or "")):
                hits += 1
    return hits


def suite_loop(args):
    names = args.harness
    results = {n: [] for n in names + ["control"]}

    def session(h, ep, base, i, hint, seed):
        fn, prompt = LOOP_TASKS[i]
        work = base / f"work{i}"
        loop_fixture(work, seed, f"git@example.com:team/stats-{base.parent.name}-{base.name}.git")
        before = len(ep.records())
        t0 = time.time()
        run([str(CONSTRUCT), "run", "--json", prompt], h.full_env(), work, timeout=args.timeout)
        if hint:
            run([str(CONSTRUCT), "run", "--json", "-c", LOOP_HINT.format(seed=seed)], h.full_env(), work, timeout=args.timeout)
        wait_for_learners()
        recs = ep.records()[before:]
        routes, calls = session_facts(h.home, t0)
        route = routes[0] if routes else {}
        return {
            "landmine_hits": landmine_hits(h.home, t0),
            "task": fn,
            "checks": check_loop(work, fn),
            "requests": sum(1 for x in recs if x.get("n_tools")),
            "learn_requests": sum(1 for x in recs if not x.get("n_tools")) ,
            "prompt_tokens": sum((x.get("usage") or {}).get("prompt_tokens") or 0 for x in recs),
            "suggested": [s[0] for s in route.get("skills", [])],
            "skill_loaded": any(c.get("name") == "skill" for c in calls),
            "outside_workdir": sum(1 for c in calls if "~" in c["args"] or "/home/" in c["args"]),
        }

    for r in range(args.rounds):
        for n in order(names, r):
            base = WORK / "loop" / n / f"r{r}"
            if base.exists():
                shutil.rmtree(base)
            h = {**HARNESSES, **VARIANTS}[n](base / "home")
            ep = Endpoint(base / "ep", upstream=args.upstream)
            h.configure(ep.url, args.model)
            seed = f"{random.getrandbits(48):012x}"
            s = [session(h, ep, base, 0, True, seed), session(h, ep, base, 1, True, seed), session(h, ep, base, 2, False, seed)]
            ledger = (h.home / "learn" / "ledger.jsonl")
            learned = {
                "ledger": [json.loads(l)["problem_class"] for l in ledger.read_text().splitlines()] if ledger.exists() else [],
                "skills": sorted(p.name for p in (h.home / "skills").iterdir() if p.is_dir() and not p.name.startswith(".")) if (h.home / "skills").exists() else [],
                "memory": (h.home / "memory" / "MEMORY.md").read_text() if (h.home / "memory" / "MEMORY.md").exists() else "",
                "learn_log": (h.home / "learn.log").read_text()[-3000:] if (h.home / "learn.log").exists() else "",
            }
            ep.close()
            assert_no_keys(base)
            results[n].append({"sessions": s, "learned": learned})
            print(f"round {r} {n:14} s3 pass={s[2]['checks']['tests_pass']} hits={[x['landmine_hits'] for x in s]} suggested={s[2]['suggested']} loaded={s[2]['skill_loaded']} "
                  f"s3 requests={s[2]['requests']} | s1/s2 pass={[x['checks']['tests_pass'] for x in s[:2]]} | ledger={learned['ledger']} skills={learned['skills']} memory={learned['memory'][:120]!r}")
        # control: session 3 with nothing learned
        base = WORK / "loop" / "control" / f"r{r}"
        if base.exists():
            shutil.rmtree(base)
        h = Construct(base / "home")
        ep = Endpoint(base / "ep", upstream=args.upstream)
        h.configure(ep.url, args.model)
        c = session(h, ep, base, 2, False, f"{random.getrandbits(48):012x}")
        ep.close()
        results["control"].append({"sessions": [c]})
        print(f"round {r} control        s3 pass={c['checks']['tests_pass']} hits={c['landmine_hits']} s3 requests={c['requests']}")
    save("loop", {"model": args.model, "rounds": args.rounds, "hint": LOOP_HINT, "tasks": LOOP_TASKS, "raw": results})

def preflight(names):
    """Stop early, with an install hint, if a requested competitor harness or
    its sandbox isn't available — rather than failing deep inside a round."""
    bins = {"hermes": HERMES, "opencode": OPENCODE}
    missing = []
    for n in names:
        if n in bins and (bins[n] is None or not bins[n].exists()):
            missing.append(f"  {n}: not installed — {INSTALL_HINTS[n]}")
    if missing:
        sys.exit("missing competitor harness(es):\n" + "\n".join(missing))
    competitors = [n for n in names if n in bins]
    if competitors and not BWRAP and os.environ.get("BENCH_ALLOW_UNSANDBOXED") != "1":
        sys.exit(
            f"bwrap is required to sandbox {', '.join(competitors)} but was not found: "
            "install bubblewrap (e.g. `sudo pacman -S bubblewrap` or `apt install bubblewrap`)."
        )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("suite", choices=["size", "frame", "tools", "memory", "task", "routing", "loop"])
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
    preflight(args.harness)
    {"size": suite_size, "frame": suite_frame, "tools": suite_tools, "memory": suite_memory, "task": suite_task, "routing": suite_routing, "loop": suite_loop}[args.suite](args)


if __name__ == "__main__":
    main()
