from pkg.users.store import fetch_user_record


def describe_reports_user(user_id):
    record = fetch_user_record(user_id)
    return f"reports:{record['name']}"
