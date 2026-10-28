def _reconcile_ledger_batch_impl(ledger, batch):
    """Old private version, kept for the importer."""
    return [e for e in batch if e not in ledger]
