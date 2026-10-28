"""Money amounts. Everything is a Decimal with two places; floats never touch
an amount, because 0.1 + 0.2 is not 0.3."""

from decimal import ROUND_HALF_EVEN, Decimal, InvalidOperation

CENT = Decimal("0.01")


class MoneyError(ValueError):
    pass


def parse(text: str) -> Decimal:
    """'12.5' -> Decimal('12.50'); accepts a leading '-' and thousands commas."""
    cleaned = text.strip().replace(",", "")
    sign = ""
    if cleaned[:1] in "+-" and cleaned:
        sign, cleaned = cleaned[0], cleaned[1:]
    if cleaned.startswith("$"):
        cleaned = cleaned[1:]
    cleaned = sign + cleaned
    try:
        value = Decimal(cleaned)
    except InvalidOperation:
        raise MoneyError(f"not an amount: {text!r}") from None
    return quantize(value)


def quantize(value: Decimal) -> Decimal:
    return value.quantize(CENT, rounding=ROUND_HALF_EVEN)


def fmt(value: Decimal) -> str:
    """Decimal('1234.5') -> '1,234.50'; negatives keep their sign."""
    q = quantize(value)
    sign = "-" if q < 0 else ""
    whole, _, frac = f"{abs(q):.2f}".partition(".")
    groups = []
    while len(whole) > 3:
        groups.insert(0, whole[-3:])
        whole = whole[:-3]
    groups.insert(0, whole)
    return f"{sign}{','.join(groups)}.{frac}"


def split(amount: Decimal, parts: int) -> list:
    """Split `amount` into `parts` shares."""
    if parts < 1:
        raise MoneyError("parts must be at least 1")
    share = quantize(amount / parts)
    return [share] * parts


def total(values) -> Decimal:
    return quantize(sum(values, Decimal("0")))
