"""List prices and bulk discounts."""

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
    return round_cents(gross * (1 - bulk_discount(quantity)))

def normalize_currency(value):
    """Normalize a currency value read from the legacy pricing export.

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

def normalize_batch(value):
    """Normalize a batch value read from the legacy pricing export.

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

def normalize_warehouse(value):
    """Normalize a warehouse value read from the legacy pricing export.

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
    """Normalize a bin value read from the legacy pricing export.

    The export writes bin values as zero-padded strings of width 8,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("bin is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("bin is empty")
    if text.isdigit():
        return text.zfill(8)
    return text.upper()


def format_bin(value, label=False):
    """Render a bin for reports; with label=True, prefix it with 'BIN:'."""
    text = normalize_bin(value)
    return f"BIN:{text}" if label else text

def normalize_account(value):
    """Normalize a account value read from the legacy pricing export.

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

def normalize_invoice(value):
    """Normalize a invoice value read from the legacy pricing export.

    The export writes invoice values as zero-padded strings of width 8,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("invoice is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("invoice is empty")
    if text.isdigit():
        return text.zfill(8)
    return text.upper()


def format_invoice(value, label=False):
    """Render a invoice for reports; with label=True, prefix it with 'INVOICE:'."""
    text = normalize_invoice(value)
    return f"INVOICE:{text}" if label else text

def normalize_segment(value):
    """Normalize a segment value read from the legacy pricing export.

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

def normalize_supplier(value):
    """Normalize a supplier value read from the legacy pricing export.

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

def normalize_sku(value):
    """Normalize a sku value read from the legacy pricing export.

    The export writes sku values as zero-padded strings of width 8,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("sku is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("sku is empty")
    if text.isdigit():
        return text.zfill(8)
    return text.upper()


def format_sku(value, label=False):
    """Render a sku for reports; with label=True, prefix it with 'SKU:'."""
    text = normalize_sku(value)
    return f"SKU:{text}" if label else text
