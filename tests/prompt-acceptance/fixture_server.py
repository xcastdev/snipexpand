#!/usr/bin/env python3
"""Disposable guest fixture. Never run on a host desktop; values are test data."""

import http.server
import json
from pathlib import Path

ROOT = Path(__file__).parent


class Handler(http.server.BaseHTTPRequestHandler):
    def log_message(self, *args):
        pass

    def do_GET(self):
        data = (ROOT / "target.html").read_bytes()
        self.send_response(200)
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        data = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        Path("/home/vagrant/chrome-state.tmp").write_text(json.dumps(data))
        Path("/home/vagrant/chrome-state.tmp").replace(
            "/home/vagrant/chrome-state.json"
        )
        self.send_response(204)
        self.end_headers()


http.server.HTTPServer(("127.0.0.1", 8765), Handler).serve_forever()
