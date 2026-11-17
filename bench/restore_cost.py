"""What restoring a long session costs, without a model (#246, tier 1).

One `construct acp` process sends N scripted turns into one session (each a
small `read`, then a short answer), against `construct-dev serve --window W`,
so compactions and their summaries happen as they would in a long-lived
coordinator. Then a fresh process restores that session, timed:

- `resume`: `construct --resume <id> run "…"` (restore, recovery check, one
  scripted turn, exit): wall time and peak RSS;
- `load`: `construct acp`, `initialize`, `session/load` until its response:
  wall time and peak RSS (VmHWM) at that point.

    python3 bench/restore_cost.py [--turns 100 1000 10000] [--strategy compact optchat]
        [--reps 3] [--summary-bytes 300] [--json out.json]

Uses `target/release/construct` (build it first) and `target/debug/construct-dev`.
Deterministic apart from timing noise; costs nothing.
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
CONSTRUCT = ROOT / "target/release/construct"
DEV = ROOT / "target/debug/construct-dev"
NOTE = "".join(f"decision {i:03}: keep the coordinator's record of what was agreed\n" for i in range(40))
WINDOW = {"compact": 16384, "optchat": 65536}


def env_for(home: Path, port: int) -> dict:
    env = {k: v for k, v in os.environ.items() if not k.endswith("_API_KEY") and not k.startswith("CONSTRUCT_")}
    env.update(CONSTRUCT_HOME=str(home), CONSTRUCT_BASE_URL=f"http://127.0.0.1:{port}/v1", OPENAI_API_KEY="x")
    return env


def serve(dir: Path, turns: int, window: int, summary: int) -> tuple[subprocess.Popen, int]:
    steps = []
    for _ in range(turns):
        steps += [{"call": "read", "args": {"path": "note.txt"}}, {"text": "noted"}]
    # Summaries as long as a real one, so optchat's view fills its byte budget
    # and stops growing as it would; the mock's default reply is ten bytes.
    line = ("summary of what was decided and why " * 20)[:summary]
    side = [line] * (6 * turns)
    (dir / "script.json").write_text(json.dumps({"steps": steps, "side": side}))
    port_file = dir / "port"
    proc = subprocess.Popen(
        [DEV, "serve", "--script", dir / "script.json", "--window", str(window), "--chunk-ms", "0",
         "--port-file", port_file],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
    )
    for _ in range(500):
        if port_file.exists() and port_file.read_text().strip():
            return proc, int(port_file.read_text())
        time.sleep(0.01)
    raise SystemExit("construct-dev serve did not start")


class Acp:
    def __init__(self, env: dict, cwd: Path):
        self.p = subprocess.Popen([CONSTRUCT, "acp"], cwd=cwd, env=env, stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, bufsize=1)
        self.next = 0

    def call(self, method: str, params: dict) -> dict:
        self.next += 1
        self.p.stdin.write(json.dumps({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params}) + "\n")
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

    def hwm_kib(self) -> int:
        for line in Path(f"/proc/{self.p.pid}/status").read_text().splitlines():
            if line.startswith("VmHWM:"):
                return int(line.split()[1])
        return 0

    def close(self):
        self.p.stdin.close()
        self.p.wait(timeout=60)


def record(home: Path, work: Path, port: int, turns: int) -> tuple[str, float]:
    acp = Acp(env_for(home, port), work)
    acp.call("initialize", {"protocolVersion": 1})
    sid = acp.call("session/new", {"cwd": str(work), "mcpServers": []})["sessionId"]
    start = time.monotonic()
    for i in range(turns):
        acp.call("session/prompt", {"sessionId": sid, "prompt": [{"type": "text", "text": f"note decision {i}"}]})
    elapsed = time.monotonic() - start
    acp.close()
    return sid, elapsed


def journal(home: Path) -> Path:
    return next((home / "projects").glob("*/journal.sqlite"))


def resume(home: Path, work: Path, port: int, sid: str) -> tuple[float, int]:
    start = time.monotonic()
    p = subprocess.Popen([CONSTRUCT, "--resume", sid, "run", "--no-learn", "one more"], cwd=work,
                         env=env_for(home, port), stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    _, status, usage = os.wait4(p.pid, 0)
    elapsed = time.monotonic() - start
    if status != 0:
        raise SystemExit(f"resume exited {status}")
    return elapsed, usage.ru_maxrss


def load(home: Path, work: Path, port: int, sid: str) -> tuple[float, int]:
    start = time.monotonic()
    acp = Acp(env_for(home, port), work)
    acp.call("initialize", {"protocolVersion": 1})
    acp.call("session/load", {"sessionId": sid, "cwd": str(work), "mcpServers": []})
    elapsed = time.monotonic() - start
    hwm = acp.hwm_kib()
    acp.close()
    return elapsed, hwm


def run(turns: int, strategy: str, reps: int, summary: int) -> dict:
    dir = Path(tempfile.mkdtemp(prefix=f"restore-{strategy}-{turns}-"))
    try:
        home, work = dir / "home", dir / "work"
        home.mkdir()
        work.mkdir()
        (work / "note.txt").write_text(NOTE)
        window = WINDOW[strategy]
        (home / "config.toml").write_text(
            f"context_window = {window}\n[router]\nmode = 'local'\n[learn]\nenabled = false\n"
            f"[context]\nstrategy = '{strategy}'\n"
        )
        server, port = serve(dir, turns, window, summary)
        try:
            sid, recorded = record(home, work, port, turns)
            db = journal(home)
            with sqlite3.connect(db) as c:
                rows = c.execute("select count(*) from events where session_id = ?", (sid,)).fetchone()[0]
                cuts = c.execute("select count(*) from events where session_id = ? and type = 'compacted'",
                                 (sid,)).fetchone()[0]
            size = sum(f.stat().st_size for f in db.parent.glob("journal.sqlite*"))
            # Each resume adds a turn; restore a copy so every rep restores the same journal.
            pristine = dir / "pristine.sqlite"
            with sqlite3.connect(db) as c:
                c.execute("VACUUM INTO ?", (str(pristine),))
            samples = {"resume": [], "load": []}
            for _ in range(reps):
                for kind, fn in (("resume", resume), ("load", load)):
                    for f in db.parent.glob("journal.sqlite*"):
                        f.unlink()
                    shutil.copy(pristine, db)
                    samples[kind].append(fn(home, work, port, sid))
        finally:
            server.kill()
            server.wait()
        row = {"strategy": strategy, "turns": turns, "summary_bytes": summary, "rows": rows, "compactions": cuts, "journal_bytes": size,
               "record_s": round(recorded, 2)}
        for kind, got in samples.items():
            row[f"{kind}_ms"] = round(statistics.median(s for s, _ in got) * 1000, 1)
            row[f"{kind}_rss_mib"] = round(statistics.median(r for _, r in got) / 1024, 1)
        return row
    finally:
        shutil.rmtree(dir, ignore_errors=True)


def main():
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("--turns", type=int, nargs="+", default=[100, 1000, 10000])
    ap.add_argument("--strategy", nargs="+", default=["compact", "optchat"], choices=sorted(WINDOW))
    ap.add_argument("--reps", type=int, default=3)
    ap.add_argument("--summary-bytes", type=int, default=300, help="length of each scripted summary line")
    ap.add_argument("--json", type=Path)
    a = ap.parse_args()
    for b in (CONSTRUCT, DEV):
        if not b.exists():
            raise SystemExit(f"missing {b}: cargo build --release -p construct && cargo build -p construct-dev")
    rows = []
    cols = ["strategy", "turns", "rows", "compactions", "journal_bytes", "record_s", "resume_ms", "resume_rss_mib",
            "load_ms", "load_rss_mib"]
    print("  ".join(cols))
    for strategy in a.strategy:
        for turns in a.turns:
            row = run(turns, strategy, a.reps, a.summary_bytes)
            rows.append(row)
            print("  ".join(str(row[c]) for c in cols), flush=True)
    if a.json:
        a.json.write_text(json.dumps({"rows": rows, "commit": subprocess.run(
            ["git", "rev-parse", "HEAD"], cwd=ROOT, capture_output=True, text=True).stdout.strip()}, indent=2))


if __name__ == "__main__":
    sys.exit(main())
