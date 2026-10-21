from inventory import low_stock, total_qty


def report(items):
    lines = [f"total: {total_qty(items)}"]
    for i in low_stock(items):
        lines.append(f"low: {i['name']}")
    return "\n".join(lines)
