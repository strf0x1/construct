from inventory import low_stock, total_quantity


def report(items):
    lines = [f"total: {total_quantity(items)}"]
    for i in low_stock(items):
        lines.append(f"low: {i['name']}")
    return "\n".join(lines)
