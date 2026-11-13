"""The precomputed sample Project (scripts/build_project_sample.py) that /demo-v2 opens.

A short variant of the sample is built through the builder: the same ProjectAPI routes
the new UI calls (scenario-preview and scenario-approve of a generated Scenario, resume,
intervention-preview and intervention-apply), with the structured form the Scenario
screen sends for 20 people and 0.1 days instead of the 7-day example text, and an
earlier fork. The 7-day sample itself is built on the judges' VM (docs/vm-deploy.md).

The browser's form of the sample (scripts/build_browser_sample.py, #265) starts from the inputs
Python writes for it: they are the records and answers the server gives for the same Scenario and
Run. Its Node half (the WASM Run and the bundle) is checked by native/wasm/tests.
"""

from __future__ import annotations

import hashlib
import html
import json
import runpy
import shutil
import sqlite3
import sys
from collections.abc import Callable
from contextlib import closing
from pathlib import Path
from typing import Any

import pytest

from sheltersim.experiments.timeline import TimelineStore
from sheltersim.storage.projects import ProjectStore
from sheltersim.web.projects import ProjectAPI

BUILDER = Path(__file__).parents[2] / "scripts" / "build_project_sample.py"
LANDING = Path(__file__).parents[2] / "deploy" / "vm" / "render_landing.py"
BROWSER = Path(__file__).parents[2] / "scripts" / "build_browser_sample.py"
END = 144
FORK = 60


@pytest.fixture(scope="module")
def builder() -> dict[str, Any]:
    return runpy.run_path(str(BUILDER))


@pytest.fixture
def build(
    builder: dict[str, Any],
    generated_request: Callable[..., dict[str, Any]],
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> Callable[[str], dict[str, Any]]:
    monkeypatch.setenv("SHELTERSIM_LLM_MODE", "offline")
    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)

    def run(name: str) -> dict[str, Any]:
        return builder["build_sample"](
            tmp_path / name,
            text="",
            request=generated_request(duration_days=END / 1440),
            fork_minute=FORK,
        )

    return run


def _results(summary: dict[str, Any]) -> list[tuple[Any, ...]]:
    return [
        (b["branch_id"], b["computed_until"], b["world_hash"], b["events_hash"], b["metrics_hash"])
        for b in summary["branches"]
    ]


def test_two_builds_of_the_sample_give_the_same_hashes(
    build: Callable[[str], dict[str, Any]],
) -> None:
    first, second = build("first"), build("second")
    assert first["scenario"]["scenario_hash"] == second["scenario"]["scenario_hash"]
    assert _results(first) == _results(second)
    assert first["intervention"] == second["intervention"]
    assert first["branches"][0]["world_hash"] != first["branches"][1]["world_hash"]


def test_the_sample_holds_a_completed_run_and_an_approved_outage_branch(
    builder: dict[str, Any], build: Callable[[str], dict[str, Any]], tmp_path: Path
) -> None:
    summary = build("projects")
    run_id, child_id = builder["RUN_ID"], builder["CHILD_ID"]
    assert summary["project_id"] == "sample"
    assert summary["run_id"] == run_id
    assert [b["branch_id"] for b in summary["branches"]] == ["main", child_id]
    assert summary["intervention"]["text"] == "Day 1 01:00 toilet_01 outage"
    assert summary["scenario"]["has_facility_geometry"] is True
    assert "toilet_01" in summary["scenario"]["toilets"]
    assert summary["open_branch_id"] == "main"
    assert summary["open_path"] == f"/demo-v2?project=sample&run={run_id}&branch=main#simulation"

    store = ProjectStore(tmp_path / "projects")
    assert [p["project_id"] for p in store.projects()] == ["sample"]
    run = store.run("sample", run_id)
    assert run["status"] == "completed"
    [saved] = store.scenarios("sample")
    provenance = saved["scenario"]["provenance"]
    assert provenance["region_research"]["warnings"] == [builder["RESEARCH_WARNING"]]
    branches = {b["branch_id"]: b for b in store.branches("sample", run_id)}
    assert branches["main"]["computed_until"] == END
    assert branches[child_id]["parent_branch_id"] == "main"
    assert branches[child_id]["fork_minute"] == FORK
    assert branches[child_id]["computed_until"] == END

    timeline = TimelineStore(store, "sample", run_id)
    main, child = timeline.seek("main", END), timeline.seek(child_id, END)
    assert main.command_log == []
    [command] = child.command_log
    assert command["command"] == "effect_program"
    assert command["minute"] == FORK
    assert command["program_hash"] == summary["intervention"]["approved_program_hash"]
    assert main.world.facilities["toilet_01"].working_units == 1
    assert child.world.facilities["toilet_01"].working_units == 0


def test_a_copy_of_the_sample_answers_the_routes_the_new_ui_opens_it_with(
    builder: dict[str, Any], build: Callable[[str], dict[str, Any]], tmp_path: Path
) -> None:
    """The VM copies the built sample into each slot; the copy opens like the original,
    with people at physical points the map draws."""
    build("built")
    shutil.copytree(tmp_path / "built" / "sample", tmp_path / "slot" / "sample")
    run_id, child_id = builder["RUN_ID"], builder["CHILD_ID"]
    api = ProjectAPI(ProjectStore(tmp_path / "slot"))
    try:
        assert [p["project_id"] for p in api.get([], {})["projects"]] == ["sample"]
        assert [r["run_id"] for r in api.get(["sample", "runs"], {})["runs"]] == [run_id]
        branch = ["sample", "runs", run_id, "branches", child_id]
        frame = api.get([*branch, "timeline"], {"minute": [str(FORK)]})
        assert (frame["branch_id"], frame["minute"]) == (child_id, FORK)
        assert frame["agents"]
        assert all(agent["point"] is not None for agent in frame["agents"])
        assert "toilet_01" in frame["geometry"]["facility_points"]
        assert api.get([*branch, "llm-status"], {})["mode"] == "offline"
        comparison = api.get(
            ["sample", "runs", run_id, "compare"],
            {"branch_ids": [f"main,{child_id}"], "minute": [str(END)]},
        )
        assert [f["branch_id"] for f in comparison["frames"]] == ["main", child_id]
    finally:
        api.close()


def test_the_vm_landing_page_links_to_the_built_sample(
    build: Callable[[str], dict[str, Any]],
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    summary = build("projects")
    path = tmp_path / "sample-build.json"
    path.write_text(json.dumps(summary), encoding="utf-8")
    monkeypatch.setattr(sys, "argv", ["render_landing.py", "example", "1", str(path)])
    runpy.run_path(str(LANDING), run_name="__main__")
    page = capsys.readouterr().out
    href = html.escape(f"https://s1.example{summary['open_path']}")
    assert f'<a href="{href}">計算済みの例をすぐ見る</a>' in page


def test_a_sample_is_rebuilt_only_when_a_build_now_would_give_another_result(
    builder: dict[str, Any],
    build: Callable[[str], dict[str, Any]],
    generated_request: Callable[..., dict[str, Any]],
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """The VM's setup.sh asks --check before it rebuilds (docs/vm-deploy.md)."""
    summary = build("sample")
    differ = builder["sample_difference"]

    def check(built: dict[str, Any], **plan: Any) -> str | None:
        return differ(
            tmp_path / "sample",
            built,
            **{
                "text": "",
                "request": generated_request(duration_days=END / 1440),
                "fork_minute": FORK,
                **plan,
            },
        )

    # Another app commit with the same plan, Scenario and model: nothing to rebuild.
    assert check(summary) is None
    assert check(summary, fork_minute=FORK + 1) == "the plan changed: fork_minute, intervention"
    other = json.loads(json.dumps(summary))
    other["scenario"]["scenario_hash"] = "0" * 64
    assert check(other) == "the Scenario the preview makes from the text changed"
    # A model version the saved Checkpoints did not record makes the sample view-only.
    from sheltersim.core import model_version

    monkeypatch.setattr(model_version, "ENGINE_VERSION", "test-next")
    assert check(summary).startswith("the model changed for branch main: engine:model")
    monkeypatch.undo()
    # An index of an older version answers no history until rebuilt (#130).
    index = tmp_path / "sample" / "sample" / "runs" / summary["run_id"] / "history.sqlite"
    with closing(sqlite3.connect(index)) as db:  # Closed before the files are removed.
        db.execute("PRAGMA user_version=3")
        db.commit()
    assert check(summary) == "the history index is of another version"
    shutil.rmtree(tmp_path / "sample" / "sample")
    monkeypatch.setenv("SHELTERSIM_LLM_MODE", "offline")
    assert check(summary).startswith("branch main cannot be read")


def test_the_check_command_answers_setup_sh_with_its_first_line_and_status(
    builder: dict[str, Any],
    build: Callable[[str], dict[str, Any]],
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    """setup.sh rebuilds unless --check prints `current`; this short sample was built from
    another text than the example the command plans with."""
    summary = build("sample")
    path = tmp_path / "sample-build.json"
    path.write_text(json.dumps(summary), encoding="utf-8")
    monkeypatch.setattr(sys, "argv", ["build", str(tmp_path / "sample"), "--check"])
    assert builder["main"]() == 3
    assert capsys.readouterr().out == "rebuild: the plan changed: fork_minute, intervention, text\n"
    path.unlink()
    assert builder["main"]() == 3
    assert capsys.readouterr().out.startswith("rebuild: no readable summary")


def test_the_browser_sample_starts_from_the_records_and_answers_of_the_server(
    builder: dict[str, Any],
    generated_request: Callable[..., dict[str, Any]],
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    """#265: the inputs of the browser's bundle are what the server answers for the same text.

    The world at minute 0 is the one a Run of the Scenario ticks from: after some ticks it is the
    world the Project's route computed.
    """
    from support.core_fixtures import run_world
    from support.core_tick import world_digests

    monkeypatch.setenv("SHELTERSIM_LLM_MODE", "offline")
    monkeypatch.delenv("ANTHROPIC_API_KEY", raising=False)
    request = generated_request(duration_days=FORK / 1440)
    inputs_dir = tmp_path / "inputs"
    inputs = runpy.run_path(str(BROWSER))["export_inputs"](inputs_dir, text="", request=request)

    run_id = builder["RUN_ID"]
    api = ProjectAPI(ProjectStore(tmp_path / "projects"), recover=False)
    api.workspace.research.research = builder["_no_research"]
    try:
        draft, _ = builder["_preview"](api, "", request)
        body = {"approved": True, "approved_hash": draft["approved_hash"], "run_id": run_id}
        api.post(["sample", "scenario-approve"], {**body, "preview_id": draft["preview_id"]})
        project = api.get(["sample"], {})
        run = api.get(["sample", "runs", run_id], {})
        evidence = api.get(["sample", "runs", run_id, "evidence"], {})
        api.post(["sample", "runs", run_id, "resume"], {"branch_id": "main", "target_minute": FORK})
        api.wait_idle(timeout=120)
        computed = TimelineStore(api.store, "sample", run_id).seek("main", FORK).world
    finally:
        api.close()

    record = ("schema_version", "project_id", "archived")
    assert inputs["project"] == {key: project[key] for key in record}
    scenario_bytes = (inputs_dir / "scenario.json").read_bytes()
    [scenario] = project["scenarios"]
    assert {**inputs["scenario_record"], "scenario": json.loads(scenario_bytes)} == scenario
    assert hashlib.sha256(scenario_bytes).hexdigest() == scenario["scenario_hash"]
    assert inputs["run"] == {key: value for key, value in run.items() if key != "branches"}
    assert json.loads((inputs_dir / "evidence.json").read_bytes()) == json.loads(
        json.dumps(evidence)
    )
    assert inputs["source"]["agent_count"] == len(scenario["scenario"]["profiles"])

    entry = json.loads((inputs_dir / "export.json").read_bytes())
    assert entry["world"]["minute_index"] == 0
    stored = api.store.scenario("sample", scenario["scenario_id"], scenario["scenario_version"])
    world, _, _ = run_world(stored, FORK)
    assert world_digests(world) == world_digests(computed)
