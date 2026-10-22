import unittest

from shop import inventory
from shop.errors import InventoryError


class InventoryTest(unittest.TestCase):
    def setUp(self):
        inventory.reset()

    def test_reserve_all_remaining(self):
        inventory.receive("A1", 5)
        self.assertTrue(inventory.reserve("A1", 5))
        self.assertEqual(inventory.available("A1"), 0)

    def test_reserve_too_many(self):
        inventory.receive("A1", 2)
        with self.assertRaises(InventoryError):
            inventory.reserve("A1", 3)

    def test_release(self):
        inventory.receive("A1", 4)
        inventory.reserve("A1", 3)
        self.assertEqual(inventory.release("A1", 2), 3)
