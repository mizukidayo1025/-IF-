"""The worlds of the core's tick test without Python (Issue #152, P10-1b; #214).

`native/core/tests/tick_core.rs` builds a World from each world of
`native/core/tests/data/tick_core.json` and runs the core's `run_tick` on it with a host that has
no Python. The fixture holds two worlds, each set up for a Run and exported at minute 0 by
`support/core_fixtures.py` (`tick_core_fixture`; `benchmarks/core_fixtures.py` writes it):

- `small_replay`: h00_small_replay (4 people, 4 zones, 6 edges, 4 toilets, 5 staff) with a toilet
  outage and its recovery, run for `TICKS` ticks. It has no floor plan and no extensions.
- `generated`: a generated Scenario (`GENERATED_EVACUEES` people, the UI's floor plan, every
  extension) with a second service point at a toilet (so `facility_queues_v1` exists) and an
  EffectProgram that moves facilities while someone walks to them (`FACILITY_MOVES`), run for
  `GENERATED_TICKS` ticks: people walk (`movement_v1`'s journeys) and the core's `update_movement`
  replans the routes the moves changed.

The same world run through `TickEngine` gives the digests the fixture freezes
(`integration/test_core_fixtures.py` checks that the file is what the native path gives today).

The digests are the sha256 of the canonical JSON of each part of `WorldState.canonical_json()`
(the rows, the queue, the log, the fields and `model_state`; not `rng_state`, which the core's
World does not hold) and of the metric snapshots, each one a JSON list of its fields in order.
"""

from __future__ import annotations

import copy
import dataclasses
import hashlib
import json
from typing import Any

from sheltersim.core.world_export import export_world
from support.core_fixtures import run_world
from support.paths import SMALL_REPLAY

# The length of the test of `small_replay`, in ticks.
TICKS = 1440
# The generated world: its people, its length in ticks, and the facilities its EffectProgram moves
# (minute, facility, the point's new y in metres; x stays). At each minute someone walks to that
# facility on a trip not yet in its queue (found by running the world without the moves), so the
# move replans the trip; each point is in the facility's zone, half a metre from its walls.
GENERATED_EVACUEES = 10
GENERATED_TICKS = 360
FACILITY_MOVES = (
    (2, "toilet_01", 6.0),
    (28, "meal_distribution_main", 6.0),
    (150, "accessible_toilet_01", 6.0),
    (252, "water_station_main", 6.0),
)
# The detail of the record of a trip's event that a replan suppresses (`rules/replan.rs`).
REPLANNED = "the route changed before travel completion"
# The parts of the canonical document the digests cover, in this order.
PARTS = (
    "agents",
    "zones",
    "zone_edges",
    "facilities",
    "staff",
    "support_requests",
    "active_tasks",
    "resources",
    "event_queue",
    "event_log",
    "id_counters",
    "metric_state",
    "model_configs",
    "model_state",
    "scalars",
)
SCALARS = ("schema_version", "scenario_id", "seed", "duration_minutes", "minute_index")


def core_tick_scenario() -> Any:
    """h00_small_replay, TICKS minutes long, with a toilet outage at 15 and its recovery at 90."""
    from sheltersim.experiments.scenario import ScenarioSpec

    document = json.loads(SMALL_REPLAY.read_text(encoding="utf-8"))
    document["duration_minutes"] = TICKS
    provenance = {
        "description": "Small regression fixture event using an existing event contract.",
        "source_id": "ASM-H00-REGRESSION-OUTAGE-V1",
        "source_type": "assumption",
    }
    document["scheduled_events"].append(
        {
            "causation_id": None,
            "due_minute": 90,
            "event_type": "facility_recovery",
            "id": "h00-toilet-recovery",
            "kind": "exogenous",
            "payload": {"facility_id": "toilet_1", "units_restored": 1},
            "priority": 0,
            "provenance": provenance,
            "sequence": 1,
            "source_id": "h00-small-replay",
        }
    )
    return ScenarioSpec.from_json(json.dumps(document))


def core_tick_run() -> tuple[Any, Any, Any]:
    """The world, its controller and its engine, as `run_scenario` sets them up, at minute 0."""
    return run_world(core_tick_scenario())


def generated_scenario() -> Any:
    """The generated Scenario, `GENERATED_TICKS` minutes long, with a second service point (and
    queue) at `toilet_01`, whose capacity is two."""
    from sheltersim.experiments.initial_scenario import (
        InitialScenarioBuilder,
        InitialScenarioRequest,
    )
    from sheltersim.experiments.scenario import ScenarioSpec

    request = InitialScenarioRequest(
        municipality="合成市",
        disaster="地震",
        evacuees=GENERATED_EVACUEES,
        duration_days=GENERATED_TICKS / 1440,
        seed=2026,
    )
    document = InitialScenarioBuilder().build(request).scenario.model_dump(mode="json")
    geometry = document["model_configs"]["facility_geometry"]
    first = next(item for item in geometry["service_points"] if item["facility_id"] == "toilet_01")
    second = copy.deepcopy(first)
    second["id"] = second["queue_id"] = "toilet_01-second"
    second["point_m"] = {"x": first["point_m"]["x"], "y": first["point_m"]["y"] - 1.0}
    geometry["service_points"].append(second)
    toilet = next(item for item in document["facilities"] if item["id"] == "toilet_01")
    toilet["capacity"] = toilet["working_units"] = 2
    return ScenarioSpec.model_validate(document)


def facility_moves(geometry: dict[str, Any]) -> Any:
    """The EffectProgram of `FACILITY_MOVES`."""
    from sheltersim.events.effects import (
        DEFAULT_PRIMITIVE_REGISTRY,
        EffectProgram,
        SupportMode,
        TargetSelector,
        TimedAction,
    )

    spec = DEFAULT_PRIMITIVE_REGISTRY.get("move_facility")
    effects = [
        TimedAction(
            effect_id=f"move-{index}",
            op="move_facility",
            target_selector=TargetSelector(kind="facility", target_id=facility_id),
            start_minute=minute,
            end_minute=None,
            parameters={"point_m": {"x": geometry["facility_points"][facility_id]["x"], "y": y}},
            units=sorted(spec.units)[0],
            source_id="ASM-TEST",
            assumption_ids=[],
            policy_dependencies=sorted(spec.required_policies),
            reversible=False,
        )
        for index, (minute, facility_id, y) in enumerate(FACILITY_MOVES)
    ]
    return EffectProgram(
        program_id="fixture-facility-moves",
        narrative="Move facilities while people walk to them.",
        source_text="Move facilities while people walk to them.",
        simulation_start_minute=0,
        effects=effects,
        assumptions=[],
        support_mode=SupportMode.NATIVE,
        compiler_provenance={"compiler_kind": "offline_rule", "compiler_version": "test-v1"},
    )


def generated_run() -> tuple[Any, Any, Any]:
    """The generated world, its controller and its engine at minute 0, with the moves scheduled."""
    from sheltersim.events.effects import schedule_effect_program

    scenario = generated_scenario()
    world, controller, engine = run_world(scenario)
    schedule_effect_program(world, facility_moves(scenario.model_configs["facility_geometry"]))
    return world, controller, engine


def _text(value: Any) -> bytes:
    return json.dumps(
        value, sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False
    ).encode("utf-8")


def _sha(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def world_digests(world: Any) -> dict[str, str]:
    """The digest of each part of the world's canonical document."""
    document = json.loads(world.canonical_json())
    digests = {}
    for part in PARTS:
        if part == "scalars":
            value: Any = [document[name] for name in SCALARS]
        else:
            value = document[part]
        digests[part] = _sha(_text(value))
    return digests


def metrics_digest(snapshots: list[Any]) -> str:
    """The digest of the metric snapshots: one JSON list of each one's fields, in order."""
    lines = [
        _text([getattr(snapshot, field.name) for field in dataclasses.fields(snapshot)])
        for snapshot in snapshots
    ]
    return _sha(b"\n".join(lines))


def event_types(world: Any) -> list[str]:
    """The event type of each record of the log, in order."""
    return [record.event.event_type for record in world.event_log.records]


def native_result(world: Any, controller: Any, engine: Any, ticks: int) -> dict[str, Any]:
    """The world run on through `TickEngine` for `ticks` ticks: its digests, and how many ticks end
    with someone on a trip (`movement_v1`'s journeys; 0 without it)."""
    walking = 0
    for _ in range(ticks):
        engine.step()
        movement = world.model_state.get("movement_v1")
        walking += bool(movement and movement["journeys"])
    return {
        "ticks": ticks,
        "world": world_digests(world),
        "metrics": metrics_digest(controller.artifacts.metrics.snapshots),
        "metric_count": len(controller.artifacts.metrics.snapshots),
        "event_types": event_types(world),
        "ticks_with_journeys": walking,
        "replanned": sum(record.detail == REPLANNED for record in world.event_log.records),
    }


def _entry(run: tuple[Any, Any, Any], ticks: int) -> dict[str, Any]:
    world, controller, engine = run
    entry = export_world(world, controller)
    entry["initial"] = world_digests(world)
    entry["expected"] = native_result(world, controller, engine, ticks)
    return entry


def tick_core_fixture() -> dict[str, Any]:
    """`tick_core.json`: each world at minute 0 (`load::entry` of the core's tests), its digests,
    and what the native path gives after its ticks."""
    return {
        "worlds": {
            "small_replay": _entry(core_tick_run(), TICKS),
            "generated": _entry(generated_run(), GENERATED_TICKS),
        }
    }
