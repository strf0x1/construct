"""Layered configuration.

Files are merged in this order, later wins:
  config/base.toml, config/<env>.toml, then every config/overrides.d/*.toml in
  name order. An override file with ``only_env`` set applies to that
  environment only. ``config/local.toml`` is read in dev and nowhere else.
"""
import os
import tomllib
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2] / "config"


def _read(path):
    with open(path, "rb") as f:
        return tomllib.load(f)


def load(env=None):
    env = env or os.environ.get("APP_ENV", "production")
    merged = {}
    files = [ROOT / "base.toml", ROOT / f"{env}.toml"]
    if env == "dev":
        files.append(ROOT / "local.toml")
    for path in files:
        if path.exists():
            merged.update(_read(path))
    for path in sorted((ROOT / "overrides.d").glob("*.toml")):
        data = _read(path)
        if data.get("only_env") not in (None, env):
            continue
        data.pop("only_env", None)
        merged.update(data)
    return merged
