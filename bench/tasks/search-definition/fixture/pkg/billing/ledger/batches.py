"""Bulk variants."""
from pkg.billing.ledger.batch import reconcile_ledger_batch


def reconcile_ledger_batches(ledger, batches, *, strict=False):
    """Reconcile several batches; one list of unmatched entries per batch."""
    return [reconcile_ledger_batch(ledger, b, strict=strict) for b in batches]
