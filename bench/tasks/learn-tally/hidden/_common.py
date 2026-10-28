"""Shared by the hidden tests: run tally's CLI on a ledger written to a temp
file, and capture what it prints."""
import contextlib
import io
import tempfile

LEDGER = """\
2026-02-01 | rent | 1,000.00 | feb
2026-02-03 | food | 54.20 | groceries, big shop
2026-02-12 | food | 18.75 | takeout
2026-02-20 | fun | 25.00 | cinema
2026-03-01 | rent | 1,000.00 | mar
2026-03-04 | food | 61.10 | groceries
2026-03-09 | transport | 12.40 | taxi
2026-03-18 | food | -2.00 | refund
2026-04-02 | food | 7.00 | coffee
"""


def run(*argv, ledger=LEDGER):
    from tally import cli

    with tempfile.NamedTemporaryFile("w", suffix=".ledger", delete=False) as f:
        f.write(ledger)
    out, err = io.StringIO(), io.StringIO()
    with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
        try:
            code = cli.main(["--ledger", f.name, *argv])
        except SystemExit as e:  # argparse errors
            code = e.code
    return code, out.getvalue().splitlines(), err.getvalue()
