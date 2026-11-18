#!/usr/bin/env python3
"""Run Construct or mini-SWE-agent on selected, pinned SWE-bench task images."""

import argparse
from concurrent.futures import ThreadPoolExecutor
import contextlib
import dataclasses
from decimal import Decimal
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import re
import shutil
import subprocess
import tarfile
import tempfile
import time
import uuid

import swebench
import swebench_mini
import swebench_orca
import swebench_proxy
import swebench_relay

BENCH = Path(__file__).resolve().parent


def config(model, limits):
    return f"""base_url = "http://127.0.0.1:{swebench_relay.PORT}/v1"
model = {json.dumps(model)}
context_window = {limits.context}
max_tokens = {limits.output}
max_steps = {limits.requests + 1}
temperature = 0.0

[provider]
dialect = "openai"
reasoning = "echo"
first_byte_timeout = {limits.seconds}

[learn]
enabled = false

[sandbox]
mode = "workspace"

[tools]
disabled = ["web_search", "web_fetch"]
"""


def images(manifest, data, evaluator, tasks, python, pinned):
    swebench.check_checkout(evaluator, data["evaluator"]["revision"])
    swebench.check_checkout(tasks, data["tasks"]["revision"])
    if set(pinned) != set(data["instance_ids"]):
        raise ValueError("supply an immutable image ID for each selected task")
    program = swebench.TASK_CHECK + """
print(json.dumps({row['instance_id']: row['image'] for row in actual}))
"""
    tags = json.loads(subprocess.check_output(
        [str(python), "-c", program, str(manifest.parent / "agent/tasks.jsonl"), str(tasks)],
        env={**os.environ, "PYTHONPATH": str(evaluator)}, text=True))
    result = {}
    for task, tag in tags.items():
        image, = json.loads(subprocess.check_output(["docker", "image", "inspect", tag]))
        if image["Id"] != pinned[task] or not re.fullmatch(r"sha256:[0-9a-f]{64}", pinned[task]):
            raise ValueError(f"{task}: local {tag} does not match its pinned Docker image ID")
        result[task] = {"tag": tag, "id": image["Id"], "repo_digests": image.get("RepoDigests") or [],
                        "platform": f"{image['Os']}/{image['Architecture']}"}
    return result


@contextlib.contextmanager
def recording_proxy(binary, script, work, paid):
    port_file = work / "port"
    mode_args = paid.proxy_args() if paid else ["--script", str(script)]
    with (work / "proxy.stderr").open("w") as errors, subprocess.Popen(
        [str(binary), "serve", *mode_args, "--port-file", str(port_file),
         "--log", str(work / "requests.jsonl"), "--dump", str(work / "requests")],
        env=paid.env if paid else {}, stdout=subprocess.DEVNULL, stderr=errors,
    ) as process:
        try:
            for _ in range(200):
                if port_file.exists() and (port := port_file.read_text().strip()):
                    yield int(port)
                    break
                if process.poll() is not None:
                    raise ValueError(f"recording proxy exited; see {work / 'proxy.stderr'}")
                time.sleep(0.025)
            else:
                raise TimeoutError("recording proxy startup timed out")
        finally:
            if process.poll() is None:
                process.terminate()
                try:
                    process.wait(timeout=3)
                except subprocess.TimeoutExpired:
                    process.kill()


def copy_artifacts(container, source, destination, allowed):
    # Keep only regular files with expected names. Do not extract agent-made
    # archives, repository metadata, symlinks, or arbitrary paths on the host.
    copied = set()
    with subprocess.Popen(["docker", "cp", f"{container}:{source}", "-"],
                          stdout=subprocess.PIPE, stderr=subprocess.DEVNULL) as process:
        try:
            with tarfile.open(fileobj=process.stdout, mode="r|*") as archive:
                for item in archive:
                    path = PurePosixPath(item.name)
                    if not item.isfile() or not allowed(path):
                        continue
                    if path in copied or item.size > 64 << 20:
                        raise ValueError(f"invalid container artifact: {path}")
                    target = destination.joinpath(*path.parts)
                    target.parent.mkdir(parents=True, exist_ok=True)
                    with target.open("xb") as output:
                        shutil.copyfileobj(archive.extractfile(item), output)
                    copied.add(path)
        except tarfile.ReadError:
            if process.wait() == 0:
                raise
        finally:
            if process.poll() is None:
                process.terminate()
    return copied


def is_journal(path):
    parts = path.parts
    return (len(parts) == 3 and parts[0] == "projects"
            and re.fullmatch(r"[a-zA-Z0-9_-]+", parts[1])
            and parts[2] in ("journal.sqlite", "journal.sqlite-wal", "journal.sqlite-shm"))


def container_run(container, work, row, image, socket_path, inputs, budget, mini_runtime):
    mounts = [f"type=bind,src={inputs},dst=/bench,readonly",
              f"type=bind,src={socket_path},dst={swebench_relay.SOCKET},readonly"]
    if mini_runtime:
        mounts.append(f"type=bind,src={mini_runtime},dst=/mini-runtime,readonly")
    command = ["docker", "create", "--name", container, "--pull", "never", "--network", "none", "--init",
               "--cap-drop", "ALL", "--security-opt", "no-new-privileges", "--pids-limit", "256",
               *[arg for mount in mounts for arg in ("--mount", mount)],
               "--entrypoint", "/bin/bash", image["id"],
               "/bench/swebench_container.sh", row["base_commit"], row["repo"]]
    subprocess.run(command, check=True, stdout=subprocess.DEVNULL)
    with (
        (work / "events.jsonl").open("w") as out,
        (work / "container.stderr").open("w") as err,
        subprocess.Popen(["docker", "start", "--attach", container], stdout=out, stderr=err) as process,
    ):
        try:
            process.wait(timeout=max(0.01, budget.deadline - time.monotonic()))
        except subprocess.TimeoutExpired:
            budget.stop = budget.stop or "time_limit"
            subprocess.run(["docker", "kill", "--signal", "INT", container],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                subprocess.run(["docker", "kill", container], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    state = subprocess.check_output(["docker", "inspect", "--format", "{{json .State}}", container], text=True)
    return json.loads(state)


def attempt(row, image, model, budget, binary, mini_runtime, dev, script, paid, work, evidence):
    work.mkdir(mode=0o700)
    container = f"construct-swe-{uuid.uuid4().hex}"
    inputs = work / "inputs"
    inputs.mkdir()
    for filename in ("swebench.py", "swebench_container.sh", "swebench_relay.py"):
        shutil.copyfile(BENCH / filename, inputs / filename)
    if mini_runtime:
        shutil.copyfile(BENCH / "swebench_mini.py", inputs / "swebench_mini.py")
        swebench.write_json(inputs / "mini-config.json", {
            "model": model, "endpoint": f"http://127.0.0.1:{swebench_relay.PORT}/v1/chat/completions",
            "max_tokens": budget.limits.output, "requests": budget.limits.requests, "seconds": budget.limits.seconds,
        })
    else:
        shutil.copyfile(binary, inputs / "construct")
        (inputs / "construct").chmod(0o555)
        (inputs / "config.toml").write_text(config(model, budget.limits))
    if script:
        shutil.copyfile(script, work / "script.json")
        evidence["script_sha256"] = swebench.sha256(work / "script.json")
    (inputs / "issue.txt").write_text(row["problem_statement"])
    evidence.update({"container": container, "inputs": {p.name: swebench.sha256(p) for p in inputs.iterdir()}})
    swebench.write_json(work / "attempt.json", evidence)
    try:
        with (
            tempfile.TemporaryDirectory(prefix="construct-swe-") as transport,
            recording_proxy(dev, work / "script.json", work, paid) as port,
        ):
            socket_path = Path(transport) / "model.sock"
            with swebench_proxy.guard(socket_path, port, model, budget, provider=paid, session_id=container):
                state = container_run(container, work, row, image, socket_path, inputs, budget, mini_runtime)
                copy_artifacts(container, "/export", work, lambda p: p.parts in {
                    ("export", "agent.patch"), ("export", "agent.exit"), ("export", "construct.version"),
                    ("export", "mini.traj.json")})
                journals = set() if mini_runtime else copy_artifacts(container, "/state/construct/projects", work, is_journal)
        patch_file = work / "export/agent.patch"
        status = work / "export/agent.exit"
        evidence.update({"container_exit": state["ExitCode"], "oom_killed": state["OOMKilled"],
                         "agent_exit": int(status.read_text()) if status.is_file() else None,
                         "journals": sorted(str(path) for path in journals),
                         "patch_sha256": swebench.sha256(patch_file) if patch_file.is_file() else None})
        if mini_runtime:
            trajectory = work / "export/mini.traj.json"
            if trajectory.is_file():
                evidence["trajectory_sha256"] = swebench.sha256(trajectory)
                info = json.loads(trajectory.read_text()).get("info", {})
                evidence["mini_exit_status"] = info.get("exit_status")
        if state["ExitCode"] == 0 and (
            not patch_file.is_file() or not status.is_file() or not (journals or "trajectory_sha256" in evidence)
        ):
            raise ValueError("task container exited without a patch, status, or trajectory")
        return patch_file.read_text() if patch_file.is_file() else ""
    finally:
        evidence.update({"budget_stop": budget.stop, "requests": budget.requests, "tokens": budget.tokens,
                         "usd": str(budget.usd)})
        swebench.write_json(work / "attempt.json", evidence)
        subprocess.run(["docker", "rm", "--force", container], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def runtime_digest(runtime):
    """Fingerprint portable Python, pinned package source and installed wheels."""
    if (runtime / "mini-revision").read_text().strip() != swebench_mini.MINI_REVISION:
        raise ValueError("mini-SWE-agent checkout must match the pinned revision")
    if not (runtime / "python/bin/python3").is_file() or not (runtime / "site/minisweagent").is_dir():
        raise ValueError("mini runtime requires python/bin/python3 and site/minisweagent")
    entries = []
    for path in sorted(runtime.rglob("*")):
        if path.is_symlink():
            entries.append((str(path.relative_to(runtime)), "symlink", os.readlink(path)))
        elif path.is_file():
            entries.append((str(path.relative_to(runtime)), "file", swebench.sha256(path)))
    return hashlib.sha256(json.dumps(entries).encode()).hexdigest()


def run(args):
    if args.workers < 1:
        raise ValueError("workers must be positive")
    manifest = args.manifest.resolve()
    data, rows = swebench.load(manifest)
    selected = images(manifest, data, args.evaluator.resolve(), args.task_repo.resolve(), args.python,
                      json.loads(args.images.read_text()))
    limits = swebench_proxy.Limits(args.requests, args.tokens, args.seconds, args.context, args.output_tokens)
    dev = args.construct_dev.resolve()
    binary = args.construct.resolve() if args.construct else None
    mini_runtime = args.mini_runtime.resolve() if args.mini_runtime else None
    paid = None
    spend = swebench_proxy.Spend(Decimal(0), Decimal(0))
    if args.upstream:
        if not args.spend_usd or not Decimal("0") < args.spend_usd <= Decimal("2"):
            raise ValueError("each paid arm requires an explicit spend cap of at most $2")
        if args.wallet_usd is None or args.wallet_usd - args.spend_usd < Decimal("5"):
            raise ValueError("paid pilots require a starting wallet balance that preserves $5 after the cap")
        paid = swebench_orca.Orca(dev, args.upstream, data["model"])
        spend = swebench_proxy.Spend(args.spend_usd, swebench_orca.RESERVATION)
    elif args.spend_usd is not None or args.wallet_usd is not None:
        raise ValueError("wallet and spend limits apply to paid pilots")
    output = args.output.resolve()
    output.mkdir(mode=0o700)
    source = {"revision": swebench.git(BENCH, "rev-parse", "HEAD"),
              "dirty": bool(swebench.git(BENCH, "status", "--porcelain")),
              "runner_sha256": swebench.sha256(__file__),
              "budget_proxy_sha256": swebench.sha256(swebench_proxy.__file__),
              "recording_proxy_sha256": swebench.sha256(dev)}
    summary = {"mode": "paid" if paid else "scripted", "manifest_sha256": swebench.sha256(manifest), "source": source,
               "harness": "mini-swe-agent" if mini_runtime else "construct",
               "model": data["model"], "limits": dataclasses.asdict(limits) |
               {"usd": str(spend.limit), "request_usd": str(spend.per_request)},
               "workers": args.workers, "attempts": []}
    if mini_runtime:
        source.update({"mini_version": swebench_mini.MINI_VERSION, "mini_revision": swebench_mini.MINI_REVISION,
                       "mini_runtime_sha256": runtime_digest(mini_runtime)})
    if paid:
        summary.update({"upstream": paid.upstream, "reasoning_effort": swebench_orca.EFFORT,
                        "pricing": swebench_orca.PRICING, "wallet_reserve_usd": "5"})
        source["receipt_adapter_sha256"] = swebench.sha256(swebench_orca.__file__)
    swebench.write_json(output / "run.json", summary)
    def run_task(row):
        task = row["instance_id"]
        evidence = {"instance_id": task, "base_commit": row["base_commit"], "image": selected[task]}
        if not spend.available():
            evidence["not_started"] = spend.snapshot()["spend_stop"] or "spend_limit"
            return evidence, ""
        work = output / task
        budget = swebench_proxy.Budget(limits, work / "budget.jsonl", spend)
        try:
            patch = attempt(row, selected[task], data["model"], budget, binary, mini_runtime, dev, args.script,
                            paid, work, evidence)
        except (OSError, ValueError, subprocess.CalledProcessError, tarfile.TarError) as error:
            evidence["runner_error"] = str(error)
            patch = ""
        return evidence, patch

    patches = {}
    with ThreadPoolExecutor(max_workers=args.workers) as pool:
        for evidence, patch in pool.map(run_task, rows):
            summary["attempts"].append(evidence)
            patches[evidence["instance_id"]] = patch
            summary.update(spend.snapshot())
            swebench.write_json(output / "run.json", summary)
    swebench.write_predictions(data, patches, output / "predictions.jsonl")
    summary["predictions_sha256"] = swebench.sha256(output / "predictions.jsonl")
    swebench.write_json(output / "run.json", summary)
    print(output / "run.json")
    return int(any("runner_error" in a or a.get("container_exit") != 0 or a.get("agent_exit") != 0
                   for a in summary["attempts"]))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("manifest", "evaluator", "task-repo", "images", "construct-dev", "output"):
        parser.add_argument(f"--{name}", required=True, type=Path)
    harness = parser.add_mutually_exclusive_group(required=True)
    harness.add_argument("--construct", type=Path, help="Linux Construct executable")
    harness.add_argument("--mini-runtime", type=Path, help="pinned mini-SWE-agent runtime directory")
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--script", type=Path, help="drive the task with a local script")
    mode.add_argument("--upstream", help="paid endpoint with a supported receipt adapter")
    parser.add_argument("--spend-usd", type=dollars, help="maximum total settled spend plus reservations")
    parser.add_argument("--wallet-usd", type=dollars, help="confirmed starting wallet balance")
    parser.add_argument("--python", required=True, type=Path, help="pinned evaluator environment's Python")
    for name, default in (("workers", 1), ("requests", 12), ("tokens", 100_000), ("seconds", 90), ("context", 16_384),
                          ("output-tokens", 2048)):
        parser.add_argument(f"--{name}", type=int, default=default)
    args = parser.parse_args()
    try:
        return run(args)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"{error}\n")


def dollars(value):
    try:
        amount = Decimal(value)
    except ArithmeticError:
        raise argparse.ArgumentTypeError("expected a USD amount") from None
    if not amount.is_finite() or amount < 0:
        raise argparse.ArgumentTypeError("expected a nonnegative, finite USD amount")
    return amount


if __name__ == "__main__":
    raise SystemExit(main())
