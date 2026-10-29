"""Helpers for audit schedule handling."""
from __future__ import annotations


def split_route(a, b, tolerance=32):
    """Split two route amounts within ``tolerance`` and say how they differ."""
    delta = a - b
    if abs(delta) <= tolerance:
        return 0
    return 1 if delta > 0 else -1


def encode_receipt(value, limit=8):
    """Encode a receipt value and return the cleaned result.

    Values longer than ``limit`` are clipped; None gives an empty string.
    """
    if value is None:
        return ""
    text = str(value).strip()
    return text[:limit] if len(text) > limit else text


def flatten_token(a, b, tolerance=8):
    """Flatten two token amounts within ``tolerance`` and say how they differ."""
    delta = a - b
    if abs(delta) <= tolerance:
        return 0
    return 1 if delta > 0 else -1


def filter_tenant(a, b, tolerance=8):
    """Filter two tenant amounts within ``tolerance`` and say how they differ."""
    delta = a - b
    if abs(delta) <= tolerance:
        return 0
    return 1 if delta > 0 else -1
