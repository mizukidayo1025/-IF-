"""The small world of the core's tick test without Python (Issue #152, P10-1b).

`native/core/tests/tick_core.rs` builds a World from `native/core/tests/data/tick_core.json`
and runs the core's `run_tick` on it with a host that has no Python. The fixture is this
module's world: h00_small_replay (4 people, 4 zones, 6 edges, 4 toilets, 5 staff) with a
toilet outage and its recovery, set up for a Run and exported at minute 0
(`benchmarks/core_tick_fixture.py` writes it). The same world run through `TickEngine`
gives the digests the fixture freezes (`tests/integration/test_core_tick.py` checks that the
native path still does).

The digests are the sha256 of the canonical JSON of each part of `WorldState.canonical_json()`
(the rows, the queue, the log, the fields and `model_state`; not `rng_state`, which the core's
World does not hold) and of the metric snapshots, each one a JSON list of its fields in order.
"""

from __future__ import annotations

import dataclasses
import hashlib
import json
import tempfile
from typing import Any

from support.paths import SMALL_REPLAY

# The length of the test, in ticks.
TICKS = 1440
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
    from sheltersim.core.engine import TickEngine, TraceLevel
    from sheltersim.core.world import WorldState
    from sheltersim.domain.simulator_state import initialize_simulator_extensions
    from sheltersim.experiments.runner import OfflineSimulationController
    from sheltersim.inspection.traces import RunArtifactWriter

    scenario = core_tick_scenario()
    world = WorldState.from_scenario(scenario)
    initialize_simulator_extensions(world)
    artifacts = RunArtifactWriter(
        scenario, tempfile.mkdtemp(prefix="core-tick-"), trace_level=TraceLevel.OFF
    )
    artifacts.write_initial(world, execution_mode="offline")
    controller = OfflineSimulationController(world, artifacts)
    engine = TickEngine(world, phase_handler=controller)
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


def fixture_document(world: Any, controller: Any) -> dict[str, Any]:
    """What the core's test reads: the world at minute 0 and what the Run read of the controller."""
    from sheltersim.contracts.policies import selected_models
    from sheltersim.core.state_mirror import OWNED_FIELDS

    core = world.__dict__["_core"]
    return {
        "world": json.loads(world.canonical_json()),
        "initial": world_digests(world),
        # The model selection, which a Python contract parses (`Host::fill_section`).
        "selection": [list(item) for item in selected_models(world.model_configs).items()],
        "slots": [[name, name in OWNED_FIELDS] for name in core.owned_names()],
        "run": {
            "route_capacities": controller.route_capacities,
            "zone_neighbors": {
                zone: list(neighbors) for zone, neighbors in controller.zone_neighbors.items()
            },
            "policies": controller._run_core.policies_document(),
        },
    }


def native_result(ticks: int = TICKS) -> dict[str, Any]:
    """The world of the fixture run through `TickEngine` for `ticks` ticks: its digests."""
    world, controller, engine = core_tick_run()
    for _ in range(ticks):
        engine.step()
    return {
        "ticks": ticks,
        "world": world_digests(world),
        "metrics": metrics_digest(controller.artifacts.metrics.snapshots),
        "metric_count": len(controller.artifacts.metrics.snapshots),
        "event_types": event_types(world),
    }
