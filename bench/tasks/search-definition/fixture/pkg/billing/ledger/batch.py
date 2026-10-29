"""Settle a batch of ledger entries against the bank file."""
from __future__ import annotations

from pkg.billing.ledger.entries import Entry


def _group(entries):
    out = {}
    for e in entries:
        out.setdefault(e.account, []).append(e)
    return out


def reconcile_ledger_batch(ledger, batch, *, strict=False):
    """Match every entry in ``batch`` to ``ledger`` and return the unmatched ones."""
    by_account = _group(ledger)
    missing = []
    for entry in batch:
        candidates = by_account.get(entry.account, [])
        if not any(c.amount == entry.amount for c in candidates):
            missing.append(entry)
    if strict and missing:
        raise ValueError(f"{len(missing)} entries do not reconcile")
    return missing
