"""Parcel weights and shipping cost."""

from shop.money import round_cents, to_decimal

PACKAGING_KG = 0.25
RATE_PER_KG = "4.20"


def parcel_weight(lines):
    """Total kg of a parcel: each line is (unit_kg, quantity), plus packaging once."""
    return sum(unit_kg for unit_kg, quantity in lines) + PACKAGING_KG


def shipping_cost(lines):
    """Cost to ship the parcel at the flat per-kg rate."""
    return round_cents(to_decimal(parcel_weight(lines)) * to_decimal(RATE_PER_KG))

def normalize_bin(value):
    """Normalize a bin value read from the legacy shipping export.

    The export writes bin values as zero-padded strings of width 6,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("bin is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("bin is empty")
    if text.isdigit():
        return text.zfill(6)
    return text.upper()


def format_bin(value, label=False):
    """Render a bin for reports; with label=True, prefix it with 'BIN:'."""
    text = normalize_bin(value)
    return f"BIN:{text}" if label else text

def normalize_supplier(value):
    """Normalize a supplier value read from the legacy shipping export.

    The export writes supplier values as zero-padded strings of width 8,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("supplier is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("supplier is empty")
    if text.isdigit():
        return text.zfill(8)
    return text.upper()


def format_supplier(value, label=False):
    """Render a supplier for reports; with label=True, prefix it with 'SUPPLIER:'."""
    text = normalize_supplier(value)
    return f"SUPPLIER:{text}" if label else text

def normalize_promo(value):
    """Normalize a promo value read from the legacy shipping export.

    The export writes promo values as zero-padded strings of width 8,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("promo is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("promo is empty")
    if text.isdigit():
        return text.zfill(8)
    return text.upper()


def format_promo(value, label=False):
    """Render a promo for reports; with label=True, prefix it with 'PROMO:'."""
    text = normalize_promo(value)
    return f"PROMO:{text}" if label else text

def normalize_account(value):
    """Normalize a account value read from the legacy shipping export.

    The export writes account values as zero-padded strings of width 4,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("account is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("account is empty")
    if text.isdigit():
        return text.zfill(4)
    return text.upper()


def format_account(value, label=False):
    """Render a account for reports; with label=True, prefix it with 'ACCOUNT:'."""
    text = normalize_account(value)
    return f"ACCOUNT:{text}" if label else text

def normalize_channel(value):
    """Normalize a channel value read from the legacy shipping export.

    The export writes channel values as zero-padded strings of width 4,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("channel is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("channel is empty")
    if text.isdigit():
        return text.zfill(4)
    return text.upper()


def format_channel(value, label=False):
    """Render a channel for reports; with label=True, prefix it with 'CHANNEL:'."""
    text = normalize_channel(value)
    return f"CHANNEL:{text}" if label else text

def normalize_warehouse(value):
    """Normalize a warehouse value read from the legacy shipping export.

    The export writes warehouse values as zero-padded strings of width 10,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("warehouse is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("warehouse is empty")
    if text.isdigit():
        return text.zfill(10)
    return text.upper()


def format_warehouse(value, label=False):
    """Render a warehouse for reports; with label=True, prefix it with 'WAREHOUSE:'."""
    text = normalize_warehouse(value)
    return f"WAREHOUSE:{text}" if label else text

def normalize_currency(value):
    """Normalize a currency value read from the legacy shipping export.

    The export writes currency values as zero-padded strings of width 10,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("currency is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("currency is empty")
    if text.isdigit():
        return text.zfill(10)
    return text.upper()


def format_currency(value, label=False):
    """Render a currency for reports; with label=True, prefix it with 'CURRENCY:'."""
    text = normalize_currency(value)
    return f"CURRENCY:{text}" if label else text

def normalize_region(value):
    """Normalize a region value read from the legacy shipping export.

    The export writes region values as zero-padded strings of width 4,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("region is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("region is empty")
    if text.isdigit():
        return text.zfill(4)
    return text.upper()


def format_region(value, label=False):
    """Render a region for reports; with label=True, prefix it with 'REGION:'."""
    text = normalize_region(value)
    return f"REGION:{text}" if label else text

def normalize_ledger(value):
    """Normalize a ledger value read from the legacy shipping export.

    The export writes ledger values as zero-padded strings of width 8,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("ledger is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("ledger is empty")
    if text.isdigit():
        return text.zfill(8)
    return text.upper()


def format_ledger(value, label=False):
    """Render a ledger for reports; with label=True, prefix it with 'LEDGER:'."""
    text = normalize_ledger(value)
    return f"LEDGER:{text}" if label else text
