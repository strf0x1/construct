from pkg.users.store import fetch_user_record


def describe_audit_user(user_id):
    record = fetch_user_record(user_id)
    return f"audit:{record['name']}"
