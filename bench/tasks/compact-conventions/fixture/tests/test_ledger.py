import unittest

from inventory import stock
from inventory.ledger import ENTRIES


class LedgerTest(unittest.TestCase):
    def test_receive_is_recorded(self):
        stock.receive("A1", 3)
        self.assertEqual(ENTRIES[-1]["quantity"], 3)
