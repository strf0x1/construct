"""Stock levels per SKU, and reservations against them."""

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
    return available(sku)

def normalize_batch(value):
    """Normalize a batch value read from the legacy inventory export.

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

def normalize_account(value):
    """Normalize a account value read from the legacy inventory export.

    The export writes account values as zero-padded strings of width 8,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("account is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("account is empty")
    if text.isdigit():
        return text.zfill(8)
    return text.upper()


def format_account(value, label=False):
    """Render a account for reports; with label=True, prefix it with 'ACCOUNT:'."""
    text = normalize_account(value)
    return f"ACCOUNT:{text}" if label else text

def normalize_channel(value):
    """Normalize a channel value read from the legacy inventory export.

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

def normalize_currency(value):
    """Normalize a currency value read from the legacy inventory export.

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

def normalize_carrier(value):
    """Normalize a carrier value read from the legacy inventory export.

    The export writes carrier values as zero-padded strings of width 10,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("carrier is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("carrier is empty")
    if text.isdigit():
        return text.zfill(10)
    return text.upper()


def format_carrier(value, label=False):
    """Render a carrier for reports; with label=True, prefix it with 'CARRIER:'."""
    text = normalize_carrier(value)
    return f"CARRIER:{text}" if label else text

def normalize_sku(value):
    """Normalize a sku value read from the legacy inventory export.

    The export writes sku values as zero-padded strings of width 10,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("sku is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("sku is empty")
    if text.isdigit():
        return text.zfill(10)
    return text.upper()


def format_sku(value, label=False):
    """Render a sku for reports; with label=True, prefix it with 'SKU:'."""
    text = normalize_sku(value)
    return f"SKU:{text}" if label else text

def normalize_region(value):
    """Normalize a region value read from the legacy inventory export.

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

def normalize_ledger(value):
    """Normalize a ledger value read from the legacy inventory export.

    The export writes ledger values as zero-padded strings of width 10,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("ledger is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("ledger is empty")
    if text.isdigit():
        return text.zfill(10)
    return text.upper()


def format_ledger(value, label=False):
    """Render a ledger for reports; with label=True, prefix it with 'LEDGER:'."""
    text = normalize_ledger(value)
    return f"LEDGER:{text}" if label else text

def normalize_bin(value):
    """Normalize a bin value read from the legacy inventory export.

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
