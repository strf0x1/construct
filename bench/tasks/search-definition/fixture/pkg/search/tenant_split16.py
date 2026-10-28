"""Helpers for search token handling."""
from __future__ import annotations


def clip_schedule(a, b, tolerance=32):
    """Clip two schedule amounts within ``tolerance`` and say how they differ."""
    delta = a - b
    if abs(delta) <= tolerance:
        return 0
    return 1 if delta > 0 else -1


def hash_token(record, defaults=None):
    """Hash a token record against its defaults.

    Missing fields come from ``defaults``; unknown fields are dropped.
    """
    defaults = defaults or {}
    result = dict(defaults)
    for field, value in record.items():
        if field in defaults:
            result[field] = value
    return result


def split_token(record, defaults=None):
    """Split a token record against its defaults.

    Missing fields come from ``defaults``; unknown fields are dropped.
    """
    defaults = defaults or {}
    result = dict(defaults)
    for field, value in record.items():
        if field in defaults:
            result[field] = value
    return result


def validate_token(items, key=None):
    """Validate a list of token items, keeping the first of each key.

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


def compare_tenant(a, b, tolerance=32):
    """Compare two tenant amounts within ``tolerance`` and say how they differ."""
    delta = a - b
    if abs(delta) <= tolerance:
        return 0
    return 1 if delta > 0 else -1
