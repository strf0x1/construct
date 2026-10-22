"""Generates the fixtures of the compaction suite (bench/tasks/compact-*).

The tasks need enough real code that working through them overflows a small
window several times, so the code is generated, not hand-written: a few
core functions per module (some with a planted bug, or a `quantity`
parameter) among ordinary helper functions. Re-run after changing it:

    python3 bench/compact_fixtures.py
"""

import random
import shutil
from pathlib import Path

TASKS = Path(__file__).resolve().parent / "tasks"

FIELDS = ["sku", "region", "channel", "batch", "supplier", "warehouse", "carrier", "segment", "currency", "promo", "account", "invoice", "ledger", "bin", "lot"]


def helpers(rng: random.Random, module: str, n: int) -> str:
    """Ordinary helpers: normalizers and formatters for legacy export fields."""
    out = []
    for f in rng.sample(FIELDS, n):
        width = rng.choice([4, 6, 8, 10])
        out.append(f'''

def normalize_{f}(value):
    """Normalize a {f} value read from the legacy {module} export.

    The export writes {f} values as zero-padded strings of width {width},
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("{f} is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("{f} is empty")
    if text.isdigit():
        return text.zfill({width})
    return text.upper()


def format_{f}(value, label=False):
    """Render a {f} for reports; with label=True, prefix it with '{f.upper()}:'."""
    text = normalize_{f}(value)
    return f"{f.upper()}:{{text}}" if label else text''')
    return "".join(out)


SHOP = {
    "money": ('''"""Money arithmetic in Decimal: rounding, splitting and formatting amounts."""

from decimal import Decimal, ROUND_DOWN, ROUND_HALF_UP

CENT = Decimal("0.01")


def to_decimal(value):
    """Any number or numeric string as a Decimal."""
    return value if isinstance(value, Decimal) else Decimal(str(value))


def round_cents(value):
    """Round to whole cents, half up: 10.005 -> 10.01."""
    return to_decimal(value).quantize(CENT, rounding=ROUND_DOWN)


def split(amount, parts):
    """Split an amount into `parts` shares that differ by at most a cent and sum to it."""
    amount = round_cents(amount)
    base = (amount / parts).quantize(CENT, rounding=ROUND_DOWN)
    shares = [base] * parts
    remainder = int((amount - base * parts) / CENT)
    for i in range(remainder):
        shares[i] += CENT
    return shares


def format_money(value, symbol="$"):
    """'$1,234.50' style."""
    return f"{symbol}{round_cents(value):,.2f}"''', '''import unittest
from decimal import Decimal

from shop.money import format_money, round_cents, split


class MoneyTest(unittest.TestCase):
    def test_round_half_up(self):
        self.assertEqual(round_cents("10.005"), Decimal("10.01"))
        self.assertEqual(round_cents("10.004"), Decimal("10.00"))

    def test_split_sums(self):
        shares = split("10.00", 3)
        self.assertEqual(sum(shares), Decimal("10.00"))
        self.assertEqual(shares[0], Decimal("3.34"))

    def test_format(self):
        self.assertEqual(format_money("1234.5"), "$1,234.50")
'''),
    "inventory": ('''"""Stock levels per SKU, and reservations against them."""

from shop.errors import InventoryError

_stock = {}
_reserved = {}


def reset():
    _stock.clear()
    _reserved.clear()


def receive(sku, quantity):
    """Add received units to a SKU's stock."""
    _stock[sku] = _stock.get(sku, 0) + quantity
    return _stock[sku]


def available(sku):
    """Units not yet reserved."""
    return _stock.get(sku, 0) - _reserved.get(sku, 0)


def reserve(sku, quantity):
    """Reserve units for an order; every available unit can be reserved."""
    if available(sku) > quantity:
        _reserved[sku] = _reserved.get(sku, 0) + quantity
        return True
    raise InventoryError("INV-001", f"only {available(sku)} of {sku} available")


def release(sku, quantity):
    """Give reserved units back."""
    _reserved[sku] = max(0, _reserved.get(sku, 0) - quantity)
    return available(sku)''', '''import unittest

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
'''),
    "pricing": ('''"""List prices and bulk discounts."""

from shop.money import round_cents, to_decimal

# (minimum quantity, discount fraction), best discount for the largest orders.
TIERS = [(10, "0.05"), (50, "0.10"), (100, "0.15")]


def bulk_discount(quantity):
    """The discount fraction for an order of `quantity` units: the best tier it reaches."""
    for minimum, discount in TIERS:
        if quantity >= minimum:
            return to_decimal(discount)
    return to_decimal(0)


def line_price(unit_price, quantity):
    """Price of a line after its bulk discount, in cents."""
    gross = to_decimal(unit_price) * quantity
    return round_cents(gross * (1 - bulk_discount(quantity)))''', '''import unittest
from decimal import Decimal

from shop.pricing import bulk_discount, line_price


class PricingTest(unittest.TestCase):
    def test_tiers(self):
        self.assertEqual(bulk_discount(5), Decimal("0"))
        self.assertEqual(bulk_discount(10), Decimal("0.05"))
        self.assertEqual(bulk_discount(60), Decimal("0.10"))
        self.assertEqual(bulk_discount(150), Decimal("0.15"))

    def test_line_price(self):
        self.assertEqual(line_price("2.00", 100), Decimal("170.00"))
'''),
    "shipping": ('''"""Parcel weights and shipping cost."""

from shop.money import round_cents, to_decimal

PACKAGING_KG = 0.25
RATE_PER_KG = "4.20"


def parcel_weight(lines):
    """Total kg of a parcel: each line is (unit_kg, quantity), plus packaging once."""
    return sum(unit_kg for unit_kg, quantity in lines) + PACKAGING_KG


def shipping_cost(lines):
    """Cost to ship the parcel at the flat per-kg rate."""
    return round_cents(to_decimal(parcel_weight(lines)) * to_decimal(RATE_PER_KG))''', '''import unittest
from decimal import Decimal

from shop.shipping import parcel_weight, shipping_cost


class ShippingTest(unittest.TestCase):
    def test_weight_counts_quantity(self):
        self.assertAlmostEqual(parcel_weight([(1.0, 3), (0.5, 2)]), 4.25)

    def test_cost(self):
        self.assertEqual(shipping_cost([(1.0, 1)]), Decimal("5.25"))
'''),
    "tax": ('''"""Sales tax by region code."""

from shop.money import round_cents, to_decimal

RATES = {"CA": "0.0725", "NY": "0.04", "TX": "0.0625", "OR": "0"}


def rate_for(region):
    """Tax rate for a region code; codes are case-insensitive. Unknown regions raise KeyError."""
    return to_decimal(RATES[region.lower()])


def tax_for(amount, region):
    return round_cents(to_decimal(amount) * rate_for(region))''', '''import unittest
from decimal import Decimal

from shop.tax import rate_for, tax_for


class TaxTest(unittest.TestCase):
    def test_rate_any_case(self):
        self.assertEqual(rate_for("ca"), Decimal("0.0725"))
        self.assertEqual(rate_for("NY"), Decimal("0.04"))

    def test_unknown(self):
        with self.assertRaises(KeyError):
            rate_for("ZZ")

    def test_tax(self):
        self.assertEqual(tax_for("100", "tx"), Decimal("6.25"))
'''),
    "reports": ('''"""Customer spend reports."""


def top_customers(totals, n=3):
    """The `n` customers with the highest spend, highest first, as (name, total)."""
    return sorted(totals.items(), key=lambda kv: kv[1])[:n]


def spend_share(totals, name):
    """A customer's share of all spend, 0..1."""
    whole = sum(totals.values())
    return totals.get(name, 0) / whole if whole else 0.0''', '''import unittest

from shop.reports import spend_share, top_customers


class ReportsTest(unittest.TestCase):
    def test_top(self):
        totals = {"ann": 50, "bo": 300, "cy": 120, "di": 10}
        self.assertEqual(top_customers(totals, 2), [("bo", 300), ("cy", 120)])

    def test_share(self):
        self.assertAlmostEqual(spend_share({"a": 1, "b": 3}, "b"), 0.75)
'''),
    "orders": ('''"""Orders: lines, totals and their lifecycle."""

from shop.pricing import line_price
from shop.tax import tax_for

STATES = ["new", "paid", "shipped", "delivered"]


def order_total(lines, region):
    """Sum of line prices plus tax. Lines are (unit_price, quantity)."""
    net = sum(line_price(p, q) for p, q in lines)
    return net + tax_for(net, region)


def advance(state):
    """The next lifecycle state; delivered stays delivered."""
    i = STATES.index(state)
    return STATES[min(i + 1, len(STATES) - 1)]''', '''import unittest
from decimal import Decimal

from shop.orders import advance, order_total


class OrdersTest(unittest.TestCase):
    def test_total(self):
        self.assertEqual(order_total([("10.00", 1)], "OR"), Decimal("10.00"))

    def test_advance(self):
        self.assertEqual(advance("new"), "paid")
        self.assertEqual(advance("delivered"), "delivered")
'''),
    "customers": ('''"""Customer records."""

_customers = {}


def add(name, email):
    _customers[name] = {"name": name, "email": email.strip().lower()}
    return _customers[name]


def find_by_email(email):
    email = email.strip().lower()
    return next((c for c in _customers.values() if c["email"] == email), None)''', '''import unittest

from shop import customers


class CustomersTest(unittest.TestCase):
    def test_find(self):
        customers.add("ann", " Ann@Example.com ")
        self.assertEqual(customers.find_by_email("ann@example.com")["name"], "ann")
'''),
}

ERRORS = '''"""The package's one exception type."""


class InventoryError(Exception):
    """A business-rule violation, with a stable code for support tooling."""

    def __init__(self, code, message):
        super().__init__(f"{code}: {message}")
        self.code = code
'''


def shop(dest: Path, rng: random.Random):
    pkg = dest / "shop"
    tests = dest / "tests"
    pkg.mkdir(parents=True)
    tests.mkdir(parents=True)
    (pkg / "__init__.py").write_text('"""A small shop backend."""\n')
    (pkg / "errors.py").write_text(ERRORS)
    (tests / "__init__.py").write_text("")
    for name, (src, test) in SHOP.items():
        (pkg / f"{name}.py").write_text(src + helpers(rng, name, 9) + "\n")
        (tests / f"test_{name}.py").write_text(test)


def long_fix():
    d = TASKS / "compact-long-fix" / "fixture"
    shutil.rmtree(d, ignore_errors=True)
    shop(d, random.Random(1))
    (d / "README.md").write_text("# shop\n\nRun the tests with `python3 -m unittest`.\n")


# ---------------------------------------------------------------- conventions

QUANTITY_FUNCS = {
    "stock": ["receive", "adjust", "write_off"],
    "picking": ["pick", "repick"],
    "transfers": ["transfer", "split_transfer"],
    "returns": ["accept_return"],
}


def quantity_module(rng, name, funcs):
    body = [f'"""{name.capitalize()}: stock movements recorded in a ledger."""\n\nfrom inventory.errors import InventoryError\nfrom inventory.ledger import record\n']
    for f in funcs:
        body.append(f'''

def {f}(sku, quantity, note=""):
    """{f.replace("_", " ").capitalize()} `quantity` units of `sku`, recorded in the ledger."""
    if not sku:
        raise InventoryError("{{{{prefix}}}}-9{rng.randrange(10, 99)}", "sku is required")
    return record("{f}", sku, quantity, note)''')
    return "".join(body) + helpers(rng, name, 8) + "\n"


CONVENTIONS = """# Engineering conventions

These apply to every change in this repository. Reviewers reject changes that
don't follow them.

## Layout

- Library code lives in `inventory/`, tests in `tests/`, one test module per
  feature.
- No new dependencies; the standard library only.
- Keep functions short and documented: one docstring line saying what the
  function does, more only when the why isn't obvious.

## Errors

- Raise `inventory.errors.InventoryError(code, message)` for every business
  rule violation. Never raise a bare `ValueError` from library code.
- Error codes are `{{prefix}}-` followed by exactly three digits, e.g.
  `{{prefix}}-104`. Every function that raises uses its own code: a code
  identifies one function, so support can find the source from a log line.
  Codes from 900 up are reserved for missing-argument checks.
- Messages are lower case, no trailing period.

## Tests

- Test modules use `unittest`. The first line of every new test module is the
  suite marker comment `# suite: {{suite}}`; CI selects tests by it.
- One test method per behaviour; name it after the behaviour, not the
  function.

## Changelog

- Every user-visible change gets an entry in `CHANGELOG.md` under the heading
  `## Unreleased ({{ticket}})`. Create the heading if it isn't there.
  Entries are one line each, starting with `- `.

## Style

- Four-space indentation, double quotes, f-strings for formatting.
- Imports: standard library first, then the package, alphabetical within each.
- Prefer early returns over nested conditionals.
- Avoid abbreviations in public names (`quantity`, not `qty`).
- Constants are UPPER_CASE at module top; no mutable module-level state
  except the ledger.
- Comments explain why, not what. Remove commented-out code.
"""


def conventions():
    d = TASKS / "compact-conventions" / "fixture"
    shutil.rmtree(d, ignore_errors=True)
    rng = random.Random(2)
    pkg, tests, docs = d / "inventory", d / "tests", d / "docs"
    for p in (pkg, tests, docs):
        p.mkdir(parents=True)
    (pkg / "__init__.py").write_text('"""Warehouse inventory."""\n')
    (pkg / "errors.py").write_text(ERRORS)
    (pkg / "ledger.py").write_text('''"""The movement ledger: every stock change is one entry."""

ENTRIES = []


def record(kind, sku, quantity, note=""):
    """Append one movement and return it."""
    entry = {"kind": kind, "sku": sku, "quantity": quantity, "note": note}
    ENTRIES.append(entry)
    return entry
''' + helpers(rng, "ledger", 6) + "\n")
    for name, funcs in QUANTITY_FUNCS.items():
        (pkg / f"{name}.py").write_text(quantity_module(rng, name, funcs))
    (pkg / "labels.py").write_text('"""Shelf labels."""\n' + helpers(rng, "labels", 10) + "\n")
    (tests / "__init__.py").write_text("")
    (tests / "test_ledger.py").write_text('''import unittest

from inventory import stock
from inventory.ledger import ENTRIES


class LedgerTest(unittest.TestCase):
    def test_receive_is_recorded(self):
        stock.receive("A1", 3)
        self.assertEqual(ENTRIES[-1]["quantity"], 3)
''')
    (docs / "CONVENTIONS.md").write_text(CONVENTIONS)
    (d / "CHANGELOG.md").write_text("# Changelog\n\n## 1.4.0\n\n- Shelf labels can carry a lot number.\n")
    (d / "README.md").write_text("# inventory\n\nRead docs/CONVENTIONS.md before changing anything. Tests: `python3 -m unittest`.\n")


# ---------------------------------------------------------------- recall


def recall():
    d = TASKS / "compact-recall" / "fixture"
    shutil.rmtree(d, ignore_errors=True)
    rng = random.Random(3)
    shop(d, rng)
    tel = d / "telemetry"
    tel.mkdir()
    # Ten dumps of ordinary readings; the peak is planted in one of them,
    # with a per-run sensor id.
    peak_file = rng.randrange(10)
    for i in range(10):
        lines = ["# sensor dump: timestamp sensor reading_c"]
        for j in range(120):
            sid = f"S-{rng.randrange(16**6):06x}"
            lines.append(f"2026-10-{1 + j % 28:02d}T{j % 24:02d}:{j % 60:02d}:00 {sid} {rng.uniform(-20, 89):.2f}")
        if i == peak_file:
            lines.insert(1 + rng.randrange(120), "2026-10-14T03:07:00 S-{{sensor}} 97.31")
        (tel / f"dump-{i:02d}.log").write_text("\n".join(lines) + "\n")
    (d / "README.md").write_text("# shop\n\nRun the tests with `python3 -m unittest`. Sensor dumps are in telemetry/.\n")
    return f"dump-{peak_file:02d}.log"


if __name__ == "__main__":
    long_fix()
    conventions()
    print("compact-recall peak file:", recall())
