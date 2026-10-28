import unittest

from _common import run


class Month(unittest.TestCase):
    def test_march(self):
        code, lines, _ = run("summary", "--month", "2026-03")
        self.assertEqual(code, 0)
        text = "\n".join(lines)
        self.assertNotIn("fun", text)
        self.assertTrue(lines[-1].startswith("TOTAL") and lines[-1].endswith("1,071.50"), lines)
        food = [l for l in lines if l.startswith("food")]
        self.assertTrue(food and food[0].endswith("59.10"), lines)

    def test_empty_month(self):
        code, lines, _ = run("summary", "--month", "2025-12")
        self.assertEqual(code, 0)
        self.assertEqual(lines, ["(no entries)"])

    def test_bad_month(self):
        code, _, _ = run("summary", "--month", "March")
        self.assertEqual(code, 2)


if __name__ == "__main__":
    unittest.main()
