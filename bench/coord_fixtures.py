"""Generates the coordinator-recall task (bench/tasks/coord-recall).

A coordinator-shaped conversation, long enough that a 64k window must cut:
twelve early turns plant decisions with per-run values, 120 middle turns are
ordinary status traffic (and override two of the decisions), and eight final
probes ask for one decision each, recorded in `answers.txt` as `key=value`.
`check.sh` scores those lines.

The generator is seeded: `python3 bench/coord_fixtures.py --seed N` writes the
same task.toml and fixture for the same N, so the committed task is a diffable
artefact. Per-run values are `{{name}}` placeholders from `[vars]`, filled by
The expected values reach `check.sh` only as `BENCH_VAR_<key>` in its own
environment. Nothing in the work dir is an answer key: an agent that cannot
recall a decision and searches the repo must find nothing to copy.
"""

import argparse
import json
import random
import shutil
from pathlib import Path

TASKS = Path(__file__).resolve().parent / "tasks"
TASK = TASKS / "coord-recall"
SEED = 0
WINDOW = 65536

# Per-run values. Templates are tasklib's ({hexN}); the turns use {{name}}.
# Every probed key has a var of the same name, which the bench hands check.sh
# as BENCH_VAR_<key>; `kernel_old` is the value the override retires.
VARS = {
    "kernel": "KCFG-{hex4}",
    "kernel_old": "KCFG-{hex4}",
    "train": "train-{hex4}",
    "staging": "acme-staging-{hex4}",
    "signing": "KEY-{hex4}",
    "flag": "ff-{hex4}",
    "channel": "#inc-{hex4}",
    "region": "eu-central-1",
    "base": "bookworm",
    "dbkey": "{hex6}",
}

# One planted decision per turn, in order. `override` is where the decision
# changes mid-conversation; `probe` is how the final turn asks for it, and
# `want` is the answer after the override. Decisions without a probe are
# context that the coordinator must not confuse with the probed ones.
DECISIONS = [
    {
        "key": "kernel",
        "plant": "The arm64 image is built with kernel config `{{kernel_old}}`. That is the only arm64 config the fleet supports; nothing else is approved.",
        "override": "Correction from the architecture review: the arm64 image now uses kernel config `{{kernel}}`. The old `{{kernel_old}}` is retired, and nothing may reference it.",
        "thing": "arm64 kernel config",
        "probe": "Which kernel config does the arm64 image use now?",
        "want": "{{kernel}}",
    },
    {
        "key": "region",
        "plant": "Rollouts target eu-west-1 for this service. We are deliberately staying out of us-east-1.",
        "override": "Change of plan: the rollout now targets `{{region}}`. eu-west-1 is retired.",
        "thing": "rollout region",
        "probe": "Which region does the rollout target now?",
        "want": "{{region}}",
    },
    {
        "key": "train",
        "plant": "The release train for this quarter is `{{train}}`. Tag every release with it.",
        "thing": "release train",
        "probe": "What is the release train for this quarter?",
        "want": "{{train}}",
    },
    {
        "key": "base",
        "plant": "The base image is Debian `{{base}}`. Trixie is not approved, whatever the build logs say.",
        "thing": "base Debian release",
        "probe": "Which Debian release is the base image on?",
        "want": "{{base}}",
    },
    {
        "key": "staging",
        "plant": "Staging artifacts go to the bucket `{{staging}}`. Nothing goes to the old bucket.",
        "thing": "staging bucket",
        "probe": "What is the staging bucket?",
        "want": "{{staging}}",
    },
    {
        "key": "signing",
        "plant": "Release artifacts are signed with key id `{{signing}}`. Any other key id is a mistake.",
        "thing": "release signing key id",
        "probe": "Which signing key id do release artifacts use?",
        "want": "{{signing}}",
    },
    {
        "key": "flag",
        "plant": "The rollout is gated on feature flag `{{flag}}`. Do not flip it without a canary window.",
        "thing": "rollout feature flag",
        "probe": "What is the rollout feature flag?",
        "want": "{{flag}}",
    },
    {
        "key": "channel",
        "plant": "Incidents for the fleet go to `{{channel}}`. Pager alerts do not belong in other channels.",
        "thing": "fleet incident channel",
        "probe": "Which channel do fleet incidents go to?",
        "want": "{{channel}}",
    },
    {
        "key": "retention",
        "plant": "Artifacts are retained for 30 days. No exceptions and no extensions.",
    },
    {
        "key": "handoff",
        "plant": "On-call handoff is Thursday at 15:00 UTC, with the outgoing engineer writing the summary first.",
    },
    {
        "key": "dbkey",
        "plant": "The migration lock key is `{{dbkey}}`. Only one migration may hold it at a time.",
    },
    {
        "key": "secrets",
        "plant": "Never paste a token or a secret into this conversation. Refer to it by name and rotate it if it leaks.",
    },
]

FILLERS = 120
# Where the two overrides sit among the fillers (0-based).
OVERRIDES = {"kernel": 39, "region": 79}

TEAMS = ["platform", "infra", "sre", "release", "data", "network"]
NAMES = ["Ana", "Ben", "Chen", "Dara", "Elif", "Farid", "Gus", "Hana"]
SERVICES = ["api", "worker", "scheduler", "auth", "ingest", "billing", "search", "gateway", "notifier", "indexer"]
LEVELS = ["INFO", "WARN", "DEBUG", "TRACE"]
WORDS = (
    "canary rollout deploy drain node shard queue backlog latency error retry budget cache miss hit "
    "index batch flush replica primary lag disk memory cpu spike steady ramp window probe timeout "
    "connect pool size request trace metric alert page incident followup summary handoff rotation"
).split()

TEMPLATES = [
    "Status check: {team} owns the {svc} work today. Overnight report is below. Tell me the one thing that needs attention, briefly.\n\n```\n{body}\n```",
    "{name} pasted the {svc} report. Anything in it that changes a decision? Keep it short.\n\n```\n{body}\n```",
    "Here is the {svc} shift report. Summarise the risk in one line.\n\n```\n{body}\n```",
]

DESCRIPTION = (
    "A long coordinator conversation: twelve decisions planted early, 120 turns of ordinary traffic "
    "(two decisions overridden on the way), then eight probes for what was decided. Run at a 64k window: "
    "`compact` has to cut and `optchat` has to batch. The score is recall of the decisions in answers.txt."
)


def report(rng: random.Random, size: int) -> str:
    """A plausible ops report of about `size` bytes, as log lines."""
    lines, n = [], 0
    while n < size:
        line = (
            f"2026-10-{rng.randrange(1, 29):02d}T{rng.randrange(24):02d}:{rng.randrange(60):02d}:{rng.randrange(60):02d} "
            f"{rng.choice(SERVICES):<9} {rng.choice(LEVELS):<5} "
            + " ".join(rng.choice(WORDS) for _ in range(rng.randrange(6, 16)))
            + f" id={rng.randrange(16**6):06x} n={rng.randrange(10000)}"
        )
        lines.append(line)
        n += len(line) + 1
    return "\n".join(lines)


def conversation(rng: random.Random) -> list:
    """The planted turns, the filler (with both overrides), then the probes."""
    turns = [d["plant"] for d in DECISIONS]
    overrides = {OVERRIDES[d["key"]]: d["override"] for d in DECISIONS if "override" in d}
    for i in range(FILLERS):
        if i in overrides:
            turns.append(overrides[i])
            continue
        body = report(rng, rng.randrange(4096, 6145))
        turns.append(
            rng.choice(TEMPLATES).format(team=rng.choice(TEAMS), name=rng.choice(NAMES), svc=rng.choice(SERVICES), body=body)
        )
    for d in DECISIONS:
        if "probe" not in d:
            continue
        turns.append(
            f"Record what you know: write one line `{d['key']}=<value>` to answers.txt with the current "
            f"{d['thing']}. Create answers.txt if it does not exist; keep any lines already in it and change "
            f"nothing else. Write the value you have, and do not ask questions."
        )
    return turns


def render(turns: list) -> str:
    probes = [d["key"] for d in DECISIONS if "probe" in d]
    out = [
        f"description = {json.dumps(DESCRIPTION)}",
        "turns = [",
        *[f"  {json.dumps(t)}," for t in turns],
        "]",
        "timeout_s = 240",
        f"window = {WINDOW}",
        'changes = ["answers.txt"]',
        f"probes = {json.dumps(probes)}",  # for tests; tasklib ignores unknown keys
        "",
        "[vars]",
        *[f"{k} = {json.dumps(v)}" for k, v in VARS.items()],
        "",
        "[budget]",
        "max_requests = 1200",
        "max_prompt_tokens = 30000000",
        "",
    ]
    return "\n".join(out)


def fixture(dest: Path):
    """The work dir: a small repo with nothing to leak, plus the answer key."""
    shutil.rmtree(dest, ignore_errors=True)
    (dest / "ops").mkdir(parents=True)
    (dest / "README.md").write_text(
        "# fleet\n\nRollout notes for the fleet team. Change a file only when a turn asks for it.\n"
    )
    (dest / "Makefile").write_text(
        ".PHONY: check rollback\n\ncheck:\n\t@echo \"no checks configured\"\n\nrollback:\n\t@echo \"rollback $(TARGET)\"\n"
    )
    (dest / "ops" / "runbook.md").write_text(
        "# Runbook\n\n- Start a rollout with a canary window and watch the error and latency dashboards.\n"
        "- Roll back first, investigate second.\n- Write the handoff summary before you leave.\n"
    )


def write(dest: Path = TASK, seed: int = SEED):
    """Write task.toml and fixture/ for `seed`; `check.sh` is not generated."""
    turns = conversation(random.Random(seed))
    fixture(dest / "fixture")
    (dest / "task.toml").write_text(render(turns))
    return dest, turns


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[1])
    ap.add_argument("--seed", type=int, default=SEED)
    ap.add_argument("--dest", type=Path, default=TASK)
    a = ap.parse_args()
    d, turns = write(a.dest, a.seed)
    chars = sum(len(t) for t in turns)
    print(f"wrote {d}: {len(turns)} turns, {chars} chars (~{chars // 3} tokens) for seed {a.seed}")
