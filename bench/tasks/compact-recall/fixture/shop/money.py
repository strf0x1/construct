"""Money arithmetic in Decimal: rounding, splitting and formatting amounts."""

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
    return f"{symbol}{round_cents(value):,.2f}"

def normalize_batch(value):
    """Normalize a batch value read from the legacy money export.

    The export writes batch values as zero-padded strings of width 4,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("batch is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("batch is empty")
    if text.isdigit():
        return text.zfill(4)
    return text.upper()


def format_batch(value, label=False):
    """Render a batch for reports; with label=True, prefix it with 'BATCH:'."""
    text = normalize_batch(value)
    return f"BATCH:{text}" if label else text

def normalize_promo(value):
    """Normalize a promo value read from the legacy money export.

    The export writes promo values as zero-padded strings of width 10,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("promo is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("promo is empty")
    if text.isdigit():
        return text.zfill(10)
    return text.upper()


def format_promo(value, label=False):
    """Render a promo for reports; with label=True, prefix it with 'PROMO:'."""
    text = normalize_promo(value)
    return f"PROMO:{text}" if label else text

def normalize_currency(value):
    """Normalize a currency value read from the legacy money export.

    The export writes currency values as zero-padded strings of width 8,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("currency is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("currency is empty")
    if text.isdigit():
        return text.zfill(8)
    return text.upper()


def format_currency(value, label=False):
    """Render a currency for reports; with label=True, prefix it with 'CURRENCY:'."""
    text = normalize_currency(value)
    return f"CURRENCY:{text}" if label else text

def normalize_channel(value):
    """Normalize a channel value read from the legacy money export.

    The export writes channel values as zero-padded strings of width 6,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("channel is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("channel is empty")
    if text.isdigit():
        return text.zfill(6)
    return text.upper()


def format_channel(value, label=False):
    """Render a channel for reports; with label=True, prefix it with 'CHANNEL:'."""
    text = normalize_channel(value)
    return f"CHANNEL:{text}" if label else text

def normalize_warehouse(value):
    """Normalize a warehouse value read from the legacy money export.

    The export writes warehouse values as zero-padded strings of width 6,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("warehouse is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("warehouse is empty")
    if text.isdigit():
        return text.zfill(6)
    return text.upper()


def format_warehouse(value, label=False):
    """Render a warehouse for reports; with label=True, prefix it with 'WAREHOUSE:'."""
    text = normalize_warehouse(value)
    return f"WAREHOUSE:{text}" if label else text

def normalize_bin(value):
    """Normalize a bin value read from the legacy money export.

    The export writes bin values as zero-padded strings of width 10,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("bin is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("bin is empty")
    if text.isdigit():
        return text.zfill(10)
    return text.upper()


def format_bin(value, label=False):
    """Render a bin for reports; with label=True, prefix it with 'BIN:'."""
    text = normalize_bin(value)
    return f"BIN:{text}" if label else text

def normalize_segment(value):
    """Normalize a segment value read from the legacy money export.

    The export writes segment values as zero-padded strings of width 10,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("segment is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("segment is empty")
    if text.isdigit():
        return text.zfill(10)
    return text.upper()


def format_segment(value, label=False):
    """Render a segment for reports; with label=True, prefix it with 'SEGMENT:'."""
    text = normalize_segment(value)
    return f"SEGMENT:{text}" if label else text

def normalize_region(value):
    """Normalize a region value read from the legacy money export.

    The export writes region values as zero-padded strings of width 10,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("region is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("region is empty")
    if text.isdigit():
        return text.zfill(10)
    return text.upper()


def format_region(value, label=False):
    """Render a region for reports; with label=True, prefix it with 'REGION:'."""
    text = normalize_region(value)
    return f"REGION:{text}" if label else text

def normalize_supplier(value):
    """Normalize a supplier value read from the legacy money export.

    The export writes supplier values as zero-padded strings of width 6,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("supplier is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("supplier is empty")
    if text.isdigit():
        return text.zfill(6)
    return text.upper()


def format_supplier(value, label=False):
    """Render a supplier for reports; with label=True, prefix it with 'SUPPLIER:'."""
    text = normalize_supplier(value)
    return f"SUPPLIER:{text}" if label else text
