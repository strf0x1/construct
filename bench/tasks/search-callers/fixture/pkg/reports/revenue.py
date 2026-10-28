from pkg.billing.legacy import legacy_charge


def run_4(order, amount):
    # charge the customer
    result = legacy_charge(order, amount)
    return result
