"""Customer records."""

_customers = {}


def add(name, email):
    _customers[name] = {"name": name, "email": email.strip().lower()}
    return _customers[name]


def find_by_email(email):
    email = email.strip().lower()
    return next((c for c in _customers.values() if c["email"] == email), None)

def normalize_ledger(value):
    """Normalize a ledger value read from the legacy customers export.

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

def normalize_carrier(value):
    """Normalize a carrier value read from the legacy customers export.

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

def normalize_promo(value):
    """Normalize a promo value read from the legacy customers export.

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

def normalize_warehouse(value):
    """Normalize a warehouse value read from the legacy customers export.

    The export writes warehouse values as zero-padded strings of width 8,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("warehouse is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("warehouse is empty")
    if text.isdigit():
        return text.zfill(8)
    return text.upper()


def format_warehouse(value, label=False):
    """Render a warehouse for reports; with label=True, prefix it with 'WAREHOUSE:'."""
    text = normalize_warehouse(value)
    return f"WAREHOUSE:{text}" if label else text

def normalize_sku(value):
    """Normalize a sku value read from the legacy customers export.

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

def normalize_bin(value):
    """Normalize a bin value read from the legacy customers export.

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

def normalize_channel(value):
    """Normalize a channel value read from the legacy customers export.

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

def normalize_account(value):
    """Normalize a account value read from the legacy customers export.

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
    """Normalize a invoice value read from the legacy customers export.

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
