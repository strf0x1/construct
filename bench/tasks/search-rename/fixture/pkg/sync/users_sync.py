from pkg.users.store import fetch_user_record


def describe_sync_user(user_id):
    record = fetch_user_record(user_id)
    return f"sync:{record['name']}"
