#!/usr/bin/env bash
# Scores the work dir: the suite passes, tests untouched, each planted bug fixed.
exec "${BENCH_PYTHON:-python3}" - <<'PY'
import json, subprocess, sys

checks = {}
r = subprocess.run([sys.executable, "-m", "unittest", "-q"], capture_output=True, timeout=120)
checks["tests_pass"] = r.returncode == 0
r = subprocess.run(["git", "diff", "--quiet", "HEAD", "--", "tests"])
checks["tests_untouched"] = r.returncode == 0
probe = {
    "money": "from shop.money import round_cents; from decimal import Decimal as D; assert round_cents('2.675') == D('2.68')",
    "inventory": "from shop import inventory as i; i.reset(); i.receive('x', 1); assert i.reserve('x', 1)",
    "pricing": "from shop.pricing import bulk_discount as b; from decimal import Decimal as D; assert b(1000) == D('0.15') and b(55) == D('0.10')",
    "shipping": "from shop.shipping import parcel_weight as w; assert abs(w([(2.0, 4)]) - 8.25) < 1e-9",
    "tax": "from shop.tax import rate_for as r; from decimal import Decimal as D; assert r('tx') == D('0.0625') and r('Or') == D('0')",
    "reports": "from shop.reports import top_customers as t; assert t({'a': 1, 'b': 9, 'c': 5}, 1) == [('b', 9)]",
}
for name, code in probe.items():
    checks[f"fixed_{name}"] = subprocess.run([sys.executable, "-c", code], capture_output=True, timeout=30).returncode == 0
print(json.dumps(checks))
PY
