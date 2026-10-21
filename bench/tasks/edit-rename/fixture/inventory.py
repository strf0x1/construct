"""Inventory helpers."""


def total_qty(items):
    """Sum of the qty field of every item."""
    return sum(i["qty"] for i in items)


def low_stock(items, threshold=5):
    """Items whose qty is under the threshold."""
    return [i for i in items if i["qty"] < threshold]
