DEFAULTS = {
    "retries": 3,
    "jitter": True,
    "max_backoff_s": 60,
    "timeout_s": 45,
    "backoff": "linear",
    "log_level": "info",
}


def load(overrides):
    cfg = dict(DEFAULTS)
    cfg.update(overrides)
    return cfg
