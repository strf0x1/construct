def parse_invoice(
    raw: str,
    currency: str = "usd",
) -> dict:
    """Two parameters, annotated, over several lines."""
    return {"raw": raw, "currency": currency}


async def parse_invoice_async(raw, currency):
    """Two parameters, async."""
    return raw
