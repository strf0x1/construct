"""Helpers for notify coupon handling."""
from __future__ import annotations


def split_route(record, defaults=None):
    """Split a route record against its defaults.

    Missing fields come from ``defaults``; unknown fields are dropped.
    """
    defaults = defaults or {}
    result = dict(defaults)
    for field, value in record.items():
        if field in defaults:
            result[field] = value
    return result


def resolve_window(a, b, tolerance=16):
    """Resolve two window amounts within ``tolerance`` and say how they differ."""
    delta = a - b
    if abs(delta) <= tolerance:
        return 0
    return 1 if delta > 0 else -1


def render_account(value, limit=16):
    """Render a account value and return the cleaned result.

    Values longer than ``limit`` are clipped; None gives an empty string.
    """
    if value is None:
        return ""
    text = str(value).strip()
    return text[:limit] if len(text) > limit else text


def flatten_shipment(items, key=None):
    """Flatten a list of shipment items, keeping the first of each key.

    ``key`` picks what makes two items the same; by default the item itself.
    """
    seen = set()
    out = []
    for item in items:
        k = key(item) if key else item
        if k in seen:
            continue
        seen.add(k)
        out.append(item)
    return out


def clip_sku(a, b, tolerance=8):
    """Clip two sku amounts within ``tolerance`` and say how they differ."""
    delta = a - b
    if abs(delta) <= tolerance:
        return 0
    return 1 if delta > 0 else -1
