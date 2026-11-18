"""The gate counts calls before dispatch, including side calls and failures."""

from decimal import Decimal
from concurrent.futures import ThreadPoolExecutor
import http.client
import http.server
import json
from pathlib import Path
import socket
import tempfile
import threading
import time
import unittest

import swebench_proxy as proxy
from swebench_orca import RESERVATION, PRICING, parse_receipt


class UnixConnection(http.client.HTTPConnection):
    def __init__(self, path):
        super().__init__("localhost", timeout=3)
        self.path = str(path)

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX)
        self.sock.settimeout(self.timeout)
        self.sock.connect(self.path)


class Upstream(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        self.server.requests.append(request)
        if self.server.fail:
            self.send_response(503)
            body = b"{}"
        else:
            self.send_response(200)
            body = (b'data: {"usage":{"prompt_tokens":5,"completion_tokens":2,"cost_usd":0.01}}\n\n'
                    b'data: [DONE]\n\n')
        self.send_header("Content-Type", "text/event-stream")
        if not self.server.fail:
            self.send_header("x-orca-request-id", f"receipt-{len(self.server.requests)}")
        self.end_headers()
        self.wfile.write(body)


class ReceiptEndpoint:
    def configure(self, request):
        pass

    def receipt(self, request_id, model):
        return proxy.Receipt(Decimal("0.10"), 7) if request_id else None


class BudgetTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        self.path = Path(temp.name)
        self.server = http.server.HTTPServer(("127.0.0.1", 0), Upstream)
        self.server.requests = []
        self.server.fail = False
        thread = threading.Thread(target=self.server.serve_forever)
        thread.start()
        self.addCleanup(self.server.server_close)
        self.addCleanup(thread.join)
        self.addCleanup(self.server.shutdown)

    def gate(self, spend=None, **limits):
        settings = {"requests": 9, "tokens": 1000, "seconds": 10, "context": 256, "output": 8, **limits}
        spend = spend or proxy.Spend(Decimal("0.22"), Decimal("0.12"))
        budget = proxy.Budget(proxy.Limits(**settings), self.path / "ledger", spend)
        guard = proxy.guard(self.path / "socket", self.server.server_port, "pinned", budget, provider=ReceiptEndpoint())
        return budget, guard

    def call(self, path=proxy.CHAT, **fields):
        conn = UnixConnection(self.path / "socket")
        try:
            conn.request("POST", path, json.dumps({"model": "pinned", "messages": [], **fields}),
                         {"Authorization": "Bearer caller-secret"})
            response = conn.getresponse()
            return response.status, response.read()
        finally:
            conn.close()

    def test_caps_total_spend_before_dispatch(self):
        budget, guard = self.gate()
        with guard:
            self.assertEqual(self.call(tools=[])[0], 200)
        second, guard = self.gate(budget.spend)
        with guard:
            self.assertEqual(self.call(max_tokens=400)[0], 200)
            status, body = self.call()
            self.assertEqual(status, 403)
            self.assertEqual(json.loads(body)["error"]["code"], "spend_limit")
        self.assertEqual(len(self.server.requests), 2)
        self.assertEqual(self.server.requests[1]["max_tokens"], 8)
        self.assertEqual(second.spend.settled, Decimal("0.20"))
        self.assertEqual(second.spend.reserved, 0)
        self.assertEqual(second.tokens + budget.tokens, 14)

    def test_rejects_unpinned_model_or_endpoint(self):
        _, guard = self.gate()
        with guard:
            self.assertEqual(self.call(model="other")[0], 400)
            self.assertEqual(self.call(path="/v1/generation")[0], 404)
            self.assertEqual(self.call(n=5)[0], 400)
            self.assertEqual(self.call(tools=[{"type": "web_search"}])[0], 400)
            self.assertEqual(self.call(messages=[{"content": [{"type": "image_url"}]}])[0], 400)
        self.assertEqual(self.server.requests, [])

    def test_missing_receipt_holds_reservation_and_blocks_later_tasks(self):
        self.server.fail = True
        budget, guard = self.gate()
        with guard:
            self.assertEqual(self.call()[0], 502)
            self.assertEqual(self.call()[0], 403)
        self.assertEqual(len(self.server.requests), 1)
        self.assertEqual(budget.spend.reserved, Decimal("0.12"))
        self.assertEqual(budget.spend.settled, 0)
        self.assertEqual(budget.tokens, 256)
        self.assertEqual(budget.stop, "unaccounted_request")
        _, guard = self.gate(budget.spend)
        with guard:
            self.assertEqual(self.call()[0], 403)
        self.assertEqual(len(self.server.requests), 1)

    def test_duplicate_receipt_cannot_release_a_second_reservation(self):
        budget, guard = self.gate()
        with guard:
            self.assertEqual(self.call()[0], 200)
        self.server.requests.clear()
        second, guard = self.gate(budget.spend)
        with guard:
            self.assertEqual(self.call()[0], 502)
        self.assertEqual(second.spend.reserved, Decimal("0.12"))
        self.assertEqual(second.spend.settled, Decimal("0.10"))

    def test_request_token_and_time_admission(self):
        for settings, reason in [({"requests": 1}, "request_limit"), ({"tokens": 260}, "token_limit"), ({}, "time_limit")]:
            with self.subTest(reason=reason):
                budget, guard = self.gate(**settings)
                with guard:
                    self.assertEqual(self.call()[0], 200)
                    if reason == "time_limit":
                        budget.deadline = 0
                    status, body = self.call()
                    self.assertEqual(status, 403)
                    self.assertEqual(json.loads(body)["error"]["code"], reason)

    def test_paid_reservation_covers_peak_full_window_and_output(self):
        input_cost = PRICING["context_tokens"] * Decimal(PRICING["peak_uncached_input_usd_per_million"])
        output_cost = PRICING["maximum_output_tokens"] * Decimal(PRICING["peak_output_usd_per_million"])
        self.assertGreater(RESERVATION, (input_cost + output_cost) / 1_000_000)

    def test_parallel_admission_waits_for_settlement(self):
        spend = proxy.Spend(Decimal("1"), Decimal("0.85"))
        deadline = time.monotonic() + 5
        waiting = threading.Event()
        original_wait = spend.changed.wait

        def wait(timeout):
            waiting.set()
            return original_wait(timeout)

        spend.changed.wait = wait
        self.assertIsNone(spend.reserve(deadline))
        with ThreadPoolExecutor(max_workers=1) as pool:
            pending = pool.submit(spend.reserve, deadline)
            self.assertTrue(waiting.wait(2), "parallel reservation never waited for credit")
            self.assertEqual(spend.snapshot()["reserved_usd"], "0.85")
            spend.finish(Decimal("0.10"), "first")
            self.assertIsNone(pending.result(timeout=2))
        self.assertEqual(spend.snapshot(), {"settled_usd": "0.10", "reserved_usd": "0.85", "spend_stop": None})

    def test_receipt_is_bound_to_request_model_currency_and_token_count(self):
        sample = {"id": "id-1", "model": "pinned", "cost_currency": "USD", "total_cost": 0.11,
                  "tokens_prompt": 4, "tokens_completion": 3, "tokens_total": 7}
        self.assertEqual(parse_receipt(json.dumps({"data": sample}), "id-1", "pinned"),
                         proxy.Receipt(Decimal("0.11"), 7))
        for field, wrong in (("id", "id-2"), ("model", "other"), ("cost_currency", "EUR"),
                             ("tokens_total", 8), ("total_cost", -0.1), ("total_cost", True)):
            with self.subTest(field=field):
                self.assertIsNone(parse_receipt(json.dumps({"data": sample | {field: wrong}}), "id-1", "pinned"))
