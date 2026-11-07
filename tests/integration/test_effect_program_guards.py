"""EffectProgram guards on a generated Scenario: a false guard suppresses an effect's start
and its expiry alike, environment changes included (#195), an and-guard applies when
every operand holds, a guard on a floor plan change reads its facility or zone target
(#194), and eq compares a count with a float by value (#196).

These were defects found while covering these paths (#178). A guard is only the start's: the
events a guarded start schedules (a staff member's return, a delayed delivery, an arrival's
approach) used to copy it and stop the Run (#89, 元 #310).
"""

from __future__ import annotations

import copy
import itertools

import pytest

from sheltersim import native
from sheltersim.events.effects import (
    DEFAULT_PRIMITIVE_REGISTRY,
    EffectProgram,
    GuardExpression,
    SupportMode,
    TargetSelector,
    TimedAction,
)
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.storage.projects import digest

END = 60
START, STOP = 10, 40
_BRANCHES = itertools.count()


@pytest.fixture(scope="module")
def main_line(tmp_path_factory, generated_previews, saved_timeline):
    """The generated Scenario's main line at the start minute; tests fork their own Branches."""
    scenario = ScenarioSpec.model_validate(
        copy.deepcopy(generated_previews(duration_days=END / 1440)["scenario"])
    )
    with pytest.MonkeyPatch.context() as monkeypatch:
        monkeypatch.setenv("SHELTERSIM_LLM_MODE", "offline")
        _, timeline = saved_timeline(tmp_path_factory.mktemp("guards") / "projects", scenario)
        timeline.advance("main", START, checkpoint_interval=START)
        yield timeline


def _with_approach_route(document: dict) -> None:
    """A walking route from outside the plan, which grows to hold it, to the corridor."""
    geometry = document["model_configs"]["facility_geometry"]
    corridor = next(row for row in geometry["zones"] if row["zone_id"] == "corridor")
    points = corridor["polygon"]
    middle = {
        "x": sum(point["x"] for point in points) / len(points),
        "y": sum(point["y"] for point in points) / len(points),
    }
    outside = float(geometry["height_m"]) + 5.0
    geometry["height_m"] = outside + 5.0
    geometry["arrival_approach_routes"] = [
        {
            "route_id": "arrival-to-corridor",
            "destination_zone_id": "corridor",
            "points_m": [{"x": middle["x"], "y": outside}, middle],
            "walking_speed_m_per_min": 1.0,
            "source_kind": "operator",
            "source_ids": ["TEST-APPROACH-ROUTE"],
        }
    ]


@pytest.fixture(scope="module")
def operations_line(tmp_path_factory, generated_previews, saved_timeline):
    """main_line with a water delivery due at minute 30 and an approach route for arrivals."""
    preview = generated_previews(
        duration_days=END / 1440,
        operational_conditions={
            "scheduled": [
                {
                    "kind": "resource_delivery",
                    "resource_id": "water_ml",
                    "quantity": 10_000,
                    "due_minute": 30,
                    "warehouse_id": "main_warehouse",
                }
            ],
        },
    )
    document = copy.deepcopy(preview["scenario"])
    _with_approach_route(document)
    scenario = ScenarioSpec.model_validate(document)
    with pytest.MonkeyPatch.context() as monkeypatch:
        monkeypatch.setenv("SHELTERSIM_LLM_MODE", "offline")
        _, timeline = saved_timeline(tmp_path_factory.mktemp("operations") / "projects", scenario)
        timeline.advance("main", START, checkpoint_interval=START)
        yield timeline


def _leaf(field: str, operator: str, value: object) -> GuardExpression:
    return GuardExpression(operator=operator, field=field, value=value)


def _outcomes(timeline, op, kind, target, parameters, guard, *, end=STOP):
    """Fork with one guarded effect, run to END and return its (minute, status, detail)."""
    return [
        (record.realized_minute, record.status.value, record.detail)
        for record in _effect_records(timeline, op, kind, target, parameters, guard, end=end)
    ]


def _effect_records(timeline, op, kind, target, parameters, guard, *, end=STOP):
    """Fork with one guarded effect, run to END and return the records of its events."""
    spec = DEFAULT_PRIMITIVE_REGISTRY.get(op)
    program = EffectProgram(
        program_id=f"program-{op}",
        narrative=op,
        source_text=op,
        simulation_start_minute=START,
        effects=[
            TimedAction(
                effect_id="guarded",
                op=op,
                target_selector=TargetSelector(kind=kind, target_id=target),
                start_minute=START,
                end_minute=end,
                parameters=parameters,
                units=sorted(spec.units)[0],
                source_id="TEST-GUARD",
                assumption_ids=[],
                policy_dependencies=sorted(spec.required_policies),
                reversible=end is not None,
                guard=guard,
            )
        ],
        assumptions=[],
        support_mode=SupportMode.NATIVE,
        compiler_provenance={"compiler_kind": "offline_rule", "compiler_version": "test-v1"},
    )
    branch_id = f"guarded-{next(_BRANCHES)}"
    timeline.fork(
        "main",
        START,
        branch_id,
        program=program,
        approved_program_hash=digest(program.model_dump(mode="json")),
    )
    world = timeline.advance(branch_id, END, checkpoint_interval=END).world
    return [
        record
        for record in world.event_log.records
        if "effect_program_v1" in record.event.provenance
    ]


NEVER = _leaf("simulation.minute", "gte", 100_000)


@pytest.mark.parametrize(
    "op,kind,target,parameters",
    [
        ("set_facility_availability", "facility", "medical_desk", {"working_units": 0}),
        ("add_temporary_space", "zone", "sleeping", {"people_capacity": 5, "sleeping_capacity": 5}),
        ("set_route_access", "route", "route_corridor_quiet", {"accessible": False}),
    ],
)
def test_a_false_guard_suppresses_the_start_and_its_expiry(main_line, op, kind, target, parameters):
    outcomes = _outcomes(main_line, op, kind, target, parameters, NEVER)

    assert [(minute, status) for minute, status, _ in outcomes] == [
        (START, "suppressed"),
        (STOP, "suppressed"),
    ]
    assert outcomes[0][2] == "EffectProgram guard evaluated false"


def test_an_and_guard_applies_when_every_operand_holds(main_line):
    guard = GuardExpression(
        operator="and",
        operands=[_leaf("zone.occupancy", "gte", 0), _leaf("simulation.minute", "lt", END)],
    )

    outcomes = _outcomes(main_line, "set_ambient_noise", "zone", "sleeping", {"level": 0.8}, guard)

    assert [(minute, status) for minute, status, _ in outcomes] == [
        (START, "applied"),
        (STOP, "applied"),
    ]


def test_an_environment_change_with_a_false_guard_has_a_suppressed_expiry(main_line):
    outcomes = _outcomes(main_line, "set_ambient_noise", "zone", "sleeping", {"level": 0.8}, NEVER)

    assert [(minute, status) for minute, status, _ in outcomes] == [
        (START, "suppressed"),
        (STOP, "suppressed"),
    ]


# Floor plan changes keep their target in target_id; guards used to ignore it (#194).
MOVE_DESK = ("move_facility", "facility", "medical_desk", {"point_m": {"x": 10.4, "y": 5.0}})
CORNER_PARTITION = (
    "set_partition",
    "zone",
    "medical",
    {
        "polygon_m": [
            {"x": 9.6, "y": 11.9},
            {"x": 9.9, "y": 11.9},
            {"x": 9.9, "y": 12.3},
            {"x": 9.6, "y": 12.3},
        ]
    },
)


@pytest.mark.parametrize(
    "change,guard,end",
    [
        (MOVE_DESK, _leaf("target.working_units", "eq", 1), None),
        (CORNER_PARTITION, _leaf("zone.occupancy", "gte", 0), STOP),
    ],
)
def test_a_true_guard_on_a_floor_plan_change_applies_it(main_line, change, guard, end):
    outcomes = _outcomes(main_line, *change, guard, end=end)

    assert outcomes[0][:2] == (START, "applied"), outcomes[0]


@pytest.mark.parametrize(
    "change,guard,end",
    [
        (MOVE_DESK, _leaf("target.working_units", "eq", 99), None),
        (CORNER_PARTITION, _leaf("zone.occupancy", "gt", 100_000), STOP),
    ],
)
def test_a_false_guard_on_a_floor_plan_change_suppresses_it(main_line, change, guard, end):
    outcomes = _outcomes(main_line, *change, guard, end=end)

    assert {status for _, status, _ in outcomes} == {"suppressed"}
    assert outcomes[0][:2] == (START, "suppressed")
    assert outcomes[0][2] == "EffectProgram guard evaluated false"


def test_eq_compares_numbers_by_value(main_line):
    if main_line.seek("main", START).world.facilities["medical_desk"].queue_agent_ids:
        pytest.fail("the medical desk queue must be empty at the start")
    guard = _leaf("target.queue_length", "eq", 0.0)

    outcomes = _outcomes(
        main_line,
        "set_facility_availability",
        "facility",
        "medical_desk",
        {"working_units": 0},
        guard,
    )

    assert outcomes[0][:2] == (START, "applied"), outcomes[0]


def test_a_guarded_absence_with_an_end_brings_the_member_back(main_line):
    """#89 (元 #310): the restoration used to inherit the absence's guard and stop the Run.

    The guard decides whether the absence starts. The restoration ends the absence that
    started, as an expiry ends any other effect, so it carries no guard.
    """
    guard = _leaf("staff.available", "eq", True)
    before = _kernel_runs("staff_unavailable")

    records = _effect_records(
        main_line, "set_staff_availability", "staff", "staff_003", {"available": False}, guard
    )

    assert _phases(records) == [
        ("staff_unavailable", "start", True, "applied"),
        ("staff_restored", "derived", False, "applied"),
    ]
    assert [record.realized_minute for record in records] == [START, STOP]
    assert _kernel_runs("staff_unavailable") - before == int(native.available())


def _kernel_runs(label):
    """How often native/src/rules/handlers.rs's `label` applied an event in this process.

    Its error on a broken world makes the Python rule run instead, so a result alone doesn't show
    that the kernel builds the same events.
    """
    if not native.available():
        return 0
    counts, _ = native.rules("handlers").stats()
    return counts.get(label, 0)


def _phases(records):
    """Each record's event type, marker phase, whether the marker has a guard, and status."""
    return [
        (
            record.event.event_type,
            record.event.provenance["effect_program_v1"]["phase"],
            "guard" in record.event.provenance["effect_program_v1"],
            record.status.value,
        )
        for record in records
    ]


def test_a_guarded_delay_moves_the_delivery_and_delivers_it(operations_line):
    """#89 (元 #310): the delayed delivery used to inherit the delay's guard."""
    world = operations_line.seek("main", START).world
    (shipment_id,) = world.model_state["operational_events_v1"]["shipments"]
    before = _kernel_runs("delivery_delay")

    records = _effect_records(
        operations_line,
        "modify_supply_schedule",
        "shipment",
        shipment_id,
        {"shipment_id": shipment_id, "to_minute": STOP},
        _leaf("simulation.minute", "gte", 0),
        end=None,
    )

    assert _phases(records) == [
        ("delivery_delayed", "start", True, "applied"),
        ("resource_delivery", "derived", False, "applied"),
    ]
    assert [record.realized_minute for record in records] == [START, STOP]
    assert _kernel_runs("delivery_delay") - before == int(native.available())


def test_a_guarded_arrival_on_foot_completes_its_approach(operations_line):
    """#89 (元 #310): the approach's completion used to inherit the arrival's guard."""
    records = _effect_records(
        operations_line,
        "add_evacuees",
        "zone",
        "corridor",
        {"count": 1, "approach_route_id": "arrival-to-corridor"},
        _leaf("zone.occupancy", "gte", 0),
        end=None,
    )

    assert _phases(records) == [
        ("effect_program_population", "start", True, "applied"),
        ("population_approach_complete", "derived", False, "applied"),
    ]
