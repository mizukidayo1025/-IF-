"""Worlds whose `model_state` has every typed namespace, for the core's typed rows (Issue #225, F1).

`native/core/tests/typed_rows.rs` reads each namespace of `native/core/tests/data/typed_rows.json`
into its row with the core's `Field::from_value` and writes it back: the bytes must be the
namespace's canonical JSON. The fixture is this module's document
(`typed_rows_document`, registered in `core_fixtures.FIXTURES`: `benchmarks/core_fixtures.py`
writes it, and `integration/test_core_fixtures.py` checks that it still is):

- `typed`: the namespaces whose row is typed, in the order of `OWNED_NAMESPACES` (the others are
  the population module's `ValueRow`s). The core's test support has the same table.
- `worlds`: each world's name, minute and the typed namespaces of its canonical document, with
  the sha256 of each one's canonical JSON as Python's `json` writes it.

The worlds are generated Scenarios with the gate's operational conditions (`world_order.OPERATIONS`)
and the toilets of an effect program, so they have receipts, audit records and a social history.
`queues` also has two service points with a queue each at two facilities (as
`generated_scenario_with_split_queues`), a shift config with per-staff limits and an int off-shift
recovery, a rumor, and a patrol ordered to staff_001: queues and their stalls, queue trips, a
message, and the patrol at work, then finished. `operations` keeps the generated config, whose
per-staff limits are absent, and has a staff support trip.
"""

from __future__ import annotations

import hashlib
import json
from typing import Any

# The minutes the worlds are taken at. `queues`: its ordered task is under way, with queue trips
# and stalls; then it is finished. `operations`: a staff support trip is under way.
QUEUES_WORKING_MINUTE = 10
QUEUES_FINISHED_MINUTE = 18
OPERATIONS_MINUTE = 14
# The population module's row is a `ValueRow` (`native/src/state/population.rs`).
VALUE_ROW_MODULES = frozenset({"population"})


def typed_namespaces() -> list[str]:
    """The owned namespaces whose row is typed, in the order of `OWNED_NAMESPACES`."""
    from sheltersim.core.state_mirror import OWNED_NAMESPACES

    return [
        name for name, (module, _) in OWNED_NAMESPACES.items() if module not in VALUE_ROW_MODULES
    ]


def _split_queues(document: dict[str, Any]) -> None:
    geometry = document["model_configs"]["facility_geometry"]
    facilities = {row["id"]: row for row in document["facilities"]}
    for facility_id, skill in (
        ("toilet_01", "toilet_service"),
        ("water_station_main", "water_service"),
    ):
        facilities[facility_id]["capacity"] = facilities[facility_id]["working_units"] = 2
        point = geometry["facility_points"][facility_id]
        geometry["service_points"] = [
            row for row in geometry["service_points"] if row["facility_id"] != facility_id
        ]
        for side, offset in (("left", -1.0), ("right", 1.0)):
            geometry["service_points"].append(
                {
                    "schema_version": 1,
                    "id": f"{facility_id}-{side}",
                    "facility_id": facility_id,
                    "point_m": {"x": point["x"], "y": point["y"] + offset},
                    "service_capacity": 1,
                    "queue_id": f"{facility_id}-{side}-fifo",
                    "access": {"accessibility": "accessible", "required_staff_skills": [skill]},
                }
            )
    document["staff"].append(
        {
            "id": "toilet_staff_01",
            "zone_id": facilities["toilet_01"]["zone_id"],
            "skills": ["toilet_service"],
        }
    )


def _shifts(document: dict[str, Any]) -> None:
    from sheltersim.domain.staff_dynamics import SHIFT_STAFF_DYNAMICS_POLICY_VERSION

    document["model_configs"]["staff_dynamics"] = {
        "policy_version": SHIFT_STAFF_DYNAMICS_POLICY_VERSION,
        "shift_windows_by_staff": {
            "staff_001": [
                {"start_minute": 0, "end_minute": 45},
                {"start_minute": 90, "end_minute": 600},
            ]
        },
        "max_continuous_work_minutes_by_staff": {"staff_002": 120},
        "off_shift_fatigue_recovery_per_minute": 0,
    }


def _rumors(document: dict[str, Any]) -> None:
    source = "CORE-TYPED-ROWS-SYNTHETIC"
    document["scheduled_events"] = [
        *document.get("scheduled_events", []),
        {
            "id": "rumor-false",
            "kind": "exogenous",
            "due_minute": 2,
            "priority": 0,
            "sequence": 9001,
            "event_type": "information_rumor",
            "payload": {
                "message_id": "claim-false",
                "truth_status": "false",
                "origin_agent_id": "A001",
            },
            "source_id": source,
        },
        {
            "id": "correction-false",
            "kind": "exogenous",
            "due_minute": 8,
            "priority": 0,
            "sequence": 9002,
            "event_type": "information_correction",
            "payload": {"message_id": "claim-false", "zone_id": "sleeping", "belief": "rejected"},
            "source_id": source,
            "causation_id": "rumor-false",
        },
    ]
    document["model_configs"]["information_spread"].update(
        base_transmission_probability=0.9, minimum_contact_minutes=1, rumor_belief_probability=0.9
    )


def _program(staff_id: str | None) -> Any:
    """Two toilets at minute 1, and a patrol ordered to staff_id at minute 0 if there is one."""
    from sheltersim.events.effects import (
        DEFAULT_PRIMITIVE_REGISTRY,
        EffectProgram,
        SupportMode,
        TargetSelector,
        TimedAction,
    )

    def action(op: str, kind: str, target: str, units: str, parameters: dict, start: int) -> Any:
        return TimedAction(
            effect_id=f"{op}-1",
            op=op,
            target_selector=TargetSelector(kind=kind, target_id=target),
            start_minute=start,
            parameters=parameters,
            units=units,
            source_id="CORE-TYPED-ROWS-SYNTHETIC",
            assumption_ids=[],
            policy_dependencies=sorted(DEFAULT_PRIMITIVE_REGISTRY.get(op).required_policies),
            reversible=False,
        )

    effects = [action("add_toilet", "zone", "toilet", "toilet_stall", {"count": 2}, 1)]
    if staff_id is not None:
        # An order starts at the minute the program is scheduled.
        order = {"task_kind": "patrol", "target_zone_id": "sleeping", "reason": "synthetic patrol"}
        effects.append(action("staff_task_order", "staff", staff_id, "staff_task_order", order, 0))
    return EffectProgram(
        program_id="program-core-typed-rows",
        narrative="core typed rows",
        source_text="core typed rows",
        simulation_start_minute=min(effect.start_minute for effect in effects),
        effects=effects,
        assumptions=[],
        support_mode=SupportMode.NATIVE,
        compiler_provenance={"compiler_kind": "offline_rule", "compiler_version": "test-v1"},
    )


def _scenario(*changes: Any) -> Any:
    from sheltersim.experiments.scenario import ScenarioSpec
    from support.world_order import OPERATIONS, generated

    document = generated(operational_conditions=OPERATIONS).model_dump(mode="json")
    for change in changes:
        change(document)
    return ScenarioSpec.model_validate(document)


def _sha(value: Any) -> str:
    text = json.dumps(
        value, sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False
    )
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def typed_rows_document() -> dict[str, Any]:
    """The fixture: the typed namespaces, and each world's typed `model_state`."""
    from support.world_order import Run

    names = typed_namespaces()
    worlds = []
    queues = Run(_scenario(_split_queues, _shifts, _rumors), _program("staff_001"))
    operations = Run(_scenario(), _program(None))
    for name, run, minute in (
        ("queues", queues, QUEUES_WORKING_MINUTE),
        ("queues", queues, QUEUES_FINISHED_MINUTE),
        ("operations", operations, OPERATIONS_MINUTE),
    ):
        state = json.loads(run.to(minute).canonical_json())["model_state"]
        typed = {key: state[key] for key in names if key in state}
        digests = {key: _sha(value) for key, value in typed.items()}
        worlds.append({"name": name, "minute": minute, "model_state": typed, "digests": digests})
    return {"typed": names, "worlds": worlds}
