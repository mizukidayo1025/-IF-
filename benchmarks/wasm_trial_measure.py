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
read back, the WASM memory and the stored sizes.

With --project (#265) it opens `project.html` once per world: the page computes the world into a
Project of OPFS (native/wasm/js/project_store.mjs), exports it to one bundle, empties OPFS, imports
the bundle and checks that the digests and the routes' answers are the same. With --bundle FILE it
imports that bundle (scripts/build_browser_sample.py's sample) into an empty OPFS and checks each
branch's digests; --worlds is not needed then.

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
# The page that exports a Project to one bundle and imports it back (#265), and its Worker.
PROJECT_PAGE = PAGE.with_name("project.html")
PROJECT_WORKER = PAGE.with_name("project_worker.mjs")
# The page's runs of one world may take a while in a slow browser.
TIMEOUT_MS = 600_000


class _Quiet(SimpleHTTPRequestHandler):
    def log_message(self, format: str, *args: Any) -> None:  # noqa: A002 - the base class's name
        pass


def _site(worlds: Path | None, pkg: Path, directory: Path, bundle: Path | None = None) -> None:
    shutil.copy(PAGE, directory / "index.html")
    shutil.copy(STORE_PAGE, directory / "store.html")
    shutil.copy(PROJECT_PAGE, directory / "project.html")
    shutil.copy(PROJECT_WORKER, directory / "project_worker.mjs")
    shutil.copytree(STORE_JS, directory / "js")
    shutil.copytree(pkg, directory / "pkg")
    if bundle is not None:
        shutil.copy(bundle, directory / "sample.shelterproj")
    if worlds is None:
        return
    expected = json.loads((worlds / "expected.json").read_text(encoding="utf-8"))
    shutil.copy(worlds / "expected.json", directory / "expected.json")
    for world in expected["worlds"].values():
        shutil.copy(worlds / world["file"], directory / world["file"])


class _Site:
    """The measuring pages served from a temporary directory, on a port of 127.0.0.1."""

    def __init__(self, worlds: Path | None, pkg: Path, bundle: Path | None = None) -> None:
        self._temporary = tempfile.TemporaryDirectory()
        directory = Path(self._temporary.name)
        _site(worlds, pkg, directory, bundle)
        handler = functools.partial(_Quiet, directory=str(directory))
        self._server = ThreadingHTTPServer(("127.0.0.1", 0), handler)
        threading.Thread(target=self._server.serve_forever, daemon=True).start()
        self.base = f"http://127.0.0.1:{self._server.server_address[1]}"

    def __enter__(self) -> _Site:
        return self

    def __exit__(self, *_: object) -> None:
        self._server.shutdown()
        self._temporary.cleanup()


def measure_store(worlds: Path, pkg: Path, browsers: list[str], ticks: int) -> dict[str, Any]:
    """Each world in each browser, on a fresh page: run in a Worker, stored in OPFS as it goes,
    read back and checked (`store.html`)."""
    from playwright.sync_api import sync_playwright

    names = list(json.loads((worlds / "expected.json").read_text(encoding="utf-8"))["worlds"])
    results: dict[str, Any] = {"ticks": ticks, "browsers": {}}
    with _Site(worlds, pkg) as site, sync_playwright() as playwright:
        for name in browsers:
            browser = getattr(playwright, name).launch()
            per_world: dict[str, Any] = {"version": browser.version, "worlds": {}}
            for world in names:
                context = browser.new_context()
                page = context.new_page()
                page.goto(f"{site.base}/store.html?world={world}&ticks={ticks}")
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
    return "\n".join(lines)


def check_projects(
    worlds: Path | None, pkg: Path, browsers: list[str], bundle: Path | None
) -> dict[str, Any]:
    """`project.html` in each browser, on a fresh page: each world exported to a bundle and
    imported back, or the bundle imported."""
    from playwright.sync_api import sync_playwright

    if bundle is not None:
        queries = {"bundle": "bundle=sample.shelterproj"}
    else:
        expected = json.loads((worlds / "expected.json").read_text(encoding="utf-8"))
        queries = {world: f"world={world}" for world in expected["worlds"]}
    results: dict[str, Any] = {"browsers": {}}
    with _Site(worlds, pkg, bundle) as site, sync_playwright() as playwright:
        for name in browsers:
            browser = getattr(playwright, name).launch()
            checked: dict[str, Any] = {"version": browser.version, "checks": {}}
            for check, query in queries.items():
                context = browser.new_context()
                page = context.new_page()
                page.goto(f"{site.base}/project.html?{query}")
                page.wait_for_function("window.projectResult !== undefined", timeout=TIMEOUT_MS)
                result = page.evaluate("window.projectResult")
                context.close()
                if "error" in result:
                    raise RuntimeError(f"{name} {check}: {result['error']}")
                checked["userAgent"] = result.pop("userAgent")
                checked["checks"][check] = result
            browser.close()
            results["browsers"][name] = checked
    return results


def project_summary(results: dict[str, Any]) -> str:
    lines = []
    mib = 2**20
    for name, browser in results["browsers"].items():
        for check, result in browser["checks"].items():
            times = f"import {result['importMs'] / 1000:.2f} s"
            if "exportMs" in result:
                times = (
                    f"compute {result['computeMs'] / 1000:.2f} s, "
                    f"export {result['exportMs'] / 1000:.2f} s, {times}"
                )
            problems = "; ".join(result["problems"])
            lines.append(
                f"{name} {browser['version']} {check}: "
                f"bundle {result['bundleBytes'] / mib:.1f} MiB, {times}, "
                f"OPFS {result['usage']['bytes'] / mib:.1f} MiB, "
                f"{'PROBLEMS ' + problems if problems else 'same'}"
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
    parser.add_argument("--worlds", type=Path, help="the output of wasm_trial_worlds.py")
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
        "--project",
        action="store_true",
        help="export each world's Project to one bundle and import it into an emptied OPFS",
    )
    parser.add_argument("--bundle", type=Path, help="import this bundle into an empty OPFS")
    arguments = parser.parse_args()
    if arguments.worlds is None and arguments.bundle is None:
        parser.error("--worlds is needed unless --bundle")
    if arguments.project or arguments.bundle:
        checked = check_projects(
            None if arguments.bundle else arguments.worlds,
            arguments.pkg,
            arguments.browsers,
            arguments.bundle,
        )
        if arguments.output:
            arguments.output.write_text(
                json.dumps(checked, indent=2, ensure_ascii=False) + "\n", encoding="utf-8"
            )
        print(project_summary(checked))
        return int(
            any(
                result["problems"]
                for browser in checked["browsers"].values()
                for result in browser["checks"].values()
            )
        )
    if arguments.store:
        stored = measure_store(arguments.worlds, arguments.pkg, arguments.browsers, arguments.ticks)
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
