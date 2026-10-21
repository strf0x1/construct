def modern(values):
    total = 0
    for v in values:
        total += v
    return total


def legacy(values):
  total = 0
  for v in values:
    if v is None:
      continue
    total += v
  return total


def other(values):
    return [v for v in values if v]
