"""Measure the browser's core on the trial worlds (stage 4 of #132, PR 4-5 of #244).

It serves the measuring page (`benchmarks/wasm_trial/index.html`) with the WASM package
(`wasm-bindgen --target web` of `sheltersim_wasm.wasm`, see docs/development.md) and the outputs of
`benchmarks/wasm_trial_worlds.py`, opens it in each browser with Playwright, one fresh page per
world, and writes what the page measured: the time to load the world and to run its ticks, the
WASM memory's size after each run (it only grows, so after the first run it is the run's peak),
and whether the digests of the world and of the metric samples are the native path's.

    python benchmarks/wasm_trial_measure.py --worlds DIR --pkg PKG [--browsers chromium firefox]
        [--repeat 3] [--output result.json]

Playwright and its browsers are not dependencies of the project: install them where you measure
(`python -m playwright install chromium firefox`).
"""

from __future__ import annotations

import argparse
import functools
import json
import shutil
import tempfile
import threading
from http.server import SimpleHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

PAGE = Path(__file__).resolve().parent / "wasm_trial" / "index.html"
# The page's runs of one world may take a while in a slow browser.
TIMEOUT_MS = 600_000


class _Quiet(SimpleHTTPRequestHandler):
    def log_message(self, format: str, *args: Any) -> None:  # noqa: A002 - the base class's name
        pass


def _site(worlds: Path, pkg: Path, directory: Path) -> None:
    shutil.copy(PAGE, directory / "index.html")
    shutil.copytree(pkg, directory / "pkg")
    expected = json.loads((worlds / "expected.json").read_text(encoding="utf-8"))
    shutil.copy(worlds / "expected.json", directory / "expected.json")
    for world in expected["worlds"].values():
        shutil.copy(worlds / world["file"], directory / world["file"])


def measure(worlds: Path, pkg: Path, browsers: list[str], repeat: int) -> dict[str, Any]:
    from playwright.sync_api import sync_playwright

    names = list(json.loads((worlds / "expected.json").read_text(encoding="utf-8"))["worlds"])
    results: dict[str, Any] = {"repeat": repeat, "browsers": {}}
    with tempfile.TemporaryDirectory() as temporary:
        directory = Path(temporary)
        _site(worlds, pkg, directory)
        handler = functools.partial(_Quiet, directory=str(directory))
        server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
        thread = threading.Thread(target=server.serve_forever, daemon=True)
        thread.start()
        base = f"http://127.0.0.1:{server.server_address[1]}/index.html"
        try:
            with sync_playwright() as playwright:
                for name in browsers:
                    browser = getattr(playwright, name).launch()
                    per_world: dict[str, Any] = {"version": browser.version, "worlds": {}}
                    for world in names:
                        context = browser.new_context()
                        page = context.new_page()
                        page.goto(f"{base}?world={world}&repeat={repeat}")
                        page.wait_for_function(
                            "window.trialResult !== undefined", timeout=TIMEOUT_MS
                        )
                        result = page.evaluate("window.trialResult")
                        if "error" in result:
                            raise RuntimeError(f"{name} {world}: {result['error']}")
                        per_world["userAgent"] = result["userAgent"]
                        per_world["worlds"][world] = {
                            "initMs": result["initMs"],
                            "memoryAfterInit": result["memoryAfterInit"],
                            **result["worlds"][world],
                        }
                        context.close()
                    browser.close()
                    results["browsers"][name] = per_world
        finally:
            server.shutdown()
    return results


def summary(results: dict[str, Any]) -> str:
    lines = []
    for name, browser in results["browsers"].items():
        for world, measured in browser["worlds"].items():
            runs = measured["runs"]
            ticks = sorted(run["tickSeconds"] for run in runs)
            median = ticks[len(ticks) // 2]
            peak = max(run["memoryBytes"] for run in runs) / 2**20
            same = all(not run["problems"] for run in runs)
            lines.append(
                f"{name} {browser['version']} {world}: ticks median {median:.3f} s "
                f"(native {measured['nativeTickSeconds']:.3f} s), load {runs[0]['loadMs']:.1f} ms, "
                f"memory {peak:.1f} MiB, digests {'same' if same else 'DIFFERENT'}"
            )
    return "\n".join(lines)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(
        "--worlds", type=Path, required=True, help="the output of wasm_trial_worlds.py"
    )
    parser.add_argument("--pkg", type=Path, required=True, help="wasm-bindgen --target web output")
    parser.add_argument("--browsers", nargs="+", default=["chromium", "firefox"])
    parser.add_argument("--repeat", type=int, default=3)
    parser.add_argument("--output", type=Path)
    arguments = parser.parse_args()
    results = measure(arguments.worlds, arguments.pkg, arguments.browsers, arguments.repeat)
    if arguments.output:
        arguments.output.write_text(
            json.dumps(results, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
        )
    print(summary(results))
    different = any(
        run["problems"]
        for browser in results["browsers"].values()
        for measured in browser["worlds"].values()
        for run in measured["runs"]
    )
    return 1 if different else 0


if __name__ == "__main__":
    raise SystemExit(main())
