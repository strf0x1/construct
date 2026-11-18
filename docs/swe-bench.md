# SWE-bench evaluation

Issue [#236](https://github.com/strf0x1/construct/issues/236) uses SWE-bench Verified
for a fresh-memory quality baseline and a separate learning experiment. The
selected model is `deepseek/deepseek-v4.1-flash-1017`, served by OrcaRouter.

## Current implementation

`bench/swebench.py` freezes selected task IDs, separates public agent inputs
from private grading data, exports official patch predictions, invokes the
pinned official v5 evaluator, and imports its reports. It uses only Python's
standard library. Grading requires an environment with upstream dependencies.

`bench/swebench_run.py` drives Construct with a scripted model inside an
official task image. It also supports a paid pilot through a host-side recording
proxy and settled receipts. Learning is disabled and each attempt starts with
a fresh Construct home.

The default pins, inspected November 17, 2026, are:

| Source | Revision |
|---|---|
| `SWE-bench/SWE-bench` | `02e7a74ffd0b707aab73d203fe87bdc7c76afc8e` |
| `SWE-bench/swe-bench-tasks` | `3d07b464b7b311a0cbfb5ed5b2d8a3b96f84a33d` |

The dataset's own revision and file digest are recorded separately. The official
task repo takes precedence during v5 grading, so the bridge refuses a different
repo, base commit or issue text under the same instance ID. Checkouts must be
clean and match the manifest pins. The runner requires and records the image
ID, repository digests when present, and platform for each selected task.

## Prepare a subset before inference

Obtain the dataset as JSON or JSONL at a pinned revision, outside every agent
workspace. It may contain gold/test patches; never expose the source file.
Select IDs before examining the model's outcomes. Pilot tasks should be kept
out of a subsequent held-out claim.

```sh
python3 bench/swebench.py prepare \
  --dataset /outside-agent/verified.jsonl \
  --dataset-revision <full-dataset-commit> \
  --instance sympy__sympy-20590 \
  --output /outside-agent/pilot
```

This writes `manifest.json`, `agent/tasks.jsonl`, and owner-private
`grader/dataset.jsonl`. Only the allowlisted fields `instance_id`, `repo`,
`base_commit`, `problem_statement`, and `created_at` reach the agent input.
For a dataset exported from a task-repo checkout, use `--dataset-id` to record
that source URL and its actual Git revision rather than claiming a Hub snapshot.

## Run an isolated scripted smoke

Use a Linux Docker daemon, clean pinned evaluator and task checkouts, and
locally available official images. Record each image ID from `docker image
inspect`. Supply an image mapping such as
`{"sympy__sympy-20590": "sha256:<full-image-id>"}`. Build Construct
against a libc supported by the task image. The release workflow uses Ubuntu
22.04 for the Linux build. Build `construct-dev` for the host.

```sh
python3 bench/swebench_run.py \
  --manifest /outside-agent/pilot/manifest.json \
  --evaluator /outside-agent/SWE-bench \
  --task-repo /outside-agent/swe-bench-tasks \
  --python /outside-agent/evaluator-venv/bin/python \
  --images /outside-agent/images.json \
  --construct /outside-agent/build/construct \
  --construct-dev target/debug/construct-dev \
  --script bench/fixtures/swebench-sympy-container.json \
  --requests 2 --seconds 60 \
  --output /outside-agent/scripted-run
```

The example script is specific to `sympy__sympy-20590`. It verifies repository
setup and tool isolation, adds a marker file, and runs SymPy's bundled tests.
The third model request hits the guard, so the command exits with status 1
and records `budget_stop: request_limit`. Use `--requests 3` to complete
the same scripted turn.

The container starts at the original base in a shallow checkout. It can run
the task's installed toolchain against the working tree. The container has
no external network and sees read-only inputs plus a Unix socket. The socket
leads to a host process that accepts only chat completions for the pinned
model. The recording proxy runs on the host with an empty environment in
scripted mode. The grader data, task source checkout, and provider credentials
stay on the host.

`run.json` binds the prepared manifest, binaries, configuration, source and
images to `predictions.jsonl`. Each attempt retains the Construct events,
SQLite journal, proxy records and budget ledger. Patches are extracted with
the bridge's exporter inside the container. The runner exports a row for
every selected task, including empty patches when setup fails.

## Run the capped paid pilot

For a paid run, select `--upstream https://api.orcarouter.ai/v1` and pass
`--spend-usd 2` with `--wallet-usd <confirmed-usd>`.
Keep at least $5 available in the starting wallet after the cap. The paid
pilot uses the manifest's pinned DeepSeek 1017 model and requests high reasoning
effort through OrcaRouter. Record the requested budget and limits before any call.
Use explicit per-task limits for request count, tokens, wall time, context and
maximum output.

Every completion reserves tokens and $0.85 before dispatch. The reservation
covers the model's full upstream input window and maximum output at published
peak rates, with rounding headroom. The guard accepts text and client-side
function tools. A single admission is in flight, and a successful settlement
releases the unused reservation. The money limit applies across all selected
tasks. A missing or duplicate receipt holds its reservation and stops the
paid run. Subsequent tasks get empty predictions and retain their place in
the denominator. [DeepSeek model limits and peak pricing](https://api-docs.deepseek.com/quick_start/pricing/)

The recording proxy keeps the real key on the host. The receipt process
uses the same credential to look up the charge by request ID before the next
call can run. The per-attempt ledger preserves both inline usage and settled
cost. A wall-time stop gives Construct a brief grace period to journal and
export the partial patch after admission has closed. The guard buffers the
provider response while it accounts for usage, so use this runner to measure
quality and spend. [OrcaRouter receipts](https://docs.orcarouter.ai/operations/per-request-cost)

## Export predictions from completed work

Create a JSON mapping from each attempted instance ID to its completed git
repository root. Every repository must retain its original base as an ancestor.
The exporter includes committed/uncommitted edits and untracked added files.
An absent worktree produces an empty prediction, so failed or unattempted
instances are never silently removed from the selected denominator.

```sh
python3 bench/swebench.py export \
  --manifest /outside-agent/pilot/manifest.json \
  --worktrees /outside-agent/worktrees.json \
  --output /outside-agent/predictions.jsonl
```

Predictions contain exactly `instance_id`, `model_name_or_path`, and
`model_patch`. Do not reuse an output file for another attempt; export refuses
to overwrite it. Extraction disables Git external diff/text conversion hooks.

## Grade with upstream

Install the pinned evaluator and its dependencies in a separate virtualenv;
keep a clean checkout of its source and the pinned task repo. Grading can build
large images and executes third-party task tests in upstream's Docker setup.
The default is one worker. No LLM judge or model credentials are required.

```sh
python3 bench/swebench.py grade \
  --manifest /outside-agent/pilot/manifest.json \
  --predictions /outside-agent/predictions.jsonl \
  --evaluator /outside-agent/SWE-bench \
  --task-repo /outside-agent/swe-bench-tasks \
  --python /outside-agent/evaluator-venv/bin/python \
  --arm nolearn --repeat 0 \
  --output /outside-agent/evaluations
```

This creates a reviewable `evaluation.json` with a unique run ID and working
directory. Add `--execute` to run it and collect grades. The subprocess imports
the checked-out evaluator via `PYTHONPATH`, not whichever `swebench` happens to
be installed elsewhere. Prediction and manifest digests bind collection to the
planned inputs. The official summary and per-instance verdicts must agree.
Missing/error/empty-patch outcomes retain their place in the denominator;
upstream infrastructure classifications are recorded alongside them.

To collect a completed plan later:

```sh
python3 bench/swebench.py collect /outside-agent/evaluations/<run>/evaluation.json
```

Do not re-run a modified patch with an old evaluation directory. Upstream caches
by run ID and instance ID. Every bridge invocation creates a new identity.

## OrcaRouter benchmark credentials and accounting

For `--upstream https://api.orcarouter.ai/v1`, the existing benchmark now defaults
to `construct-dev serve --key-secret ORCAROUTER_API_KEY`. SecretSpec resolves
that secret in Construct's namespace. Agent audit errors propagate before the
proxy listens; the benchmark supplies an audit reason if none was provided.
OpenRouter retains its existing `~/.config/construct-bench.key` default only
for the exact `openrouter.ai` host. Loopback defaults to a placeholder key.

Explicit alternatives, one at a time:

- `BENCH_API_KEY_SECRET`: a SecretSpec name, never its value.
- `BENCH_API_KEY_ENV`: the name of a nonempty environment variable.
- `BENCH_API_KEY_FILE`: an owner-private file holding the key or `NAME=key`.

Other remote hosts require an explicit source. A missing explicit source never
falls back to another provider's credential. Explicit environment secrets are
removed from harness environments; real keys are held only by the proxy.

For the exact Orca host, the proxy requests inline cost automatically. It passes
only an allowlist of headers, including `X-OrcaRouter-Session-Id` and
`X-OrcaRouter-Include-Cost`; cookies and caller authorization are not forwarded.
The configured upstream key replaces the harness placeholder. No fixed session
ID is generated for all tasks: use a per-conversation ID when the runner has one,
or Orca's automatic prefix affinity. Both client and benchmark read
`usage.cost_usd` before legacy `usage.cost`, preserving an explicit zero and an
unknown cost. The recording log retains Orca's request ID for receipt lookup.
Inline figures are provider-reported estimates; settled receipts remain the
authority. The paid runner uses `construct-dev orca-receipt` to reconcile
each request before releasing its reservation.

`BENCH_CONSTRUCT=/absolute/path/to/construct` selects the binary under test
(default `target/release/construct`). A debug binary is useful for plumbing
smokes; performance comparisons should use consistent release builds and
record their manifests.

## Learning protocol and next work

The ordinary baseline uses fresh memory per task, learning off, with a matched
mini-SWE-agent run on the same model and predeclared task IDs. The online arm
keeps an isolated project kit while resetting code to each issue's base commit;
learning must finish before grading and before the next issue begins. Preserve
repo identity, not previous patches, and exclude grader data from reflection.

Start with a five-task environment/pilot proof, spend at most $2 on the initial
inference pilot, and retain $5 of the roughly $20 budget. Final sample size and
repeats depend on actual inference and learning costs. The five-task
environment proof and paid pilot are tracked in #236. The matched
mini-SWE-agent baseline and learning sequences follow the pilot.

References: [official evaluator](https://github.com/SWE-bench/SWE-bench),
[official evaluation guide](https://www.swebench.com/SWE-bench/guides/evaluation/),
[submission protocol](https://github.com/SWE-bench/experiments/blob/main/checklist.md),
[Orca cost reporting](https://docs.orcarouter.ai/operations/per-request-cost).
