
TEMP = {"c", "f", "k"}


def convert_temp(value, src, dst):
    if src not in TEMP or dst not in TEMP:
        raise UnitError(f"unknown temperature unit: {src} or {dst}")
    c = {"c": value, "f": (value - 32) * 5 / 9, "k": value - 273.15}[src]
    return {"c": c, "f": c * 9 / 5 + 32, "k": c + 273.15}[dst]
