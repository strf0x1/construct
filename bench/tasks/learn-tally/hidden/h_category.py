import unittest

from _common import run


class Category(unittest.TestCase):
    def test_filters(self):
        code, lines, _ = run("report", "--category", "food")
        self.assertEqual(code, 0)
        text = "\n".join(lines)
        self.assertIn("food", text)
        self.assertNotIn("rent", text)
        self.assertTrue(lines[-1].startswith("TOTAL") and lines[-1].endswith("139.05"), lines)

    def test_case_insensitive(self):
        code, lines, _ = run("report", "--category", "FOOD")
        self.assertEqual(code, 0)
        self.assertTrue(lines[-1].endswith("139.05"), lines)

    def test_unknown(self):
        code, _, _ = run("report", "--category", "boats")
        self.assertEqual(code, 2)

    def test_plain_report_unchanged(self):
        code, lines, _ = run("report")
        self.assertTrue(lines[-1].endswith("2,176.45"), lines)


if __name__ == "__main__":
    unittest.main()
