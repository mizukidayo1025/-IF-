"""Measure the browser's core on the trial worlds (stage 4 of #132, PR 4-5 of #244).

It serves the measuring page (`benchmarks/wasm_trial/index.html`) with the WASM package
(`wasm-bindgen --target web` of `sheltersim_wasm.wasm`, see docs/development.md) and the outputs of
`benchmarks/wasm_trial_worlds.py`, opens it in each browser with Playwright, one fresh page per
world, and writes what the page measured: the time to load the world and to run its ticks, the
WASM memory's size after each run (it only grows, so after the first run it is the run's peak),
and whether the digests of the world and of the metric samples are the native path's.

    python benchmarks/wasm_trial_measure.py --worlds DIR --pkg PKG [--browsers chromium firefox]
        [--repeat 3] [--output result.json]

With --store (#252, PR 6-5) it opens `store.html` instead, once per world: the page runs the world
in a Worker, stores it in the browser's origin private file system as it goes (native/wasm/js),
reads it back and checks the digests, and the script writes the times to run, compress, write and
read back, the WASM memory and the stored sizes. The Run keeps a saved point every `--save-every`
minutes (#251; 360 by default, 0 for none), and the page seeks to the minute farthest from a saved
point and writes the time to restore and compute; with `--chain` it also restores each saved point,
computes to the next and checks that it is the one stored.

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
# The page that stores a Run in OPFS as it goes and reads it back (#252, PR 6-5), and its JS.
STORE_PAGE = PAGE.with_name("store.html")
STORE_JS = Path(__file__).resolve().parents[1] / "native" / "wasm" / "js"
# The page's runs of one world may take a while in a slow browser.
TIMEOUT_MS = 600_000


class _Quiet(SimpleHTTPRequestHandler):
    def log_message(self, format: str, *args: Any) -> None:  # noqa: A002 - the base class's name
        pass


def _site(worlds: Path, pkg: Path, directory: Path) -> None:
    shutil.copy(PAGE, directory / "index.html")
    shutil.copy(STORE_PAGE, directory / "store.html")
    shutil.copytree(STORE_JS, directory / "js")
    shutil.copytree(pkg, directory / "pkg")
    expected = json.loads((worlds / "expected.json").read_text(encoding="utf-8"))
    shutil.copy(worlds / "expected.json", directory / "expected.json")
    for world in expected["worlds"].values():
        shutil.copy(worlds / world["file"], directory / world["file"])


class _Site:
    """The measuring pages served from a temporary directory, on a port of 127.0.0.1."""

    def __init__(self, worlds: Path, pkg: Path) -> None:
        self._temporary = tempfile.TemporaryDirectory()
        directory = Path(self._temporary.name)
        _site(worlds, pkg, directory)
        handler = functools.partial(_Quiet, directory=str(directory))
        self._server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
        threading.Thread(target=self._server.serve_forever, daemon=True).start()
        self.base = f"http://127.0.0.1:{self._server.server_address[1]}"

    def __enter__(self) -> _Site:
        return self

    def __exit__(self, *_: object) -> None:
        self._server.shutdown()
        self._temporary.cleanup()


def measure_store(
    worlds: Path,
    pkg: Path,
    browsers: list[str],
    ticks: int,
    save_every: int = 360,
    chain: bool = False,
) -> dict[str, Any]:
    """Each world in each browser, on a fresh page: run in a Worker, stored in OPFS as it goes,
    read back and checked (`store.html`)."""
    from playwright.sync_api import sync_playwright

    names = list(json.loads((worlds / "expected.json").read_text(encoding="utf-8"))["worlds"])
    results: dict[str, Any] = {
        "ticks": ticks,
        "save_every": save_every,
        "chain": chain,
        "browsers": {},
    }
    with _Site(worlds, pkg) as site, sync_playwright() as playwright:
        for name in browsers:
            browser = getattr(playwright, name).launch()
            per_world: dict[str, Any] = {"version": browser.version, "worlds": {}}
            for world in names:
                context = browser.new_context()
                page = context.new_page()
                query = f"world={world}&ticks={ticks}&save={save_every}&chain={int(chain)}"
                page.goto(f"{site.base}/store.html?{query}")
                page.wait_for_function("window.storeResult !== undefined", timeout=TIMEOUT_MS)
                result = page.evaluate("window.storeResult")
                context.close()
                if "error" in result:
                    raise RuntimeError(f"{name} {world}: {result['error']}")
                per_world["userAgent"] = result.pop("userAgent")
                per_world["worlds"][world] = result
            browser.close()
            results["browsers"][name] = per_world
    return results


def store_summary(results: dict[str, Any]) -> str:
    lines = []
    mib = 2**20
    for name, browser in results["browsers"].items():
        for world, measured in browser["worlds"].items():
            ran, back = measured["ran"], measured["back"]
            raw = sum(size["raw"] for size in ran["sizes"].values()) / mib
            stored = sum(size["stored"] for size in ran["sizes"].values()) / mib
            lines.append(
                f"{name} {browser['version']} {world}: ticks {ran['tickMs'] / 1000:.2f} s, "
                f"compress {ran['compressMs'] / 1000:.2f} s, write {ran['writeMs'] / 1000:.2f} s, "
                f"read back {back['totalMs'] / 1000:.2f} s, "
                f"memory {ran['memoryBytes'] / mib:.1f} MiB "
                f"(after reading {measured['memoryAfterRead'] / mib:.1f}), stored {raw:.1f} -> "
                f"{stored:.1f} MiB, digests {'same' if not measured['problems'] else 'DIFFERENT'}"
            )
            saved = ran["sizes"].get("saved", {"stored": 0})["stored"] / mib
            seconds = ran["saveMs"] / 1000
            lines.append(f"  saved points {ran['saved']}: {saved:.1f} MiB, {seconds:.2f} s")
            seek = measured.get("seek")
            if seek:
                worst = seek["worst"]
                lines.append(
                    f"  seek to {worst['minute']} from {worst['from']}: restore "
                    f"{worst['restoreMs'] / 1000:.2f} s, "
                    f"compute {worst['recomputeMs'] / 1000:.2f} s"
                )
                if "chain" in seek:
                    chain = seek["chain"]
                    lines.append(
                        f"  each saved point to the next: {chain['points']} points, restore max "
                        f"{max(chain['restoreMs']) / 1000:.2f} s, "
                        f"{'same' if not chain['mismatches'] else 'DIFFERENT'}"
                    )
    return "\n".join(lines)


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
    parser.add_argument(
        "--store",
        action="store_true",
        help="run each world once in a Worker, stored in OPFS as it goes, and read it back",
    )
    parser.add_argument("--ticks", type=int, default=1440, help="--store: ticks per advance")
    parser.add_argument(
        "--save-every",
        type=int,
        default=360,
        help="--store: minutes between saved points (0: none)",
    )
    parser.add_argument(
        "--chain",
        action="store_true",
        help="--store: restore each saved point, compute to the next and check it",
    )
    arguments = parser.parse_args()
    if arguments.store:
        stored = measure_store(
            arguments.worlds,
            arguments.pkg,
            arguments.browsers,
            arguments.ticks,
            arguments.save_every,
            arguments.chain,
        )
        if arguments.output:
            arguments.output.write_text(
                json.dumps(stored, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
            )
        print(store_summary(stored))
        return (
            1
            if any(
                measured["problems"]
                for browser in stored["browsers"].values()
                for measured in browser["worlds"].values()
            )
            else 0
        )
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
