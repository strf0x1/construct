"""Unit conversions for the kitchen. Each table maps a unit to its size in
the base unit (grams for mass, millilitres for volume)."""

MASS = {"g": 1.0, "kg": 1000.0, "oz": 28.349523125, "lb": 453.59237}
VOLUME = {"ml": 1.0, "l": 1000.0, "tsp": 4.92892159375, "tbsp": 14.78676478125, "cup": 236.5882365}


class UnitError(ValueError):
    pass


def _table(unit):
    for t in (MASS, VOLUME):
        if unit in t:
            return t
    raise UnitError(f"unknown unit: {unit}")


def convert(value: float, src: str, dst: str) -> float:
    t = _table(src)
    if dst not in t:
        raise UnitError(f"can't convert {src} to {dst}")
    return value * t[src] / t[dst]
