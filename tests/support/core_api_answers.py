"""The Python server's answers to the KPI history, report and LLM status of a Run (#266).

`native/wasm/tests/node_api_answers.mjs` computes the same Run in WASM, stores it as the browser
does (`project_store.mjs`) and answers the same routes with `native/wasm/js/api_answers.mjs`: each
answer must be the server's, byte for byte, but for the branch's head (`ANCHOR`, below).

The Run is the one the Project screen makes of a generated Scenario (`EVACUEES` people, `MINUTES`
minutes, seed 2026): `scenario-preview` of the structured form with the offline parser,
`scenario-approve`, `resume` to the end, as `scripts/build_project_sample.py` does. The fixture
holds the world at minute 0 as a Run sets it up (`export_world`, the world `history`), the
Scenario's canonical JSON (its sha256 is its hash), and for each case (`CASES`) the route, the
branch, the query and each page's status and the sha256 of its body (a refusal's body as it is).
A paged case asks again with the page's `next_cursor` until there is none.

What may differ, the branch's head, is made the same on both sides before the sha256
(`normalize`): the Checkpoint's hash on the server and the head's on the browser are replaced by
`ANCHOR`, also in the JSON a `next_cursor` encodes. `api_answers_fixture` builds the fixture;
`benchmarks/core_fixtures.py api_answers` writes it.
"""

from __future__ import annotations

import base64
import contextlib
import hashlib
import json
import os
import runpy
import tempfile
from collections.abc import Iterator
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs

from support.paths import FIXTURES

EVACUEES = 12
MINUTES = 150
RUN_ID = "r"
BRANCH_ID = "main"
ANCHOR = "ANCHOR"
# The pages a paged case may take at most.
MAX_PAGES = 20

# Each case: (route, branch, query, paged). The screen asks for kpis with start, end, step,
# limit=100 and the cursor (`loadHistory` of demo-v2.js); the rest are the server's checks.
CASES: list[tuple[str, str, str, bool]] = [
    ("history", BRANCH_ID, f"kind=kpis&end={MINUTES}", True),
    ("history", BRANCH_ID, "kind=kpis&start=13&end=140&step=7&limit=5", True),
    ("history", BRANCH_ID, f"kind=kpis&start=0&end={MINUTES}&step=2&limit=100", True),
    ("history", BRANCH_ID, f"kind=kpis&start={MINUTES - 1}&end={MINUTES}", True),
    ("history", BRANCH_ID, f"kind=kpis&start={MINUTES}&end={MINUTES}", True),
    ("history", BRANCH_ID, f"kind=kpis&end={MINUTES}&step=1000", True),
    ("history", BRANCH_ID, "kind=kpis&end=100&zone_id=quiet", True),
    ("history", BRANCH_ID, "kind=kpis&end=100&text=toilet", True),
    ("history", BRANCH_ID, "kind=kpis&end=30&limit=7&text=%21%21", True),
    ("history", BRANCH_ID, "kind=kpis&start=&end=20&cursor=&limit=+1_0", True),
    ("history", BRANCH_ID, "kind=kpis&end=20&limit=3&include_related=false", False),
    ("history", BRANCH_ID, "kind=kpis", False),
    ("history", BRANCH_ID, f"kind=kpis&end={MINUTES + 1}", False),
    ("history", BRANCH_ID, "kind=kpis&end=10&limit=101", False),
    ("history", BRANCH_ID, "kind=kpis&start=5&end=4", False),
    ("history", BRANCH_ID, "kind=kpis&end=abc", False),
    ("history", BRANCH_ID, "kind=kpis&end=10&bogus=1", False),
    ("history", BRANCH_ID, "kind=kpis&end=10&end=11", False),
    ("history", BRANCH_ID, "kind=kpis&end=10&cursor=xyz", False),
    ("history", BRANCH_ID, "kind=kpis&end=10&cursor=e30%3D", False),
    ("history", BRANCH_ID, "kind=kpis&end=10&agent_id=A1", False),
    ("history", BRANCH_ID, "kind=nope&end=10", False),
    ("history", "nope", "kind=kpis&end=10", False),
    ("report", BRANCH_ID, "", False),
    ("report", BRANCH_ID, "x=1", False),
    ("report", "nope", "", False),
    ("llm-status", BRANCH_ID, "", False),
]
# A cursor of one query given to another of the same branch: (the query whose first page's cursor
# is taken, the query it is given to). The server takes it: the index binds a cursor to the
# branch's rows and head, not to the query (`HistoryIndex.search` checks the cursor against the SQL
# term of the branch's rows, which it also names `scope`), and so does the browser.
FOREIGN_CURSOR = ("kind=kpis&start=13&end=140&step=7&limit=5", "kind=kpis&end=140&limit=5")


def normalize(body: str, anchor: str | None) -> str:
    """The body with the branch's head replaced by ANCHOR, also in its next_cursor."""
    if not anchor:
        return body
    document = json.loads(body)
    cursor = document.get("next_cursor") if isinstance(document, dict) else None
    if isinstance(cursor, str):
        decoded = base64.urlsafe_b64decode(cursor).decode()
        again = base64.urlsafe_b64encode(decoded.replace(anchor, ANCHOR).encode()).decode()
        body = body.replace(f'"next_cursor":"{cursor}"', f'"next_cursor":"{again}"')
    return body.replace(anchor, ANCHOR)


def _answer(api: Any, branch: str, route: str, query: str) -> tuple[int, str]:
    """The server's status and body (web/server.py's do_GET and _json_response)."""
    from sheltersim.storage.projects import StorageError

    parts = ["sample", "runs", RUN_ID, "branches", branch, route]
    try:
        status, payload = 200, api.get(parts, parse_qs(query))
    except StorageError as exc:
        status, payload = 409, {"error": str(exc), "code": exc.code}
    except ValueError:
        status, payload = 400, {"error": "invalid query or saved contract", "code": "invalid_input"}
    body = json.dumps(
        payload, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False
    )
    return status, body


def _page(status: int, body: str, anchor: str) -> dict[str, Any]:
    text = normalize(body, anchor) if status == 200 else body
    page: dict[str, Any] = {"status": status}
    if status == 200:
        page["sha256"] = hashlib.sha256(text.encode()).hexdigest()
        page["length"] = len(text.encode())
    else:
        page["body"] = text
    return page


def _with_cursor(query: str, cursor: str) -> str:
    return f"{query}&cursor={cursor}"


@contextlib.contextmanager
def _isolated(directory: Path) -> Iterator[None]:
    """The usage ledgers in `directory`, not the user's (the builder runs outside pytest too)."""
    names = ("SHELTERSIM_PROJECTS_DIR", "XDG_STATE_HOME", "SHELTERSIM_LLM_MODE")
    saved = {name: os.environ.get(name) for name in names}
    os.environ.update(
        {
            "SHELTERSIM_PROJECTS_DIR": str(directory / "ledgers"),
            "XDG_STATE_HOME": str(directory / "state"),
            "SHELTERSIM_LLM_MODE": "offline",
        }
    )
    try:
        yield
    finally:
        for name, value in saved.items():
            if value is None:
                os.environ.pop(name, None)
            else:
                os.environ[name] = value


def api_answers_fixture() -> dict[str, Any]:
    from sheltersim.core.world_export import export_world
    from sheltersim.storage.projects import ProjectStore, canonical, digest
    from sheltersim.web.projects import ProjectAPI
    from support.core_fixtures import run_world
    from support.generated import _generated_request

    sample = runpy.run_path(str(FIXTURES.parent / "scripts" / "build_project_sample.py"))
    request = _generated_request(evacuees=EVACUEES, duration_days=MINUTES / 1440)
    with tempfile.TemporaryDirectory(prefix="api-answers-") as scratch, _isolated(Path(scratch)):
        store = ProjectStore(Path(scratch) / "projects")
        api = ProjectAPI(store, recover=False)
        api.workspace.research.research = sample["_no_research"]
        try:
            draft, _ = sample["_preview"](api, "", request)
            api.post(
                ["sample", "scenario-approve"],
                {
                    "preview_id": draft["preview_id"],
                    "approved_hash": draft["approved_hash"],
                    "approved": True,
                    "run_id": RUN_ID,
                },
            )
            api.post(
                ["sample", "runs", RUN_ID, "resume"],
                {"branch_id": BRANCH_ID, "target_minute": MINUTES},
            )
            api.wait_idle(timeout=600)
            reference = store.branch_reference("sample", RUN_ID, BRANCH_ID)
            status = store.branch_status("sample", RUN_ID, BRANCH_ID)
            if status != "completed" or reference.computed_until != MINUTES:
                raise RuntimeError(f"the Run ended {status} at minute {reference.computed_until}")
            anchor = reference.checkpoint_hash
            cases = []
            for route, branch, query, paged in CASES:
                pages = []
                asked = query
                for _ in range(MAX_PAGES):
                    status, body = _answer(api, branch, route, asked)
                    pages.append(_page(status, body, anchor))
                    cursor = json.loads(body).get("next_cursor") if status == 200 else None
                    if not paged or cursor is None:
                        break
                    asked = _with_cursor(query, cursor)
                else:
                    raise AssertionError(f"{query}: more than {MAX_PAGES} pages")
                cases.append({"route": route, "branch": branch, "query": query, "pages": pages})
            source, target = FOREIGN_CURSOR
            _, first = _answer(api, BRANCH_ID, "history", source)
            status, body = _answer(
                api, BRANCH_ID, "history", _with_cursor(target, json.loads(first)["next_cursor"])
            )
            foreign = {"from": source, "to": target, **_page(status, body, anchor)}
            run = store.run("sample", RUN_ID)
            scenario = store.scenario("sample", run["scenario_id"], run["scenario_version"])
        finally:
            api.close()
        scenario_text = canonical(json.loads(scenario.canonical_json()))
        if digest(json.loads(scenario_text)) != run["scenario_hash"]:
            raise RuntimeError("the Scenario's canonical JSON is not the one its hash is of")
        world, controller, _ = run_world(scenario)
        entry = export_world(world, controller)
    return {
        "worlds": {"history": entry},
        "run": {
            "project_id": "sample",
            "run_id": RUN_ID,
            "branch_id": BRANCH_ID,
            "seed": run["seed"],
            "minutes": MINUTES,
            "evacuees": EVACUEES,
        },
        "scenario_json": scenario_text,
        "cases": cases,
        "foreign_cursor": foreign,
    }
