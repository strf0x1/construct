def check(user):
    if user.banned:
        return None
    audit(user)
    return None


def audit(user):
    user.audited = True
