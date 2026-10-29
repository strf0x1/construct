"""The user store: an in-memory table standing in for the database."""

_TABLE = {1: {"name": "ada"}, 2: {"name": "grace"}, 3: {"name": "edsger"}}


def fetch_user_record(user_id):
    """Return one user's record, or raise KeyError."""
    return dict(_TABLE[user_id])


def fetch_user_records(user_ids):
    """Return the records of several users, skipping unknown ids."""
    return [fetch_user_record(i) for i in user_ids if i in _TABLE]
