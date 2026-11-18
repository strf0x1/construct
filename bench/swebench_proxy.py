"""Budget the task container's only route to the host recording endpoint."""

import contextlib
import dataclasses
from decimal import Decimal
import http.client
import http.server
import json
import os
import socketserver
import threading
import time
from pathlib import Path

CHAT = "/v1/chat/completions"
MAX_REQUEST_BYTES = 1 << 20
MAX_RESPONSE_BYTES = 8 << 20


@dataclasses.dataclass(frozen=True)
class Limits:
    requests: int
    tokens: int
    seconds: int
    context: int
    output: int

    def __post_init__(self):
        if (min(self.requests, self.tokens, self.seconds, self.context, self.output) <= 0
                or self.output >= self.context):
            raise ValueError("invalid benchmark limits")


class Spend:
    """Track settled spend and any outstanding reservation across all tasks."""

    def __init__(self, limit, per_request):
        if not all(d.is_finite() and d >= 0 for d in (limit, per_request)) or per_request > limit:
            raise ValueError("invalid spend limits")
        self.limit = limit
        self.per_request = per_request
        self.settled = self.reserved = Decimal(0)
        self.stop = None
        self.receipts = set()
        self.changed = threading.Condition()

    def available(self):
        with self.changed:
            return self.stop is None and self.settled + self.per_request <= self.limit

    def snapshot(self):
        with self.changed:
            return {"settled_usd": str(self.settled), "reserved_usd": str(self.reserved), "spend_stop": self.stop}

    def reserve(self, deadline):
        with self.changed:
            while True:
                if self.stop:
                    return self.stop
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    return "time_limit"
                if self.settled + self.per_request > self.limit:
                    return "spend_limit"
                if self.settled + self.reserved + self.per_request <= self.limit:
                    self.reserved += self.per_request
                    return None
                self.changed.wait(remaining)

    def block(self, reason):
        with self.changed:
            self.stop = self.stop or reason
            self.changed.notify_all()

    def finish(self, cost, request_id):
        with self.changed:
            if request_id and request_id in self.receipts:
                self.stop = self.stop or "duplicate_receipt"
                cost = None
            if request_id:
                self.receipts.add(request_id)
            if cost is None:
                self.stop = self.stop or "unaccounted_request"
            else:
                self.reserved -= self.per_request
                self.settled += cost
                if cost > self.per_request:
                    self.stop = self.stop or "cost_reservation_exceeded"
            self.changed.notify_all()
            return cost, self.stop


@dataclasses.dataclass(frozen=True)
class Receipt:
    usd: Decimal
    tokens: int


class Budget:
    """Reserve before dispatch. A failed or unaccounted call retains its reservation."""

    def __init__(self, limits, ledger, spend):
        self.limits = limits
        self.spend = spend
        self.ledger = ledger
        self.deadline = time.monotonic() + limits.seconds
        self.requests = self.tokens = 0
        self.usd = Decimal(0)
        self.stop = None

    def log(self, event, **fields):
        spend = self.spend.snapshot()
        with self.ledger.open("a") as stream:
            stream.write(json.dumps({"event": event, "requests": self.requests, "tokens": self.tokens,
                                     "usd": str(self.usd), "run_settled_usd": spend["settled_usd"],
                                     "run_reserved_usd": spend["reserved_usd"], **fields}) + "\n")
            stream.flush()
            if event == "admitted" and self.spend.per_request:
                os.fsync(stream.fileno())

    def admit(self, reserve_tokens):
        cap = self.limits
        reason = self.stop
        if not reason:
            if time.monotonic() >= self.deadline:
                reason = "time_limit"
            elif self.requests >= cap.requests:
                reason = "request_limit"
            elif self.tokens + reserve_tokens > cap.tokens:
                reason = "token_limit"
            else:
                reason = self.spend.reserve(self.deadline)
        if reason:
            self.stop = reason
            self.log("rejected", reason=reason)
            return False
        self.requests += 1
        self.tokens += reserve_tokens
        try:
            self.log("admitted", reserved_tokens=reserve_tokens)
        except OSError:
            self.spend.block("ledger_error")
            raise
        return True

    def finish(self, usage, status, receipt, reserve_tokens, request_id=None, error=None):
        tokens = receipt.tokens if receipt else sum_usage(usage)
        cost, spend_stop = self.spend.finish(receipt.usd if receipt else None, request_id)
        if tokens is not None:
            self.tokens += tokens - reserve_tokens
        if cost is not None:
            self.usd += cost
        if self.stop is None:
            if spend_stop:
                self.stop = spend_stop
            elif error or tokens is None:
                self.stop = "unaccounted_request"
            elif tokens > reserve_tokens:
                self.stop = "token_reservation_exceeded"
        try:
            self.log("finished", status=status, usage=json.loads(json.dumps(usage, default=str)),
                     error=error, stop=self.stop, request_id=request_id,
                     settled_usd=None if cost is None else str(cost), billed_tokens=tokens)
        except OSError:
            self.spend.block("ledger_error")
            raise


def sum_usage(usage):
    prompt, completion = (usage.get(key) for key in ("prompt_tokens", "completion_tokens"))
    if all(type(n) is int and n >= 0 for n in (prompt, completion)):
        return prompt + completion
    return None


def response_usage(body, content_type):
    if content_type.startswith("text/event-stream"):
        payloads = (line[5:].strip() for line in body.splitlines() if line.startswith(b"data:"))
    else:
        payloads = (body,)
    usage = {}
    for payload in payloads:
        try:
            data = json.loads(payload, parse_float=Decimal)
        except (ValueError, UnicodeError):
            continue
        if isinstance(data, dict) and isinstance(data.get("usage"), dict):
            usage = data["usage"]
    return usage


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, status, data, content_type="application/json"):
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        budget = self.server.budget
        try:
            length = int(self.headers.get("Content-Length", 0))
            if not 0 < length <= MAX_REQUEST_BYTES:
                raise ValueError("invalid request size")
            raw = self.rfile.read(length)
            if self.path != CHAT:
                self.reply(404, b'{"error":{"message":"only chat completions are available"}}')
                return
            request = json.loads(raw)
            if not isinstance(request, dict) or request.get("model") != self.server.model:
                raise ValueError("request must use the pinned model")
            messages = request.get("messages")
            if not isinstance(messages, list) or any(
                not isinstance(m, dict) or (m.get("content") is not None and not isinstance(m["content"], str))
                for m in messages
            ):
                raise ValueError("only text chat messages are supported")
            tools = request.get("tools", [])
            if not isinstance(tools, list) or any(not isinstance(t, dict) or t.get("type") != "function" for t in tools):
                raise ValueError("only client-side function tools are supported")
            output = request.get("max_tokens", budget.limits.output)
            if type(output) is not int or output <= 0:
                raise ValueError("invalid output limit")
            request["max_tokens"] = min(output, budget.limits.output)
            if set(request) - {"messages", "model", "stream", "stream_options", "temperature",
                               "max_tokens", "tools", "tool_choice"}:
                raise ValueError("unsupported completion fields")
            if self.server.provider is not None:
                self.server.provider.configure(request)
            payload = json.dumps(request, allow_nan=False)
        except (ValueError, UnicodeError) as error:
            self.reply(400, json.dumps({"error": {"message": str(error)}}).encode())
            return
        reserve_tokens = budget.limits.context
        if self.server.provider is not None:
            # UTF-8 bytes overestimate text tokens. Leave room for provider
            # framing and the full requested output when a history gets large.
            reserve_tokens = max(reserve_tokens, len(payload.encode()) + request["max_tokens"]
                                 + 64 * (len(messages) + len(tools) + 1))
        if not budget.admit(reserve_tokens):
            self.reply(403, json.dumps({"error": {"code": budget.stop, "message": "benchmark stopped"}}).encode())
            return
        remaining = max(0.001, budget.deadline - time.monotonic())
        conn = http.client.HTTPConnection("127.0.0.1", self.server.port, timeout=remaining)
        response = None
        content_type = "application/json"
        chunks = bytearray()
        error = None
        try:
            conn.request("POST", CHAT, body=payload,
                         headers={"Content-Type": "application/json", "Authorization": "Bearer via-proxy",
                                  "X-OrcaRouter-Session-Id": self.server.session_id})
            response = conn.getresponse()
            content_type = response.getheader("Content-Type", "application/json")
            while True:
                if time.monotonic() >= budget.deadline:
                    raise TimeoutError("budget deadline while reading upstream")
                chunk = response.read1(65536)
                if not chunk:
                    break
                chunks.extend(chunk)
                if len(chunks) > MAX_RESPONSE_BYTES:
                    raise ValueError("response exceeds size limit")
        except (OSError, ValueError, http.client.HTTPException) as failure:
            error = type(failure).__name__
            if isinstance(failure, TimeoutError) and time.monotonic() >= budget.deadline:
                budget.stop = budget.stop or "time_limit"
        finally:
            conn.close()
        usage = response_usage(chunks, content_type)
        request_id = response.getheader("x-orca-request-id") if response else None
        status = response.status if response else None
        if self.server.provider is None:
            receipt = Receipt(Decimal(0), sum_usage(usage))
        else:
            receipt = self.server.provider.receipt(request_id, self.server.model)
        budget.finish(usage, status, receipt, reserve_tokens, request_id, error)
        if error or receipt is None or budget.stop in ("duplicate_receipt", "cost_reservation_exceeded"):
            self.reply(502, b'{"error":{"message":"upstream response could not be accounted for"}}')
        else:
            self.reply(status, chunks, content_type)


class Server(socketserver.UnixStreamServer):
    # No concurrent dispatches can share or outrun a reservation. Bound how
    # long a client can leave an accepted HTTP connection idle.
    def get_request(self):
        sock, addr = super().get_request()
        sock.settimeout(2)
        return sock, addr

    def handle_error(self, request, client_address):
        # Container termination or an expired socket can interrupt a reply.
        pass


@contextlib.contextmanager
def guard(socket_path, port, model, budget, *, provider=None, session_id="scripted"):
    socket_path = Path(socket_path)
    with Server(str(socket_path), Handler) as server:
        server.port, server.model, server.budget = port, model, budget
        server.provider, server.session_id = provider, session_id
        socket_path.chmod(0o666)  # Parent directory on the host is private.
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            yield
        finally:
            server.shutdown()
            thread.join()
            socket_path.unlink()
