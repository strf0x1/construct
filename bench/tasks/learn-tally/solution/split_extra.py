
def split(amount: Decimal, parts: int) -> list:
    if parts < 1:
        raise MoneyError("parts must be at least 1")
    cents = int(quantize(amount) * 100)
    q, r = divmod(abs(cents), parts)
    sign = -1 if cents < 0 else 1
    return [Decimal(sign * (q + (1 if i < r else 0))) / 100 for i in range(parts)]
