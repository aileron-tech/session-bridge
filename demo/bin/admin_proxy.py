#!/usr/bin/env python3
"""Makes this laptop's admin page reachable from the Mac (demo only).

The page listens on 127.0.0.1:<random port> inside the container and accepts
only requests whose Host (and Origin, when sent) name that address. This
proxy listens on the container's port 7100, which compose publishes as
127.0.0.1:<public port> on the Mac, and forwards each request with Host and
Origin rewritten to the page's own address. The bearer token is still
required: the proxy only adapts the address."""
import argparse
import http.client
import http.server
import json
import os
import socket
import sys
import time

CONTROL = os.path.join(os.environ.get("SESSION_BRIDGE_STATE") or os.path.expanduser("~/.local/state/session-bridge"), "control.sock")
LISTEN_PORT = int(os.environ.get("ADMIN_PROXY_PORT", "7100"))
HOP = {"connection", "keep-alive", "transfer-encoding", "te", "trailer", "upgrade",
       "proxy-authorization", "proxy-authenticate"}


def admin_url():
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as conn:
        conn.settimeout(30)
        conn.connect(CONTROL)
        conn.sendall(b'{"method":"admin"}\n')
        reply = json.loads(conn.makefile().readline())
    if reply.get("result") != "admin":
        raise RuntimeError(f"admin page did not start: {reply}")
    return reply["url"]


class State:
    port = 0


class Proxy(http.server.BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, format, *args):  # noqa: A002 (base class name)
        del format, args

    def _forward(self):
        length = int(self.headers.get("Content-Length") or 0)
        body = self.rfile.read(length) if length else None
        headers = {k: v for k, v in self.headers.items() if k.lower() not in HOP}
        headers["Host"] = f"127.0.0.1:{State.port}"
        if "Origin" in headers:
            headers["Origin"] = f"http://127.0.0.1:{State.port}"
        try:
            upstream = http.client.HTTPConnection("127.0.0.1", State.port, timeout=60)
            upstream.request(self.command, self.path, body=body, headers=headers)
            response = upstream.getresponse()
            data = response.read()
        except OSError as error:
            print(f"admin proxy: upstream failed: {error}", file=sys.stderr, flush=True)
            self.send_error(502, "Admin page unavailable")
            return
        self.send_response(response.status, response.reason)
        for key, value in response.getheaders():
            if key.lower() not in HOP and key.lower() != "content-length":
                self.send_header(key, value)
        self.send_header("Content-Length", str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    do_GET = _forward
    do_POST = _forward


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--public-port", type=int, required=True)
    args = parser.parse_args()
    url = admin_url()
    State.port = int(url.split("://127.0.0.1:")[1].split("/")[0])
    token = url.split("#token=")[1]
    public = f"http://127.0.0.1:{args.public_port}/#token={token}"
    path = os.path.expanduser("~/admin-url.txt")
    with open(path, "w") as f:
        f.write(public + "\n")
    print(f"Admin page of {socket.gethostname()}: {public}", flush=True)
    server = http.server.ThreadingHTTPServer(("0.0.0.0", LISTEN_PORT), Proxy)
    server.serve_forever()


if __name__ == "__main__":
    for attempt in range(30):
        try:
            main()
        except (OSError, RuntimeError, json.JSONDecodeError) as error:
            print(f"admin proxy: {error}; retrying", file=sys.stderr, flush=True)
            time.sleep(1)
