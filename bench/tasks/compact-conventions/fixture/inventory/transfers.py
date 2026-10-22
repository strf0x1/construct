"""Transfers: stock movements recorded in a ledger."""

from inventory.errors import InventoryError
from inventory.ledger import record


def transfer(sku, quantity, note=""):
    """Transfer `quantity` units of `sku`, recorded in the ledger."""
    if not sku:
        raise InventoryError("{{prefix}}-961", "sku is required")
    return record("transfer", sku, quantity, note)

def split_transfer(sku, quantity, note=""):
    """Split transfer `quantity` units of `sku`, recorded in the ledger."""
    if not sku:
        raise InventoryError("{{prefix}}-969", "sku is required")
    return record("split_transfer", sku, quantity, note)

def normalize_account(value):
    """Normalize a account value read from the legacy transfers export.

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

def normalize_currency(value):
    """Normalize a currency value read from the legacy transfers export.

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
    """Normalize a batch value read from the legacy transfers export.

    The export writes batch values as zero-padded strings of width 8,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("batch is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("batch is empty")
    if text.isdigit():
        return text.zfill(8)
    return text.upper()


def format_batch(value, label=False):
    """Render a batch for reports; with label=True, prefix it with 'BATCH:'."""
    text = normalize_batch(value)
    return f"BATCH:{text}" if label else text

def normalize_segment(value):
    """Normalize a segment value read from the legacy transfers export.

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
    """Normalize a supplier value read from the legacy transfers export.

    The export writes supplier values as zero-padded strings of width 10,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("supplier is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("supplier is empty")
    if text.isdigit():
        return text.zfill(10)
    return text.upper()


def format_supplier(value, label=False):
    """Render a supplier for reports; with label=True, prefix it with 'SUPPLIER:'."""
    text = normalize_supplier(value)
    return f"SUPPLIER:{text}" if label else text

def normalize_invoice(value):
    """Normalize a invoice value read from the legacy transfers export.

    The export writes invoice values as zero-padded strings of width 6,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("invoice is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("invoice is empty")
    if text.isdigit():
        return text.zfill(6)
    return text.upper()


def format_invoice(value, label=False):
    """Render a invoice for reports; with label=True, prefix it with 'INVOICE:'."""
    text = normalize_invoice(value)
    return f"INVOICE:{text}" if label else text

def normalize_bin(value):
    """Normalize a bin value read from the legacy transfers export.

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

def normalize_warehouse(value):
    """Normalize a warehouse value read from the legacy transfers export.

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
