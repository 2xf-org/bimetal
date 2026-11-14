#!/usr/bin/env python3
"""
Open a trial's page in headless Chrome, and report what the page found.

Usage: apps/trials/browser.py <trial>

The page posts its results to this server when it finishes. Exits with status 1
when the page reports a failure, or sends nothing within two minutes.
"""

import functools
import http.server
import json
import pathlib
import queue
import shutil
import subprocess
import sys
import tempfile
import threading

TIMEOUT_SECONDS = 120
CHROME_PATHS = ["/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"]


def find_chrome() -> str | None:
    """
    Find Chrome or Chromium on the path or in its usual place.

    Returns:
        The browser's executable, or None if it is not installed.
    """
    found = shutil.which("google-chrome") or shutil.which("chromium")
    return found or next((path for path in CHROME_PATHS if pathlib.Path(path).exists()), None)


def main() -> int:
    """
    Serve the trials, open the trial's page, and wait for its results.

    Returns:
        The exit status: 0 when the page passes or Chrome is missing, and 1 otherwise.
    """
    chrome = find_chrome()
    if chrome is None:
        print("browser: skipped, Chrome is not installed")
        return 0
    results: queue.Queue[dict] = queue.Queue()

    class Handler(http.server.SimpleHTTPRequestHandler):
        """
        Serve the trials' files, and queue the results that a page posts.
        """

        protocol_version = "HTTP/1.1"

        def do_POST(self) -> None:  # noqa: N802
            body = self.rfile.read(int(self.headers["Content-Length"]))
            results.put(json.loads(body))
            self.send_response(204)
            self.send_header("Content-Length", "0")
            self.end_headers()

        def log_message(self, *_: object) -> None:
            pass

    # A page's module imports arrive together, and a connection that overflows
    # the accept queue waits a second to retry.
    class Server(http.server.ThreadingHTTPServer):
        request_queue_size = 256

    handler = functools.partial(Handler, directory=str(pathlib.Path(__file__).parent))
    server = Server(("127.0.0.1", 0), handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    url = f"http://127.0.0.1:{server.server_port}/{sys.argv[1]}/typescript/browser/index.html"
    with tempfile.TemporaryDirectory() as profile:
        browser = subprocess.Popen(
            [chrome, "--headless=new", "--disable-gpu", f"--user-data-dir={profile}", url],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        try:
            found = results.get(timeout=TIMEOUT_SECONDS)
        except queue.Empty:
            found = {"ok": False, "error": f"no results within {TIMEOUT_SECONDS} seconds"}
        finally:
            browser.terminate()
            browser.wait()
    server.shutdown()
    print("browser:", "ok" if found.get("ok") else "failed", json.dumps(found)[:400])
    return 0 if found.get("ok") else 1


if __name__ == "__main__":
    sys.exit(main())
