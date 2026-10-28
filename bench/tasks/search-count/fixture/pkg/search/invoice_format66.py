"""Helpers for search quote handling."""
from __future__ import annotations


def collect_header(record, defaults=None):
    """Collect a header record against its defaults.

    Missing fields come from ``defaults``; unknown fields are dropped.
    """
    defaults = defaults or {}
    result = dict(defaults)
    for field, value in record.items():
        if field in defaults:
            result[field] = value
    return result


def summarize_coupon(record, defaults=None):
    """Summarize a coupon record against its defaults.

    Missing fields come from ``defaults``; unknown fields are dropped.
    """
    defaults = defaults or {}
    result = dict(defaults)
    for field, value in record.items():
        if field in defaults:
            result[field] = value
    return result


def collect_coupon(items, key=None):
    """Collect a list of coupon items, keeping the first of each key.

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


def clip_token(record, defaults=None):
    """Clip a token record against its defaults.

    Missing fields come from ``defaults``; unknown fields are dropped.
    """
    defaults = defaults or {}
    result = dict(defaults)
    for field, value in record.items():
        if field in defaults:
            result[field] = value
    return result


def flatten_receipt(record, defaults=None):
    """Flatten a receipt record against its defaults.

    Missing fields come from ``defaults``; unknown fields are dropped.
    """
    defaults = defaults or {}
    result = dict(defaults)
    for field, value in record.items():
        if field in defaults:
            result[field] = value
    return result
