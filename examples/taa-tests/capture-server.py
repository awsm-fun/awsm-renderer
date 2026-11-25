#!/usr/bin/env python3
"""Capture store for the TAA harness: PUT/GET /<set>/<file>.png <-> <root>/<set>/<file>.png.

usage: capture-server.py <root-dir> [port=9095]
Local dev tool only: binds 127.0.0.1, CORS open, refuses paths outside <root>.
"""
import http.server
import os
import sys

ROOT = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else ".")
PORT = int(sys.argv[2]) if len(sys.argv) > 2 else 9095


class Handler(http.server.BaseHTTPRequestHandler):
    def _path(self):
        rel = self.path.split("?", 1)[0].lstrip("/")
        full = os.path.abspath(os.path.join(ROOT, rel))
        if not full.startswith(ROOT + os.sep):
            return None
        return full

    def _cors(self):
        self.send_header("Access-Control-Allow-Origin", "*")
        self.send_header("Access-Control-Allow-Methods", "GET, PUT, OPTIONS")
        self.send_header("Access-Control-Allow-Headers", "content-type")

    def do_OPTIONS(self):
        self.send_response(204)
        self._cors()
        self.end_headers()

    def do_PUT(self):
        full = self._path()
        if full is None:
            self.send_response(403); self._cors(); self.end_headers(); return
        n = int(self.headers.get("content-length", "0"))
        data = self.rfile.read(n)
        os.makedirs(os.path.dirname(full), exist_ok=True)
        with open(full, "wb") as f:
            f.write(data)
        self.send_response(201); self._cors(); self.end_headers()

    def do_GET(self):
        full = self._path()
        if full is None or not os.path.isfile(full):
            self.send_response(404); self._cors(); self.end_headers(); return
        with open(full, "rb") as f:
            data = f.read()
        self.send_response(200)
        self._cors()
        self.send_header("content-type", "image/png" if full.endswith(".png") else "application/octet-stream")
        self.send_header("content-length", str(len(data)))
        self.send_header("cache-control", "no-store")
        self.end_headers()
        self.wfile.write(data)

    def log_message(self, fmt, *args):
        pass


if __name__ == "__main__":
    os.makedirs(ROOT, exist_ok=True)
    print(f"taa capture store: {ROOT} on http://127.0.0.1:{PORT}", flush=True)
    http.server.ThreadingHTTPServer(("127.0.0.1", PORT), Handler).serve_forever()
