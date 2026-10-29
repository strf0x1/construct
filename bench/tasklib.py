"""Bench tasks as directories, so adding one never means editing bench.py.

    bench/tasks/<name>/
    ├── task.toml   turns, timeout_s, changes, [vars], [budget]   (see below)
    ├── fixture/    copied into the work dir and committed to a fresh git repo
    │               (or, with `git` in task.toml, this repo's tree at a revision)
    ├── home/       optional: copied into the harness home (e.g. a skill)
    ├── outside/    optional: copied to a folder beside the work dir, outside
    │               the sandbox; its path is the var {{outside}}
    ├── hooks/      optional: after-turn-N.sh runs in the work dir after turn
    │               N, for a world that changes between turns
    ├── expected/   optional: the files a correct run leaves, for check.sh
    └── check.sh    the scorer: runs in the work dir after the last turn and
                    prints one JSON object of named booleans. It never reads
                    the transcript, and the agent never sees it. It may write
                    unscored details as JSON to $BENCH_REPORT; they land in
                    the run's result as `report`.

task.toml keys:
    turns       prompts, one per turn, all in one session
    timeout_s   per turn
    changes     globs of paths the task is expected to change; any other
                change in the work dir is reported as unintended
    [vars]      per-run values: name = template, filled into fixture/, home/
                and outside/ (contents and file names) as {{name}}. Templates: {hexN} is N random hex digits,
                {sha256:other:N} the first N hex of sha256 of an earlier var.
    [budget]    max_requests, max_prompt_tokens: past either, the run is
                stopped and scored over_budget instead of running to timeout.
    config      construct only: TOML appended to the harness home's
                config.toml (e.g. an [agents.<name>] table)
    window      the model's context window in tokens: the endpoint rejects a
                longer prompt as a provider would, and construct is told the
                same window (the compaction suite, bench/compaction.py)
    generate    "module:function" in bench/: returns (Tree, expected) and the
                tree is the fixture, built per run (a repo too big to commit)
    git         a revision of this repo: the fixture is its tree (`git
                archive`), with no history, so the agent can't read ahead.
                A warm cargo target for it, if `bench/replay.py warm` made
                one, is reflinked into the work dir (rust-replay-*)
    check_timeout_s  how long check.sh may run (default 300)
    base        another task whose fixture/, check.sh and other files this one
                uses; its own directory then holds only task.toml and overlays
"""

import fnmatch
import hashlib
import importlib
import json
import os
import random
import re
import shutil
import subprocess
import sys
import tempfile
import tomllib
from dataclasses import dataclass, field
from pathlib import Path

TASKS = Path(__file__).resolve().parent / "tasks"
ROOT = TASKS.parent.parent
# Warm cargo targets for `git` fixtures, one per task (bench/replay.py warm).
WARM = Path(os.environ.get("BENCH_CACHE", Path.home() / ".cache" / "construct-bench")) / "warm"

# Never counted as unintended: what running a project's tests leaves behind.
IGNORED = ["__pycache__/*", "*/__pycache__/*", "*.pyc", ".pytest_cache/*"]


@dataclass
class Task:
    name: str
    dir: Path
    spec: dict = field(repr=False)

    @property
    def src(self) -> Path:
        """Where fixture/ and check.sh live: this task, or its `base`."""
        return TASKS / self.spec["base"] if "base" in self.spec else self.dir

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
    def config(self) -> str:
        return self.spec.get("config", "")

    @property
    def window(self):
        return self.spec.get("window")

    @property
    def budget(self) -> dict:
        return self.spec.get("budget", {})

    @property
    def git(self):
        return self.spec.get("git")

    @property
    def check_timeout_s(self) -> int:
        return self.spec.get("check_timeout_s", 300)


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
    for p in sorted(dest.rglob("*{{*"), key=lambda p: -len(p.parts)):
        p.rename(p.with_name(fill(p.name, vars)))
    for p in dest.rglob("*"):
        if p.is_file() and not p.is_symlink():
            try:
                t = p.read_text()
            except UnicodeDecodeError:
                continue
            if "{{" in t:
                p.write_text(fill(t, vars))


def materialize(task: Task, dest: Path, vars: dict, remote=None, warm=True, src=None, overlays=(), remove=()):
    """A fresh work dir: the fixture (or `src`), then each overlay copied over
    it and the `remove` paths deleted, filled, committed to a new git repo."""
    if dest.exists():
        shutil.rmtree(dest)
    if task.spec.get("generate"):
        generate(task.spec["generate"], dest)
    elif task.git:
        export_tree(task.git, dest)
    else:
        _copy_filled(src or task.src / "fixture", dest, vars)
    for o in overlays:
        _copy_filled(o, dest, vars)
    for r in remove:
        (dest / r).unlink()
    git = lambda *a: subprocess.run(["git", *a], cwd=dest, check=True)
    git("init", "-q")
    if remote:
        git("remote", "add", "origin", remote)
    git("add", "-A")
    git("-c", "user.name=bench", "-c", "user.email=bench@example.com", "commit", "-qm", "init")
    if task.git and warm:
        seed_target(task, dest)


def generate(spec: str, dest: Path):
    """The fixture a task builds instead of committing: `module:function` in bench/."""
    here = str(Path(__file__).resolve().parent)
    if here not in sys.path:
        sys.path.insert(0, here)
    module, _, fn = spec.partition(":")
    getattr(importlib.import_module(module), fn)()[0].write(dest)


def has_rev(rev: str) -> bool:
    """Whether this clone has `rev`: a shallow clone lacks a `git` task's past commits."""
    return subprocess.run(["git", "cat-file", "-e", f"{rev}^{{commit}}"], cwd=ROOT, capture_output=True).returncode == 0


def export_tree(rev: str, dest: Path):
    """This repo's tree at `rev`, without its history, at `dest`. Files get
    the time of extraction (-m), not the commit's: a warm target's workspace
    crates must look stale, or cargo reuses test binaries that have another
    tree's path compiled in (`env!("CARGO_MANIFEST_DIR")`)."""
    if not has_rev(rev):
        sys.exit(f"this clone has no commit {rev[:8]}: a `git` task needs the repo's history (git fetch --unshallow)")
    dest.mkdir(parents=True)
    archive = subprocess.run(["git", "archive", "--format=tar", rev], cwd=ROOT, check=True, capture_output=True).stdout
    subprocess.run(["tar", "-x", "-m", "-C", str(dest)], input=archive, check=True)


def seed_target(task: Task, dest: Path):
    """Reflink the task's warm cargo target into the work dir, so the agent's
    first build compiles only the workspace crates. Copy-on-write only: a full
    copy of a target is gigabytes, so on another filesystem the build is cold."""
    warm = WARM / task.name / "target"
    if not warm.exists() or (dest / "target").exists():
        return
    r = subprocess.run(["cp", "-a", "--reflink=always", str(warm), str(dest / "target")], capture_output=True)
    if r.returncode != 0:
        shutil.rmtree(dest / "target", ignore_errors=True)
        print(f"  {task.name}: no reflink from {warm} (put BENCH_WORK on its filesystem); the first build is cold", file=sys.stderr)


def seed_outside(task: Task, dest: Path, vars: dict):
    """The task's outside/ folder, filled, at `dest` (a fresh one)."""
    if dest.exists():
        shutil.rmtree(dest)
    if (task.dir / "outside").exists():
        _copy_filled(task.dir / "outside", dest, vars)


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
    return check_report(task, work, env)[0]


def check_report(task: Task, work: Path, env=None):
    """check() and the details check.sh wrote to $BENCH_REPORT (or None)."""
    with tempfile.TemporaryDirectory() as tmp:
        report = Path(tmp) / "report.json"
        e = {**os.environ, "BENCH_PYTHON": sys.executable, "BENCH_REPORT": str(report), **(env or {})}
        try:
            r = subprocess.run(["bash", str(task.src / "check.sh")], cwd=work, env=e, capture_output=True, text=True, timeout=task.check_timeout_s)
        except subprocess.TimeoutExpired:
            print(f"  {task.name}: check.sh ran past {task.check_timeout_s}s", file=sys.stderr)
            return {"check_ran": False}, None
        try:
            details = json.loads(report.read_text()) if report.exists() else None
        except ValueError:
            details = None
    lines = r.stdout.strip().splitlines()
    try:
        out = json.loads(lines[-1])
        assert isinstance(out, dict) and all(isinstance(v, bool) for v in out.values())
        return out, details
    except (IndexError, ValueError, AssertionError):
        print(f"  {task.name}: check.sh gave no verdict (rc={r.returncode}): {(r.stderr or r.stdout)[-500:]}", file=sys.stderr)
        return {"check_ran": False}, details


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
