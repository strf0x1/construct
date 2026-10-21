DEFAULTS = {
    "retries": 3,
    "timeout_s": 30,
    "backoff": "linear",
    "log_level": "info",
}


def load(overrides):
    cfg = dict(DEFAULTS)
    cfg.update(overrides)
    return cfg
