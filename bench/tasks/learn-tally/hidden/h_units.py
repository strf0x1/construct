import unittest

import units


class Temp(unittest.TestCase):
    def test_c_to_f(self):
        self.assertAlmostEqual(units.convert_temp(100, "c", "f"), 212.0)

    def test_f_to_c(self):
        self.assertAlmostEqual(units.convert_temp(-40, "f", "c"), -40.0)

    def test_kelvin(self):
        self.assertAlmostEqual(units.convert_temp(0, "c", "k"), 273.15)

    def test_unknown(self):
        with self.assertRaises(units.UnitError):
            units.convert_temp(1, "c", "x")

    def test_old_api(self):
        self.assertAlmostEqual(units.convert(1, "kg", "g"), 1000.0)


if __name__ == "__main__":
    unittest.main()
