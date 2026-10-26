#!/usr/bin/env python3
"""Build the current checkout and open construct in a fresh manual-test project."""

import argparse
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parent.parent
MODEL = "anthropic/claude-haiku-5.5"
URL = "https://openrouter.ai/api/v1"
KEY_FILE = Path.home() / ".config/construct-bench.key"


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--project", type=Path, help="copy this project instead of the sample stats project")
    parser.add_argument("--model", default=MODEL, help=f"model (default: {MODEL})")
    parser.add_argument("--base-url", default=URL, help="OpenAI-compatible endpoint")
    parser.add_argument("--release", action="store_true", help="use an optimized build (slower to compile)")
    parser.add_argument("args", nargs=argparse.REMAINDER, help="construct arguments, after --")
    args = parser.parse_args()
    project = (args.project or ROOT / "bench/tasks/stats-median/fixture").resolve()
    if not project.is_dir():
        parser.error(f"project directory does not exist: {project}")

    env = dict(os.environ)
    # A stale one-off override must not disable the test workspace's sandbox.
    env["CONSTRUCT_SANDBOX"] = "workspace"
    env["CONSTRUCT_MODEL"] = args.model
    env["CONSTRUCT_BASE_URL"] = args.base_url
    if not env.get("OPENROUTER_API_KEY") and KEY_FILE.is_file() and args.base_url == URL:
        value = KEY_FILE.read_text().strip()
        value = re.sub(r"^(export\s+)?[A-Z][A-Z0-9_]*\s*=\s*", "", value).strip("'\"")
        if not value.startswith("sk-or-"):
            parser.error(f"{KEY_FILE} does not contain an OpenRouter key")
        env["OPENROUTER_API_KEY"] = value

    # Cargo's artifact messages also respect CARGO_TARGET_DIR and target config.
    print("Building construct…", flush=True)
    command = ["cargo", "build", "-p", "construct", "--bin", "construct", "--message-format=json-render-diagnostics"]
    if args.release:
        command.append("--release")
    build = subprocess.run(command, cwd=ROOT, stdout=subprocess.PIPE, text=True)
    if build.returncode:
        return build.returncode
    binary = None
    for line in build.stdout.splitlines():
        message = json.loads(line)
        if message.get("reason") == "compiler-artifact" and message.get("target", {}).get("name") == "construct":
            if message.get("executable"):
                binary = Path(message["executable"])
    if binary is None:
        raise RuntimeError("cargo did not report a construct executable")

    runs = ROOT / ".construct/try"
    runs.mkdir(parents=True, exist_ok=True)
    run = Path(tempfile.mkdtemp(prefix="run-", dir=runs))
    run.chmod(0o700)
    work = run / "project"
    # Dereference symlinks: edits in a copied project must not reach the source.
    # Skip credentials and heavyweight/generated directories, including our runs.
    shutil.copytree(project, work, ignore=shutil.ignore_patterns(
        ".git", ".construct", ".env", ".env.*", "*.key", "*.pem",
        "target", "node_modules", ".venv", "__pycache__",
    ))
    # Give the copy its own repo identity instead of inheriting this checkout's.
    subprocess.run(["git", "init", "--quiet", str(work)], check=True)
    home = run / "home"
    home.mkdir()
    # Keep binaries and harness state outside the agent's working directory.
    shutil.copy2(binary, run / "construct")
    secret = "OPENROUTER_API_KEY" if args.base_url == URL else "OPENAI_API_KEY"
    (home / "config.toml").write_text(
        f"model = {json.dumps(args.model)}\nbase_url = {json.dumps(args.base_url)}\n"
        f"api_key_secret = {json.dumps(secret)}\n\n[sandbox]\nmode = \"workspace\"\n"
    )
    env["CONSTRUCT_HOME"] = str(home)
    print(f"\nTest folder: {run}\nProject:     {work}\nModel:       {args.model}\n"
          f"Config:      {home / 'config.toml'}\n"
          "Files and journals are kept after exit.\n", flush=True)
    forwarded = args.args[1:] if args.args[:1] == ["--"] else args.args
    os.chdir(work)
    os.execve(run / "construct", [str(run / "construct"), *forwarded], env)


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, RuntimeError, subprocess.CalledProcessError) as error:
        print(f"try: {error}", file=sys.stderr)
        sys.exit(1)
