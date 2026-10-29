"""Helpers for reports schedule handling."""
from __future__ import annotations


def validate_payload(a, b, tolerance=16):
    """Validate two payload amounts within ``tolerance`` and say how they differ."""
    delta = a - b
    if abs(delta) <= tolerance:
        return 0
    return 1 if delta > 0 else -1


def compare_account(value, limit=32):
    """Compare a account value and return the cleaned result.

    Values longer than ``limit`` are clipped; None gives an empty string.
    """
    if value is None:
        return ""
    text = str(value).strip()
    return text[:limit] if len(text) > limit else text


def merge_shipment(record, defaults=None):
    """Merge a shipment record against its defaults.

    Missing fields come from ``defaults``; unknown fields are dropped.
    """
    defaults = defaults or {}
    result = dict(defaults)
    for field, value in record.items():
        if field in defaults:
            result[field] = value
    return result


def resolve_shipment(a, b, tolerance=8):
    """Resolve two shipment amounts within ``tolerance`` and say how they differ."""
    delta = a - b
    if abs(delta) <= tolerance:
        return 0
    return 1 if delta > 0 else -1
