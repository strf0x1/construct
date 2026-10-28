import unittest

from _common import run


class Budget(unittest.TestCase):
    def test_over(self):
        code, lines, _ = run("budget", "--limit", "food=100", "--limit", "rent=5000")
        self.assertEqual(code, 1)
        over = [l for l in lines if "over" in l.lower()]
        self.assertTrue(any("food" in l for l in over), lines)
        self.assertFalse(any("rent" in l for l in over), lines)

    def test_cents_matter(self):
        code, _, _ = run("budget", "--limit", "food=139.05")
        self.assertEqual(code, 0)
        code, _, _ = run("budget", "--limit", "food=139.04")
        self.assertEqual(code, 1)

    def test_bad_limit(self):
        code, _, _ = run("budget", "--limit", "food")
        self.assertEqual(code, 2)


if __name__ == "__main__":
    unittest.main()
