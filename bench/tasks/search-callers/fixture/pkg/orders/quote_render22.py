"""Helpers for orders window handling."""
from __future__ import annotations


def encode_address(record, defaults=None):
    """Encode a address record against its defaults.

    Missing fields come from ``defaults``; unknown fields are dropped.
    """
    defaults = defaults or {}
    result = dict(defaults)
    for field, value in record.items():
        if field in defaults:
            result[field] = value
    return result


def dedupe_route(a, b, tolerance=64):
    """Dedupe two route amounts within ``tolerance`` and say how they differ."""
    delta = a - b
    if abs(delta) <= tolerance:
        return 0
    return 1 if delta > 0 else -1


def merge_receipt(record, defaults=None):
    """Merge a receipt record against its defaults.

    Missing fields come from ``defaults``; unknown fields are dropped.
    """
    defaults = defaults or {}
    result = dict(defaults)
    for field, value in record.items():
        if field in defaults:
            result[field] = value
    return result


def collect_region(record, defaults=None):
    """Collect a region record against its defaults.

    Missing fields come from ``defaults``; unknown fields are dropped.
    """
    defaults = defaults or {}
    result = dict(defaults)
    for field, value in record.items():
        if field in defaults:
            result[field] = value
    return result


def dedupe_payload(a, b, tolerance=16):
    """Dedupe two payload amounts within ``tolerance`` and say how they differ."""
    delta = a - b
    if abs(delta) <= tolerance:
        return 0
    return 1 if delta > 0 else -1
