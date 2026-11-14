"""Serve the static site of scripts/build_static_site.py locally, as a static host would (#266).

    python scripts/serve_static_site.py SITE_DIR [--port 8000]

For checks on this machine only (it listens on 127.0.0.1). It serves the site's files at their
paths, `/` and a folder's path from its index.html, and a path without a file from the same path
with `.html` (`/demo-v2` from demo-v2.html), as Cloudflare Workers' static assets (where the site is
published, deploy/workers/wrangler.jsonc), Cloudflare Pages, GitHub Pages and Netlify do; the
standard library's http.server does not, which is why this exists. JavaScript modules (.mjs) and
WASM get their types. Anything else is 404, with the site's 404.html.
"""

from __future__ import annotations

import argparse
import functools
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from urllib.parse import unquote, urlsplit

TYPES = {
    ".css": "text/css; charset=utf-8",
    ".html": "text/html; charset=utf-8",
    ".jpg": "image/jpeg",
    ".js": "text/javascript; charset=utf-8",
    ".json": "application/json; charset=utf-8",
    ".mjs": "text/javascript; charset=utf-8",
    ".txt": "text/plain; charset=utf-8",
    ".wasm": "application/wasm",
}


def resolve(site: Path, request_path: str) -> Path | None:
    """The file a static host serves for a request path, or None."""
    path = unquote(urlsplit(request_path).path)
    if not path.startswith("/") or "\\" in path or any(part == ".." for part in path.split("/")):
        return None
    candidate = (site / path.lstrip("/")).resolve()
    if candidate != site and site not in candidate.parents:
        return None
    if candidate.is_dir():
        candidate = candidate / "index.html"
    elif not candidate.is_file() and not path.endswith("/"):
        candidate = candidate.with_name(candidate.name + ".html")
    return candidate if candidate.is_file() else None


class Handler(BaseHTTPRequestHandler):
    server_version = "SheltersimStaticSite/1"

    def __init__(self, *args: object, site: Path, **kwargs: object) -> None:
        self.site = site
        super().__init__(*args, **kwargs)  # type: ignore[arg-type]

    def do_GET(self) -> None:  # noqa: N802 - the base class's hook name
        self._answer(body=True)

    def do_HEAD(self) -> None:  # noqa: N802 - the base class's hook name
        self._answer(body=False)

    def _answer(self, *, body: bool) -> None:
        found = resolve(self.site, self.path)
        if found is None:
            missing = self.site / "404.html"
            payload = missing.read_bytes() if missing.is_file() else b"not found\n"
            self.send_response(404)
            self.send_header(
                "Content-Type", TYPES[".html"] if missing.is_file() else "text/plain; charset=utf-8"
            )
        else:
            payload = found.read_bytes()
            self.send_response(200)
            self.send_header(
                "Content-Type", TYPES.get(found.suffix.lower(), "application/octet-stream")
            )
        self.send_header("Content-Length", str(len(payload)))
        self.send_header("Cache-Control", "no-cache")
        self.end_headers()
        if body:
            self.wfile.write(payload)

    def log_message(self, format: str, *args: object) -> None:  # noqa: A002 - the base's name
        pass


def start(site: Path, host: str = "127.0.0.1", port: int = 0) -> ThreadingHTTPServer:
    """A server of `site` running in a thread; `server.server_port` is its port."""
    server = ThreadingHTTPServer((host, port), functools.partial(Handler, site=site.resolve()))
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("site", type=Path, help="the output of build_static_site.py")
    parser.add_argument("--port", type=int, default=8000)
    arguments = parser.parse_args()
    server = ThreadingHTTPServer(
        ("127.0.0.1", arguments.port), functools.partial(Handler, site=arguments.site.resolve())
    )
    print(f"http://127.0.0.1:{server.server_port}/")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
