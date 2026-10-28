import contextlib
import io
import unittest
from pathlib import Path

from tally import cli

SAMPLE = str(Path(__file__).resolve().parent.parent / "data" / "sample.ledger")


def run(*argv):
    out = io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(io.StringIO()):
        code = cli.main(["--ledger", SAMPLE, *argv])
    return code, out.getvalue().splitlines()


class CliTest(unittest.TestCase):
    def test_report_total(self):
        code, lines = run("report")
        self.assertEqual(code, 0)
        self.assertTrue(lines[-1].startswith("TOTAL"))
        self.assertTrue(lines[-1].endswith("2,386.41"))

    def test_categories(self):
        code, lines = run("categories")
        self.assertEqual(lines, ["food", "fun", "rent", "transport", "utilities"])

    def test_balance_range(self):
        code, lines = run("balance", "--from", "2026-03-01", "--to", "2026-03-31")
        self.assertEqual(code, 0)
        self.assertTrue(lines[0].startswith("6 entries"))

    def test_missing_ledger(self):
        code = cli.main(["--ledger", "/nonexistent.ledger", "report"])
        self.assertEqual(code, 2)


if __name__ == "__main__":
    unittest.main()
