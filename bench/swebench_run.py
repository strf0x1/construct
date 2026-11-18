#!/usr/bin/env python3
"""Run a scripted Construct smoke on selected, pinned SWE-bench task images."""

import argparse
import contextlib
import dataclasses
from decimal import Decimal
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
def recording_proxy(binary, script, work):
    port_file = work / "port"
    with (work / "proxy.stderr").open("w") as errors, subprocess.Popen(
        [str(binary), "serve", "--script", str(script), "--port-file", str(port_file),
         "--log", str(work / "requests.jsonl"), "--dump", str(work / "requests")],
        env={}, stdout=subprocess.DEVNULL, stderr=errors,
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


def container_run(container, work, row, image, socket_path, inputs, budget):
    mounts = [f"type=bind,src={inputs},dst=/bench,readonly",
              f"type=bind,src={socket_path},dst={swebench_relay.SOCKET},readonly"]
    command = ["docker", "create", "--name", container, "--pull", "never", "--network", "none", "--init",
               "--cap-drop", "ALL", "--security-opt", "no-new-privileges", "--pids-limit", "256",
               *[arg for mount in mounts for arg in ("--mount", mount)],
               "--entrypoint", "/bin/bash", image["id"],
               "/bench/swebench_container.sh", row["base_commit"], row["repo"]]
    subprocess.run(command, check=True, stdout=subprocess.DEVNULL)
    with (work / "events.jsonl").open("w") as out, (work / "container.stderr").open("w") as err:
        try:
            subprocess.run(["docker", "start", "--attach", container], timeout=max(0.01, budget.deadline - time.monotonic()),
                           stdout=out, stderr=err)
        except subprocess.TimeoutExpired:
            budget.stop = budget.stop or "time_limit"
            subprocess.run(["docker", "kill", container], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    state = subprocess.check_output(["docker", "inspect", "--format", "{{json .State}}", container], text=True)
    return json.loads(state)


def attempt(row, image, model, limits, binary, dev, script, work, evidence):
    work.mkdir(mode=0o700)
    container = f"construct-swe-{uuid.uuid4().hex}"
    inputs = work / "inputs"
    inputs.mkdir()
    for filename in ("swebench.py", "swebench_container.sh", "swebench_relay.py"):
        shutil.copyfile(BENCH / filename, inputs / filename)
    shutil.copyfile(binary, inputs / "construct")
    (inputs / "construct").chmod(0o555)
    shutil.copyfile(script, work / "script.json")
    (inputs / "issue.txt").write_text(row["problem_statement"])
    (inputs / "config.toml").write_text(config(model, limits))
    evidence.update({"container": container, "inputs": {p.name: swebench.sha256(p) for p in inputs.iterdir()},
                     "script_sha256": swebench.sha256(work / "script.json")})
    swebench.write_json(work / "attempt.json", evidence)
    try:
        with (
            tempfile.TemporaryDirectory(prefix="construct-swe-") as transport,
            recording_proxy(dev, work / "script.json", work) as port,
        ):
            socket_path = Path(transport) / "model.sock"
            budget = swebench_proxy.Budget(limits, work / "budget.jsonl", scripted=True)
            with swebench_proxy.guard(socket_path, port, model, budget):
                state = container_run(container, work, row, image, socket_path, inputs, budget)
                copy_artifacts(container, "/export", work, lambda p: p.parts in {
                    ("export", "agent.patch"), ("export", "agent.exit"), ("export", "construct.version")})
                journals = copy_artifacts(container, "/state/construct/projects", work, is_journal)
            evidence.update({"budget_stop": budget.stop, "requests": budget.requests, "tokens": budget.tokens,
                             "usd": str(budget.usd)})
        patch_file = work / "export/agent.patch"
        status = work / "export/agent.exit"
        evidence.update({"container_exit": state["ExitCode"], "oom_killed": state["OOMKilled"],
                         "agent_exit": int(status.read_text()) if status.is_file() else None,
                         "journals": sorted(str(path) for path in journals),
                         "patch_sha256": swebench.sha256(patch_file) if patch_file.is_file() else None})
        if state["ExitCode"] == 0 and (not patch_file.is_file() or not status.is_file() or not journals):
            raise ValueError("task container exited without a patch, agent exit status, or journal")
        return patch_file.read_text() if patch_file.is_file() else ""
    finally:
        swebench.write_json(work / "attempt.json", evidence)
        subprocess.run(["docker", "rm", "--force", container], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def run(args):
    manifest = args.manifest.resolve()
    data, rows = swebench.load(manifest)
    selected = images(manifest, data, args.evaluator.resolve(), args.task_repo.resolve(), args.python,
                      json.loads(args.images.read_text()))
    limits = swebench_proxy.Limits(args.requests, args.tokens, args.seconds, args.context, args.output_tokens,
                                   Decimal(0), Decimal(0))
    binary, dev, script = (p.resolve() for p in (args.construct, args.construct_dev, args.script))
    output = args.output.resolve()
    output.mkdir(mode=0o700)
    source = {"revision": swebench.git(BENCH, "rev-parse", "HEAD"),
              "dirty": bool(swebench.git(BENCH, "status", "--porcelain")),
              "runner_sha256": swebench.sha256(__file__),
              "budget_proxy_sha256": swebench.sha256(swebench_proxy.__file__),
              "recording_proxy_sha256": swebench.sha256(dev)}
    summary = {"mode": "scripted", "manifest_sha256": swebench.sha256(manifest), "source": source,
               "model": data["model"], "limits": dataclasses.asdict(limits) | {"usd": "0", "request_usd": "0"},
               "attempts": []}
    patches = {}
    for row in rows:
        task = row["instance_id"]
        evidence = {"instance_id": task, "base_commit": row["base_commit"], "image": selected[task]}
        summary["attempts"].append(evidence)
        try:
            patches[task] = attempt(row, selected[task], data["model"], limits, binary, dev, script, output / task, evidence)
        except (OSError, ValueError, subprocess.CalledProcessError, tarfile.TarError) as error:
            evidence["runner_error"] = str(error)
    swebench.write_predictions(data, patches, output / "predictions.jsonl")
    summary["predictions_sha256"] = swebench.sha256(output / "predictions.jsonl")
    swebench.write_json(output / "run.json", summary)
    print(output / "run.json")
    return int(any("runner_error" in a or a.get("container_exit") != 0 or a.get("agent_exit") != 0
                   for a in summary["attempts"]))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ("manifest", "evaluator", "task-repo", "images", "construct", "construct-dev", "script", "output"):
        parser.add_argument(f"--{name}", required=True, type=Path)
    parser.add_argument("--python", required=True, type=Path, help="pinned evaluator environment's Python")
    for name, default in (("requests", 12), ("tokens", 100_000), ("seconds", 90), ("context", 16_384),
                          ("output-tokens", 2048)):
        parser.add_argument(f"--{name}", type=int, default=default)
    args = parser.parse_args()
    try:
        return run(args)
    except (OSError, ValueError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"{error}\n")


if __name__ == "__main__":
    raise SystemExit(main())
