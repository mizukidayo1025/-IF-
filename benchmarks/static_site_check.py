"""Check the browser version's static site against the Python server (#266, stage A of #263).

    python benchmarks/static_site_check.py --site SITE_DIR [--python-projects DIR]
        [--browsers chromium firefox] [--output result.json] [--screenshots DIR]

SITE_DIR is the output of scripts/build_static_site.py; it is served as a static host would
(scripts/serve_static_site.py). In each browser, on a fresh profile (an empty OPFS):

1. The screen: `/` goes to `/demo-v2?…`, the sample is fetched and imported, the simulation page
   shows the sample's Run; seeking to a minute (the time slider), playing for a few seconds and
   the report page's history graph work, and the page reports no error (page errors, console
   errors, the notice's error state).
2. With --python-projects (a ProjectStore root holding Project `sample` with the main branch of
   the sample's Scenario, as scripts/build_project_sample.py computes it on the Python Project
   route), the stage-A routes' JSON the browser answers is compared with what the Python server
   answers (ProjectAPI in this process, serialized as server.py does): the records, timeline at
   saved points and minutes between, replay windows (one after a jump, then consecutive ones), the
   history of the KPIs at several steps and pages, report and llm-status. Values are compared with
   their types (1 and 1.0 differ). The fields EXCLUDED lists differ by design; any other
   difference fails the check. The times of the browser's answers are recorded.

Playwright and its browsers are not dependencies of the project: install them where you check
(`python -m playwright install chromium firefox`). The result's figures go to the Issue, not to
the docs. Exit 0 when every check passes, 1 otherwise.
"""

from __future__ import annotations

import argparse
import base64
import json
import os
import re
import sys
import tempfile
import time
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "scripts"))
import serve_static_site  # noqa: E402 - the local server of the site, a script

PROJECT, RUN, BRANCH = "sample", "run-sample-toilet-outage", "main"
RUN_PATH = f"/api/v1/projects/{PROJECT}/runs/{RUN}"
BRANCH_PATH = f"{RUN_PATH}/branches/{BRANCH}"
TIMEOUT_MS = 600_000
# Fields that differ by design, as (route pattern, field path pattern) and why. "*" is any key or
# index.
EXCLUDED = [
    (
        r"^/api/v1/projects$",
        ("storage", "*"),
        "the store's size, files and limit: the browser counts its OPFS folder and the origin's "
        "quota, Python its projects directory",
    ),
    (
        r".*",
        ("runs", "*", "branches", "*", "checkpoint_hash"),
        "a branch's head: the browser's is the sha256 of its stored Run's last journal line "
        "(docs/project-persistence.md), Python's a Checkpoint's hash",
    ),
    (r".*", ("branches", "*", "checkpoint_hash"), "the same, in a Run's record"),
    (r"llm-status$", ("checkpoint_hash",), "the same, in llm-status"),
    (
        r"history\?",
        ("anchor_hash",),
        "the same: the browser's history pages are anchored to its branch's head",
    ),
    (
        r"history\?",
        ("next_cursor",),
        "the cursor holds the anchor (the branch's head); what else it holds (scope, minute, key) "
        "is compared",
    ),
    (
        r"timeline\?minute=0$",
        ("state_hash",),
        "the world's digest at minute 0: Python's Project route saves the world before the first "
        "tick with rng_state null, the exported world the browser starts from holds the RNG's "
        "initial state (algorithm and seed); every part of the world is the same, and from minute "
        "1 the digests are equal",
    ),
    (
        r"replay-frames\?branch_ids=main&start=0&",
        ("samples", "0", "state_hash"),
        "the same, in the first replay sample",
    ),
]


def _routes() -> list[str]:
    routes = [
        "/api/v1/projects",
        f"/api/v1/projects/{PROJECT}",
        f"/api/v1/projects/{PROJECT}/runs",
        RUN_PATH,
        f"{RUN_PATH}/evidence",
    ]
    # Saved points (every 360 minutes in the browser), then minutes between them.
    for minute in (0, 360, 720, 5040, 9720, 10080, 1, 359, 361, 4321, 7777, 10079):
        routes.append(f"{BRANCH_PATH}/timeline?minute={minute}")
    # Replay windows after a jump, then consecutive ones (the Worker steps its Simulation).
    for start in (0, 2000, 5152, 10072, 9000, 9008, 9016, 9024):
        routes.append(f"{RUN_PATH}/replay-frames?branch_ids={BRANCH}&start={start}&end={start + 7}")
    # The history graph's query (step ceil(10080 / 90) = 112, 100 a page) and others.
    for query in (
        "kind=kpis&start=0&end=10080&step=112&limit=100",
        "kind=kpis&start=1000&end=1100&step=1&limit=50",
        "kind=kpis&start=0&end=10080&step=1440&limit=10",
        "kind=kpis&start=0&end=10080&step=60",
    ):
        routes.append(f"{BRANCH_PATH}/history?{query}")
    routes += [f"{BRANCH_PATH}/report", f"{BRANCH_PATH}/llm-status"]
    return routes


def _differences(a: Any, b: Any, path: tuple[Any, ...], out: list[tuple[Any, ...]]) -> None:
    if type(a) is not type(b):
        out.append((path, repr(a)[:100], repr(b)[:100]))
    elif isinstance(a, dict):
        for key in sorted(set(a) | set(b)):
            if key not in a or key not in b:
                held = ("held" if key in a else "missing", "held" if key in b else "missing")
                out.append((path + (key,), *held))
            else:
                _differences(a[key], b[key], path + (key,), out)
    elif isinstance(a, list):
        if len(a) != len(b):
            out.append((path + ("length",), len(a), len(b)))
        for index, (x, y) in enumerate(zip(a, b, strict=False)):
            _differences(x, y, path + (index,), out)
    elif a != b:
        out.append((path, repr(a)[:100], repr(b)[:100]))


def _excluded(route: str, path: tuple[Any, ...]) -> str | None:
    for pattern, fields, reason in EXCLUDED:
        if not re.search(pattern, route) or len(fields) != len(path):
            continue
        if all(field == "*" or field == str(key) for field, key in zip(fields, path, strict=True)):
            return reason
    return None


class Python:
    """The Python server's answers on a ProjectStore, as server.py serializes them."""

    def __init__(self, projects: Path) -> None:
        os.environ["SHELTERSIM_LLM_MODE"] = "offline"
        os.environ.pop("ANTHROPIC_API_KEY", None)
        os.environ["XDG_STATE_HOME"] = tempfile.mkdtemp(prefix="sheltersim-site-check-")
        os.environ["SHELTERSIM_PROJECTS_DIR"] = str(projects)
        from sheltersim.storage.projects import ProjectStore
        from sheltersim.web.projects import ProjectAPI

        self.api = ProjectAPI(ProjectStore(projects), recover=False)

    def answer(self, route: str) -> tuple[int, str]:
        from sheltersim.storage.projects import StorageError

        path, _, query = route.partition("?")
        try:
            payload = self.api.get(path.split("/")[4:], parse_qs(query))
        except StorageError as error:
            return 409, json.dumps({"error": str(error), "code": error.code})
        return 200, json.dumps(
            payload, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False
        )

    def close(self) -> None:
        self.api.close()


FETCH = """async (route) => {
  const started = performance.now();
  const response = await fetch(route);
  const text = await response.text();
  return { status: response.status, text, ms: performance.now() - started };
}"""


def _cursor(cursor: str | None) -> dict[str, Any] | None:
    """A history cursor's content without its anchor (the branch's head, which differs)."""
    if cursor is None:
        return None
    document = json.loads(base64.urlsafe_b64decode(cursor + "=" * (-len(cursor) % 4)))
    return {key: value for key, value in document.items() if key != "anchor"}


def compare(page: Any, python: Python) -> dict[str, Any]:
    """The routes' answers in the browser and in Python, with their differences."""
    # Each item: the route asked of the browser and the one asked of Python (a history page after
    # the first carries each side's own cursor).
    queue = [(route, route) for route in _routes()]
    results = []
    while queue:
        route, python_route = queue.pop(0)
        got = page.evaluate(FETCH, route)
        started = time.perf_counter()
        status, text = python.answer(python_route)
        python_ms = (time.perf_counter() - started) * 1000
        found: list[tuple[Any, ...]] = []
        if got["status"] != status:
            found.append((("status",), status, got["status"]))
        else:
            _differences(json.loads(text), json.loads(got["text"]), (), found)
        if "history?" in route and status == got["status"] == 200:
            # The cursors name the branch's head: compare what else they hold.
            ours, theirs = json.loads(got["text"]), json.loads(text)
            if _cursor(ours.get("next_cursor")) != _cursor(theirs.get("next_cursor")):
                found.append(
                    (("next_cursor", "content"), theirs["next_cursor"], ours["next_cursor"])
                )
            # The screen follows next_cursor: so does the check, once.
            if ours.get("next_cursor") and "cursor=" not in route:
                label = route.split("?", 1)[1]
                queue.append(
                    (
                        f"{route}&cursor={ours['next_cursor']}",
                        f"{python_route}&cursor={theirs['next_cursor']}",
                    )
                )
                print(f"  (next page of {label})", flush=True)
        excluded = sorted({reason for path, *_ in found if (reason := _excluded(route, path))})
        left = [item for item in found if _excluded(route, item[0]) is None]
        results.append(
            {
                "route": route
                if "cursor=" not in route
                else route.split("&cursor=")[0] + "&cursor=…",
                "status": got["status"],
                "browser_ms": round(got["ms"], 1),
                "python_ms": round(python_ms, 1),
                "excluded": excluded,
                "differences": [list(map(str, item)) for item in left[:20]],
            }
        )
        verdict = "ok" if not left else f"{len(left)} differences"
        print(
            f"  {results[-1]['route']}: {got['status']} {got['ms']:.0f} ms, {verdict}", flush=True
        )
    return {"routes": results, "failed": [r["route"] for r in results if r["differences"]]}


SCREEN_STATE = """() => ({
  url: location.pathname + location.search + location.hash,
  view: document.body.dataset.view,
  notice: document.getElementById("notice").textContent,
  noticeError: document.getElementById("notice").classList.contains("error")
    && !document.getElementById("notice").hidden,
  seekMax: Number(document.getElementById("seek").max),
  seekDisabled: document.getElementById("seek").disabled,
  seekLabel: document.getElementById("seek-label").textContent,
  clock: document.getElementById("clock").textContent,
  replay: window.HinanjoReplayView ? { ...window.HinanjoReplayView } : null,
  historyNote: document.getElementById("history-note").textContent,
})"""


def screen(page: Any, base: str, screenshots: Path | None, name: str) -> dict[str, Any]:
    """Open `/`, then seek, play and look at the history graph, as a person would."""
    problems: list[str] = []
    steps: dict[str, Any] = {}
    started = time.perf_counter()
    page.goto(base + "/")
    page.wait_for_url(re.compile(r"/demo-v2\?project="), timeout=TIMEOUT_MS)
    steps["ready"] = page.evaluate("window.HinanjoBrowserApi.ready")
    page.wait_for_function(
        "() => document.body.dataset.view === 'simulation'"
        " && !document.getElementById('seek').disabled",
        timeout=TIMEOUT_MS,
    )
    steps["open_s"] = round(time.perf_counter() - started, 2)
    state = page.evaluate(SCREEN_STATE)
    steps["opened"] = state
    if state["seekMax"] != 10080:
        problems.append(f"the Run is not shown computed to 10080: {state['seekMax']}")
    # Seek: the slider's change asks for the frame of the minute.
    started = time.perf_counter()
    page.evaluate(
        "(minute) => { const s = document.getElementById('seek'); s.value = String(minute);"
        " s.dispatchEvent(new Event('change')); }",
        7777,
    )
    page.wait_for_function(
        "() => document.getElementById('seek').value === '7777'"
        " && !document.body.classList.contains('is-busy')"
        " && document.getElementById('seek-label').textContent.includes('6日目')",
        timeout=TIMEOUT_MS,
    )
    steps["seek_s"] = round(time.perf_counter() - started, 2)
    steps["sought"] = page.evaluate(SCREEN_STATE)
    if screenshots:
        page.screenshot(path=str(screenshots / f"{name}-seek.png"))
    # Play at 240x for a few seconds, then pause.
    page.select_option("#speed", "240")
    page.click("#play")
    page.wait_for_timeout(6000)
    playing = page.evaluate(SCREEN_STATE)
    page.click("#play")
    steps["played"] = playing
    replay = playing["replay"] or {}
    if not replay or replay.get("minute", 0) <= 7777 + 5:
        problems.append(f"playing did not move the clock: {replay}")
    if screenshots:
        page.screenshot(path=str(screenshots / f"{name}-play.png"))
    # The report page's history graph.
    started = time.perf_counter()
    page.click("[data-page=report]")
    page.wait_for_function(
        "() => /記録 [0-9]+点/.test(document.getElementById('history-note').textContent)",
        timeout=TIMEOUT_MS,
    )
    steps["report_s"] = round(time.perf_counter() - started, 2)
    steps["report"] = page.evaluate(SCREEN_STATE)
    if screenshots:
        page.screenshot(path=str(screenshots / f"{name}-report.png"), full_page=True)
    for key in ("opened", "sought", "played", "report"):
        if steps[key]["noticeError"]:
            problems.append(f"{key}: the notice shows an error: {steps[key]['notice']}")
    steps["stats"] = page.evaluate("window.HinanjoBrowserApi.stats()")
    return {"steps": steps, "problems": problems}


def check(
    site: Path, browsers: list[str], projects: Path | None, screenshots: Path | None
) -> dict[str, Any]:
    from playwright.sync_api import sync_playwright

    server = serve_static_site.start(site)
    base = f"http://127.0.0.1:{server.server_port}"
    python = Python(projects) if projects else None
    results: dict[str, Any] = {}
    try:
        with sync_playwright() as playwright:
            for name in browsers:
                print(name, flush=True)
                browser = getattr(playwright, name).launch()
                errors: list[str] = []
                result: dict[str, Any] = {}
                try:
                    page = browser.new_context().new_page()
                    page.set_default_timeout(TIMEOUT_MS)
                    page.on("pageerror", lambda error: errors.append(f"page: {error}"))
                    page.on(
                        "console",
                        lambda message: errors.append(f"console: {message.text}")
                        if message.type == "error"
                        else None,
                    )
                    result["screen"] = screen(page, base, screenshots, name)
                    if python is not None:
                        result["compare"] = compare(page, python)
                finally:
                    browser.close()
                result["errors"] = errors
                results[name] = result
    finally:
        if python is not None:
            python.close()
        server.shutdown()
        server.server_close()
    results["passed"] = all(
        not r["errors"] and not r["screen"]["problems"] and not r.get("compare", {}).get("failed")
        for name, r in results.items()
    )
    results["excluded"] = [
        {"routes": pattern, "field": ".".join(fields), "reason": reason}
        for pattern, fields, reason in EXCLUDED
    ]
    return results


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("--site", type=Path, required=True)
    parser.add_argument("--python-projects", type=Path)
    parser.add_argument("--browsers", nargs="+", default=["chromium", "firefox"])
    parser.add_argument("--output", type=Path)
    parser.add_argument("--screenshots", type=Path)
    arguments = parser.parse_args()
    if arguments.screenshots:
        arguments.screenshots.mkdir(parents=True, exist_ok=True)
    results = check(
        arguments.site.resolve(),
        arguments.browsers,
        arguments.python_projects.resolve() if arguments.python_projects else None,
        arguments.screenshots,
    )
    text = json.dumps(results, ensure_ascii=False, indent=1)
    if arguments.output:
        arguments.output.write_text(text + "\n", encoding="utf-8")
    for name in arguments.browsers:
        result = results[name]
        print(
            f"{name}: errors {len(result['errors'])}, screen problems "
            f"{result['screen']['problems']}, routes failing "
            f"{result.get('compare', {}).get('failed')}"
        )
    print("passed" if results["passed"] else "FAILED")
    return 0 if results["passed"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
