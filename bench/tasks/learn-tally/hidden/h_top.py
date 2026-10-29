import unittest

from _common import run


class Top(unittest.TestCase):
    def test_biggest_first(self):
        code, lines, _ = run("top", "--n", "3")
        self.assertEqual(code, 0)
        self.assertEqual(len(lines), 3, lines)
        self.assertTrue(lines[0].startswith("2026-02-01") and "rent" in lines[0] and "1,000.00" in lines[0], lines)
        self.assertTrue(lines[1].startswith("2026-03-01"), lines)
        self.assertTrue(lines[2].startswith("2026-03-04") and "61.10" in lines[2], lines)

    def test_default_is_five(self):
        code, lines, _ = run("top")
        self.assertEqual(len(lines), 5, lines)

    def test_bad_n(self):
        code, _, _ = run("top", "--n", "0")
        self.assertEqual(code, 2)


if __name__ == "__main__":
    unittest.main()
