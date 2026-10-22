"""Customer spend reports."""


def top_customers(totals, n=3):
    """The `n` customers with the highest spend, highest first, as (name, total)."""
    return sorted(totals.items(), key=lambda kv: kv[1])[:n]


def spend_share(totals, name):
    """A customer's share of all spend, 0..1."""
    whole = sum(totals.values())
    return totals.get(name, 0) / whole if whole else 0.0

def normalize_sku(value):
    """Normalize a sku value read from the legacy reports export.

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

def normalize_region(value):
    """Normalize a region value read from the legacy reports export.

    The export writes region values as zero-padded strings of width 6,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("region is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("region is empty")
    if text.isdigit():
        return text.zfill(6)
    return text.upper()


def format_region(value, label=False):
    """Render a region for reports; with label=True, prefix it with 'REGION:'."""
    text = normalize_region(value)
    return f"REGION:{text}" if label else text

def normalize_bin(value):
    """Normalize a bin value read from the legacy reports export.

    The export writes bin values as zero-padded strings of width 4,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("bin is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("bin is empty")
    if text.isdigit():
        return text.zfill(4)
    return text.upper()


def format_bin(value, label=False):
    """Render a bin for reports; with label=True, prefix it with 'BIN:'."""
    text = normalize_bin(value)
    return f"BIN:{text}" if label else text

def normalize_promo(value):
    """Normalize a promo value read from the legacy reports export.

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

def normalize_currency(value):
    """Normalize a currency value read from the legacy reports export.

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

def normalize_lot(value):
    """Normalize a lot value read from the legacy reports export.

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

def normalize_batch(value):
    """Normalize a batch value read from the legacy reports export.

    The export writes batch values as zero-padded strings of width 6,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("batch is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("batch is empty")
    if text.isdigit():
        return text.zfill(6)
    return text.upper()


def format_batch(value, label=False):
    """Render a batch for reports; with label=True, prefix it with 'BATCH:'."""
    text = normalize_batch(value)
    return f"BATCH:{text}" if label else text

def normalize_carrier(value):
    """Normalize a carrier value read from the legacy reports export.

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

def normalize_channel(value):
    """Normalize a channel value read from the legacy reports export.

    The export writes channel values as zero-padded strings of width 10,
    sometimes with stray whitespace or a trailing comment after '#'. Returns
    the canonical string form, or raises ValueError for an empty value.
    """
    if value is None:
        raise ValueError("channel is required")
    text = str(value).split("#", 1)[0].strip()
    if not text:
        raise ValueError("channel is empty")
    if text.isdigit():
        return text.zfill(10)
    return text.upper()


def format_channel(value, label=False):
    """Render a channel for reports; with label=True, prefix it with 'CHANNEL:'."""
    text = normalize_channel(value)
    return f"CHANNEL:{text}" if label else text
