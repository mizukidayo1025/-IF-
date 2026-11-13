"""Walk the whole /demo-v2 flow in the browser version and compare it with the Python server (#311).

    python benchmarks/static_site_e2e.py --site SITE_DIR --work DIR [--python-projects DIR]
        [--browsers chromium firefox] [--modes full pieces] [--stages ...] [--output result.json]
        [--screenshots DIR] [--require-all]
    python benchmarks/static_site_e2e.py --url https://example.org [--browsers ...] [--output ...]

SITE_DIR is the output of scripts/build_static_site.py, served as a static host would
(scripts/serve_static_site.py). Each browser walks the screen on a fresh profile (an empty OPFS),
as a person would, in five stages:

1. scenario: on `/demo-v2?project=e2e#scenario`, 「例文を使う」 → preview → approve (a Run), then
   one form-input variant → preview → approve (a second Run); then `POST runs` of the first
   Scenario under another id, and of another Scenario under that id (409 run_id_conflict).
2. compute: the first Run computed on the screen to its end: at once in the first browser
   (`--modes`, full), a day, cancel, resume to the end in the second (pieces).
3. intervention: at Day 4 14:00 (minute 5160) the screen previews a few sentences (SENTENCES: the
   offline parser's add_staff, whose Zone is then picked with the screen's chip, add_toilet,
   water_outage, one it cannot read, and facility_outage), then applies "Day 4 14:00 toilet_01
   outage" and computes the child to the end.
4. compare: the screen's comparison of the two branches, then `compare` at several minutes and
   two-branch replay windows.
5. report: the report page (report, evidence, llm-status, the KPI history), a question (the
   offline fixed answer) and the PDF (409 pdf_analysis_offline, as the offline VM).

At each step the JSON the browser answered (the screen's own requests, caught by a wrapper of
`fetch`, and the checks' requests) is compared with what the Python server answers to the same
operation: `ProjectAPI` in this process on its own ProjectStore under --work, serialized as
server.py does, in the offline mode without an API key. The Python side does the same operations
in the same order once and its answers are shared by the browsers. Values are compared with their
types; only the fields EXCLUDED and MASKED list (random or time-dependent, or a head that differs
by design) are left out, and the result lists them with the reasons. What is pinned so the
answers can be equal: the screen's `crypto.randomUUID` (the Run and branch ids, PAGE_INIT), the
regional research's clock (Python's `datetime.now`, the browser Worker's `Date`) and the GSI
address search (recorded answers on both sides); see `python_pins` and `pin_browser`.

A step the browser answers with 409 `browser_unsupported` is "not available yet": the stage
stops there and the stages that need it are skipped. When stage 1 is not available, the stages
after it use the site's sample instead (with --python-projects, a ProjectStore of
scripts/build_project_sample.py): stage 2 computes the site's compute Run and compares it with
the sample's main branch, stages 3–5 work on the sample's Run. Exit 0 when no check fails (with
--require-all, only when every stage passed in every browser), 1 otherwise.

--work keeps the Python side between runs: its stores and answers are reused while the Python
code, the native extension, the plan (inputs, routes, PYTHON_SIDE_VERSION) and the inputs are the
same (`--fresh` starts again). One run takes about 7 minutes on 16 cores (the Python side about 4
of them, once).

--url checks a deployed site on the screen only: every stage's steps must answer (or be
"not available yet") and the screen must show no error; nothing is compared with Python and
nothing is pinned (the GSI address search is the live one).

Playwright and its browsers are not dependencies of the project (see static_site_check.py). Run
it under a memory cap (docs/development.md).
"""

from __future__ import annotations

import argparse
import contextlib
import hashlib
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from collections.abc import Callable, Iterator
from dataclasses import dataclass
from datetime import UTC, datetime
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs, quote, unquote, urlsplit

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "benchmarks"))
sys.path.insert(0, str(ROOT / "scripts"))
import serve_static_site  # noqa: E402 - the local server of the site, a script
import static_site_check as site_check  # noqa: E402 - its comparison helpers

STAGES = ("scenario", "compute", "intervention", "compare", "report")
PROJECT = "e2e"
SAMPLE_PROJECT, SAMPLE_RUN = site_check.PROJECT, site_check.RUN
COMPUTE_RUN = site_check.COMPUTE_RUN
TIMEOUT_MS = site_check.TIMEOUT_MS
# The screen's 「例文を使う」 (demo-v2.js).
EXAMPLE_TEXT = "墨田区で地震が発生。避難者100人、7日間。高齢者や子ども連れの支援ニーズも見たい。"
# The form variant: the fields the harness fills (element id → value) and the request the screen
# makes of them (demo-v2.js scenarioRequest), with no text.
FORM_FIELDS = {
    "municipality": "墨田区",
    "disaster": "洪水",
    "evacuees": "40",
    "duration": "1.5",
    "facility-type": "公民館",
    "staff-count": "3",
    "area": "600",
    "toilets": "1",
    "food": "120",
    "water": "200",
}
FORM_REQUEST = {
    "municipality": "墨田区",
    "disaster": "洪水",
    "evacuees": 40,
    "duration_days": 1.5,
    "staff_count": 3,
    "gross_area_m2": 600,
    "accessible_toilet_count": 1,
    "facility_conditions": ["施設種別（利用者入力）: 公民館"],
    "operational_conditions": {
        "initial_resources": {
            "status": "entered",
            "food_meal_units": 120,
            "water_ml": 200000,
            "source_kind": "operator",
        }
    },
    "field_origins": {
        key: "user"
        for key in (
            "municipality",
            "disaster",
            "evacuees",
            "duration_days",
            "staff_count",
            "gross_area_m2",
            "accessible_toilet_count",
            "facility_conditions",
            "operational_conditions",
        )
    },
}
FORK_MINUTE = 3 * 1440 + 14 * 60
# The sentences previewed at FORK_MINUTE, as (label, text, the Zone chip clicked after it): the
# offline parser's add_staff (which asks for the Zone: the screen's chip previews again with
# `zone_id:`), add_toilet, water_outage (not simulatable: the example's shelter has no water
# station), a sentence it cannot read, and facility_outage, which is applied
# (scripts/build_project_sample.py's intervention).
SENTENCES = (
    ("add_staff", "Day 4 14:00 スタッフを2人追加", "staff"),
    ("add_toilet", "Day 4 14:00 トイレを1基追加", None),
    ("water_outage", "Day 4 14:00 断水", None),
    ("unsupported", "避難所の雰囲気を明るくする", None),
    ("facility_outage", "Day 4 14:00 toilet_01 outage", None),
)
QUESTION = "トイレの待ち時間はどう変わりましたか？"
# Minutes and windows the checks ask for.
TIMELINE_MINUTES = (1, 360, 1440, 4321, FORK_MINUTE, 7777, 10080)
REPLAY_STARTS = (0, 5152, 9000, 9008, 10072)
CHILD_MINUTES = (FORK_MINUTE, FORK_MINUTE + 1, 6000, 8640, 10080)
COMPARE_MINUTES = (FORK_MINUTE, FORK_MINUTE + 1, 7200, 10080)
PAIR_REPLAY_STARTS = (FORK_MINUTE, 8000, 8008, 10073)
HISTORY_QUERIES = (
    "kind=kpis&start=0&end=10080&step=112&limit=100",
    "kind=kpis&start=5000&end=5400&step=1&limit=50",
)
# A child's history starts at its fork.
CHILD_HISTORY_QUERIES = (
    f"kind=kpis&start={FORK_MINUTE}&end=10080&step=55&limit=100",
    f"kind=kpis&start={FORK_MINUTE}&end=5400&step=1&limit=50",
)
UNSUPPORTED = "browser_unsupported"


def uuid(number: int) -> str:
    """The screen's n-th `crypto.randomUUID()` in a profile (PAGE_INIT). Its groups hold
    letters: Python refuses to save text that looks like a phone number (three groups of
    digits, storage/projects.py PHONE_NUMBER), as `0000-4000-8000` would."""
    return f"e2e00000-e2e0-4e2e-ae2e-{number:012x}"


# What the browser answers that differs by design, beyond static_site_check.EXCLUDED, as (route
# pattern, field path pattern, why); "*" is any key or index.
EXCLUDED = [
    *site_check.EXCLUDED,
    (
        r"scenario-preview$",
        ("preview_id",),
        "a preview's id: Python's is a uuid4, the browser's its own random id",
    ),
    (r"scenario-preview$", ("draft_id",), "the draft's id: a uuid4 in Python"),
    (
        r"intervention-preview$",
        ("preview_id",),
        "a preview's id: Python's is a uuid4, the browser's its own random id",
    ),
]
# Keys left out wherever they are, and why.
MASKED = {
    "checkpoint_hash": "a branch's head (or a fork's origin): the browser's is the sha256 of its "
    "stored Run's journal line (docs/project-persistence.md), Python's a Checkpoint's hash",
}
CONTEXT_ID = re.compile(r"^(c[0-9a-f]{32}-)[0-9a-f]{24}$")
NORMALIZED = {
    "context_id": "a question's context: c<sha256 of the context>-<12 random bytes>; the random "
    "tail is left out",
    "projects": "the Project list holds only the Project the stage works on (the browser also "
    "holds the site's sample)",
}

# ---- The page and the Worker -------------------------------------------------------------------

# Before the page's scripts: pins `crypto.randomUUID` (e2e00000-e2e0-4e2e-ae2e-<n>, n counted in
# the origin's localStorage, so the Run and branch ids the screen makes are known) and catches each
# `/api/v1/...` request the page makes, wrapping whatever `window.fetch` is (the browser build's
# api_fetch.js replaces it).
PAGE_INIT = """(() => {
  const log = [];
  Object.defineProperty(window, "__e2eLog", { value: log });
  const KEY = "sheltersim-e2e-uuid";
  try {
    Object.defineProperty(Crypto.prototype, "randomUUID", {
      configurable: true,
      value() {
        let n = 1;
        try {
          n = Number(localStorage.getItem(KEY) || "0") + 1;
          localStorage.setItem(KEY, String(n));
        } catch {}
        return `e2e00000-e2e0-4e2e-ae2e-${n.toString(16).padStart(12, "0")}`;
      },
    });
  } catch {}
  const wrap = (inner) => function fetch(input, init) {
    let url;
    try {
      url = new URL(typeof input === "string" ? input : input.url, location.href);
    } catch {
      return inner(input, init);
    }
    if (url.origin !== location.origin || !url.pathname.startsWith("/api/v1/")) {
      return inner(input, init);
    }
    const asked = init?.method || (typeof input === "string" ? "GET" : input.method);
    const entry = {
      method: String(asked || "GET").toUpperCase(),
      path: url.pathname,
      search: url.search,
      body: typeof init?.body === "string" ? init.body : null,
      started: performance.now(),
      status: null,
      text: null,
    };
    log.push(entry);
    return inner(input, init).then(async (response) => {
      try {
        entry.text = await response.clone().text();
      } catch {}
      entry.ms = performance.now() - entry.started;
      entry.status = response.status;
      return response;
    }, (error) => {
      entry.status = -1;
      entry.text = String(error);
      throw error;
    });
  };
  let current = wrap(window.fetch.bind(window));
  Object.defineProperty(window, "fetch", {
    configurable: true,
    get: () => current,
    set: (value) => {
      current = wrap(value);
    },
  });
})();"""

# The research's clock in the browser: prepended to the API Worker's module, `Date` reads NOW.
WORKER_PREAMBLE = (
    "const __e2eNow = %d;\n"
    "{ const RealDate = Date; class FrozenDate extends RealDate {"
    " constructor(...a) { super(...(a.length ? a : [__e2eNow])); }"
    " static now() { return __e2eNow; } }"
    " globalThis.Date = FrozenDate; }\n"
)

POST = """async ([route, body]) => {
  const started = performance.now();
  const headers = { "Content-Type": "application/json" };
  const response = await fetch(route, { method: "POST", headers, body });
  const text = await response.text();
  return { status: response.status, text, ms: performance.now() - started };
}"""


# ---- Pinning -----------------------------------------------------------------------------------


def _corpus() -> Any:
    """The scenario corpus's recordings (#293): the clock and the public services, pinned."""
    sys.path.insert(0, str(ROOT / "tests"))
    try:
        from support import scenario_corpus
    except ImportError as error:  # pragma: no cover - before #293's tooling is on main
        raise RuntimeError(
            "pinning the regional research needs tests/support/scenario_corpus.py (#293)"
        ) from error
    finally:
        sys.path.remove(str(ROOT / "tests"))
    return scenario_corpus


@contextlib.contextmanager
def python_pins(directory: Path, shelter_csv: Path | None) -> Iterator[None]:
    """Python's research with the clock at the corpus's NOW and the public services recorded
    (the GSI address search's ADDRESS_HITS; the national shelter CSV: shelter_csv, else the
    corpus's synthetic one). The site must be built with the same CSV (its sha256 is baked in)."""
    corpus = _corpus()
    with corpus.recorded_world(directory):
        if shelter_csv is None:
            yield
            return
        saved = corpus.SHELTER_CSV
        corpus.SHELTER_CSV = shelter_csv.read_text(encoding="utf-8")
        try:
            yield
        finally:
            corpus.SHELTER_CSV = saved


def python_shelter_index(shelter_csv: Path | None) -> dict[str, Any] | None:
    """The shelter index (#300) of the CSV Python's research reads under python_pins: what the
    site's `/browser/gsi-shelter-index.json` holds when built from the same CSV, but for the time
    and HTTP metadata of its retrieval. None before the browser's research is on main."""
    from sheltersim.region import providers

    if not hasattr(providers, "gsi_shelter_index"):
        return None
    with tempfile.TemporaryDirectory(prefix="sheltersim-e2e-index-") as scratch:
        with python_pins(Path(scratch), shelter_csv):
            snapshot = providers._get_gsi_shelter_snapshot(providers._GSI_SHELTER_CSV_URL)
            return providers.gsi_shelter_index(snapshot)


# The fields of the site's shelter index that tell when and how the CSV was retrieved: the site's
# are its build's, Python's its research's (pinned), so the browser is given Python's.
INDEX_RETRIEVAL = ("retrieved_at", "etag", "last_modified")


def pin_browser(context: Any, index: dict[str, Any] | None, notes: list[str]) -> None:
    """The browser's research pinned as Python's: the API Worker's `Date` at NOW, the GSI
    address search answered from the same recordings (in the service's GeoJSON form), and the
    site's shelter index with Python's retrieval time and metadata (`index`, when the site's is
    of the same CSV; otherwise a note says so and the site's is served as it is)."""
    corpus = _corpus()
    now_ms = int(corpus.NOW.timestamp() * 1000)

    def shelters(route: Any) -> None:
        response = route.fetch()
        try:
            document = json.loads(response.text())
        except ValueError:
            document = None
        if not isinstance(document, dict) or index is None:
            route.fulfill(response=response)
            return
        if document.get("sha256") != index.get("sha256"):
            note = (
                "the site's shelter index is of another CSV than Python's research reads: build "
                "the site with --shelter-csv of the same file"
            )
            if note not in notes:
                notes.append(note)
            route.fulfill(response=response)
            return
        pinned = {**document, **{key: index.get(key) for key in INDEX_RETRIEVAL}}
        route.fulfill(response=response, body=json.dumps(pinned, ensure_ascii=False))

    def worker(route: Any) -> None:
        response = route.fetch()
        route.fulfill(response=response, body=WORKER_PREAMBLE % now_ms + response.text())

    def address(route: Any) -> None:
        query = parse_qs(urlsplit(route.request.url).query).get("q", [""])[0]
        document = [
            {
                "geometry": {"coordinates": [longitude, latitude], "type": "Point"},
                "type": "Feature",
                "properties": {"addressCode": "", "title": title},
            }
            for title, longitude, latitude in corpus.ADDRESS_HITS.get(query, [])
        ]
        route.fulfill(
            status=200,
            headers={"access-control-allow-origin": "*", "content-type": "application/json"},
            body=json.dumps(document, ensure_ascii=False),
        )

    context.route("**/browser/api_worker.mjs", worker)
    context.route("**/browser/gsi-shelter-index.json", shelters)
    context.route("https://msearch.gsi.go.jp/**", address)


# ---- Comparing ---------------------------------------------------------------------------------


def _masked(route: str, path: tuple[Any, ...]) -> str | None:
    if path and str(path[-1]) in MASKED:
        return MASKED[str(path[-1])]
    return site_check._excluded(route, path) or next(
        (
            reason
            for pattern, fields, reason in EXCLUDED[len(site_check.EXCLUDED) :]
            if re.search(pattern, route)
            and len(fields) == len(path)
            and all(f == "*" or f == str(k) for f, k in zip(fields, path, strict=True))
        ),
        None,
    )


def _normalize(route: str, document: Any, project: str) -> tuple[Any, list[str]]:
    """The answer with what NORMALIZED lists made comparable, and which of it applied."""
    applied = []
    if isinstance(document, dict):
        if route.endswith("/questions") and isinstance(document.get("context_id"), str):
            match = CONTEXT_ID.match(document["context_id"])
            if match:
                document = {**document, "context_id": match.group(1) + "…"}
                applied.append(NORMALIZED["context_id"])
        if route == "/api/v1/projects" and isinstance(document.get("projects"), list):
            document = {
                **document,
                "projects": [p for p in document["projects"] if p.get("project_id") == project],
            }
            applied.append(NORMALIZED["projects"])
    return document, applied


def _unsupported(answer: dict[str, Any]) -> bool:
    if answer.get("status") != 409:
        return False
    try:
        return json.loads(answer["text"]).get("code") == UNSUPPORTED
    except (TypeError, ValueError, AttributeError):
        return False


def compare_answers(
    name: str,
    route: str,
    browser: dict[str, Any],
    python: tuple[int, str] | None,
    *,
    project: str,
    translate: Callable[[str], str] = lambda text: text,
    expect: Callable[[int, Any], str | None] | None = None,
) -> dict[str, Any]:
    """One check: the browser's answer {status, text, ms} against Python's (status, text), or
    with python None (--url) only that the browser answered (and `expect`, if given)."""
    check: dict[str, Any] = {
        "name": name,
        "route": route if "cursor=" not in route else route.split("&cursor=")[0] + "&cursor=…",
        "status": browser.get("status"),
        "browser_ms": round(browser.get("ms") or 0, 1),
    }
    if _unsupported(browser):
        check["verdict"] = "not_available"
        check["note"] = json.loads(browser["text"]).get("error")
        return check
    try:
        ours = json.loads(translate(browser["text"])) if browser.get("text") else None
    except ValueError:
        ours = browser.get("text")
    found: list[tuple[Any, ...]] = []
    applied: list[str] = []
    if python is not None:
        status, text = python
        theirs = json.loads(text) if text.startswith(("{", "[")) else text
        if browser.get("status") != status:
            found.append((("status",), status, browser.get("status")))
        ours, applied_ours = _normalize(route, ours, project)
        theirs, _ = _normalize(route, theirs, project)
        applied += applied_ours
        site_check._differences(theirs, ours, (), found)
    elif browser.get("status") != 200 and expect is None:
        found.append((("status",), 200, browser.get("status")))
    if expect is not None:
        problem = expect(browser.get("status"), ours)
        if problem:
            found.append((("expected",), problem, ""))
    excluded = sorted({reason for path, *_ in found if (reason := _masked(route, path))})
    left = [item for item in found if _masked(route, item[0]) is None]
    check["excluded"] = sorted(set(excluded) | set(applied))
    check["differences"] = [list(map(str, item)) for item in left[:20]]
    check["verdict"] = "ok" if not left else "failed"
    return check


# ---- The Python side ---------------------------------------------------------------------------


def _answer_text(payload: Any) -> str:
    return json.dumps(
        payload, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False
    )


@dataclass
class Subject:
    """What stages 2–5 work on: a Run of `project` in both, its id in the browser and in Python
    (they differ only for the sample's compute Run), the child branch's id."""

    chain: str
    project: str
    run: str
    child: str
    browser_run: str | None = None
    form_run: str | None = None
    copy_run: str | None = None

    def browser(self, stage: str) -> str:
        return self.browser_run if stage == "compute" and self.browser_run else self.run

    def translate(self, stage: str) -> Callable[[str], str]:
        browser = self.browser(stage)
        if browser == self.run:
            return lambda text: text
        return lambda text: text.replace(browser, self.run)


def e2e_subject() -> Subject:
    return Subject(
        "e2e",
        PROJECT,
        f"run-{uuid(1)}",
        f"branch-{uuid(3)}",
        form_run=f"run-{uuid(2)}",
        copy_run="run-e2e-copy",
    )


def sample_subject() -> Subject:
    return Subject("sample", SAMPLE_PROJECT, SAMPLE_RUN, f"branch-{uuid(1)}", COMPUTE_RUN)


def _key(*parts: bytes) -> str:
    digest = hashlib.sha256()
    for part in parts:
        digest.update(hashlib.sha256(part).digest())
    return digest.hexdigest()


# The version of the Python side's operations (PythonSide's stages): bump it when they change, so
# answers kept in --work are made again.
PYTHON_SIDE_VERSION = 1


def _plan() -> bytes:
    """The operations' inputs and the checks' routes, as bytes."""
    subjects = (e2e_subject(), sample_subject())
    plan = {
        "version": PYTHON_SIDE_VERSION,
        "inputs": [EXAMPLE_TEXT, FORM_REQUEST, FORK_MINUTE, SENTENCES[-1]],
        "routes": [
            [route(subject) for route in (scenario_routes, compute_routes, intervention_routes)]
            + [compare_routes(subject), report_routes(subject)]
            for subject in subjects
        ],
    }
    return _answer_text(plan).encode()


def _code_key(extra: bytes) -> str:
    """What the Python side's answers depend on: the Python code and the corpus's pins
    (committed and changed), the native extension, the plan, and the inputs (`extra`)."""
    import sheltersim_native

    native = Path(sheltersim_native.__file__).parent
    native_bytes = b"".join(path.read_bytes() for path in sorted(native.glob("*.so")))

    def git(*arguments: str) -> bytes:
        return subprocess.run(
            ["git", "-C", str(ROOT), *arguments], capture_output=True, check=False
        ).stdout

    return _key(
        git("rev-parse", "HEAD:src", "HEAD:tests/support"),
        git("diff", "HEAD", "--", "src", "tests/support"),
        native_bytes,
        _plan(),
        extra,
    )


class PythonSide:
    """The Python server's answers to the stages of one chain (`e2e`: the Runs the stages make;
    `sample`: the sample's Run), in this process, each stage done once and in order.

    A stage's operations (the ones that change the store) are Python's own, with the same bodies
    the screen sends (the ids pinned) and Python's own preview ids; its checks' answers (GETs and
    the stage's other POSTs) are kept with the stage's state. Answers to other requests are made
    on demand at the state the chain has reached. With `work`, the store and answers stay in
    `work/<chain>` and are reused while `key` is the same."""

    def __init__(
        self,
        subject: Subject,
        work: Path,
        *,
        sample_projects: Path | None,
        shelter_csv: Path | None,
        fresh: bool,
    ) -> None:
        self.subject = subject
        self.directory = work / subject.chain
        self.sample_projects = sample_projects
        self.shelter_csv = shelter_csv
        self.stack = contextlib.ExitStack()
        self.api: Any = None
        csv = shelter_csv.read_bytes() if shelter_csv else b""
        sample = (sample_projects / "sample-build.json") if sample_projects else None
        if sample is not None and not sample.exists():
            sample = sample_projects.parent / "sample-build.json"
        self.key = _code_key(csv + (sample.read_bytes() if sample and sample.exists() else b""))
        self.answers_path = self.directory / "answers.json"
        saved = None
        if not fresh and self.answers_path.exists():
            saved = json.loads(self.answers_path.read_text(encoding="utf-8"))
        if saved is None or saved.get("key") != self.key:
            shutil.rmtree(self.directory, ignore_errors=True)
            saved = {"key": self.key, "done": [], "answers": {}}
        self.state = saved
        self.timings: dict[str, float] = {}

    # The store and the API, opened at first use.
    def _open(self) -> Any:
        if self.api is not None:
            return self.api
        os.environ["SHELTERSIM_LLM_MODE"] = "offline"
        os.environ.pop("ANTHROPIC_API_KEY", None)
        self.directory.mkdir(parents=True, exist_ok=True)
        store = self.directory / "store"
        if self.subject.chain == "sample" and not store.exists():
            if self.sample_projects is None:
                raise RuntimeError("the sample's stages need --python-projects")
            shutil.copytree(self.sample_projects, store)
        if self.subject.chain == "e2e":
            # The clock and the public services pinned; the usage ledgers under the directory.
            self.stack.enter_context(python_pins(self.directory / "pins", self.shelter_csv))
        else:
            state = Path(tempfile.mkdtemp(prefix="sheltersim-e2e-state-", dir=self.directory))
            os.environ["XDG_STATE_HOME"] = str(state)
            os.environ["SHELTERSIM_PROJECTS_DIR"] = str(store)
        from sheltersim.storage.projects import ProjectStore
        from sheltersim.web.projects import ProjectAPI

        # As server.py: each computation and fork in its own process.
        self.api = ProjectAPI(ProjectStore(store), worker="process", recover=False)
        self.stack.callback(self.api.close)
        return self.api

    def close(self) -> None:
        self.stack.close()
        self.api = None

    def _save(self) -> None:
        self.directory.mkdir(parents=True, exist_ok=True)
        self.answers_path.write_text(_answer_text(self.state), encoding="utf-8")

    def request(self, method: str, route: str, body: Any = None) -> tuple[int, str]:
        """Python's answer, as server.py makes it, to the request at the store's state now."""
        from sheltersim.storage.projects import StorageError
        from sheltersim.web.inspector import WebQueryError

        api = self._open()
        path, _, query = route.partition("?")
        parts = [unquote(part) for part in path.split("/") if part][3:]
        try:
            if method == "GET":
                payload = api.get(parts, parse_qs(query))
            elif len(parts) == 4 and parts[1] == "runs" and parts[3] == "pdf-report":
                pdf = api.pdf_reports.export(parts[0], parts[2], body)
                return 200, f"<application/pdf, {len(pdf)} bytes>"
            else:
                payload = api.post(parts, body)
        except StorageError as error:
            return 409, _answer_text({"error": str(error), "code": error.code})
        except WebQueryError as error:
            if method == "GET":
                return 400, _answer_text({"error": str(error)})
            return 400, _answer_text({"error": "invalid project request"})
        except (ValueError, UnicodeDecodeError):
            if method == "GET":
                return 400, _answer_text(
                    {"error": "invalid query or saved contract", "code": "invalid_input"}
                )
            return 400, _answer_text({"error": "invalid project request"})
        return 200, _answer_text(payload)

    def _keep(self, key: str, status: int, text: str) -> None:
        """Keeps an answer: its text in a file named by its sha256, its status in the index."""
        data = text.encode()
        name = hashlib.sha256(data).hexdigest()
        folder = self.directory / "answers"
        folder.mkdir(parents=True, exist_ok=True)
        if not (folder / name).exists():
            (folder / name).write_bytes(data)
        self.state["answers"][key] = [status, name]

    def _kept(self, key: str) -> tuple[int, str] | None:
        found = self.state["answers"].get(key)
        if not found:
            return None
        return found[0], (self.directory / "answers" / found[1]).read_text(encoding="utf-8")

    def _memo(self, stage: str, name: str, method: str, route: str, body: Any = None) -> None:
        self._keep(f"{stage}:{name}", *self.request(method, route, body))

    def answer(self, stage: str, name: str) -> tuple[int, str] | None:
        """The answer a stage kept under `name`."""
        self.at(stage)
        return self._kept(f"{stage}:{name}")

    def on_demand(self, stage: str, method: str, route: str, body: Any = None) -> tuple[int, str]:
        """The answer to a request a stage did not make itself (kept with the chain)."""
        self.at(stage)
        key = f"{stage}:on-demand {method} {route} {_answer_text(body)}"
        if key not in self.state["answers"]:
            self._keep(key, *self.request(method, route, body))
            self._save()
        found = self._kept(key)
        assert found is not None
        return found

    def at(self, stage: str) -> None:
        """Do the stages up to `stage` that are not done yet."""
        for each in STAGES[: STAGES.index(stage) + 1]:
            if each in self.state["done"]:
                continue
            started = time.perf_counter()
            print(f"  python: stage {each} ({self.subject.chain})", flush=True)
            getattr(self, f"_stage_{each}")()
            self.timings[each] = round(time.perf_counter() - started, 1)
            self.state["done"].append(each)
            self._save()

    def _compute(self, stage: str, name: str, branch: str, end: int) -> None:
        """Resumes `branch` to `end` as the screen's compute button does (the answer kept under
        `name`) and waits for it."""
        s = self.subject
        path = f"{_run_path(s.project, s.run)}/resume"
        self._memo(stage, name, "POST", path, {"branch_id": branch, "target_minute": end})
        self._open().wait_idle()
        reached = self.api.store.branch_reference(s.project, s.run, branch).computed_until
        if reached != end:
            raise RuntimeError(f"Python computed {branch} to {reached}, not {end}")

    def _probe(self, stage: str, routes: list[tuple[str, str]]) -> None:
        for name, route in routes:
            self._memo(stage, name, "GET", route)

    # The stages: the operations, then the checks' answers.
    def _stage_scenario(self) -> None:
        s = self.subject
        if s.chain != "e2e":
            return
        api = self._open()
        base = f"/api/v1/projects/{s.project}"
        self._memo(
            "scenario", "create_project", "POST", "/api/v1/projects", {"project_id": s.project}
        )
        for name, body, run in (
            ("text", {"text": EXAMPLE_TEXT}, s.run),
            ("form", {"text": "", "request": FORM_REQUEST}, s.form_run),
        ):
            self._memo("scenario", f"preview_{name}", "POST", f"{base}/scenario-preview", body)
            preview = json.loads(self._kept(f"scenario:preview_{name}")[1])
            approve = {
                "preview_id": preview.get("preview_id"),
                "approved_hash": preview.get("approved_hash"),
                "approved": True,
                "run_id": run,
            }
            self._memo("scenario", f"approve_{name}", "POST", f"{base}/scenario-approve", approve)
        text_run = api.store.run(s.project, s.run)
        form_run = api.store.run(s.project, s.form_run)
        copy = {
            "run_id": s.copy_run,
            "scenario_id": text_run["scenario_id"],
            "scenario_version": text_run["scenario_version"],
        }
        conflict = {
            **copy,
            "scenario_id": form_run["scenario_id"],
            "scenario_version": form_run["scenario_version"],
        }
        self._memo("scenario", "post_run", "POST", f"{base}/runs", copy)
        self._memo("scenario", "post_run_conflict", "POST", f"{base}/runs", conflict)
        self._probe("scenario", scenario_routes(s))

    def _stage_compute(self) -> None:
        if self.subject.chain == "e2e":
            self._compute("compute", "resume_main", "main", 10080)
        self._probe("compute", compute_routes(self.subject))

    def _stage_intervention(self) -> None:
        s = self.subject
        branch = f"/api/v1/projects/{s.project}/runs/{s.run}/branches/main"
        text = SENTENCES[-1][1]
        self._memo(
            "intervention",
            "preview_apply",
            "POST",
            f"{branch}/intervention-preview",
            {"text": text, "minute": FORK_MINUTE},
        )
        preview = json.loads(self._kept("intervention:preview_apply")[1])
        self._memo(
            "intervention",
            "apply",
            "POST",
            f"{branch}/intervention-apply",
            {
                "preview_id": preview.get("preview_id"),
                "approved_program_hash": preview.get("approved_program_hash"),
                "approved": True,
                "child_id": s.child,
            },
        )
        self._open().wait_idle()
        forked = self.api.get([s.project, "runs", s.run], {}).get("last_fork") or {}
        if forked.get("status") != "created":
            raise RuntimeError(f"Python's fork did not create {s.child}: {forked}")
        self._compute("intervention", "resume_child", s.child, 10080)
        self._probe("intervention", intervention_routes(s))

    def _stage_compare(self) -> None:
        self._probe("compare", compare_routes(self.subject))

    def _stage_report(self) -> None:
        self._probe("report", report_routes(self.subject))


# ---- The checks' routes ------------------------------------------------------------------------


def _run_path(project: str, run: str) -> str:
    return f"/api/v1/projects/{project}/runs/{run}"


def scenario_routes(s: Subject) -> list[tuple[str, str]]:
    base = f"/api/v1/projects/{s.project}"
    routes = [
        ("projects", "/api/v1/projects"),
        ("project", base),
        ("runs", f"{base}/runs"),
    ]
    for label, run in (("text", s.run), ("form", s.form_run), ("copy", s.copy_run)):
        routes += [
            (f"run_{label}", _run_path(s.project, run)),
            (f"evidence_{label}", f"{_run_path(s.project, run)}/evidence"),
            (f"timeline0_{label}", f"{_run_path(s.project, run)}/branches/main/timeline?minute=0"),
        ]
    return routes


def compute_routes(s: Subject, run: str | None = None) -> list[tuple[str, str]]:
    path = _run_path(s.project, run or s.run)
    routes = [] if s.chain == "sample" else [("run", path)]
    routes += [
        (f"timeline_{m}", f"{path}/branches/main/timeline?minute={m}") for m in TIMELINE_MINUTES
    ]
    routes += [
        (f"replay_{start}", f"{path}/replay-frames?branch_ids=main&start={start}&end={start + 7}")
        for start in REPLAY_STARTS
    ]
    routes += [
        (f"history_{i}", f"{path}/branches/main/history?{query}")
        for i, query in enumerate(HISTORY_QUERIES)
    ]
    return routes


def intervention_routes(s: Subject) -> list[tuple[str, str]]:
    path = _run_path(s.project, s.run)
    child = f"{path}/branches/{quote(s.child)}"
    return [
        ("run", path),
        *[(f"child_timeline_{m}", f"{child}/timeline?minute={m}") for m in CHILD_MINUTES],
        ("child_history", f"{child}/history?{CHILD_HISTORY_QUERIES[1]}"),
    ]


def compare_routes(s: Subject) -> list[tuple[str, str]]:
    path = _run_path(s.project, s.run)
    pair = quote(f"main,{s.child}")
    return [
        *[
            (f"compare_{m}", f"{path}/compare?branch_ids={pair}&minute={m}")
            for m in COMPARE_MINUTES
        ],
        *[
            (
                f"replay_pair_{start}",
                f"{path}/replay-frames?branch_ids={pair}&start={start}&end={start + 7}",
            )
            for start in PAIR_REPLAY_STARTS
        ],
    ]


def report_routes(s: Subject, child: bool = True) -> list[tuple[str, str]]:
    path = _run_path(s.project, s.run)
    routes = []
    for branch in ("main", s.child) if child else ("main",):
        b = f"{path}/branches/{quote(branch)}"
        label = "main" if branch == "main" else "child"
        queries = HISTORY_QUERIES if branch == "main" else CHILD_HISTORY_QUERIES
        routes += [
            (f"report_{label}", f"{b}/report"),
            (f"llm_status_{label}", f"{b}/llm-status"),
            (f"history_{label}", f"{b}/history?{queries[0]}"),
        ]
    routes.append(("evidence", f"{path}/evidence"))
    return routes


# ---- The browser's walk ------------------------------------------------------------------------

SCREEN = """() => ({
  view: document.body.dataset.view,
  busy: document.body.classList.contains("is-busy"),
  notice: document.getElementById("notice").textContent,
  noticeError: document.getElementById("notice").classList.contains("error")
    && !document.getElementById("notice").hidden,
  computed: (/([0-9]+)分まで計算済み/.exec(
    document.getElementById("computed-label").textContent) || [])[1] ?? null,
  cancel: !document.getElementById("cancel").disabled,
  seek: document.getElementById("seek").value,
  seekLabel: document.getElementById("seek-label").textContent,
})"""


class Walk:
    """One browser's walk: the page, the requests it caught, the stages' results."""

    def __init__(
        self,
        name: str,
        page: Any,
        base: str,
        python: dict[str, PythonSide] | None,
        screenshots: Path | None,
        mode: str,
    ) -> None:
        self.name = name
        self.page = page
        self.base = base
        self.python = python
        self.screenshots = screenshots
        self.mode = mode
        self.errors: list[str] = []
        self.stages: dict[str, dict[str, Any]] = {}
        self.subject: Subject | None = None
        self.ok: dict[str, bool] = {}
        self.child_ready = False

    def blocked(self, stage: str, sample_ok: bool) -> str | None:
        """Why the stage cannot run now (a stage it needs did not pass), or None. Without a Run
        from the scenario stage, the stages after it work on the site's sample."""
        if stage == "scenario":
            return None
        if self.subject is None:
            if not sample_ok:
                return "no Run from the scenario stage; the sample's stages need --python-projects"
            self.subject = sample_subject()
        if self.subject.chain == "sample":
            self.stages[stage]["subject"] = "the site's sample (no Run from the scenario stage)"
        needs = {"compute": None, "intervention": "compute", "compare": "intervention"}
        needs["report"] = "compute"
        need = needs.get(stage)
        if self.subject.chain == "sample" and stage in ("intervention", "report"):
            need = None  # the sample's main branch is computed already
        if need and not self.ok.get(need):
            return f"needs the {need} stage"
        return None

    # Helpers.
    def mark(self) -> int:
        return self.page.evaluate("window.__e2eLog.length")

    def caught(self, since: int, method: str, pattern: str) -> dict[str, Any] | None:
        entries = self.page.evaluate("(since) => window.__e2eLog.slice(since)", since)
        for entry in entries:
            if entry["method"] == method and re.search(pattern, entry["path"]):
                return entry
        return None

    def wait_caught(
        self, since: int, method: str, pattern: str, timeout_s: float = 600
    ) -> dict[str, Any] | None:
        """The first request since `since` matching, once answered and the screen is idle."""
        deadline = time.monotonic() + timeout_s
        while time.monotonic() < deadline:
            entry = self.caught(since, method, pattern)
            if entry is not None and entry["status"] is not None:
                self.idle()
                return entry
            self.page.wait_for_timeout(200)
        return None

    def idle(self) -> None:
        self.page.wait_for_function(
            "() => !document.body.classList.contains('is-busy')", timeout=TIMEOUT_MS
        )

    def screen(self) -> dict[str, Any]:
        return self.page.evaluate(SCREEN)

    def shot(self, label: str) -> None:
        if self.screenshots:
            self.page.screenshot(path=str(self.screenshots / f"{self.name}-{label}.png"))

    def fetch(self, route: str) -> dict[str, Any]:
        return self.page.evaluate(site_check.FETCH, route)

    def post(self, route: str, body: Any) -> dict[str, Any]:
        return self.page.evaluate(POST, [route, json.dumps(body, ensure_ascii=False)])

    def side(self) -> PythonSide | None:
        if self.python is None or self.subject is None:
            return None
        return self.python[self.subject.chain]

    def check(
        self,
        stage: str,
        name: str,
        route: str,
        browser: dict[str, Any] | None,
        *,
        ask: tuple[Any, ...] | None = None,
        expect: Callable[[int, Any], str | None] | None = None,
        body: Any = None,
        sent: Any = None,
        screen_only: bool = False,
    ) -> dict[str, Any]:
        """Records a check of the stage: the browser's answer against Python's, the answer the
        stage kept under `name`, or with `ask` (method, route[, body]) Python's answer to that
        request. `sent`, the body the screen sent, must equal `body`."""
        result = self.stages[stage]
        if browser is None:
            item = {"name": name, "route": route, "verdict": "failed", "differences": ["no answer"]}
            result["checks"].append(item)
            return item
        side = None if screen_only or _unsupported(browser) else self.side()
        python = None
        if side is not None and ask is not None:
            python = side.on_demand(stage, *ask)
        elif side is not None:
            python = side.answer(stage, name)
            if python is None:
                raise RuntimeError(f"the Python side kept no answer {stage}:{name}")
        s = self.subject
        item = compare_answers(
            name,
            route,
            browser,
            python if side is not None else None,
            project=s.project if s else PROJECT,
            translate=s.translate(stage) if s else (lambda text: text),
            expect=expect,
        )
        if sent is not None and body is not None and item["verdict"] == "ok":
            found: list[tuple[Any, ...]] = []
            site_check._differences(body, json.loads(sent), ("sent",), found)
            if found:
                item["verdict"] = "failed"
                item["differences"] = [list(map(str, each)) for each in found[:20]]
        result["checks"].append(item)
        mark = {"ok": "ok", "not_available": "not available yet", "failed": "FAILED"}[
            item["verdict"]
        ]
        extra = f" {item['differences'][:3]}" if item["verdict"] == "failed" else ""
        print(f"    {stage}/{name}: {item.get('status')} {mark}{extra}", flush=True)
        return item

    def probes(self, stage: str, routes: list[tuple[str, str]]) -> None:
        s = self.subject
        assert s is not None
        browser_run = s.browser(stage)
        for name, route in routes:
            asked = route.replace(f"/runs/{s.run}", f"/runs/{browser_run}")
            self.check(stage, name, route, self.fetch(asked))

    def problem(self, stage: str, text: str) -> None:
        self.stages[stage]["problems"].append(text)
        print(f"    {stage}: PROBLEM {text}", flush=True)

    def screen_error(self, stage: str, what: str, allowed: str | None = None) -> None:
        state = self.screen()
        if state["noticeError"] and not (allowed and allowed in state["notice"]):
            if UNSUPPORTED_TEXT in state["notice"]:
                return
            self.problem(stage, f"{what}: the notice shows an error: {state['notice']}")

    def unavailable(self, stage: str, item: dict[str, Any]) -> bool:
        if item["verdict"] == "not_available":
            self.stages[stage]["stopped"] = f"{item['name']}: {item.get('note')}"
            return True
        return False

    def open(self, url: str, view: str) -> None:
        self.page.goto(self.base + url)
        self.page.evaluate("window.HinanjoBrowserApi.ready")
        self.page.wait_for_function(
            f"() => document.body.dataset.view === '{view}'"
            " && !document.body.classList.contains('is-busy')",
            timeout=TIMEOUT_MS,
        )

    # The stages.
    def stage_scenario(self) -> bool:
        s = e2e_subject()
        self.subject = s
        stage = "scenario"
        self.open(f"/demo-v2?project={s.project}#scenario", "scenario")
        base = f"/api/v1/projects/{s.project}"
        for label, run in (("text", s.run), ("form", s.form_run)):
            if label == "text":
                self.page.click("#example")
            else:
                self.page.click("[data-page=scenario]")
                self.page.evaluate(
                    """(fields) => {
                      document.getElementById("scenario-text").value = "";
                      for (const [id, value] of Object.entries(fields)) {
                        const node = document.getElementById(id);
                        node.value = value;
                        node.dispatchEvent(new Event("input", { bubbles: true }));
                        node.dispatchEvent(new Event("change", { bubbles: true }));
                      }
                    }""",
                    FORM_FIELDS,
                )
            since = self.mark()
            self.page.click("#preview")
            preview = self.wait_caught(since, "POST", r"/scenario-preview$")
            if label == "text":
                created = self.caught(since, "POST", r"^/api/v1/projects$")
                if created is not None:
                    # Compared once the stage is available (Python's pins need its tooling).
                    self.check(
                        stage,
                        "create_project",
                        "/api/v1/projects",
                        created,
                        screen_only=preview is None or _unsupported(preview),
                        expect=_status(200),
                    )
            body = (
                {"text": EXAMPLE_TEXT} if label == "text" else {"text": "", "request": FORM_REQUEST}
            )
            item = self.check(
                stage,
                f"preview_{label}",
                f"{base}/scenario-preview",
                preview,
                body=body,
                sent=preview and preview["body"],
            )
            if self.unavailable(stage, item):
                return False
            self.shot(f"preview-{label}")
            if preview is None or preview["status"] != 200:
                return False
            self.page.check("#scenario-check")
            since = self.mark()
            self.page.click("#approve")
            approve = self.wait_caught(since, "POST", r"/scenario-approve$")
            item = self.check(stage, f"approve_{label}", f"{base}/scenario-approve", approve)
            if self.unavailable(stage, item):
                return False
            sent = json.loads(approve["body"]) if approve and approve["body"] else {}
            if sent.get("run_id") != run:
                self.problem(stage, f"the screen's Run id is {sent.get('run_id')}, not {run}")
                return False
            self.page.wait_for_function(
                "() => document.body.dataset.view === 'simulation'"
                " && !document.body.classList.contains('is-busy')",
                timeout=TIMEOUT_MS,
            )
            self.screen_error(stage, f"approve {label}")
        # POST runs: the first Scenario under another id, then another Scenario under that id.
        records = {
            label: json.loads(self.fetch(_run_path(s.project, run))["text"])
            for label, run in (("text", s.run), ("form", s.form_run))
        }
        copy = {
            "run_id": s.copy_run,
            "scenario_id": records["text"].get("scenario_id"),
            "scenario_version": records["text"].get("scenario_version"),
        }
        conflict = {
            **copy,
            "scenario_id": records["form"].get("scenario_id"),
            "scenario_version": records["form"].get("scenario_version"),
        }
        self.check(stage, "post_run", f"{base}/runs", self.post(f"{base}/runs", copy))
        self.check(
            stage,
            "post_run_conflict",
            f"{base}/runs",
            self.post(f"{base}/runs", conflict),
            expect=_code(409, "run_id_conflict"),
        )
        self.probes(stage, scenario_routes(s))
        return True

    def stage_compute(self) -> bool:
        s = self.subject
        assert s is not None
        stage = "compute"
        run = s.browser(stage)
        self.open(f"/demo-v2?project={s.project}&run={run}&branch=main#simulation", "simulation")
        record = json.loads(self.fetch(_run_path(s.project, run))["text"])
        if record["branches"][0]["computed_until"] < 10080:
            if not self.compute(stage, s.project, run, "main", self.mode):
                return False
        self.shot("computed")
        self.probes(stage, compute_routes(s))
        return True

    def compute(self, stage: str, project: str, run: str, branch: str, mode: str) -> bool:
        """Computes `branch` to its end on the screen (`mode` full: at once; pieces: a day,
        cancel, then the rest)."""
        path = _run_path(project, run)

        def computed(at_least: int) -> Callable[[dict[str, Any]], bool]:
            return lambda state: int(state["computed"] or 0) >= at_least and not state["cancel"]

        since = self.mark()
        self.page.click("[data-page=simulation]")
        self.page.click("#compute-end")
        resume = self.wait_caught(since, "POST", r"/resume$", timeout_s=60)
        started = time.perf_counter()
        # Python's answer to the same resume (none for the site's compute Run).
        item = self.check(
            stage,
            "resume_main" if branch == "main" else "resume_child",
            f"{path}/resume",
            resume,
            expect=_status(200),
            screen_only=run != self.subject.run,
        )
        if self.unavailable(stage, item) or resume is None or resume["status"] != 200:
            return False
        if mode == "pieces":
            if not self.watch(stage, lambda state: int(state["computed"] or 0) >= 1440, "a day"):
                return False
            since = self.mark()
            self.page.click("#cancel")
            self.wait_caught(since, "POST", r"/cancel$", timeout_s=60)
            if not self.watch(stage, lambda state: not state["cancel"], "cancel"):
                return False
            status = json.loads(self.fetch(path)["text"]).get("status")
            if status != "paused":
                self.problem(stage, f"cancelled: the Run is {status}")
            self.page.click("#compute-end")
        if not self.watch(stage, computed(10080), "the end"):
            return False
        self.stages[stage].setdefault("compute_s", {})[branch] = round(
            time.perf_counter() - started, 1
        )
        record = json.loads(self.fetch(path)["text"])
        reached = next(b for b in record["branches"] if b["branch_id"] == branch)
        if reached["computed_until"] != 10080:
            self.problem(stage, f"{branch} computed to {reached['computed_until']}")
            return False
        return True

    def watch(self, stage: str, until: Callable[[dict[str, Any]], bool], what: str) -> bool:
        started = time.monotonic()
        while True:
            state = self.screen()
            if state["noticeError"]:
                self.problem(stage, f"{what}: the notice shows an error: {state['notice']}")
                return False
            if until(state):
                return True
            if time.monotonic() - started > TIMEOUT_MS / 1000:
                self.problem(stage, f"{what}: timed out at {state}")
                return False
            self.page.wait_for_timeout(500)

    def seek(self, minute: int) -> None:
        self.page.click("[data-page=simulation]")
        self.page.evaluate(
            "(minute) => { const s = document.getElementById('seek'); s.value = String(minute);"
            " s.dispatchEvent(new Event('change')); }",
            minute,
        )
        self.page.wait_for_function(
            "(minute) => document.getElementById('seek').value === String(minute)"
            " && !document.body.classList.contains('is-busy')",
            arg=minute,
            timeout=TIMEOUT_MS,
        )

    def stage_intervention(self) -> bool:
        s = self.subject
        assert s is not None
        stage = "intervention"
        self.open(f"/demo-v2?project={s.project}&run={s.run}&branch=main#simulation", "simulation")
        self.seek(FORK_MINUTE)
        branch = f"{_run_path(s.project, s.run)}/branches/main"
        preview = None
        for label, text, choice in SENTENCES:
            self.page.fill("#intervention-text", text)
            asks = [(label, text, "#intervention-preview")]
            if choice:
                # The screen's chip of the Zone: a preview of the text with `zone_id:`.
                chip = f"#intervention-result button:has-text('({choice})')"
                asks.append((f"{label}_zone", f"{text}\nzone_id: {choice}", chip))
            for name, asked, button in asks:
                since = self.mark()
                self.page.click(button)
                preview = self.wait_caught(since, "POST", r"/intervention-preview$")
                body = {"text": asked, "minute": FORK_MINUTE}
                item = self.check(
                    stage,
                    f"preview_{name}",
                    f"{branch}/intervention-preview",
                    preview,
                    ask=("POST", f"{branch}/intervention-preview", body),
                    body=body,
                    sent=preview and preview["body"],
                )
                if self.unavailable(stage, item):
                    return False
        self.shot("intervention-preview")
        if preview is None or preview["status"] != 200:
            return False
        if not json.loads(preview["text"]).get("can_apply"):
            self.problem(stage, "the toilet_01 outage cannot be applied")
            return False
        self.page.check("#intervention-check")
        since = self.mark()
        self.page.click("#intervention-apply")
        apply = self.wait_caught(since, "POST", r"/intervention-apply$")
        item = self.check(stage, "apply", f"{branch}/intervention-apply", apply)
        if self.unavailable(stage, item):
            return False
        sent = json.loads(apply["body"]) if apply and apply["body"] else {}
        if sent.get("child_id") != s.child:
            self.problem(stage, f"the screen's child id is {sent.get('child_id')}, not {s.child}")
            return False
        self.page.wait_for_function(
            "() => document.body.dataset.view === 'comparison'"
            " && !document.body.classList.contains('is-busy')",
            timeout=TIMEOUT_MS,
        )
        self.screen_error(stage, "apply")
        if not self.compute(stage, s.project, s.run, s.child, "full"):
            return False
        self.child_ready = True
        self.probes(stage, intervention_routes(s))
        return True

    def stage_compare(self) -> bool:
        s = self.subject
        assert s is not None
        stage = "compare"
        self.open(
            f"/demo-v2?project={s.project}&run={s.run}&branch={quote(s.child)}#comparison",
            "comparison",
        )
        since = self.mark()
        self.page.click("#compare-refresh")
        shown = self.wait_caught(since, "GET", r"/compare$", timeout_s=120)
        path = _run_path(s.project, s.run)
        route = f"{path}/compare?branch_ids={quote(f'main,{s.child}')}&minute=10080"
        if shown is not None:
            shown_route = shown["path"] + shown["search"]
            item = self.check(
                stage,
                "screen_compare",
                shown_route,
                shown,
                ask=("GET", shown_route),
            )
            if self.unavailable(stage, item):
                return False
        else:
            self.problem(stage, f"the screen asked no comparison ({route})")
        self.screen_error(stage, "comparison")
        self.shot("comparison")
        self.probes(stage, compare_routes(s))
        return True

    def stage_report(self) -> bool:
        s = self.subject
        assert s is not None
        stage = "report"
        side = self.side()
        if self.child_ready:
            # From the comparison: the question and the PDF are of the two branches.
            self.open(
                f"/demo-v2?project={s.project}&run={s.run}&branch={quote(s.child)}#comparison",
                "comparison",
            )
            since = self.mark()
            self.page.click("#compare-refresh")
            self.wait_caught(since, "GET", r"/compare$", timeout_s=120)
        else:
            self.open(
                f"/demo-v2?project={s.project}&run={s.run}&branch=main#simulation", "simulation"
            )
        since = self.mark()
        self.page.click("[data-page=report]")
        self.wait_caught(since, "GET", r"/report$", timeout_s=600)
        self.idle()
        for entry in self.page.evaluate("(since) => window.__e2eLog.slice(since)", since):
            if entry["method"] != "GET" or entry["status"] is None:
                continue
            route = entry["path"] + entry["search"]
            label = route.rsplit("/", 1)[-1].split("?")[0]
            if label not in {"report", "evidence", "llm-status", "history"}:
                continue
            self.check(
                stage,
                f"screen_{label}",
                route,
                entry,
                ask=("GET", route),
            )
        self.screen_error(stage, "report")
        self.shot("report")
        # A question: the offline fixed answer.
        self.page.fill("#question", QUESTION)
        since = self.mark()
        self.page.click("#ask")
        asked = self.wait_caught(since, "POST", r"/questions$", timeout_s=120)
        if asked is not None:
            route = asked["path"]
            self.check(
                stage,
                "question",
                route,
                asked,
                ask=("POST", route, json.loads(asked["body"])),
                expect=None if side else _status(200),
            )
        else:
            self.problem(stage, "the screen asked no question")
        self.screen_error(stage, "question")
        # The PDF: 409 pdf_analysis_offline, as the offline VM.
        since = self.mark()
        self.page.click("#save-report")
        pdf = self.wait_caught(since, "POST", r"/pdf-report$", timeout_s=120)
        if pdf is not None:
            route = pdf["path"]
            self.check(
                stage,
                "pdf",
                route,
                pdf,
                ask=("POST", route, json.loads(pdf["body"])),
                expect=_code(409, "pdf_analysis_offline"),
            )
        else:
            self.problem(stage, "the screen asked for no PDF")
        self.screen_error(stage, "pdf", allowed="pdf_analysis_offline")
        self.probes(stage, report_routes(s, child=self.child_ready))
        return True


UNSUPPORTED_TEXT = "ブラウザー版では未対応です"


def _status(expected: int) -> Callable[[int, Any], str | None]:
    return lambda status, _document: None if status == expected else f"status {expected}"


def _code(status: int, code: str) -> Callable[[int, Any], str | None]:
    def expect(got: int, document: Any) -> str | None:
        if got != status or not isinstance(document, dict) or document.get("code") != code:
            return f"{status} {code}"
        return None

    return expect


def _verdict(stage: dict[str, Any]) -> str:
    checks = stage["checks"]
    if stage["problems"] or any(c["verdict"] == "failed" for c in checks):
        return "failed"
    if stage.get("stopped") or any(c["verdict"] == "not_available" for c in checks):
        return "not_available"
    if stage.get("skipped"):
        return "skipped"
    return "passed"


def walk(walker: Walk, stages: list[str], sample_ok: bool) -> None:
    """The stages in order; a stage that is not available skips the stages that need it."""
    for stage in STAGES:
        if stage not in stages:
            continue
        result = walker.stages[stage] = {"checks": [], "problems": []}
        reason = walker.blocked(stage, sample_ok)
        if reason:
            result["skipped"] = reason
            result["verdict"] = "skipped"
            walker.ok[stage] = False
            print(f"  stage {stage}: skipped ({reason})", flush=True)
            continue
        print(f"  stage {stage}", flush=True)
        started = time.perf_counter()
        try:
            walker.ok[stage] = getattr(walker, f"stage_{stage}")()
        except Exception as error:  # A broken step fails its stage; the walk goes on.
            walker.problem(stage, f"{type(error).__name__}: {error}")
            walker.ok[stage] = False
            with contextlib.suppress(Exception):
                walker.shot(f"{stage}-error")
        result["seconds"] = round(time.perf_counter() - started, 1)
        result["verdict"] = _verdict(result)
        if stage == "scenario" and not walker.ok[stage]:
            walker.subject = None


def check(
    *,
    site: Path | None,
    url: str | None,
    browsers: list[str],
    modes: list[str],
    stages: list[str],
    work: Path | None,
    sample_projects: Path | None,
    shelter_csv: Path | None,
    screenshots: Path | None,
    fresh: bool,
) -> dict[str, Any]:
    from playwright.sync_api import sync_playwright

    server = serve_static_site.start(site) if site else None
    base = url.rstrip("/") if url else f"http://127.0.0.1:{server.server_port}"
    python: dict[str, PythonSide] | None = None
    if work is not None:
        python = {
            subject.chain: PythonSide(
                subject,
                work,
                sample_projects=sample_projects,
                shelter_csv=shelter_csv,
                fresh=fresh,
            )
            for subject in (e2e_subject(), sample_subject())
        }
    results: dict[str, Any] = {"base": base, "browsers": {}, "notes": []}
    shelter_index = None
    if python is not None:
        try:
            shelter_index = python_shelter_index(shelter_csv)
        except RuntimeError as error:
            results["notes"].append(f"no shelter index: {error}")
    try:
        with sync_playwright() as playwright:
            for index, name in enumerate(browsers):
                mode = modes[index % len(modes)]
                print(f"{name} (compute {mode})", flush=True)
                browser = getattr(playwright, name).launch()
                context = browser.new_context()
                errors: list[str] = []
                try:
                    context.add_init_script(PAGE_INIT)
                    if python is not None:
                        try:
                            pin_browser(context, shelter_index, results["notes"])
                            results["pins"] = "pinned"
                        except RuntimeError as error:
                            # Until the corpus's tooling is on main; the scenario stage then
                            # answers browser_unsupported anyway.
                            results["pins"] = f"not pinned: {error}"
                            print(f"  {results['pins']}", flush=True)
                    page = context.new_page()
                    page.set_default_timeout(TIMEOUT_MS)
                    site_check._attach(page, errors)
                    walker = Walk(name, page, base, python, screenshots, mode)
                    walk(walker, stages, sample_ok=python is None or sample_projects is not None)
                finally:
                    browser.close()
                results["browsers"][name] = {
                    "mode": mode,
                    "errors": errors,
                    "stages": walker.stages,
                }
    finally:
        if python is not None:
            for side in python.values():
                results.setdefault("python_s", {})[side.subject.chain] = side.timings
                side.close()
        if server is not None:
            server.shutdown()
            server.server_close()
    verdicts = [
        result["verdict"]
        for browser in results["browsers"].values()
        for result in browser["stages"].values()
    ]
    console = [
        error
        for browser in results["browsers"].values()
        for error in browser["errors"]
        if not _expected_console(error)
    ]
    results["console_errors"] = console
    results["failed"] = "failed" in verdicts or bool(console)
    results["all_passed"] = (
        not results["failed"]
        and all(v == "passed" for v in verdicts)
        and len(verdicts) == len(stages) * len(browsers)
    )
    results["excluded"] = [
        {"routes": pattern, "field": ".".join(fields), "reason": reason}
        for pattern, fields, reason in EXCLUDED
    ] + [{"routes": ".*", "field": f"*.{key}", "reason": reason} for key, reason in MASKED.items()]
    results["normalized"] = NORMALIZED
    return results


def _expected_console(error: str) -> bool:
    """Console errors a step expects: a refused request the screen reports (409 for the PDF, a
    route not available yet)."""
    return "409" in error or "Failed to load resource" in error


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    where = parser.add_mutually_exclusive_group(required=True)
    where.add_argument("--site", type=Path, help="the static site to serve and compare")
    where.add_argument("--url", help="a deployed site: screen-only checks")
    where.add_argument(
        "--write-corpus-csv",
        type=Path,
        metavar="PATH",
        help="write the corpus's synthetic shelter CSV (build the site with it) and exit",
    )
    parser.add_argument("--work", type=Path, help="the Python side's directory (with --site)")
    parser.add_argument("--python-projects", type=Path, help="the Python sample's ProjectStore")
    parser.add_argument("--shelter-csv", type=Path, help="the shelter CSV the site was built with")
    parser.add_argument("--browsers", nargs="+", default=["chromium", "firefox"])
    parser.add_argument(
        "--modes", nargs="+", choices=["full", "pieces"], default=["full", "pieces"]
    )
    parser.add_argument("--stages", nargs="+", choices=STAGES, default=list(STAGES))
    parser.add_argument("--output", type=Path)
    parser.add_argument("--screenshots", type=Path)
    parser.add_argument("--fresh", action="store_true", help="do the Python side again")
    parser.add_argument("--require-all", action="store_true", help="fail unless every stage passed")
    arguments = parser.parse_args()
    if arguments.write_corpus_csv:
        arguments.write_corpus_csv.write_text(_corpus().SHELTER_CSV, encoding="utf-8")
        return 0
    if arguments.site and arguments.work is None:
        parser.error("--site needs --work (the Python side's directory)")
    if arguments.screenshots:
        arguments.screenshots.mkdir(parents=True, exist_ok=True)
    started = datetime.now(UTC)
    results = check(
        site=arguments.site.resolve() if arguments.site else None,
        url=arguments.url,
        browsers=arguments.browsers,
        modes=arguments.modes,
        stages=arguments.stages,
        work=arguments.work.resolve() if arguments.work else None,
        sample_projects=arguments.python_projects.resolve() if arguments.python_projects else None,
        shelter_csv=arguments.shelter_csv.resolve() if arguments.shelter_csv else None,
        screenshots=arguments.screenshots,
        fresh=arguments.fresh,
    )
    results["started"] = started.isoformat(timespec="seconds")
    if arguments.output:
        arguments.output.write_text(
            json.dumps(results, ensure_ascii=False, indent=1) + "\n", encoding="utf-8"
        )
    print()
    for name, browser in results["browsers"].items():
        print(f"{name} (compute {browser['mode']}):")
        for stage, result in browser["stages"].items():
            failed = [c["name"] for c in result["checks"] if c["verdict"] == "failed"]
            note = result.get("stopped") or result.get("skipped") or ""
            print(
                f"  {stage:13} {result['verdict']:14} checks {len(result['checks'])}"
                f"{', failing ' + ', '.join(failed) if failed else ''}"
                f"{', problems ' + str(result['problems']) if result['problems'] else ''}"
                f"{' — ' + note if note else ''}"
            )
    if results["console_errors"]:
        print(f"console/page errors: {results['console_errors'][:10]}")
    for note in results["notes"]:
        print(f"note: {note}")
    verdict = (
        "all stages passed"
        if results["all_passed"]
        else ("FAILED" if results["failed"] else "no failures (some stages not available yet)")
    )
    print(verdict)
    if results["failed"]:
        return 1
    return 1 if arguments.require_all and not results["all_passed"] else 0


if __name__ == "__main__":
    raise SystemExit(main())
