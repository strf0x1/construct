from pkg.users.store import fetch_user_record

_CACHE = {}


def prefetch_user_record(user_id):
    """Warm the cache for one user."""
    _CACHE[user_id] = fetch_user_record(user_id)
    return _CACHE[user_id]
