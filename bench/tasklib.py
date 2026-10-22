"""Bench tasks as directories, so adding one never means editing bench.py.

    bench/tasks/<name>/
    ├── task.toml   turns, timeout_s, changes, [vars], [budget]   (see below)
    ├── fixture/    copied into the work dir and committed to a fresh git repo
    ├── home/       optional: copied into the harness home (e.g. a skill)
    ├── hooks/      optional: after-turn-N.sh runs in the work dir after turn
    │               N, for a world that changes between turns
    ├── expected/   optional: the files a correct run leaves, for check.sh
    └── check.sh    the scorer: runs in the work dir after the last turn and
                    prints one JSON object of named booleans. It never reads
                    the transcript, and the agent never sees it.

task.toml keys:
    turns       prompts, one per turn, all in one session
    timeout_s   per turn
    changes     globs of paths the task is expected to change; any other
                change in the work dir is reported as unintended
    [vars]      per-run values: name = template, filled into fixture/ and
                home/ as {{name}}. Templates: {hexN} is N random hex digits,
                {sha256:other:N} the first N hex of sha256 of an earlier var.
    [budget]    max_requests, max_prompt_tokens: past either, the run is
                stopped and scored over_budget instead of running to timeout.
    window      the model's context window in tokens: the endpoint rejects a
                longer prompt as a provider would, and construct is told the
                same window (the compaction suite, bench/compaction.py)
"""

import fnmatch
import hashlib
import json
import os
import random
import re
import shutil
import subprocess
import sys
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

TASKS = Path(__file__).resolve().parent / "tasks"

# Never counted as unintended: what running a project's tests leaves behind.
IGNORED = ["__pycache__/*", "*/__pycache__/*", "*.pyc", ".pytest_cache/*"]


@dataclass
class Task:
    name: str
    dir: Path
    spec: dict = field(repr=False)

    @property
    def turns(self) -> list:
        return self.spec.get("turns", [])

    @property
    def timeout_s(self) -> int:
        return self.spec.get("timeout_s", 900)

    @property
    def changes(self) -> list:
        return self.spec.get("changes", [])

    @property
    def window(self):
        return self.spec.get("window")

    @property
    def budget(self) -> dict:
        return self.spec.get("budget", {})


def names() -> list:
    return sorted(p.parent.name for p in TASKS.glob("*/task.toml"))


def load(name: str) -> Task:
    d = TASKS / name
    if not (d / "task.toml").exists():
        sys.exit(f"no task {name!r}: expected {d / 'task.toml'} (have: {', '.join(names())})")
    return Task(name, d, tomllib.loads((d / "task.toml").read_text()))


def make_vars(task: Task, rng=random) -> dict:
    out = {}
    for k, template in task.spec.get("vars", {}).items():

        def sub(m):
            kind, *rest = m.group(1).split(":")
            if re.fullmatch(r"hex\d+", kind):
                n = int(kind[3:])
                return f"{rng.getrandbits(4 * n):0{n}x}"
            if kind == "sha256":
                return hashlib.sha256(out[rest[0]].encode()).hexdigest()[: int(rest[1])]
            raise ValueError(f"{task.name}: unknown var template {m.group(0)}")

        out[k] = re.sub(r"\{([^{}]+)\}", sub, template)
    return out


def fill(text: str, vars: dict) -> str:
    for k, v in vars.items():
        text = text.replace("{{" + k + "}}", v)
    return text


def _copy_filled(src: Path, dest: Path, vars: dict):
    shutil.copytree(src, dest, dirs_exist_ok=True)  # copy2: keeps the executable bit
    for p in dest.rglob("*"):
        if p.is_file() and not p.is_symlink():
            try:
                t = p.read_text()
            except UnicodeDecodeError:
                continue
            if "{{" in t:
                p.write_text(fill(t, vars))


def materialize(task: Task, dest: Path, vars: dict, remote=None):
    """A fresh work dir: the fixture, filled, committed to a new git repo."""
    if dest.exists():
        shutil.rmtree(dest)
    _copy_filled(task.dir / "fixture", dest, vars)
    git = lambda *a: subprocess.run(["git", *a], cwd=dest, check=True)
    git("init", "-q")
    if remote:
        git("remote", "add", "origin", remote)
    git("add", "-A")
    git("-c", "user.name=bench", "-c", "user.email=bench@example.com", "commit", "-qm", "init")


def seed_home(task: Task, home: Path, vars: dict):
    if (task.dir / "home").exists():
        _copy_filled(task.dir / "home", home, vars)


def after_turn(task: Task, n: int, work: Path):
    """Run hooks/after-turn-<n>.sh in the work dir, if the task has one."""
    hook = task.dir / "hooks" / f"after-turn-{n}.sh"
    if hook.exists():
        subprocess.run(["bash", str(hook)], cwd=work, check=True, capture_output=True, timeout=60)


def check(task: Task, work: Path, env=None) -> dict:
    """Run check.sh; its last stdout line is the JSON verdict. A scorer that
    crashes scores as one failed check rather than stopping the bench."""
    e = {**os.environ, "BENCH_PYTHON": sys.executable, **(env or {})}
    r = subprocess.run(["bash", str(task.dir / "check.sh")], cwd=work, env=e, capture_output=True, text=True, timeout=300)
    lines = r.stdout.strip().splitlines()
    try:
        out = json.loads(lines[-1])
        assert isinstance(out, dict) and all(isinstance(v, bool) for v in out.values())
        return out
    except (IndexError, ValueError, AssertionError):
        print(f"  {task.name}: check.sh gave no verdict (rc={r.returncode}): {(r.stderr or r.stdout)[-500:]}", file=sys.stderr)
        return {"check_ran": False}


def changed_paths(work: Path) -> list:
    """Paths that differ from the fixture commit, tracked or not."""
    r = subprocess.run(["git", "status", "--porcelain=v1", "-z", "-uall"], cwd=work, capture_output=True, check=True)
    entries = r.stdout.decode(errors="replace").split("\0")
    out, i = [], 0
    while i < len(entries):
        e = entries[i]
        i += 1
        if len(e) < 4:
            continue
        out.append(e[3:])
        if e[0] in "RC":  # a rename's source path follows as its own entry
            out.append(entries[i])
            i += 1
    return sorted(set(out))


def unintended(task: Task, work: Path) -> list:
    allowed = task.changes + IGNORED
    return [p for p in changed_paths(work) if not any(fnmatch.fnmatch(p, g) for g in allowed)]


class Budget:
    """Polled while a turn runs: returns why the run should stop, or None."""

    def __init__(self, task: Task, endpoint, since: int = 0):
        self.max_requests = task.budget.get("max_requests")
        self.max_prompt_tokens = task.budget.get("max_prompt_tokens")
        self.endpoint = endpoint
        self.since = since

    def __call__(self):
        recs = self.endpoint.records()[self.since :]
        if self.max_requests and len(recs) > self.max_requests:
            return f"over_budget: {len(recs)} requests > {self.max_requests}"
        tokens = sum((r.get("usage") or {}).get("prompt_tokens") or 0 for r in recs)
        if self.max_prompt_tokens and tokens > self.max_prompt_tokens:
            return f"over_budget: {tokens} prompt tokens > {self.max_prompt_tokens}"
        return None
