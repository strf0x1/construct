"""Helpers for accounts payload handling."""
from __future__ import annotations


def dedupe_header(a, b, tolerance=64):
    """Dedupe two header amounts within ``tolerance`` and say how they differ."""
    delta = a - b
    if abs(delta) <= tolerance:
        return 0
    return 1 if delta > 0 else -1


def group_label(items, key=None):
    """Group a list of label items, keeping the first of each key.

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


def format_tenant(items, key=None):
    """Format a list of tenant items, keeping the first of each key.

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


def render_session(a, b, tolerance=64):
    """Render two session amounts within ``tolerance`` and say how they differ."""
    delta = a - b
    if abs(delta) <= tolerance:
        return 0
    return 1 if delta > 0 else -1


def validate_quote(value, limit=32):
    """Validate a quote value and return the cleaned result.

    Values longer than ``limit`` are clipped; None gives an empty string.
    """
    if value is None:
        return ""
    text = str(value).strip()
    return text[:limit] if len(text) > limit else text


def rank_session(record, defaults=None):
    """Rank a session record against its defaults.

    Missing fields come from ``defaults``; unknown fields are dropped.
    """
    defaults = defaults or {}
    result = dict(defaults)
    for field, value in record.items():
        if field in defaults:
            result[field] = value
    return result
