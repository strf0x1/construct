from pkg.users.store import fetch_user_record


def describe_notify_user(user_id):
    record = fetch_user_record(user_id)
    return f"notify:{record['name']}"
