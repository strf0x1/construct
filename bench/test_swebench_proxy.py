"""The gate counts calls before dispatch, including side calls and failures."""

from decimal import Decimal
import http.client
import http.server
import json
from pathlib import Path
import socket
import tempfile
import threading
import unittest

import swebench_proxy as proxy


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
            body = (b'data: {"usage":{"prompt_tokens":5,"completion_tokens":2,"cost_usd":0.10}}\n\n'
                    b'data: [DONE]\n\n')
        self.send_header("Content-Type", "text/event-stream")
        self.end_headers()
        self.wfile.write(body)


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

    def gate(self, **limits):
        settings = {"requests": 9, "tokens": 100, "seconds": 10, "context": 20, "output": 8,
                    "usd": Decimal("0.22"), "request_usd": Decimal("0.12"), **limits}
        budget = proxy.Budget(proxy.Limits(**settings), self.path / "ledger", scripted=False)
        guard = proxy.guard(self.path / "socket", self.server.server_port, "pinned", budget)
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
            self.assertEqual(self.call(max_tokens=400)[0], 200)
            status, body = self.call()
            self.assertEqual(status, 403)
            self.assertEqual(json.loads(body)["error"]["code"], "spend_limit")
        self.assertEqual(len(self.server.requests), 2)
        self.assertEqual(self.server.requests[1]["max_tokens"], 8)
        self.assertEqual(budget.usd, Decimal("0.20"))
        self.assertEqual(budget.tokens, 14)

    def test_rejects_unpinned_model_or_endpoint(self):
        _, guard = self.gate()
        with guard:
            self.assertEqual(self.call(model="other")[0], 400)
            self.assertEqual(self.call(path="/v1/generation")[0], 404)
            self.assertEqual(self.call(n=5)[0], 400)
            self.assertEqual(self.call(tools=[{"type": "web_search"}])[0], 400)
            self.assertEqual(self.call(messages=[{"content": [{"type": "image_url"}]}])[0], 400)
        self.assertEqual(self.server.requests, [])

    def test_missing_usage_preserves_reservation_and_blocks_retry(self):
        self.server.fail = True
        budget, guard = self.gate()
        with guard:
            self.assertEqual(self.call()[0], 503)
            self.assertEqual(self.call()[0], 403)
        self.assertEqual(len(self.server.requests), 1)
        self.assertEqual(budget.usd, Decimal("0.12"))
        self.assertEqual(budget.tokens, 20)
        self.assertEqual(budget.stop, "unaccounted_request")

    def test_request_token_and_time_admission(self):
        for settings, reason in [({"requests": 1}, "request_limit"), ({"tokens": 26}, "token_limit"), ({}, "time_limit")]:
            with self.subTest(reason=reason):
                budget, guard = self.gate(**settings)
                with guard:
                    self.assertEqual(self.call()[0], 200)
                    if reason == "time_limit":
                        budget.deadline = 0
                    status, body = self.call()
                    self.assertEqual(status, 403)
                    self.assertEqual(json.loads(body)["error"]["code"], reason)
