from pkg.users.store import fetch_user_record


def describe_shipping_user(user_id):
    record = fetch_user_record(user_id)
    return f"shipping:{record['name']}"
