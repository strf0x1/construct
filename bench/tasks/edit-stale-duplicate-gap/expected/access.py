def check(user):
    if user.is_admin:
        return None
    if user.banned:
        return None
    audit(user)
    return user.id


def audit(user):
    user.audited = True
