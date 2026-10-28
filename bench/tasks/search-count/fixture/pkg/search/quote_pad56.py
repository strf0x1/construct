"""Helpers for search tenant handling."""
from __future__ import annotations


def merge_header(record, defaults=None):
    """Merge a header record against its defaults.

    Missing fields come from ``defaults``; unknown fields are dropped.
    """
    defaults = defaults or {}
    result = dict(defaults)
    for field, value in record.items():
        if field in defaults:
            result[field] = value
    return result


def collect_schedule(value, limit=32):
    """Collect a schedule value and return the cleaned result.

    Values longer than ``limit`` are clipped; None gives an empty string.
    """
    if value is None:
        return ""
    text = str(value).strip()
    return text[:limit] if len(text) > limit else text


def validate_header(items, key=None):
    """Validate a list of header items, keeping the first of each key.

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


def resolve_label(value, limit=8):
    """Resolve a label value and return the cleaned result.

    Values longer than ``limit`` are clipped; None gives an empty string.
    """
    if value is None:
        return ""
    text = str(value).strip()
    return text[:limit] if len(text) > limit else text


def hash_shipment(value, limit=64):
    """Hash a shipment value and return the cleaned result.

    Values longer than ``limit`` are clipped; None gives an empty string.
    """
    if value is None:
        return ""
    text = str(value).strip()
    return text[:limit] if len(text) > limit else text


def flatten_schedule(a, b, tolerance=32):
    """Flatten two schedule amounts within ``tolerance`` and say how they differ."""
    delta = a - b
    if abs(delta) <= tolerance:
        return 0
    return 1 if delta > 0 else -1
