"""OrcaRouter's pricing, credentials and settled receipts for a bounded pilot."""

from decimal import Decimal
import json
import os
from pathlib import Path
import subprocess

import provider
import swebench
from swebench_proxy import Receipt

UPSTREAM = "https://api.orcarouter.ai/v1"
PRICING = {
    "peak_uncached_input_usd_per_million": "0.30",
    "peak_output_usd_per_million": "1.20",
    "context_tokens": 1_048_576,
    "maximum_output_tokens": 400_000,  # Covers the published 384K, even in binary units.
    "price_source": "https://api-docs.deepseek.com/quick_start/pricing/",
}
# Reserve for a full provider window and full 384K output at peak price,
# including a margin for billing rounding. No routing fallbacks or server tools.
RESERVATION = Decimal("0.85")
EFFORT = "high"


class Orca:
    def __init__(self, dev, upstream, model):
        if upstream != UPSTREAM or model != swebench.MODEL:
            raise ValueError(f"paid accounting currently supports {swebench.MODEL} at {UPSTREAM}")
        self.dev = dev
        self.upstream = upstream
        self.env = dict(os.environ)
        config = Path.home() / ".config"
        self.key_args, local_env = provider.auth(upstream, self.env, config / "construct-bench.key",
                                                 config / "construct-bench-jev.key")
        self.env.update(local_env)
        self.env["SECRETSPEC_REASON"] = "Run and reconcile the authorized capped Construct SWE-bench pilot for issue 236"

    def proxy_args(self):
        return ["--upstream", self.upstream, *self.key_args]

    def configure(self, request):
        request["reasoning_effort"] = EFFORT

    def receipt(self, request_id, model):
        if not request_id:
            return None
        try:
            value = subprocess.check_output([str(self.dev), "orca-receipt", *self.key_args, "--", request_id],
                                            env=self.env, stderr=subprocess.DEVNULL, timeout=9)
            return parse_receipt(value, request_id, model)
        except (OSError, ValueError, subprocess.CalledProcessError, subprocess.TimeoutExpired):
            return None


def parse_receipt(raw, request_id, model):
    payload = json.loads(raw, parse_float=Decimal)
    data = payload.get("data") if isinstance(payload, dict) else None
    if not isinstance(data, dict):
        return None
    if data.get("id") != request_id or data.get("model") != model or data.get("cost_currency") != "USD":
        return None
    usd = data.get("total_cost")
    prompt = data.get("tokens_prompt")
    completion = data.get("tokens_completion")
    count = data.get("tokens_total")
    if type(usd) not in (Decimal, int) or not Decimal(usd).is_finite() or usd < 0:
        return None
    if any(type(n) is not int or n < 0 for n in (prompt, completion, count)) or count != prompt + completion:
        return None
    return Receipt(Decimal(usd), count)
