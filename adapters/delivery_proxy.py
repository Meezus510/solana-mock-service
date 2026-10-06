"""Loopback transport fault injector. Always executes the real upstream handler."""

import argparse
import json
import socket
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from urllib.error import HTTPError
from urllib.request import HTTPRedirectHandler, ProxyHandler, Request, build_opener


class NoRedirect(HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        raise ValueError("redirect forbidden")


class Server(ThreadingHTTPServer):
    def __init__(self, address, targets):
        super().__init__(address, Handler)
        self.targets = targets
        self.records = []
        self.faults = {}
        self.lock = threading.Lock()
        self.opener = build_opener(ProxyHandler({}), NoRedirect())


class Handler(BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def reply(self, status, body):
        raw = json.dumps(body).encode()
        self.send_response(status)
        self.send_header("Content-Length", str(len(raw)))
        self.end_headers()
        self.wfile.write(raw)

    def do_GET(self):
        self.forward()

    def do_POST(self):
        self.forward()

    def forward(self):
        size = int(self.headers.get("Content-Length", "0"))
        if not 0 <= size <= 1048576:
            return self.reply(413, {"error": "body too large"})
        raw = self.rfile.read(size)
        if self.path == "/health":
            return self.reply(200, {"ready": True})
        if self.path == "/__proxy/requests":
            with self.server.lock:
                records = list(self.server.records)
            return self.reply(200, records)
        if self.path == "/__proxy/drop-ack" and self.command == "POST":
            payload = json.loads(raw)
            path = payload["path"]
            if not path.startswith(("/market/v1/", "/strategy/v1/")):
                return self.reply(400, {"error": "invalid path"})
            with self.server.lock:
                self.server.faults[path] = int(payload.get("count", 1))
            return self.reply(200, {"armed": path})
        parts = self.path.split("/", 2)
        if len(parts) != 3 or parts[1] not in self.server.targets:
            return self.reply(404, {"error": "unknown target"})
        request = Request(
            self.server.targets[parts[1]] + "/" + parts[2],
            data=raw if self.command == "POST" else None,
            headers={"Content-Type": "application/json"},
            method=self.command,
        )
        try:
            try:
                response = self.server.opener.open(request, timeout=25)
            except HTTPError as exc:
                response = exc
            with response:
                status = response.code
                body = response.read()
        except Exception as exc:
            return self.reply(503, {"error": type(exc).__name__})
        with self.server.lock:
            count = self.server.faults.get(self.path, 0)
            drop = 200 <= status < 300 and count > 0
            if drop:
                self.server.faults[self.path] = count - 1
            # Query responses are large; only retain exact mutation wires.
            if self.command == "POST":
                self.server.records.append(
                    {
                        "path": self.path,
                        "request": raw.decode(),
                        "upstream_status": status,
                        "drop_ack": drop,
                        "response": body.decode(),
                        "observed_at": time.time(),
                    }
                )
        if drop:
            self.close_connection = True
            self.connection.shutdown(socket.SHUT_RDWR)
            self.connection.close()
            return
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass


def main():
    p = argparse.ArgumentParser()
    p.add_argument("--port", type=int, required=True)
    p.add_argument("--market-port", type=int, required=True)
    p.add_argument("--strategy-port", type=int, required=True)
    a = p.parse_args()
    Server(
        ("127.0.0.1", a.port),
        {
            "market": f"http://127.0.0.1:{a.market_port}",
            "strategy": f"http://127.0.0.1:{a.strategy_port}",
        },
    ).serve_forever()


if __name__ == "__main__":
    main()
