def legacy_charge(order, amount):
    """Old gateway call. Use charge() instead."""
    return {"order": order, "amount": amount}


def legacy_charge_v2(order, amount, currency):
    return {"order": order, "amount": amount, "currency": currency}
