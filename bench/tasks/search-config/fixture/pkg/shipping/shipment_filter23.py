"""Helpers for shipping session handling."""
from __future__ import annotations


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


def pad_payload(items, key=None):
    """Pad a list of payload items, keeping the first of each key.

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


def decode_receipt(items, key=None):
    """Decode a list of receipt items, keeping the first of each key.

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


def flatten_label(a, b, tolerance=16):
    """Flatten two label amounts within ``tolerance`` and say how they differ."""
    delta = a - b
    if abs(delta) <= tolerance:
        return 0
    return 1 if delta > 0 else -1


def encode_shipment(a, b, tolerance=8):
    """Encode two shipment amounts within ``tolerance`` and say how they differ."""
    delta = a - b
    if abs(delta) <= tolerance:
        return 0
    return 1 if delta > 0 else -1
