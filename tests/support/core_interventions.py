"""The Python server's answers to an intervention and its child branch (#306).

`native/wasm/tests/node_interventions.mjs` computes the same Run in WASM, stores it as the browser
does and asks the same routes through `native/wasm/js/browser_api.mjs`: each answer must be the
server's, byte for byte, once what may differ is masked (`normalize`).

The Run is the one the Project screen makes of a generated Scenario (`EVACUEES` people, `MINUTES`
minutes, seed 2026), computed to its end, as `core_api_answers.py` makes it, on a server whose
worker is a process (as the VM's). Then, at `FORK_MINUTE` of `main`: `intervention-preview` of
each of `PREVIEWS` (and the server's refusals), `intervention-apply` of the first one to the child
`CHILD`, the Run's record with its `last_fork`, the refusals of `APPLY_REFUSALS`, the child
computed to the end, and its frames and KPI history.

Masked (`normalize`): `preview_id` (the server's uuid4) and each branch's head (the server's
Checkpoint's hash, the browser's journal line's), in a Run's record and in a history's
`anchor_hash` and cursor, are replaced by fixed words. The fixture holds the world at minute 0, the
Scenario's canonical JSON and, for each request, its status and the sha256 of its body (a refusal's
body as it is).
"""

from __future__ import annotations

import hashlib
import json
import re
import runpy
import tempfile
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs

from support.core_api_answers import _isolated, normalize
from support.paths import FIXTURES

EVACUEES = 12
MINUTES = 480
RUN_ID = "r"
CHILD = "c1"
FORK_MINUTE = 400
PREVIEW = "PREVIEW"
HEAD = "HEAD"

# Bodies of `intervention-preview` on main; the first one is applied.
PREVIEWS: list[str] = [
    json.dumps({"minute": FORK_MINUTE, "text": "7:00 toilet_01 outage"}),
    json.dumps({"minute": FORK_MINUTE, "text": "トイレを1基追加"}),
    json.dumps({"minute": FORK_MINUTE, "text": "7:00から7:30まで断水"}),
    json.dumps({"minute": FORK_MINUTE, "text": "7:00 断水"}),
    json.dumps({"minute": FORK_MINUTE, "text": "スタッフを2人追加"}),
    json.dumps({"minute": FORK_MINUTE, "text": "7:00 避難者が5人到着"}),
    json.dumps({"minute": FORK_MINUTE, "text": "雨が降る"}),
    json.dumps({"minute": FORK_MINUTE, "text": "Day 2 toilet outage"}),
    json.dumps({"minute": 0, "text": "toilet_01 outage"}),
    json.dumps({"minute": MINUTES, "text": "toilet_01 outage"}),
    json.dumps({"minute": FORK_MINUTE, "text": "連絡先 a@b.cc 7:00 toilet_01 outage"}),
    json.dumps({"minute": FORK_MINUTE, "text": ""}),
    json.dumps({"minute": FORK_MINUTE, "text": 5}),
    json.dumps({"minute": FORK_MINUTE}),
    json.dumps({"minute": MINUTES + 1, "text": "x"}),
    json.dumps({"minute": "1", "text": "x"}),
    json.dumps({"minute": FORK_MINUTE, "text": "x", "extra": 1}),
    json.dumps({"text": "x"}),
    "[1]",
    "{bad",
]

# `compare` and `replay-frames` of the two branches once the child is computed (#307).
COMPARISONS: list[tuple[str, str]] = [
    ("compare", f"branch_ids=main,{CHILD}&minute={FORK_MINUTE}"),
    ("compare", f"branch_ids=main,{CHILD}&minute=421"),
    ("compare", f"branch_ids={CHILD},main&minute={MINUTES}"),
    ("compare", f"branch_ids=main,{CHILD}&minute=4_21"),
    ("compare", f"branch_ids=main,{CHILD}&minute={FORK_MINUTE - 1}"),
    ("compare", "branch_ids=main&minute=420"),
    ("compare", "branch_ids=main,main&minute=420"),
    ("compare", "branch_ids=main,nope&minute=420"),
    ("compare", f"branch_ids=main,{CHILD}&minute=x"),
    ("compare", f"branch_ids=main,{CHILD}&minute=420&minute=421"),
    ("compare", f"branch_ids=main,{CHILD}"),
    ("replay-frames", f"branch_ids=main,{CHILD}&start=418&end=425"),
    ("replay-frames", f"branch_ids={CHILD},main&start=476&end={MINUTES}"),
    ("replay-frames", f"branch_ids=main,{CHILD}&start={FORK_MINUTE - 1}&end={FORK_MINUTE + 1}"),
    ("replay-frames", f"branch_ids=main,{CHILD},x&start=420&end=421"),
]

# `questions` once the child is computed (#308): (branch, body). "$AGENT" is a person at the end
# of main, "$CONTEXT" the context_id of the first answer.
QUESTIONS: list[tuple[str, dict[str, Any]]] = [
    ("main", {"question": "何が起きた?"}),
    ("main", {"question": "何が起きた?", "context_id": "$CONTEXT"}),
    ("main", {"question": "x", "context_id": "c" + "0" * 32 + "-" + "0" * 24}),
    ("main", {"question": "トイレの列は?", "minute": 420, "comparison_branch_ids": [CHILD]}),
    (CHILD, {"question": "分岐の後は?", "minute": 450, "comparison_branch_ids": ["main"]}),
    ("main", {"question": "この人は?", "agent_id": "$AGENT", "minute": MINUTES}),
    ("main", {"question": "連絡先 a@b.cc agent A001 は?", "run_id": "other"}),
    ("main", {"question": "x", "agent_id": "nope"}),
    ("main", {"question": ""}),
    ("main", {"question": "   "}),
    ("main", {"question": "a" * 2001}),
    ("main", {"question": "x", "extra": 1}),
    ("main", {"question": "x", "minute": -1}),
    ("main", {"question": "x", "minute": True}),
    ("main", {"question": "x", "comparison_branch_ids": CHILD}),
    ("main", {"question": "x", "comparison_branch_ids": ["a"] * 8}),
    ("main", {"question": "x", "comparison_branch_ids": ["nope"]}),
    ("main", {"question": "x", "comparison_branch_ids": ["main"]}),
    ("main", {"question": "x", "minute": FORK_MINUTE - 1, "comparison_branch_ids": [CHILD]}),
    ("main", {"question": "x", "minute": MINUTES + 1}),
    ("main", {"question": "x", "demo_branch_id": "d"}),
    ("main", {"question": "x", "context_id": 5}),
    ("main", {"question": "x", "context_id": "a" * 65}),
    ("nope", {"question": "x"}),
]


def _placeholder(document: dict[str, Any], preview_id: str) -> dict[str, Any]:
    """The document with the server's (random) preview_id written as "$PREVIEW"."""
    return {
        key: "$PREVIEW" if key == "preview_id" and value == preview_id else value
        for key, value in document.items()
    }


def _canonical(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def _post(api: Any, parts: list[str], body: str) -> tuple[int, str]:
    """The server's status and body of a project POST (web/server.py's do_POST)."""
    from sheltersim.storage.projects import StorageError

    try:
        raw = body.encode()
        if not 0 < len(raw) <= 2_097_152:
            raise StorageError("project request exceeds the body limit")
        document = json.loads(raw.decode("utf-8"))
        if not isinstance(document, dict):
            raise StorageError("project request must be an object")
        status, payload = 200, api.post(parts, document)
    except StorageError as exc:
        status, payload = 409, {"error": str(exc), "code": exc.code}
    except (ValueError, UnicodeDecodeError):
        status, payload = 400, {"error": "invalid project request"}
    return status, _canonical(payload)


def _get(api: Any, parts: list[str], query: str = "") -> tuple[int, str]:
    from sheltersim.storage.projects import StorageError

    try:
        status, payload = 200, api.get(parts, parse_qs(query))
    except StorageError as exc:
        status, payload = 409, {"error": str(exc), "code": exc.code}
    except ValueError:
        status, payload = 400, {"error": "invalid query or saved contract", "code": "invalid_input"}
    return status, _canonical(payload)


def mask(body: str, heads: dict[str, str]) -> str:
    """The body with `preview_id` and the branches' heads replaced, as the Node test does."""
    body = re.sub(r'"preview_id":"[0-9a-f]{32}"', f'"preview_id":"{PREVIEW}"', body)
    # A fork receipt names the parent's Checkpoint (the server's) or head (the browser's).
    body = re.sub(r'"checkpoint_hash":"[0-9a-f]{64}"', f'"checkpoint_hash":"{HEAD}"', body)
    # A question's context id ends with random digits.
    body = re.sub(r'"context_id":"(c[0-9a-f]{32})-[0-9a-f]{24}"', r'"context_id":"\1-RANDOM"', body)
    for head in heads.values():
        if head:
            body = normalize(body, head).replace("ANCHOR", HEAD)
    return body


def _case(name: str, status: int, body: str, heads: dict[str, str]) -> dict[str, Any]:
    text = mask(body, heads) if status == 200 else body
    case: dict[str, Any] = {"name": name, "status": status}
    if status == 200:
        case["sha256"] = hashlib.sha256(text.encode()).hexdigest()
        case["length"] = len(text.encode())
        case["text"] = text if len(text) < 4000 else None
    else:
        case["body"] = text
    return case


def interventions_fixture() -> dict[str, Any]:
    from sheltersim.core.world_export import export_world
    from sheltersim.storage.projects import ProjectStore, canonical, digest
    from sheltersim.web.projects import ProjectAPI
    from support.core_fixtures import run_world
    from support.generated import _generated_request

    sample = runpy.run_path(str(FIXTURES.parent / "scripts" / "build_project_sample.py"))
    request = _generated_request(evacuees=EVACUEES, duration_days=MINUTES / 1440)
    branch = ["sample", "runs", RUN_ID, "branches"]
    with tempfile.TemporaryDirectory(prefix="interventions-") as scratch, _isolated(Path(scratch)):
        store = ProjectStore(Path(scratch) / "projects")
        api = ProjectAPI(store, worker="process", recover=False)
        api.workspace.research.research = sample["_no_research"]

        def heads() -> dict[str, str]:
            return {
                item["branch_id"]: item["checkpoint_hash"]
                for item in store.branches("sample", RUN_ID)
            }

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
                {"branch_id": "main", "target_minute": MINUTES},
            )
            api.wait_idle(timeout=600)
            cases = []
            previews = []
            for body in PREVIEWS:
                status, text = _post(api, [*branch, "main", "intervention-preview"], body)
                previews.append((status, text))
                cases.append(
                    {"route": "preview", "request": body, **_case(body, status, text, heads())}
                )
            applied = json.loads(previews[0][1])
            if not applied.get("can_apply"):
                raise RuntimeError(f"the first preview cannot be applied: {previews[0][1][:400]}")
            apply = {
                "preview_id": applied["preview_id"],
                "approved_program_hash": applied["approved_program_hash"],
                "approved": True,
                "child_id": CHILD,
            }
            refusals = [
                {**apply, "approved": False},
                {**apply, "approved_program_hash": "0" * 64},
                {**apply, "preview_id": "0" * 32},
                {**apply, "child_id": "../x"},
                {key: value for key, value in apply.items() if key != "child_id"},
            ]
            for document in refusals:
                status, text = _post(
                    api, [*branch, "main", "intervention-apply"], json.dumps(document)
                )
                cases.append(
                    {
                        "route": "apply",
                        "document": _placeholder(document, apply["preview_id"]),
                        **_case("refused apply", status, text, heads()),
                    }
                )
            status, text = _post(api, [*branch, "main", "intervention-apply"], json.dumps(apply))
            cases.append(
                {
                    "route": "apply",
                    "document": _placeholder(apply, apply["preview_id"]),
                    **_case("apply", status, text, heads()),
                }
            )
            api.wait_idle(timeout=600)
            status, text = _get(api, ["sample", "runs", RUN_ID])
            cases.append({"route": "run", **_case("run after the fork", status, text, heads())})
            # The same preview again: the child exists now.
            status, text = _post(api, [*branch, "main", "intervention-apply"], json.dumps(apply))
            cases.append(
                {
                    "route": "apply",
                    "document": _placeholder(apply, apply["preview_id"]),
                    **_case("apply again", status, text, heads()),
                }
            )
            api.wait_idle(timeout=600)
            status, text = _get(api, ["sample", "runs", RUN_ID])
            cases.append(
                {"route": "run", **_case("run after a refused fork", status, text, heads())}
            )
            api.post(
                ["sample", "runs", RUN_ID, "resume"], {"branch_id": CHILD, "target_minute": MINUTES}
            )
            api.wait_idle(timeout=600)
            for minute in (FORK_MINUTE, 420, 421, MINUTES):
                status, text = _get(api, [*branch, CHILD, "timeline"], f"minute={minute}")
                cases.append(
                    {
                        "route": "timeline",
                        "minute": minute,
                        **_case(f"child at {minute}", status, text, heads()),
                    }
                )
            for query in (
                f"kind=kpis&start={FORK_MINUTE}&end={MINUTES}&limit=100",
                f"kind=kpis&end={MINUTES}&step=7",
            ):
                status, text = _get(api, [*branch, CHILD, "history"], query)
                cases.append(
                    {
                        "route": "history",
                        "query": query,
                        **_case(f"child history {query}", status, text, heads()),
                    }
                )
            status, text = _get(api, ["sample", "runs", RUN_ID])
            cases.append({"route": "run", **_case("run after the child", status, text, heads())})
            _, frame = _get(api, [*branch, "main", "timeline"], f"minute={MINUTES}")
            agent = json.loads(frame)["people"][0]["id"]
            first_context = None
            for branch_id, question in QUESTIONS:
                document = {
                    key: agent
                    if value == "$AGENT"
                    else first_context
                    if value == "$CONTEXT"
                    else value
                    for key, value in question.items()
                }
                status, text = _post(api, [*branch, branch_id, "questions"], json.dumps(document))
                if first_context is None and status == 200:
                    first_context = json.loads(text)["context_id"]
                name = f"question {branch_id} {json.dumps(question)[:80]}"
                cases.append(
                    {
                        "route": "questions",
                        "branch": branch_id,
                        "document": question,
                        **_case(name, status, text, heads()),
                    }
                )
            from sheltersim.storage.history_index import HistoryIndex

            turns = HistoryIndex(store, "sample", RUN_ID).conversation_tail("main", None, 100)
            stored_questions = [turn["question"] for turn in turns]
            for route, query in COMPARISONS:
                status, text = _get(api, ["sample", "runs", RUN_ID, route], query)
                cases.append(
                    {
                        "route": route,
                        "query": query,
                        **_case(f"{route} {query}", status, text, heads()),
                    }
                )
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
        "worlds": {"interventions": entry},
        "run": {
            "project_id": "sample",
            "run_id": RUN_ID,
            "seed": run["seed"],
            "minutes": MINUTES,
            "evacuees": EVACUEES,
            "fork_minute": FORK_MINUTE,
            "child": CHILD,
            "model_versions": run["model_versions"],
        },
        "scenario_json": scenario_text,
        "cases": cases,
        "stored_questions": stored_questions,
    }
