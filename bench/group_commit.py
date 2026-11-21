#!/usr/bin/env python3
"""Turn latency and durable-event count for N parallel tool calls (issue #248).

One `construct acp` process sends scripted turns into one session against
`construct-dev serve`. Each turn is one assistant reply carrying N parallel
`read` calls, then a short answer. Learning and compaction are off, so the
only journal rows are the ones that turn needs.

For each N the bench reports prompt round trip (p50/p95), journal rows added
per turn, and fsyncs per turn. ADR 0034 makes each durable row one WAL fsync,
so rows == fsyncs by construction; with `--strace` on Linux the two
syscalls are counted directly instead and the source is recorded in the JSON.

    python3 bench/group_commit.py [--n 1 4 16] [--turns 3] [--reps 5]
        [--label local] [--strace] [--json out.json]

Build first: cargo build --release -p construct && cargo build -p construct-dev.
BENCH_CONSTRUCT / BENCH_DEV override either binary path. Run the same command
inside a Deck guest with --label guest to get the second row of the measurement.
Costs nothing (mock model, tiny files).
"""

import argparse
import json
import os
import shutil
import sqlite3
import statistics
import subprocess
import sys
import tempfile
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
CONSTRUCT = Path(os.environ.get("BENCH_CONSTRUCT", ROOT / "target/release/construct"))
DEV = Path(os.environ.get("BENCH_DEV", ROOT / "target/debug/construct-dev"))
WINDOW = 65536  # no compaction in a short session


def env_for(home: Path, port: int) -> dict:
    env = {k: v for k, v in os.environ.items()
           if not k.endswith("_API_KEY") and not k.startswith("CONSTRUCT_")}
    env.update(CONSTRUCT_HOME=str(home), CONSTRUCT_BASE_URL=f"http://127.0.0.1:{port}/v1",
               OPENAI_API_KEY="x")
    return env


def script_for(n: int, turns: int) -> dict:
    """`turns` replies of N parallel reads followed by a final short answer."""
    steps = []
    for _ in range(turns):
        steps.append({"calls": [{"call": "read", "args": {"path": "data.txt"}} for _ in range(n)]})
        steps.append({"text": "done"})
    return {"steps": steps, "side": ["summary"] * (4 * turns)}


def serve(directory: Path, script: dict) -> tuple[subprocess.Popen, int]:
    (directory / "script.json").write_text(json.dumps(script))
    port_file = directory / "port"
    port_file.unlink(missing_ok=True)  # a previous rep's port must not be reused
    proc = subprocess.Popen(
        [DEV, "serve", "--script", directory / "script.json", "--window", str(WINDOW),
         "--chunk-ms", "0", "--port-file", port_file],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    for _ in range(500):
        if port_file.exists() and port_file.read_text().strip():
            return proc, int(port_file.read_text())
        time.sleep(0.01)
    proc.kill()
    raise SystemExit("construct-dev serve did not start")


class Acp:
    def __init__(self, env: dict, cwd: Path, trace: Path | None = None):
        cmd = [CONSTRUCT, "acp"]
        self.trace = None
        if trace is not None and shutil.which("strace"):
            self.trace = trace
            cmd = ["strace", "-f", "-e", "trace=fsync,fdatasync", "-o", str(trace), *cmd]
        self.p = subprocess.Popen(cmd, cwd=cwd, env=env, stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                  text=True, bufsize=1)
        self.next = 0

    def call(self, method: str, params: dict) -> dict:
        assert self.p.stdin is not None and self.p.stdout is not None
        self.next += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.next,
                                       "method": method, "params": params}) + "\n")
        self.p.stdin.flush()
        while True:
            line = self.p.stdout.readline()
            if not line:
                raise SystemExit(f"construct acp closed during {method}")
            msg = json.loads(line)
            if msg.get("id") == self.next and "method" not in msg:
                if "error" in msg:
                    raise SystemExit(f"{method}: {msg['error']}")
                return msg["result"]

    def close(self) -> None:
        assert self.p.stdin is not None
        self.p.stdin.close()
        self.p.wait(timeout=60)

    def fsyncs(self) -> int | None:
        if self.trace is None or not self.trace.exists():
            return None
        # -f prefixes each line with a pid; count successful returns only.
        return sum(1 for line in self.trace.read_text(errors="replace").splitlines()
                   if ("fsync(" in line or "fdatasync(" in line) and line.rstrip().endswith("= 0"))


def counts(home: Path, sid: str) -> dict:
    """Row types of `sid` over every project journal, as {type: count}."""
    out = {}
    for f in sorted((home / "projects").glob("*/journal.sqlite")):
        db = sqlite3.connect(f"file:{f}?mode=ro", uri=True)
        try:
            for ty, n in db.execute("SELECT type, count(*) FROM events WHERE session_id = ? GROUP BY type", (sid,)):
                out[ty] = out.get(ty, 0) + n
        finally:
            db.close()
    return out


def fsync_cost_ms(directory: Path, count: int = 200) -> float:
    """Wall time of one WAL FULL commit (one fsync), as ADR 0034 pays it."""
    db = sqlite3.connect(directory / "fsync.sqlite")
    try:
        db.execute("PRAGMA journal_mode=WAL")
        db.execute("PRAGMA synchronous=FULL")
        db.execute("CREATE TABLE IF NOT EXISTS t (x INTEGER)")
        start = time.monotonic()
        for i in range(count):
            db.execute("INSERT INTO t VALUES (?)", (i,))
            db.commit()
        return (time.monotonic() - start) * 1000 / count
    finally:
        db.close()


def summarize(values: list[float]) -> dict:
    """Median and p95 of one metric, rounded for the report."""
    s = sorted(values)
    if not s:
        return {"p50": None, "p95": None}
    p95 = s[min(len(s) - 1, int(0.95 * (len(s) - 1) + 0.5))]
    return {"p50": round(statistics.median(s), 2), "p95": round(p95, 2)}


def run(n: int, turns: int, reps: int, label: str, strace: bool) -> dict:
    directory = Path(tempfile.mkdtemp(prefix=f"group-commit-{n}-"))
    try:
        home, work = directory / "home", directory / "work"
        home.mkdir()
        work.mkdir()
        (work / "data.txt").write_text("".join(f"line {i}: ordinary content\n" for i in range(60)))
        (home / "config.toml").write_text(
            f"context_window = {WINDOW}\n[router]\nmode = 'local'\n[learn]\nenabled = false\n")
        samples, fsync_samples, rows_per_turn, tools_per_turn, by_type = [], [], [], [], {}
        # The mock server's script counter is global to the process: a fresh
        # server per rep keeps every rep's turns on the same scripted steps.
        for rep in range(reps):
            server, port = serve(directory, script_for(n, turns))
            try:
                trace = directory / f"strace-{rep}.log" if strace else None
                acp = Acp(env_for(home, port), work, trace)
                acp.call("initialize", {"protocolVersion": 1})
                sid = acp.call("session/new", {"cwd": str(work), "mcpServers": []})["sessionId"]
                timings = []
                for i in range(turns):
                    before = counts(home, sid)
                    start = time.monotonic()
                    acp.call("session/prompt", {"sessionId": sid, "prompt": [
                        {"type": "text", "text": f"turn {i}: read the file"}]})
                    timings.append((time.monotonic() - start) * 1000)
                    after = counts(home, sid)
                    rows_per_turn.append(sum(after.values()) - sum(before.values()))
                    tools_per_turn.append(after.get("tool_end", 0) - before.get("tool_end", 0))
                    for ty in set(before) | set(after):
                        by_type[ty] = by_type.get(ty, 0) + after.get(ty, 0) - before.get(ty, 0)
                acp.close()
                samples.extend(timings)
                measured = acp.fsyncs()
                if measured is not None:
                    fsync_samples.append(measured / turns)
            finally:
                server.kill()
                server.wait()
        journal = next(iter((home / "projects").glob("*/journal.sqlite")), None)
        size = sum(f.stat().st_size for f in journal.parent.glob("journal.sqlite*")) if journal else 0
        sync_ms = statistics.median([fsync_cost_ms(directory) for _ in range(3)])
        timing = summarize(samples)
        median_rows = round(statistics.median(rows_per_turn), 2) if rows_per_turn else 0
        median_tools = round(statistics.median(tools_per_turn), 2) if tools_per_turn else 0
        if median_tools != n:
            raise SystemExit(f"script misaligned: expected {n} tool_end rows per turn, "
                             f"got {median_tools}; rows_by_type={by_type}")
        return {
            "label": label, "parallel_calls": n, "turns": turns, "reps": reps,
            "rows_per_turn": median_rows,
            "tools_per_turn": median_tools,
            "rows_by_type": by_type,
            # ADR 0034: one WAL fsync per durable row. strace replaces the
            # derivation with a direct count when it is available.
            "fsyncs_per_turn": round(statistics.median(fsync_samples), 2) if fsync_samples else median_rows,
            "fsync_source": "strace" if fsync_samples else "adr0034-row-count",
            "journal_bytes": size,
            "fsync_ms": round(sync_ms, 4),
            "fsync_share_pct": round(100 * median_rows * sync_ms / timing["p50"], 1) if timing["p50"] else None,
            "turn_ms": timing,
            "samples_ms": [round(x, 2) for x in samples],
        }
    finally:
        shutil.rmtree(directory, ignore_errors=True)


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--n", type=int, nargs="+", default=[1, 4, 16], help="parallel calls per turn")
    ap.add_argument("--turns", type=int, default=3)
    ap.add_argument("--reps", type=int, default=5)
    ap.add_argument("--label", default="local", help="disk label: local or guest")
    ap.add_argument("--strace", action="store_true", help="count fsync syscalls (Linux only)")
    ap.add_argument("--json", type=Path)
    a = ap.parse_args()
    for binary in (CONSTRUCT, DEV):
        if not binary.exists():
            raise SystemExit(f"missing {binary}: cargo build --release -p construct && cargo build -p construct-dev")
    if a.strace and not shutil.which("strace"):
        print("strace not found; fsyncs fall back to the ADR 0034 row count", file=sys.stderr)
    rows = [run(n, a.turns, a.reps, a.label, a.strace) for n in a.n]
    cols = ["label", "parallel_calls", "tools_per_turn", "turns", "reps", "p50_ms", "p95_ms",
            "rows_per_turn", "fsyncs_per_turn", "fsync_ms", "fsync_share_pct", "journal_bytes"]
    print("  ".join(cols))
    for r in rows:
        r["p50_ms"], r["p95_ms"] = r["turn_ms"]["p50"], r["turn_ms"]["p95"]
        print("  ".join(str(r[c]) for c in cols), flush=True)
    if a.json:
        a.json.write_text(json.dumps({
            "rows": rows,
            "commit": subprocess.run(["git", "rev-parse", "HEAD"], cwd=ROOT,
                                     capture_output=True, text=True).stdout.strip(),
        }, indent=2))
    return 0


if __name__ == "__main__":
    sys.exit(main())
