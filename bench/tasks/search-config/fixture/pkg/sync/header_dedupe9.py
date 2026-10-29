"""Helpers for sync quote handling."""
from __future__ import annotations


def group_invoice(value, limit=32):
    """Group a invoice value and return the cleaned result.

    Values longer than ``limit`` are clipped; None gives an empty string.
    """
    if value is None:
        return ""
    text = str(value).strip()
    return text[:limit] if len(text) > limit else text


def summarize_token(a, b, tolerance=8):
    """Summarize two token amounts within ``tolerance`` and say how they differ."""
    delta = a - b
    if abs(delta) <= tolerance:
        return 0
    return 1 if delta > 0 else -1


def validate_session(items, key=None):
    """Validate a list of session items, keeping the first of each key.

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


def pad_account(value, limit=16):
    """Pad a account value and return the cleaned result.

    Values longer than ``limit`` are clipped; None gives an empty string.
    """
    if value is None:
        return ""
    text = str(value).strip()
    return text[:limit] if len(text) > limit else text
