#!/bin/bash
# Trusted setup and export run in the task image, outside Construct's tool sandbox.
set -euo pipefail
base=$1
repo=$2
cd /testbed
export HOME=/state/home CONSTRUCT_HOME=/state/construct GIT_CONFIG_NOSYSTEM=1
export OPENAI_API_KEY=via-proxy
mkdir -p "$HOME" "$CONSTRUCT_HOME" /export
cp /bench/config.toml "$CONSTRUCT_HOME/config.toml"

# Give the agent the exact base, with no extra image-build commits, remotes,
# reflogs, or unreachable objects. Keep the installed environment in the image.
seed=$(mktemp -d)
git init -q "$seed"
git -C "$seed" fetch -q --depth=1 --no-tags /testbed "$base"
rm -rf .git
mv "$seed/.git" .git
rmdir "$seed"
git reset -q --hard "$base"
git clean -q -fdx
git remote add origin "https://github.com/$repo.git"
git config user.name "Benchmark agent"
git config user.email "agent@example.invalid"
test -z "$(git status --porcelain)"
test "$(git rev-parse HEAD)" = "$base"

# Some task images use a system environment. Activate testbed when supplied.
if [[ -x /opt/miniconda3/envs/testbed/bin/python ]]; then
    source /opt/miniconda3/etc/profile.d/conda.sh
    conda activate testbed
fi
relay_python=$(command -v python3)
# Python 3.9+ from the image's base environment can drive old task environments.
if [[ -x /opt/miniconda3/bin/python3 ]]; then
    relay_python=/opt/miniconda3/bin/python3
fi
"$relay_python" -I /bench/swebench_relay.py &
relay=$!
trap 'kill "$relay" 2>/dev/null || true' EXIT
"$relay_python" -I -c '
import socket, time
for _ in range(100):
    try:
        with socket.create_connection(("127.0.0.1", 8765), timeout=.1):
            break
    except OSError:
        time.sleep(.05)
else:
    raise RuntimeError("container model relay did not start")
'
/bench/construct --version > /export/construct.version

status=0
/bench/construct run --json -- "$(cat /bench/issue.txt)" &
agent=$!
trap 'kill -INT "$agent" 2>/dev/null || true' INT
wait "$agent" || status=$?
# A trap interrupts Bash's wait before the agent has finished journaling.
if kill -0 "$agent" 2>/dev/null; then
    wait "$agent" || status=$?
fi
echo "$status" > /export/agent.exit
"$relay_python" -I -c '
import sys
sys.path.insert(0, "/bench")
from swebench import patch
from pathlib import Path
Path("/export/agent.patch").write_text(patch("/testbed", sys.argv[1]))
' "$base"
