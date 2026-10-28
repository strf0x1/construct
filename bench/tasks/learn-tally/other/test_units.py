import unittest

import units


class ConvertTest(unittest.TestCase):
    def test_kg_to_lb(self):
        self.assertAlmostEqual(units.convert(1, "kg", "lb"), 2.20462, places=4)

    def test_cups(self):
        self.assertAlmostEqual(units.convert(1, "cup", "tbsp"), 16.0, places=6)

    def test_mismatch(self):
        with self.assertRaises(units.UnitError):
            units.convert(1, "g", "ml")


if __name__ == "__main__":
    unittest.main()
