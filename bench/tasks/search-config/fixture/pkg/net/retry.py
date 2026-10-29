from pkg.config.loader import load


def delay_for(attempt):
    cfg = load()
    return min(cfg['base_delay_ms'] * 2 ** attempt, cfg['max_retry_delay_ms'])
