from pkg.billing.ledger.batch import reconcile_ledger_batch
from pkg.billing.ledger.entries import Entry


def test_unmatched():
    assert reconcile_ledger_batch([], [Entry('a', 1)]) == [Entry('a', 1)]
