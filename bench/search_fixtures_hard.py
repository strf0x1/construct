"""The harder tier of the search suite (bench/tasks/search-hard-*).

The first tier (search_fixtures.py) was solved by Haiku 5.5 in 17 of 18 runs
with or without a search tool, in 3-5 requests: ~90 files are small enough to
read around in. These repos are ~1,500 files, with the words the questions
turn on sprinkled through hundreds of docstrings, and each answer needs more
than one search: a call chain, a dozen per-name look-ups, a union of access
patterns, references that a search for the name misses (aliased imports,
dotted strings), and a constant that hides a code. The trees are too big to
commit, so each task builds its tree per run (tasklib `generate`); the same
seed gives the same tree, and the answers are computed from it, never from
the generator's bookkeeping.

    python3 bench/search_fixtures.py        (writes these tasks too)
"""

import ast
import json
import random
import re

from search_fixtures import Tree, emit_generated, filler, gitignore

FILES = 1500


def _tracked(t):
    return t.tracked()


# ------------------------------------------------------------------ trace
def trace_tree():
    rng = random.Random(201)
    t = Tree()
    filler(rng, t, FILES, keep_free=("issue_refund", "refund.issued", "audit_events", "cancel_order"), noise=("refund", "audit", "route", "order", "cancel"))
    gitignore(t, "build")
    t.add("pkg/audit/writer.py", '"""Append-only audit trail."""\nfrom pkg.db import execute\n\n\ndef record(action, actor, subject):\n    """Write one row to the audit trail."""\n    execute("INSERT INTO audit_events (action, actor, subject) VALUES (?, ?, ?)", (action, actor, subject))\n')
    t.add("pkg/metrics/writer.py", '"""Counters."""\nfrom pkg.db import execute\n\n\ndef record(action, actor, subject):\n    """Count one event (not the audit trail)."""\n    execute("UPDATE counters SET n = n + 1 WHERE name = ?", (action,))\n')
    t.add("pkg/db.py", "def execute(sql, params=()):\n    return None\n")
    t.add("pkg/ledger/postings.py", "def post_entry(order, amount):\n    return (order, amount)\n")
    t.add(
        "pkg/refunds/service.py",
        '''"""Issue refunds against settled orders."""
from pkg.audit.writer import record
from pkg.ledger.postings import post_entry


def issue_refund(order, amount, actor):
    """Post the refund and audit it."""
    post_entry(order, -amount)
    record(action="refund.issued", actor=actor, subject=order)
    return amount


def issue_refund_dry_run(order, amount, actor):
    """Same checks as issue_refund, nothing written."""
    return amount


def issue_refund_legacy(order, amount, actor):
    """The v1 flow, audited under its own action name."""
    post_entry(order, -amount)
    record(action="refund.issued.v1", actor=actor, subject=order)
    return amount
''',
    )
    t.add(
        "pkg/orders/cancel.py",
        '''"""Cancelling an order."""
from pkg.refunds.service import issue_refund


def _refund_if_paid(order, actor):
    if order.paid:
        return issue_refund(order, order.total, actor)
    return 0


def cancel_order(order, actor):
    """Cancel, refunding what was paid."""
    _refund_if_paid(order, actor)
    order.status = "cancelled"
''',
    )
    t.add("pkg/jobs/nightly.py", "from pkg.refunds.service import issue_refund\n\n\ndef sweep(stale_orders, actor):\n    \"\"\"Refund orders stuck in limbo. A job, not a route.\"\"\"\n    for order in stale_orders:\n        issue_refund(order, order.total, actor)\n")
    t.add("pkg/api/router.py", "ROUTES = []\n\n\ndef route(method, path):\n    def wrap(fn):\n        ROUTES.append((method, path, fn))\n        return fn\n\n    return wrap\n")
    t.add(
        "pkg/api/refund_routes.py",
        '''from pkg.api.router import route
from pkg.refunds.service import issue_refund, issue_refund_dry_run, issue_refund_legacy


@route("POST", "/v2/refunds")
def create_refund(req):
    return issue_refund(req.order, req.amount, req.actor)


@route("POST", "/v1/refunds")
def create_refund_v1(req):
    return issue_refund_legacy(req.order, req.amount, req.actor)


@route("POST", "/v2/refunds/preview")
def preview_refund(req):
    return issue_refund_dry_run(req.order, req.amount, req.actor)
''',
    )
    t.add(
        "pkg/api/order_routes.py",
        '''from pkg.api.router import route
from pkg.orders.cancel import cancel_order


@route("POST", "/v2/orders/{id}/cancel")
def cancel(req):
    return cancel_order(req.order, req.actor)


@route("GET", "/v2/orders/{id}")
def show_order(req):
    return req.order
''',
    )
    for i in range(40):  # routes that touch none of it
        verb, noun = rng.choice(["GET", "POST", "PUT", "DELETE"]), rng.choice(["coupons", "carriers", "labels", "regions", "tenants", "windows"])
        t.add(f"pkg/api/routes_{noun}_{i}.py", f'from pkg.api.router import route\n\n\n@route("{verb}", "/v2/{noun}/{i}")\ndef handle_{noun}_{i}(req):\n    return {{"ok": True}}\n')
    track = _tracked(t)
    # who can reach the audit write: callers of the function that writes "refund.issued", by name, to routes
    funcs = {}
    for p, x in track.items():
        if not p.endswith(".py"):
            continue
        for n in ast.walk(ast.parse(x)):
            if isinstance(n, ast.FunctionDef):
                routes = [(d.args[0].value, d.args[1].value) for d in n.decorator_list if isinstance(d, ast.Call) and getattr(d.func, "id", "") == "route"]
                calls = {c.func.id if isinstance(c.func, ast.Name) else c.func.attr for c in ast.walk(n) if isinstance(c, ast.Call) and isinstance(c.func, (ast.Name, ast.Attribute))}
                funcs.setdefault(n.name, []).append((p, routes, calls, x))
    start = [n for n, defs in funcs.items() if any('action="refund.issued"' in d[3] and n in re.findall(r"def (\w+)", d[3]) for d in defs) and n == "issue_refund"]
    assert start == ["issue_refund"], start
    seen, todo, routes = {"issue_refund"}, ["issue_refund"], set()
    while todo:
        target = todo.pop()
        for name, defs in funcs.items():
            for p, rs, calls, _ in defs:
                if target in calls and name not in seen:
                    seen.add(name)
                    todo.append(name)
                    routes.update(f"{m} {path}" for m, path in rs)
    assert sorted(routes) == ["POST /v2/orders/{id}/cancel", "POST /v2/refunds"], routes
    return t, {"routes": sorted(routes)}


def trace():
    _, expected = trace_tree()
    body = '''p = Path("answer.txt")
got = sorted({" ".join(l.strip().strip("`").split()) for l in p.read_text().splitlines() if l.strip()}) if p.exists() else []
want = EXPECTED["routes"]
hit = len(set(got) & set(want))
Path(os.environ["BENCH_REPORT"]).write_text(json.dumps({"found": hit, "wanted": len(want), "extra": sorted(set(got) - set(want)), "missing": sorted(set(want) - set(got))}))
print(json.dumps({"answered": bool(got), "complete": hit == len(want), "no_extras": not (set(got) - set(want))}))'''
    emit_generated(
        "search-hard-trace", "trace_tree",
        "Audit rows with action `refund.issued` are showing up in the `audit_events` table. List every HTTP route in this repo that can cause one to be written, directly or through other code (a scheduled job is not a route; the `refund.issued.v1` action is a different one). Write one `METHOD /path` per line, sorted, exactly as the route is registered, to answer.txt, and nothing else.",
        expected, body,
        "Follow a write back through a helper, a wrapper and two layers to the routes that reach it, among 1,500 files and look-alike names.",
        ["answer.txt"],
    )


# ------------------------------------------------------------------ unused
LEGACY = "fold_rows pad_sku strip_bom parse_stamp merge_ledgers clip_notes hash_row hash_row_v2 expand_codes swap_keys round_half split_lines join_paths mask_pan count_rows".split()


def unused_tree():
    rng = random.Random(202)
    t = Tree()
    filler(rng, t, FILES, keep_free=("legacy_",), noise=("legacy", "migrate", "export", "import"))
    gitignore(t, "build")
    files = {"rows": ["fold_rows", "split_lines", "count_rows"], "text": ["strip_bom", "clip_notes", "join_paths"], "keys": ["expand_codes", "swap_keys", "pad_sku"], "stamps": ["parse_stamp", "round_half", "mask_pan"], "ledger": ["merge_ledgers", "hash_row", "hash_row_v2"]}
    for mod, names in files.items():
        body = f'"""Old {mod} helpers, kept for the importers."""\n'
        for n in names:
            call = "\n    return legacy_swap_keys(value)" if n == "expand_codes" else "\n    return value"
            body += f"\n\ndef legacy_{n}(value):\n    \"\"\"Old {n.replace('_', ' ')}.\"\"\"{call}\n"
        t.add(f"pkg/legacy/{mod}.py", body)
    t.add("pkg/legacy/__init__.py", "")
    t.add("pkg/sync/replay_legacy.py", "from pkg.legacy.rows import legacy_fold_rows\n\n\ndef replay(rows):\n    return [legacy_fold_rows(r) for r in rows]\n")
    t.add("pkg/catalog/import_job.py", "from pkg.legacy import keys\n\n\ndef run(items):\n    return [keys.legacy_pad_sku(i) for i in items]\n")
    t.add("pkg/reports/export_legacy.py", "def export(rows):\n    # legacy_strip_bom is handled upstream now\n    return rows\n")
    t.add("pkg/registry_legacy.py", 'HANDLERS = {\n    "stamp": "pkg.legacy.stamps.legacy_parse_stamp",\n}\n')
    t.add("scripts/backfill.py", "from pkg.legacy.stamps import legacy_round_half\n\nprint('backfill')\n")
    t.add("pkg/billing/cards_legacy.py", "from pkg.legacy.stamps import legacy_mask_pan\n\n\ndef show(pan):\n    return legacy_mask_pan(pan)\n")
    t.add("pkg/reports/paths_legacy.py", 'def build(parts):\n    """Join like legacy_join_paths used to."""\n    return "/".join(parts)\n')
    t.add("pkg/reports/totals_legacy.py", "def legacy_count_rows_total(rows):\n    return len(rows)\n\n\nTOTAL = legacy_count_rows_total([])\n")
    t.add("pkg/ledger/v2_users.py", "from pkg.legacy.ledger import legacy_hash_row_v2\n\n\ndef h(row):\n    return legacy_hash_row_v2(row)\n")
    t.add("tests/test_legacy.py", "from pkg.legacy.ledger import legacy_merge_ledgers\n\n\ndef test_merge():\n    assert legacy_merge_ledgers([]) == []\n")
    t.add("docs/legacy.md", "`legacy_clip_notes` is slated for removal.\n")
    t.add("build/lib/pkg/legacy_use.py", "from pkg.legacy.keys import legacy_expand_codes\nlegacy_expand_codes(1)\n")
    track = _tracked(t)
    scope = {p: x for p, x in track.items() if p.startswith(("pkg/", "scripts/")) and p.endswith(".py")}
    unref = []
    for n in LEGACY:
        name = f"legacy_{n}"
        refs = 0
        for p, x in scope.items():
            for line in x.split("\n"):
                if re.search(rf"\b{name}\b", line) and not re.match(rf"\s*def {name}\(", line):
                    refs += 1
        if refs == 0:
            unref.append(name)
    assert unref == ["legacy_merge_ledgers", "legacy_clip_notes", "legacy_hash_row", "legacy_expand_codes", "legacy_split_lines", "legacy_count_rows"], unref
    return t, {"unused": sorted(unref)}


def unused():
    _, expected = unused_tree()
    body = '''p = Path("answer.txt")
got = sorted({l.strip().strip("`") for l in p.read_text().splitlines() if l.strip()}) if p.exists() else []
want = EXPECTED["unused"]
hit = len(set(got) & set(want))
Path(os.environ["BENCH_REPORT"]).write_text(json.dumps({"found": hit, "wanted": len(want), "extra": sorted(set(got) - set(want)), "missing": sorted(set(want) - set(got))}))
print(json.dumps({"answered": bool(got), "complete": hit == len(want), "no_extras": not (set(got) - set(want))}))'''
    emit_generated(
        "search-hard-unused", "unused_tree",
        "pkg/legacy/ defines 15 helper functions named `legacy_*`. Which of them are never referenced anywhere else in the repo's source? A function counts as referenced if its exact name appears on any line of a .py file under pkg/ or scripts/ other than its own `def` line (comments, strings and imports count, and so do mentions from other unused code). Mentions in tests/, docs/ and generated output (build/) don't count, and a longer name such as `legacy_x_v2` is a different name. Write the unreferenced function names, one per line, sorted, to answer.txt, and nothing else.",
        expected, body,
        "Fifteen per-name look-ups with mentions in comments, strings, imports, tests, docs, build output and longer names.",
        ["answer.txt"],
    )


# ------------------------------------------------------------------ env
def env_tree():
    rng = random.Random(203)
    t = Tree()
    filler(rng, t, FILES, keep_free=("environ", "getenv", "env_int", "env_str", "env_flag"), noise=("config", "settings", "deploy", "env"))
    gitignore(t, "build")
    t.add(
        "pkg/config/envutil.py",
        '''"""Typed reads of the process environment."""
import os


def env_str(name, default=None):
    return os.environ.get(name, default)


def env_int(name, default=0):
    return int(os.environ.get(name, default))


def env_flag(name):
    return os.environ.get(name, "") == "1"
''',
    )
    mods = {
        "pkg/config/app.py": 'import os\n\nSECRET = os.environ["APP_SECRET"]\nDB = os.environ.get("DB_URL")\nTTL = os.getenv("CACHE_TTL", "60")\n',
        "pkg/config/region.py": 'from os import environ\n\nREGION = environ.get("REGION")\nZONE = environ["ZONE_NAME"]\n',
        "pkg/obs/sentry.py": 'import os\n\nDSN = os.environ.get(\n    "SENTRY_DSN",\n    None,\n)\nENV = os.getenv(\n    \'SENTRY_ENV\'\n)\n',
        "pkg/workers/pool.py": "from pkg.config.envutil import env_int, env_flag\n\nCOUNT = env_int(\"WORKER_COUNT\", 4)\nBETA = env_flag('ENABLE_BETA')\n",
        "pkg/mail/smtp.py": 'from pkg.config.envutil import env_str\n\nHOST = env_str("SMTP_HOST")\nPORT = env_str("SMTP_PORT", "25")\n',
        "pkg/billing/gateway.py": 'import os\n\nMODE = os.getenv(\'PAYMENT_MODE\')\nKEY = os.environ["GATEWAY_KEY"]\n',
        "pkg/search/index.py": 'import os\n\n\ndef host():\n    return os.environ.get("SEARCH_HOST", "localhost")\n',
        "pkg/sync/queue.py": 'from pkg.config import envutil\n\nURL = envutil.env_str("QUEUE_URL")\nDEPTH = envutil.env_int("QUEUE_DEPTH", 10)\n',
        "pkg/reports/render.py": 'import os\n\nif os.getenv("RENDER_DEBUG"):\n    pass\nFONT = os.environ.get("REPORT_FONT")\n',
        "pkg/notify/push.py": 'import os\n\n\ndef token():\n    return os.environ["PUSH_TOKEN"]\n',
        "pkg/audit/sink.py": 'import os\n\nSINK = os.environ.get("AUDIT_SINK", "stdout")\n',
        # not reads of the process environment
        "pkg/config/runtime.py": 'import os\n\nos.environ["PYTHONHASHSEED"] = "0"\nos.environ.setdefault("TZ", "UTC")\nSTAMP = "export OLD_FLAG=1 before running"\n',
        "pkg/config/fake.py": 'class Settings:\n    environ = {}\n\n\nsettings = Settings()\nVALUE = settings.environ.get("NOT_OS_VAR")\nOTHER = settings.environ["ALSO_NOT_OS"]\n',
        "pkg/config/comments.py": "# set TEMP_ONLY=1 in your shell to try this\n# os.environ.get(\"COMMENTED_OUT\")\nX = 1\n",
    }
    for p, x in mods.items():
        t.add(p, x)
    t.add("tests/test_env.py", 'import os\n\n\ndef test_x(monkeypatch):\n    monkeypatch.setenv("TEST_ONLY", "1")\n    os.environ["ANOTHER_TEST_ONLY"] = "1"\n    assert os.environ.get("TEST_READ_ONLY") is None\n')
    t.add("docs/deploy.md", "Set `DEPLOY_DOC_VAR` before starting.\n")
    t.add("build/lib/pkg/old.py", 'import os\nOLD = os.environ["BUILD_ONLY"]\n')
    track = _tracked(t)
    helpers = {"env_str", "env_int", "env_flag"}
    names = set()

    def is_os_environ(node):  # os.environ, or a bare `environ` imported from os
        return (isinstance(node, ast.Attribute) and node.attr == "environ" and isinstance(node.value, ast.Name) and node.value.id == "os") or (isinstance(node, ast.Name) and node.id == "environ")

    for p, x in track.items():
        if not p.endswith(".py") or p.startswith("tests/") or p == "pkg/config/envutil.py":
            continue
        for n in ast.walk(ast.parse(x)):
            if isinstance(n, ast.Subscript) and isinstance(n.ctx, ast.Load) and is_os_environ(n.value) and isinstance(n.slice, ast.Constant):
                names.add(n.slice.value)
            if isinstance(n, ast.Call) and n.args and isinstance(n.args[0], ast.Constant) and isinstance(n.args[0].value, str):
                f = n.func
                if isinstance(f, ast.Attribute) and f.attr == "get" and is_os_environ(f.value):
                    names.add(n.args[0].value)
                elif isinstance(f, ast.Attribute) and f.attr == "getenv" and isinstance(f.value, ast.Name) and f.value.id == "os":
                    names.add(n.args[0].value)
                elif (isinstance(f, ast.Name) and f.id in helpers) or (isinstance(f, ast.Attribute) and f.attr in helpers):
                    names.add(n.args[0].value)
    assert len(names) == 20, sorted(names)
    return t, {"vars": sorted(names)}


def env():
    _, expected = env_tree()
    body = '''p = Path("answer.txt")
got = sorted({l.strip().strip("`") for l in p.read_text().splitlines() if l.strip()}) if p.exists() else []
want = EXPECTED["vars"]
hit = len(set(got) & set(want))
Path(os.environ["BENCH_REPORT"]).write_text(json.dumps({"found": hit, "wanted": len(want), "extra": sorted(set(got) - set(want)), "missing": sorted(set(want) - set(got))}))
print(json.dumps({"answered": bool(got), "complete": hit == len(want), "no_extras": not (set(got) - set(want))}))'''
    emit_generated(
        "search-hard-env", "env_tree",
        "Which environment variables does the service read? List the name of every variable that source code under pkg/ reads from the process environment: `os.environ[...]`, `os.environ.get(...)`, `os.getenv(...)`, `environ` imported from os, and the typed helpers `env_str`, `env_int` and `env_flag` in pkg/config/envutil.py. Writes such as `os.environ[X] = ...` or `setdefault`, tests, docs, comments, strings, generated output (build/) and objects that merely have an attribute called `environ` do not count. Write one variable name per line, sorted, to answer.txt, and nothing else.",
        expected, body,
        "Union of six access patterns (including wrappers, aliases and multi-line calls) against decoys that look alike.",
        ["answer.txt"],
    )


# ------------------------------------------------------------------ rename
def rename_tree():
    rng = random.Random(204)
    t = Tree()
    filler(rng, t, FILES, keep_free=("compute_rate", "quote_rate"), noise=("pricing", "rate", "plan", "quote"))
    gitignore(t, "build")
    t.add("pkg/pricing/rates.py", '"""Price a plan."""\n\n_UNIT = {"free": 0, "pro": 12, "team": 9}\n\n\ndef compute_rate(plan, units):\n    """The price of ``units`` on ``plan``."""\n    return _UNIT[plan] * units\n\n\ndef compute_rate_card(plan):\n    """A plan\'s rate card: price at 1, 10 and 100 units."""\n    return {n: compute_rate(plan, n) for n in (1, 10, 100)}\n')
    t.add("pkg/pricing/__init__.py", 'from pkg.pricing.rates import compute_rate, compute_rate_card\n\n__all__ = ["compute_rate", "compute_rate_card"]\n')
    for i, area in enumerate(["billing", "catalog", "orders", "shipping", "accounts", "reports", "search", "notify"]):
        t.add(f"pkg/{area}/priced_{i}.py", f"from pkg.pricing import compute_rate\n\n\ndef price_{area}(plan, units):\n    return compute_rate(plan, units) + {i}\n")
    t.add("pkg/orders/priced_alias.py", "from pkg.pricing.rates import compute_rate as rate_fn\n\n\ndef price_alias(plan, units):\n    return rate_fn(plan, units) * 2\n")
    t.add("pkg/reports/priced_alias2.py", "from pkg.pricing.rates import compute_rate as unit_price\n\n\ndef totals(plans):\n    return [unit_price(p, 1) for p in plans]\n")
    t.add("pkg/pricing/dynamic.py", 'from pkg.pricing import rates\n\n\ndef by_name(plan, units):\n    return getattr(rates, "compute_rate")(plan, units)\n')
    t.add(
        "pkg/registry.py",
        '''"""Handlers looked up by name, from code and from config/handlers.json."""
import importlib
import json
from pathlib import Path

HANDLERS = {
    "rate": "pkg.pricing.rates:compute_rate",
    "card": "pkg.pricing.rates:compute_rate_card",
}


def resolve(spec):
    """``module:attr`` or ``module.attr`` to the object."""
    module, _, attr = spec.replace(":", ".").rpartition(".")
    return getattr(importlib.import_module(module), attr)


def from_config(root="."):
    data = json.loads((Path(root) / "config" / "handlers.json").read_text())
    return {name: resolve(entry["handler"]) for name, entry in data.items()}
''',
    )
    t.add("config/handlers.json", json.dumps({"rate": {"handler": "pkg.pricing.rates.compute_rate", "units": "seat"}, "card": {"handler": "pkg.pricing.rates.compute_rate_card"}}, indent=2) + "\n")
    t.add(
        "tests/test_pricing.py",
        '''import unittest

from pkg import registry
from pkg.billing.priced_0 import price_billing
from pkg.orders.priced_alias import price_alias
from pkg.pricing.dynamic import by_name
from pkg.reports.priced_alias2 import totals


class T(unittest.TestCase):
    def test_callers(self):
        self.assertEqual(price_billing("pro", 2), 24)
        self.assertEqual(price_alias("team", 2), 36)
        self.assertEqual(by_name("pro", 1), 12)
        self.assertEqual(totals(["pro", "team"]), [12, 9])

    def test_registry(self):
        self.assertEqual(registry.resolve(registry.HANDLERS["rate"])("pro", 3), 36)
        self.assertEqual(registry.resolve(registry.HANDLERS["card"])("pro")[10], 120)
        loaded = registry.from_config(".")
        self.assertEqual(loaded["rate"]("team", 2), 18)
        self.assertEqual(loaded["card"]("team")[1], 9)
''',
    )
    t.add("tests/__init__.py", "")
    t.add("docs/pricing.md", "Call `compute_rate(plan, units)` for a price. Rate cards come from `compute_rate_card`.\n")
    t.add("CHANGELOG.md", "## 2.0\n- Added compute_rate.\n")
    t.add("build/lib/pkg/pricing/rates.py", "def compute_rate(plan, units):\n    return 0\n")
    track = _tracked(t)
    sites = sum(len(re.findall(r"\bcompute_rate\b", x)) for p, x in track.items() if p != "CHANGELOG.md")
    assert sites >= 24, sites
    return t, {"sites": sites}


def rename():
    _, expected = rename_tree()
    body = '''def read(p):
    return Path(p).read_text() if Path(p).exists() else ""

remaining = 0
for p in subprocess.run(["git", "ls-files"], capture_output=True, text=True).stdout.split():
    if p != "CHANGELOG.md" and p.endswith((".py", ".md", ".json")):
        remaining += len(re.findall(r"\\bcompute_rate\\b", read(p)))
rates = read("pkg/pricing/rates.py")
tests = subprocess.run([sys.executable, "-m", "unittest", "discover", "-s", "tests", "-t", "."], capture_output=True, text=True)
print(json.dumps({
    "renamed": "def quote_rate(" in rates,
    "no_old_name_left": remaining == 0,
    "card_untouched": "def compute_rate_card(" in rates and "pkg.pricing.rates:compute_rate_card" in read("pkg/registry.py") and "pkg.pricing.rates.compute_rate_card" in read("config/handlers.json"),
    "changelog_untouched": "Added compute_rate." in read("CHANGELOG.md"),
    "tests_pass": tests.returncode == 0,
}))'''
    emit_generated(
        "search-hard-rename", "rename_tree",
        "Rename the function `compute_rate` to `quote_rate` everywhere it is referenced: its definition, imports (including aliased ones), calls, `__all__`, and every place that refers to it by name as a string or a dotted path (the registry, config files, getattr). Docs too. Do not touch `compute_rate_card` (a different function), CHANGELOG.md (history), or generated output in build/. Run the tests (`python3 -m unittest discover -s tests -t .`) when you are done.",
        expected, body,
        "A rename where aliased imports, string and dotted-path references, and a look-alike function defeat a plain search-and-replace.",
        ["pkg/*", "tests/*", "docs/*", "config/*"], budget=(100, 3_000_000), timeout=900,
    )


# ------------------------------------------------------------------ errorcode
def errorcode_tree():
    rng = random.Random(205)
    t = Tree()
    filler(rng, t, FILES, keep_free=("E4417", "E4471", "ERR_QUOTA"), noise=("quota", "limit", "upload", "error", "plan"))
    gitignore(t, "build")
    t.add("pkg/errors/codes.py", '"""Error codes shown to customers."""\n\nERR_RATE_LIMIT = "E4290"\nERR_QUOTA_EXCEEDED = "E4417"\nERR_QUOTA_SOFT = "E4471"\nERR_NOT_FOUND = "E4040"\n')
    t.add("pkg/errors/__init__.py", "")
    t.add("pkg/errors/base.py", "class AppError(Exception):\n    def __init__(self, code, detail=''):\n        super().__init__(code, detail)\n        self.code = code\n")
    t.add("pkg/uploads/limits.py", 'from pkg.errors import codes\nfrom pkg.errors.base import AppError\n\n\ndef check_upload_quota(user, size):\n    """Refuse an upload that would put the user over their plan."""\n    if user.used + size > user.plan_bytes:\n        raise AppError(codes.ERR_QUOTA_EXCEEDED, "plan storage full")\n')
    t.add("pkg/sync/limits.py", 'from pkg.errors.base import AppError\nfrom pkg.errors.codes import ERR_QUOTA_EXCEEDED as QUOTA_CODE\n\n\ndef check_sync_quota(user, n):\n    if user.synced + n > user.plan_items:\n        raise AppError(QUOTA_CODE, "sync limit")\n')
    t.add("pkg/exports/limits.py", 'from pkg.errors import codes\nfrom pkg.errors.base import AppError\n\n\ndef check_export_quota(user, n):\n    """Soft limit: warn before the hard one."""\n    if user.exports + n > user.soft_exports:\n        raise AppError(codes.ERR_QUOTA_SOFT, "close to the export limit")\n')
    t.add("pkg/api/guards.py", 'from pkg.errors import codes\nfrom pkg.errors.base import AppError\n\n\ndef throttle(user):\n    if user.hits > 100:\n        raise AppError(codes.ERR_RATE_LIMIT)\n')
    t.add("web/errors.js", 'export const MESSAGES = {\n  E4417: "Your plan is full.",\n  E4471: "You are close to a limit.",\n  E4290: "Slow down.",\n};\n')
    t.add("docs/errors.md", "| Code | Meaning |\n|---|---|\n| E4417 | quota exceeded |\n| E4471 | soft quota |\n")
    t.add("tests/test_limits.py", "def test_codes():\n    assert 'E4417' != 'E4471'\n")
    track = _tracked(t)
    raisers = []
    for p, x in track.items():
        if not (p.startswith("pkg/") and p.endswith(".py")):
            continue
        tree = ast.parse(x)
        aliases = {a.asname for n in ast.walk(tree) if isinstance(n, ast.ImportFrom) for a in n.names if a.name == "ERR_QUOTA_EXCEEDED" and a.asname}
        for fn in [n for n in ast.walk(tree) if isinstance(n, ast.FunctionDef)]:
            for r in [n for n in ast.walk(fn) if isinstance(n, ast.Raise)]:
                src = ast.unparse(r)
                if "ERR_QUOTA_EXCEEDED" in src or any(a in src for a in aliases):
                    raisers.append(f"{p}:{fn.name}")
    assert sorted(raisers) == ["pkg/sync/limits.py:check_sync_quota", "pkg/uploads/limits.py:check_upload_quota"], raisers
    return t, {"answer": "pkg/uploads/limits.py:check_upload_quota", "all_raisers": sorted(raisers)}


def errorcode():
    _, expected = errorcode_tree()
    body = '''a = Path("answer.txt").read_text().split() if Path("answer.txt").exists() else []
got = a[0].strip("`") if a else ""
print(json.dumps({"answered": bool(got), "correct": got == EXPECTED["answer"]}))'''
    emit_generated(
        "search-hard-errorcode", "errorcode_tree",
        "Customers who upload a file that would take them over their plan's storage limit are shown error code E4417. Which function raises it for that case? Write `path:function_name` (repo-relative path) to answer.txt, and nothing else.",
        expected, body,
        "Code to constant to raise sites, one reached through an aliased import, with a transposed-digits code (E4471) as a decoy.",
        ["answer.txt"],
    )


ALL = (trace, unused, env, rename, errorcode)
