"""The frames of the core's frame test (#264): what `world_frame` gives at some minutes of a Run.

`native/core/tests/frames.rs` (cargo test) and `native/wasm/tests/node_frames.mjs` (Node) run the
worlds of `tick_core.json` (`support/core_tick.py`) and the world `arrivals` of `frames.json` in
the core, from minute 0, and check at each minute here that the core's frame
(`sheltersim_core::frame`) has the sha256 of the frame the native path gave on the same world, and
that the world's digest (`state_digest`) is the frame's `state_hash`. `frames_fixture` builds
`frames.json`; `benchmarks/core_fixtures.py` writes it.

- `small_replay` has no floor plan: its people are at their zones' display points
  (`display_only`).
- `generated` moves four facilities with an EffectProgram (`FACILITY_MOVES`); the minutes are
  before and after the moves.
- `arrivals` is a generated Scenario (`ARRIVALS_EVACUEES` people, one day) whose floor plan has an
  arrival approach route into `quiet`, with `ARRIVAL_COUNT` more people coming on it at
  `ARRIVAL_MINUTE`: they walk in (`approaching`), and with room in `quiet` for one
  (`QUIET_CAPACITY`) the others wait at the door (`awaiting_admission`). By the end of the day
  its log is longer than the frame's scan of interactions (`event_window_truncated`), and people
  have open care requests.

The builder checks that each of these is in the frames it records.
"""

from __future__ import annotations

import copy
import hashlib
from typing import Any

from sheltersim.core.checkpoint_cache import canonical_bytes
from sheltersim.core.world_export import export_world
from support.core_fixtures import run_world
from support.core_tick import GENERATED_TICKS, TICKS, core_tick_run, generated_run

ARRIVALS_EVACUEES = 12
ARRIVALS_TICKS = 1440
ARRIVAL_MINUTE = 30
ARRIVAL_COUNT = 3
ARRIVAL_ROUTE = "arrival-to-quiet"
ARRIVAL_ZONE = "quiet"
QUIET_CAPACITY = 1

# The minutes of each world's frames.
MINUTES: dict[str, tuple[int, ...]] = {
    "small_replay": (0, 90, 600, TICKS),
    "generated": (0, 30, 152, GENERATED_TICKS),
    "arrivals": (
        0,
        ARRIVAL_MINUTE + 1,
        ARRIVAL_MINUTE + 3,
        ARRIVAL_MINUTE + 10,
        720,
        ARRIVALS_TICKS - 1,
    ),
}
# The run and the branch the frames name.
RUN_ID = "run"
BRANCH_ID = "main"


def arrivals_scenario() -> Any:
    """The generated Scenario of `arrivals`: its building is taller, so that the approach route can
    start outside every zone, and the route ends in `quiet`, which has room for one."""
    from sheltersim.experiments.initial_scenario import (
        InitialScenarioBuilder,
        InitialScenarioRequest,
    )
    from sheltersim.experiments.scenario import ScenarioSpec

    request = InitialScenarioRequest(
        municipality="合成市",
        disaster="地震",
        evacuees=ARRIVALS_EVACUEES,
        duration_days=ARRIVALS_TICKS / 1440,
        seed=2026,
    )
    document = InitialScenarioBuilder().build(request).scenario.model_dump(mode="json")
    arrival_zone = next(zone for zone in document["zones"] if zone["id"] == ARRIVAL_ZONE)
    arrival_zone["capacity"] = QUIET_CAPACITY
    geometry = document["model_configs"]["facility_geometry"]
    quiet = next(zone for zone in geometry["zones"] if zone["zone_id"] == ARRIVAL_ZONE)
    bottom = max(point["y"] for point in quiet["polygon"])
    middle = sum(point["x"] for point in quiet["polygon"]) / len(quiet["polygon"])
    geometry["height_m"] = bottom + 4.0
    geometry["arrival_approach_routes"] = [
        {
            "route_id": ARRIVAL_ROUTE,
            "destination_zone_id": ARRIVAL_ZONE,
            "points_m": [
                {"x": middle, "y": bottom + 3.0},
                {"x": middle + 0.5, "y": bottom + 1.5},
                {"x": middle, "y": bottom - 1.0},
            ],
            "walking_speed_m_per_min": 1.0,
            "source_kind": "operator",
            "source_ids": ["SRC-TEST-APPROACH-ROUTE"],
        }
    ]
    return ScenarioSpec.model_validate(copy.deepcopy(document))


def arrivals_program() -> Any:
    """The EffectProgram that brings `ARRIVAL_COUNT` people in on the approach route."""
    from sheltersim.events.effects import (
        EffectProgram,
        SupportMode,
        TargetSelector,
        TimedAction,
    )

    effect = TimedAction(
        effect_id="arrivals",
        op="add_evacuees",
        target_selector=TargetSelector(kind="zone", target_id=ARRIVAL_ZONE),
        start_minute=ARRIVAL_MINUTE,
        parameters={
            "count": ARRIVAL_COUNT,
            "household_size": 1,
            "mobility": "independent",
            "language": "ja",
            "admission_zone_ids": [],
            "approach_route_id": ARRIVAL_ROUTE,
            "person_context_overrides": [],
        },
        units="evacuee",
        source_id="ASM-TEST-DYNAMIC-ARRIVAL-V1",
        policy_dependencies=["population_dynamics_v1"],
        reversible=False,
    )
    return EffectProgram(
        program_id="fixture-arrivals",
        narrative="People come in on the approach route.",
        source_text="People come in on the approach route.",
        simulation_start_minute=0,
        effects=[effect],
        assumptions=[],
        support_mode=SupportMode.NATIVE,
        compiler_provenance={"compiler_kind": "offline_rule", "compiler_version": "test-v1"},
    )


def arrivals_run() -> tuple[Any, Any, Any]:
    """The world of `arrivals`, its controller and its engine at minute 0, with the arrivals."""
    from sheltersim.events.effects import schedule_effect_program

    world, controller, engine = run_world(arrivals_scenario())
    schedule_effect_program(world, arrivals_program())
    return world, controller, engine


def frames(run: tuple[Any, Any, Any], minutes: tuple[int, ...]) -> tuple[list[Any], list[Any]]:
    """The world run on to each minute: what the fixture records of its frame there (the minute,
    the sha256 of the frame's canonical JSON, its `state_hash`), and the frames."""
    from sheltersim.inspection.frames import world_frame

    world, _, engine = run
    recorded, made = [], []
    for minute in minutes:
        while world.minute_index < minute:
            engine.step()
        frame = world_frame(world, run_id=RUN_ID, branch_id=BRANCH_ID, computed_until=minute)
        digest = hashlib.sha256(canonical_bytes(frame)).hexdigest()
        recorded.append([minute, digest, frame["state_hash"]])
        made.append(frame)
    return recorded, made


def _shown(made: list[Any]) -> dict[str, bool]:
    """What the frames show of the cases the fixture is for."""
    people = [person for frame in made for person in frame["people"]]
    arrivals = [item["status"] for frame in made for item in frame.get("incoming_arrivals", [])]
    return {
        "display_only": any(
            agent["position_kind"] == "display_only" for frame in made for agent in frame["agents"]
        ),
        "moved": any(frame["geometry_overlay"].get("facility_points") for frame in made),
        "approaching": "approaching" in arrivals,
        "awaiting_admission": "awaiting_admission" in arrivals,
        "care_requests": any(person["care_requests"] for person in people),
        "truncated": any(
            frame["recorded_history"]["social"]["event_window_truncated"] for frame in made
        ),
    }


def frames_fixture() -> dict[str, Any]:
    """`frames.json`: the world `arrivals` at minute 0, and each world's frames at its minutes."""
    runs = {
        "small_replay": core_tick_run(),
        "generated": generated_run(),
        "arrivals": arrivals_run(),
    }
    entry = export_world(runs["arrivals"][0], runs["arrivals"][1])
    expected, shown = {}, {}
    for name, run in runs.items():
        expected[name], made = frames(run, MINUTES[name])
        shown[name] = _shown(made)
    assert shown["small_replay"]["display_only"], shown
    assert shown["generated"]["moved"], shown
    for case in ("approaching", "awaiting_admission", "care_requests", "truncated"):
        assert shown["arrivals"][case], (case, shown)
    return {
        "worlds": {"arrivals": entry},
        "files": {"small_replay": "tick_core", "generated": "tick_core", "arrivals": "frames"},
        "expected": expected,
    }
