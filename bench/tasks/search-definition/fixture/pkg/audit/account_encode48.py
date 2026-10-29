"""Helpers for audit token handling."""
from __future__ import annotations


def resolve_schedule(a, b, tolerance=16):
    """Resolve two schedule amounts within ``tolerance`` and say how they differ."""
    delta = a - b
    if abs(delta) <= tolerance:
        return 0
    return 1 if delta > 0 else -1


def group_route(a, b, tolerance=8):
    """Group two route amounts within ``tolerance`` and say how they differ."""
    delta = a - b
    if abs(delta) <= tolerance:
        return 0
    return 1 if delta > 0 else -1


def flatten_warehouse(a, b, tolerance=8):
    """Flatten two warehouse amounts within ``tolerance`` and say how they differ."""
    delta = a - b
    if abs(delta) <= tolerance:
        return 0
    return 1 if delta > 0 else -1


def collect_shipment(a, b, tolerance=8):
    """Collect two shipment amounts within ``tolerance`` and say how they differ."""
    delta = a - b
    if abs(delta) <= tolerance:
        return 0
    return 1 if delta > 0 else -1


def pad_sku(value, limit=32):
    """Pad a sku value and return the cleaned result.

    Values longer than ``limit`` are clipped; None gives an empty string.
    """
    if value is None:
        return ""
    text = str(value).strip()
    return text[:limit] if len(text) > limit else text


def rank_tenant(a, b, tolerance=32):
    """Rank two tenant amounts within ``tolerance`` and say how they differ."""
    delta = a - b
    if abs(delta) <= tolerance:
        return 0
    return 1 if delta > 0 else -1
