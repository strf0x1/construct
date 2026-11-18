"""Upstream credentials stay in the recording proxy, selected by exact endpoint host.

OpenRouter retains its legacy key file. Orca defaults to its own SecretSpec
keyring entry; other hosts require an explicit source. No secret is returned
in a command argument or written into a harness home.
"""

import re
from pathlib import Path
from urllib.parse import urlparse

TOKEN_ENV = "BENCH_UPSTREAM_TOKEN"
SOURCES = ("BENCH_API_KEY_FILE", "BENCH_API_KEY_ENV", "BENCH_API_KEY_SECRET")


def read_key(path):
    text = Path(path).read_text().strip()
    return re.sub(r"^(export\s+)?[A-Z][A-Z0-9_]*\s*=\s*", "", text).strip("'\"")


def auth(upstream, env, legacy_file, jev_file):
    """Return proxy-only CLI arguments and environment additions; never infer a remote credential."""
    url = urlparse(upstream)
    host = url.hostname
    if url.scheme not in ("http", "https") or not host or url.username or url.password:
        raise ValueError("upstream must be an http(s) URL without embedded credentials")
    chosen = [name for name in SOURCES if env.get(name)]
    if len(chosen) > 1:
        raise ValueError("set only one of " + ", ".join(SOURCES))
    if not chosen and host in ("localhost", "127.0.0.1", "::1"):
        return ["--key-env", TOKEN_ENV], {TOKEN_ENV: "local"}
    source = chosen[0] if chosen else None
    name = env.get(source) if source else None
    if not source and host == "api.orcarouter.ai":
        source, name = "BENCH_API_KEY_SECRET", "ORCAROUTER_API_KEY"
    if source == "BENCH_API_KEY_SECRET":
        if not re.fullmatch(r"[A-Z][A-Z0-9_]*", name):
            raise ValueError("BENCH_API_KEY_SECRET must name a secret, not contain its value")
        return ["--key-secret", name], {}
    if not source and host == "openrouter.ai":
        source, name = ("BENCH_API_KEY_FILE", str(legacy_file)) if legacy_file.exists() else ("BENCH_API_KEY_ENV", "OPENROUTER_API_KEY")
    if source == "BENCH_API_KEY_FILE":
        path = Path(name)
        if path.stat().st_mode & 0o077:
            raise ValueError(f"{path} is readable by other users; chmod 600 {path}")
        key = read_key(path)
    elif source == "BENCH_API_KEY_ENV":
        if not re.fullmatch(r"[A-Z][A-Z0-9_]*", name):
            raise ValueError("BENCH_API_KEY_ENV must name an environment variable")
        key = env.get(name, "")
    else:
        raise ValueError(f"{host}: select BENCH_API_KEY_ENV, BENCH_API_KEY_FILE or BENCH_API_KEY_SECRET")
    if not key:
        raise ValueError(f"no upstream API key in {name}")
    prefix = {"openrouter.ai": "sk-or-", "api.orcarouter.ai": "sk-orca-"}.get(host)
    if prefix and not key.startswith(prefix):
        raise ValueError(f"{name} is not a {host} key; not sending it upstream")
    if jev_file.exists() and key == read_key(jev_file):
        raise ValueError("upstream key is the Jev key; not sending it upstream")
    return ["--key-env", TOKEN_ENV], {TOKEN_ENV: key}


def cost(usage):
    """Orca's USD field takes precedence; absent accounting remains unknown, not free."""
    for name in ("cost_usd", "cost"):
        value = usage.get(name)
        if isinstance(value, (int, float)) and not isinstance(value, bool):
            return value
    return None
