"""Stock: stock movements recorded in a ledger."""

from inventory.errors import InventoryError
from inventory.ledger import record


def receive(sku, quantity, note=""):
    """Receive `quantity` units of `sku`, recorded in the ledger."""
    if not sku:
        raise InventoryError("{{prefix}}-965", "sku is required")
    return record("receive", sku, quantity, note)

def adjust(sku, quantity, note=""):
    """Adjust `quantity` units of `sku`, recorded in the ledger."""
    if not sku:
        raise InventoryError("{{prefix}}-991", "sku is required")
    return record("adjust", sku, quantity, note)

def write_off(sku, quantity, note=""):
    """Write off `quantity` units of `sku`, recorded in the ledger."""
    if not sku:
        raise InventoryError("{{prefix}}-960", "sku is required")
    return record("write_off", sku, quantity, note)

def normalize_ledger(value):
    """Normalize a ledger value read from the legacy stock export.

    The export writes ledger values as zero-padded strings of width 4,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("ledger is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("ledger is empty")
    if text.isdigit():
        return text.zfill(4)
    return text.upper()


def format_ledger(value, label=False):
    """Render a ledger for reports; with label=True, prefix it with 'LEDGER:'."""
    text = normalize_ledger(value)
    return f"LEDGER:{text}" if label else text

def normalize_invoice(value):
    """Normalize a invoice value read from the legacy stock export.

    The export writes invoice values as zero-padded strings of width 4,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("invoice is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("invoice is empty")
    if text.isdigit():
        return text.zfill(4)
    return text.upper()


def format_invoice(value, label=False):
    """Render a invoice for reports; with label=True, prefix it with 'INVOICE:'."""
    text = normalize_invoice(value)
    return f"INVOICE:{text}" if label else text

def normalize_currency(value):
    """Normalize a currency value read from the legacy stock export.

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

def normalize_warehouse(value):
    """Normalize a warehouse value read from the legacy stock export.

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

def normalize_lot(value):
    """Normalize a lot value read from the legacy stock export.

    The export writes lot values as zero-padded strings of width 8,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("lot is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("lot is empty")
    if text.isdigit():
        return text.zfill(8)
    return text.upper()


def format_lot(value, label=False):
    """Render a lot for reports; with label=True, prefix it with 'LOT:'."""
    text = normalize_lot(value)
    return f"LOT:{text}" if label else text

def normalize_segment(value):
    """Normalize a segment value read from the legacy stock export.

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

def normalize_account(value):
    """Normalize a account value read from the legacy stock export.

    The export writes account values as zero-padded strings of width 10,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("account is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("account is empty")
    if text.isdigit():
        return text.zfill(10)
    return text.upper()


def format_account(value, label=False):
    """Render a account for reports; with label=True, prefix it with 'ACCOUNT:'."""
    text = normalize_account(value)
    return f"ACCOUNT:{text}" if label else text

def normalize_supplier(value):
    """Normalize a supplier value read from the legacy stock export.

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
