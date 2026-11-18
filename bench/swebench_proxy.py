"""Budget the task container's only route to the host recording endpoint."""

import contextlib
import dataclasses
from decimal import Decimal
import http.client
import http.server
import json
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
    usd: Decimal
    request_usd: Decimal

    def __post_init__(self):
        if (min(self.requests, self.tokens, self.seconds, self.context, self.output) <= 0
                or self.output >= self.context
                or not all(d.is_finite() and d >= 0 for d in (self.usd, self.request_usd))):
            raise ValueError("invalid benchmark limits")


class Budget:
    """Reserve before dispatch. A failed or unaccounted call retains its reservation."""

    def __init__(self, limits, ledger, *, scripted):
        if not scripted and limits.request_usd <= 0:
            raise ValueError("charged requests require a positive cost reservation")
        self.limits = limits
        self.scripted = scripted
        self.ledger = ledger
        self.deadline = time.monotonic() + limits.seconds
        self.requests = self.tokens = 0
        self.usd = Decimal(0)
        self.stop = None

    def log(self, event, **fields):
        with self.ledger.open("a") as stream:
            stream.write(json.dumps({"event": event, "requests": self.requests, "tokens": self.tokens,
                                     "usd": str(self.usd), **fields}) + "\n")

    def admit(self):
        cap = self.limits
        reason = self.stop
        if not reason:
            if time.monotonic() >= self.deadline:
                reason = "time_limit"
            elif self.requests >= cap.requests:
                reason = "request_limit"
            elif self.tokens + cap.context > cap.tokens:
                reason = "token_limit"
            elif self.usd + cap.request_usd > cap.usd:
                reason = "spend_limit"
        if reason:
            self.stop = reason
            self.log("rejected", reason=reason)
            return False
        self.requests += 1
        self.tokens += cap.context
        self.usd += cap.request_usd
        self.log("admitted")
        return True

    def finish(self, usage, status, error=None):
        tokens = sum_usage(usage)
        cost = Decimal(0) if self.scripted else usage.get("cost_usd")
        known_cost = type(cost) in (int, Decimal) and Decimal(cost).is_finite() and cost >= 0
        if tokens is not None:
            self.tokens += tokens - self.limits.context
        if known_cost:
            self.usd += cost - self.limits.request_usd
        if error or tokens is None or not known_cost:
            self.stop = "unaccounted_request"
        elif tokens > self.limits.context or cost > self.limits.request_usd:
            self.stop = "reservation_exceeded"
        self.log("finished", status=status, usage=json.loads(json.dumps(usage, default=str)),
                 error=error, stop=self.stop)


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
        if self.path != CHAT:
            self.reply(404, b'{"error":{"message":"only chat completions are available"}}')
            return
        try:
            length = int(self.headers.get("Content-Length", 0))
            if not 0 < length <= MAX_REQUEST_BYTES:
                raise ValueError("invalid request size")
            request = json.loads(self.rfile.read(length))
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
            payload = json.dumps(request, allow_nan=False)
        except (ValueError, UnicodeError) as error:
            self.reply(400, json.dumps({"error": {"message": str(error)}}).encode())
            return
        if not budget.admit():
            self.reply(403, json.dumps({"error": {"code": budget.stop, "message": "benchmark stopped"}}).encode())
            return
        remaining = max(0.001, budget.deadline - time.monotonic())
        conn = http.client.HTTPConnection("127.0.0.1", self.server.port, timeout=remaining)
        try:
            conn.request("POST", CHAT, body=payload,
                         headers={"Content-Type": "application/json", "Authorization": "Bearer via-proxy"})
            response = conn.getresponse()
            content_type = response.getheader("Content-Type", "application/json")
            chunks = bytearray()
            while True:
                if time.monotonic() >= budget.deadline:
                    raise TimeoutError("budget deadline while reading upstream")
                chunk = response.read1(65536)
                if not chunk:
                    break
                chunks.extend(chunk)
                if len(chunks) > MAX_RESPONSE_BYTES:
                    raise ValueError("response exceeds size limit")
        except (OSError, ValueError, http.client.HTTPException) as error:
            budget.finish({}, None, type(error).__name__)
            self.reply(502, b'{"error":{"message":"upstream response could not be accounted for"}}')
        else:
            budget.finish(response_usage(chunks, content_type), response.status)
            self.reply(response.status, chunks, content_type)
        finally:
            conn.close()


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
def guard(socket_path, port, model, budget):
    socket_path = Path(socket_path)
    with Server(str(socket_path), Handler) as server:
        server.port, server.model, server.budget = port, model, budget
        socket_path.chmod(0o666)  # Parent directory on the host is private.
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        try:
            yield
        finally:
            server.shutdown()
            thread.join()
            socket_path.unlink()
