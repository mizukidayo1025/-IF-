"""Build the precomputed sample Project that the new UI (/demo-v2) opens at once.

    SHELTERSIM_LLM_MODE=offline python scripts/build_project_sample.py OUTPUT_PROJECTS_DIR

OUTPUT_PROJECTS_DIR is a ProjectStore root, such as `<runs-dir>/.projects` of
`sheltersim web`. The script creates the Project `sample` in it with one Run of the
Scenario the new UI generates from its example text (the 「例文を使う」 button:
100 synthetic people in a standard synthetic shelter in 墨田区, earthquake, 7 days,
seed 2026): the branch `main` computed to the end, and a branch forked at Day 4 14:00
(minute 5160) with "Day 4 14:00 toilet_01 outage", computed to the end. It is the
comparison of the old UI's fixed replay (scripts/build_demo_replay.py), on a Scenario
whose map the new UI draws.

It goes through the ProjectAPI routes the new UI calls, in this process: the Project,
`scenario-preview` of the text with the offline parser, `scenario-approve`, `resume` to
the end, `intervention-preview` with the offline parser, `intervention-apply` with
explicit approval, and `resume` of the child. No Provider is called: the LLM mode is
forced offline, the API key is dropped, and the usage ledger (XDG_STATE_HOME) and
response tape point to a temporary directory. The regional research of the preview,
which reads public web services, is not run: the Scenario is built from the standard
synthetic shelter and the recorded defaults alone, as a preview without research
results is, and the research record says so with a fixed date.

The same text, seed and model give the same hashes on every build. A JSON summary
(ids, Scenario, minutes, hashes, size) is printed and written to
`<OUTPUT>/../sample-build.json` unless --summary names another file. Open the result
with `/demo-v2?project=sample&run=<run_id>&branch=<branch_id>` (docs/demo-guide.md).
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import tempfile
import time
from datetime import UTC, datetime
from pathlib import Path
from typing import Any
from urllib.parse import urlencode

from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.llm.config import LLMMode, LLMSettings
from sheltersim.storage.projects import ProjectStore, digest

PROJECT_ID = "sample"
RUN_ID = "run-sample-toilet-outage"
CHILD_ID = "branch-toilet01-outage"
# The branch the landing page opens: the original Scenario from Day 1 00:00, which plays
# from the start; the comparison screen pairs it with the outage branch. (Playing the
# outage branch from its fork minute stops at once in the screen's path drawing.)
OPEN_BRANCH_ID = "main"
# What the new UI's 「例文を使う」 button puts in the Scenario text (demo-v2.js).
TEXT = "墨田区で地震が発生。避難者100人、7日間。高齢者や子ども連れの支援ニーズも見たい。"
FORK_MINUTE = 3 * 24 * 60 + 14 * 60
# One of the generated standard shelter's three ordinary toilets; the accessible one stays.
FACILITY_ID = "toilet_01"
# The research record's date; fixed, since no research is run (see the module docstring).
RESEARCHED_AT = datetime(2026, 1, 1, tzinfo=UTC)
RESEARCH_WARNING = (
    "計算済みの例のため、地域の公開データは調べていません。"
    "標準の合成施設と既定値だけで作ったシナリオです。"
)
# As the old demo's replay says of itself (scripts/build_demo_replay.py).
NOTE = (
    "Precomputed offline sample of a synthetic scenario, not a live simulation and not "
    "real shelter records. The single-seed comparison is descriptive and is not a "
    "prediction or evidence of real-world effects."
)


def intervention_text(minute: int) -> str:
    """The operator's request at minute, as the old demo's 'Day 4 14:00 toilet_1 outage'."""
    day, clock = minute // 1440 + 1, f"{minute % 1440 // 60:02d}:{minute % 60:02d}"
    return f"Day {day} {clock} {FACILITY_ID} outage"


def open_path(project_id: str, run_id: str, branch_id: str) -> str:
    """The new UI's address of a saved branch; the VM landing page links to it."""
    query = urlencode({"project": project_id, "run": run_id, "branch": branch_id})
    return f"/demo-v2?{query}#simulation"


def _no_research(query: Any, **_kwargs: Any) -> Any:
    """The research of a preview, without the public web services it would read."""
    from sheltersim.region.models import RegionResearchResult

    return RegionResearchResult(
        request=query, researched_at=RESEARCHED_AT, warnings=(RESEARCH_WARNING,)
    )


def build_sample(
    projects_dir: Path,
    *,
    text: str = TEXT,
    request: dict[str, Any] | None = None,
    fork_minute: int = FORK_MINUTE,
) -> dict[str, Any]:
    """Create Project `sample` in projects_dir and compute both branches to the end.

    text is what the operator types on the Scenario screen; request, when given, is the
    structured form the screen sends with it (tests use a short one).
    """
    from sheltersim.web.projects import ProjectAPI

    if LLMSettings.from_env().effective_mode is not LLMMode.OFFLINE:
        raise RuntimeError("the sample is built offline only; set SHELTERSIM_LLM_MODE=offline")
    store = ProjectStore(projects_dir)
    if (store.root / PROJECT_ID).exists():
        raise FileExistsError(f"{store.root / PROJECT_ID} exists; build into an empty directory")
    started = time.perf_counter()
    api = ProjectAPI(store, recover=False)
    api.workspace.research.research = _no_research
    try:
        api.post([], {"project_id": PROJECT_ID})
        draft = api.post(
            [PROJECT_ID, "scenario-preview"],
            {"text": text, **({"request": request} if request is not None else {})},
        )
        if not draft.get("can_start"):
            raise RuntimeError(f"the Scenario preview cannot start: {draft.get('missing_fields')}")
        if draft["llm_usage"]["api_request_count"] != 0:
            raise RuntimeError("the Scenario preview called a Provider")
        scenario = ScenarioSpec.model_validate(draft["scenario"])
        end = scenario.duration_minutes
        if not 0 < fork_minute < end:
            raise ValueError("the fork minute must fall inside the Scenario")
        if not any(item.id == FACILITY_ID for item in scenario.facilities):
            raise ValueError(f"the Scenario has no {FACILITY_ID} facility")
        api.post(
            [PROJECT_ID, "scenario-approve"],
            {
                "preview_id": draft["preview_id"],
                "approved_hash": draft["approved_hash"],
                "approved": True,
                "run_id": RUN_ID,
            },
        )
        _compute(api, "main", end)

        order = intervention_text(fork_minute)
        branch = [PROJECT_ID, "runs", RUN_ID, "branches", "main"]
        preview = api.post(
            [*branch, "intervention-preview"], {"text": order, "minute": fork_minute}
        )
        if preview.get("status") != "compiled" or not preview.get("can_apply"):
            raise RuntimeError(f"the intervention preview cannot be applied: {preview}")
        if preview.get("usage", {}).get("api_request_count", 0) != 0:
            raise RuntimeError("the intervention preview called a Provider")
        api.post(
            [*branch, "intervention-apply"],
            {
                "preview_id": preview["preview_id"],
                "approved_program_hash": preview["approved_program_hash"],
                "approved": True,
                "child_id": CHILD_ID,
            },
        )
        _compute(api, CHILD_ID, end)
    finally:
        api.close()

    geometry = scenario.model_configs.get("facility_geometry") or {}
    return {
        "schema_version": 1,
        "kind": "precomputed_project_sample",
        "note": NOTE,
        "project_id": PROJECT_ID,
        "run_id": RUN_ID,
        "scenario": {
            "text": text,
            "municipality": draft["request"]["municipality"],
            "disaster": draft["request"]["disaster"],
            "scenario_id": scenario.id,
            "scenario_hash": draft["approved_hash"],
            "seed": scenario.seed,
            "facilities": sorted(item.id for item in scenario.facilities),
            "toilets": sorted(item.id for item in scenario.facilities if item.kind == "toilet"),
            "has_facility_geometry": bool(geometry),
            "regional_research": "not_run",
        },
        "agent_count": len(scenario.profiles),
        "duration_minutes": end,
        "fork_minute": fork_minute,
        "intervention": {
            "text": order,
            "support_mode": preview.get("support_mode"),
            "fallback_used": preview.get("fallback_used"),
            "approved_program_hash": preview["approved_program_hash"],
        },
        "branches": [_branch_summary(store, "main"), _branch_summary(store, CHILD_ID)],
        "open_branch_id": OPEN_BRANCH_ID,
        "open_path": open_path(PROJECT_ID, RUN_ID, OPEN_BRANCH_ID),
        "size_bytes": sum(
            path.stat().st_size for path in (store.root / PROJECT_ID).rglob("*") if path.is_file()
        ),
        "build_seconds": round(time.perf_counter() - started, 1),
    }


def _compute(api: Any, branch_id: str, end: int) -> None:
    """Resume a branch to the end, as the new UI's compute button does, and wait for it."""
    api.post([PROJECT_ID, "runs", RUN_ID, "resume"], {"branch_id": branch_id, "target_minute": end})
    api.wait_idle()
    status = api.store.branch_status(PROJECT_ID, RUN_ID, branch_id)
    reached = api.store.branch_reference(PROJECT_ID, RUN_ID, branch_id).computed_until
    if status != "completed" or reached != end:
        raise RuntimeError(f"branch {branch_id} ended {status} at minute {reached}, not {end}")


def _branch_summary(store: ProjectStore, branch_id: str) -> dict[str, Any]:
    """The branch's head and the hashes of its world, event history and metrics."""
    reference = store.branch_reference(PROJECT_ID, RUN_ID, branch_id)
    saved = store.checkpoint_fields(
        PROJECT_ID,
        RUN_ID,
        reference.checkpoint_hash,
        ("state_hash", "world_state", "metrics_state", "model_versions", "runtime_state"),
    )
    origin = saved["runtime_state"].get("fork_origin")
    return {
        "branch_id": branch_id,
        "parent_branch_id": reference.parent_branch_id,
        "fork_minute": reference.fork_minute,
        "computed_until": reference.computed_until,
        "world_hash": saved["state_hash"],
        "events_hash": digest(saved["world_state"]["event_log"]),
        "event_count": len(saved["world_state"]["event_log"]),
        "metrics_hash": digest(saved["metrics_state"]),
        "model_versions": saved["model_versions"],
        "fork_origin_state_hash": origin["state_hash"] if origin else None,
    }


def main() -> int:
    parser = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    parser.add_argument("output", type=Path, help="ProjectStore root to create Project sample in")
    parser.add_argument(
        "--summary",
        type=Path,
        help="where to write the JSON summary (default: sample-build.json next to OUTPUT)",
    )
    arguments = parser.parse_args()
    output = arguments.output.resolve()
    summary_path = (arguments.summary or output.parent / "sample-build.json").resolve()
    with tempfile.TemporaryDirectory(prefix="sheltersim-sample-") as scratch:
        # Never a Provider, and never the shared usage ledger or response tape.
        os.environ["SHELTERSIM_LLM_MODE"] = "offline"
        os.environ.pop("ANTHROPIC_API_KEY", None)
        os.environ["XDG_STATE_HOME"] = str(Path(scratch) / "state")
        os.environ["SHELTERSIM_LLM_CACHE_PATH"] = str(Path(scratch) / "llm-tape.json")
        os.environ["SHELTERSIM_PROJECTS_DIR"] = str(output)
        summary = build_sample(output)
    summary_path.parent.mkdir(parents=True, exist_ok=True)
    summary_path.write_text(
        json.dumps(summary, ensure_ascii=False, sort_keys=True, indent=2) + "\n", encoding="utf-8"
    )
    json.dump(summary, sys.stdout, ensure_ascii=False, sort_keys=True, indent=2)
    print()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
