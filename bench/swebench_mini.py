"""Launch upstream mini-SWE-agent with its SWE-bench prompt inside the task image."""

import json
import os
from pathlib import Path
import signal
import sys
from urllib.error import HTTPError
from urllib.request import Request, urlopen

MINI_VERSION = "2.4.6"
MINI_REVISION = "a83fcae82d2a08f0ee0c688f9d137b3566c097f8"


def run(config_path, issue_path):
    # Only the harness uses this runtime. The shell still uses the task's
    # activated environment and its original Python and toolchain.
    sys.path.insert(0, "/mini-runtime/site")
    from minisweagent import __version__, package_dir
    from minisweagent.agents.default import DefaultAgent
    from minisweagent.environments.local import LocalEnvironment
    from minisweagent.models.openrouter_model import OpenRouterModel, BASH_TOOL
    import yaml

    if __version__ != MINI_VERSION:
        raise ValueError(f"expected mini-SWE-agent {MINI_VERSION}, found {__version__}")
    settings = json.loads(Path(config_path).read_text())
    config = yaml.safe_load((package_dir / "config/benchmarks/swebench.yaml").read_text())

    class ProxyModel(OpenRouterModel):
        """Use upstream Bash formatting and history with the local OpenAI proxy."""

        # A rejected admission is terminal. Transport retries still go through the guard.
        abort_exceptions = [*OpenRouterModel.abort_exceptions, HTTPError]

        def _query(self, messages, **kwargs):
            payload = {
                "model": self.config.model_name, "messages": messages, "tools": [BASH_TOOL],
                **self.config.model_kwargs, **kwargs,
            }
            request = Request(settings["endpoint"], data=json.dumps(payload).encode(),
                              headers={"Content-Type": "application/json", "Authorization": "Bearer via-proxy"})
            with urlopen(request, timeout=settings["seconds"]) as response:
                return json.load(response)

        def _calculate_cost(self, response):
            return {"cost": response.get("usage", {}).get("cost_usd", 0)}

    model = ProxyModel(**(config["model"] | {
        "model_name": settings["model"],
        "model_kwargs": {"temperature": 0.0, "max_tokens": settings["max_tokens"], "stream": False},
    }))
    config["environment"]["env"].pop("BASH_ENV", None)  # The common launcher activated testbed.
    environment = LocalEnvironment(**{k: v for k, v in config["environment"].items()
                                      if k != "environment_class"})
    trajectory = Path("/export/mini.traj.json")
    agent = DefaultAgent(model, environment, **(config["agent"] | {
        "step_limit": settings["requests"] + 1, "cost_limit": 0,
        "output_path": trajectory,
    }))
    try:
        info = agent.run(Path(issue_path).read_text())
    except KeyboardInterrupt:
        return 130
    finally:
        agent.save(trajectory)
    if info["exit_status"] == "Submitted":
        Path("/export/agent.patch").write_text(info["submission"])
        return 0
    return 1


if __name__ == "__main__":
    signal.signal(signal.SIGINT, signal.default_int_handler)
    os.environ["MSWEA_SILENT_STARTUP"] = "1"
    raise SystemExit(run(*sys.argv[1:]))
