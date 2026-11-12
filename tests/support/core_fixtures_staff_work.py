"""The inputs of the core's tests of staff shifts, fatigue and orders (Issue #225, lane M11).

`native/core/tests/staff_dynamics_rules.rs` and `staff_order_texts.rs` check the rules that
`integration/test_staff_dynamics_row.py` and `test_staff_operation_texts.py` checked through the
PyO3 entries. This module builds the same worlds and inputs and exports them
(`core_fixtures.export_world`):

- `staff_dynamics.json`: `base`, the small world of `test_staff_dynamics_row.py` (three staff
  members and the namespace `create_staff_dynamics_state` made for an empty config), and
  (`availability`) the staff operations documents of the test of their shapes. Only the world and
  its slots are written: these rules read no section. The tests of that file that draw hundreds of
  random worlds and questions with `random.Random` stay in pytest (decision 5 of #225).
- `staff_order_texts.json`: `generated`, the gate's generated Scenario (20 evacuees, 120 minutes)
  at minute 0 with the Run's policies, and its variants: an operator's order scheduled as an
  EffectProgram (`ordered`, and `ordered_stale` near the Run's end), and an order under way whose
  worker became unavailable before the arrival (`travelling`) or before the completion
  (`working`), with the worlds the Python rule left when it stopped them (`*_stopped`). The cases
  (`cases`, as `core_texts.Fixture` writes them) give each event to its handler with the outcome the
  Python rule raised, and for a stop the world it left.
"""

from __future__ import annotations

import copy
import json
from typing import Any

from support.core_fixtures import export_world
from support.core_texts import Fixture, _gate, handler_of

# ---- staff_dynamics.json -----------------------------------------------------------------------

KEY = "staff_dynamics_v1"
IDS = ("S01", "S02", "S03")


def staff_world(**raw_config: Any) -> Any:
    """`_world` of test_staff_dynamics_row.py: three staff members and the namespace."""
    from sheltersim.core.world import WorldState
    from sheltersim.domain.models import StaffState
    from sheltersim.domain.staff_dynamics import create_staff_dynamics_state

    world = WorldState(
        "s",
        1,
        500,
        staff={staff_id: StaffState(staff_id, "z", ("support",)) for staff_id in IDS},
        model_configs={"staff_dynamics": raw_config},
    )
    world.model_state[KEY] = create_staff_dynamics_state(world)
    return world


def _idle_operation(phase: str) -> dict[str, Any]:
    """A record of `staff_operations_v1` in this phase ("work" is the working phase)."""
    from sheltersim.domain.staff_operations import _idle_record

    return {**_idle_record(), "phase": "working" if phase == "work" else phase}


def operations(staff: dict[str, Any]) -> dict[str, Any]:
    """A `staff_operations_v1` document of the row's shape with these records."""
    return {
        "schema_version": 1,
        "policy_version": "autonomous-routed-staff-v1",
        "source_id": "ASM-STAFF-OPERATIONS-V1",
        "config": {},
        "zones": {},
        "staff": staff,
        "history": [],
    }


def _small(world: Any) -> dict[str, Any]:
    entry = export_world(world, validate=False)
    return json.loads(json.dumps({"world": entry["world"], "slots": entry["slots"]}))


def staff_dynamics_fixture() -> dict[str, Any]:
    """`staff_dynamics.json`: the small world, and the staff operations documents of the test of
    their shapes with the availability the Python rule gave for each."""
    availability = [
        [operations({"S01": _idle_operation("idle")}), True],
        [operations({"S01": _idle_operation("travel")}), False],
        [operations({"S01": _idle_operation("working")}), False],
        [operations({}), True],
        [None, True],
    ]
    return {
        "worlds": {"base": _small(staff_world())},
        "availability": json.loads(json.dumps(availability)),
    }


# ---- staff_order_texts.json --------------------------------------------------------------------


def _controller(world: Any) -> Any:
    from sheltersim.experiments.metrics import MetricsRecorder
    from sheltersim.experiments.runner import OfflineSimulationController
    from sheltersim.experiments.timeline import _MetricSink

    return OfflineSimulationController(world, _MetricSink(MetricsRecorder()))


def order(**changes: Any) -> dict[str, Any]:
    """`_order` of test_staff_operation_texts.py."""
    return {
        "staff_id": "staff_001",
        "task_kind": "patrol",
        "target_zone_id": "sleeping",
        "target_id": None,
        "reason": "a test order",
        "causation_event_id": "order-1",
        "source_id": None,
        **changes,
    }


def ordered_program(world: Any) -> Any:
    """`_ordered_program` of test_staff_operation_texts.py: the order scheduled as an EffectProgram;
    its structure event."""
    from sheltersim.events.effects import (
        DEFAULT_PRIMITIVE_REGISTRY,
        EffectProgram,
        SupportMode,
        TargetSelector,
        TimedAction,
        schedule_effect_program,
    )

    spec = DEFAULT_PRIMITIVE_REGISTRY.get("staff_task_order")
    action = TimedAction(
        effect_id="effect-1",
        op="staff_task_order",
        target_selector=TargetSelector(kind="staff", target_id="staff_001"),
        start_minute=world.minute_index,
        end_minute=None,
        parameters={"task_kind": "patrol", "target_zone_id": "sleeping", "reason": "a test order"},
        units=sorted(spec.units)[0],
        source_id="TEST-STAFF-ORDER",
        assumption_ids=[],
        policy_dependencies=sorted(spec.required_policies),
        reversible=False,
    )
    schedule_effect_program(
        world,
        EffectProgram(
            program_id="program-1",
            narrative="a test order",
            source_text="a test order",
            simulation_start_minute=world.minute_index,
            effects=[action],
            assumptions=[],
            support_mode=SupportMode.NATIVE,
            compiler_provenance={"compiler_kind": "offline_rule", "compiler_version": "test-v1"},
        ),
    )
    return next(
        event
        for event in world.event_queue.pending()
        if event.event_type == "effect_program_structure"
    )


def _changed(event: Any, **changes: Any) -> Any:
    """`_changed` of test_staff_operation_texts.py."""
    from sheltersim.events.models import ScheduledEvent

    document = event.to_dict()
    payload = {**document["payload"], **changes.pop("payload", {})}
    marker = {**document["provenance"]["effect_program_v1"], **changes.pop("marker", {})}
    provenance = {**document["provenance"], "effect_program_v1": marker}
    return ScheduledEvent(**{**document, **changes, "payload": payload, "provenance": provenance})


# The changes of an order's event `EffectProgramStructureHandler` turns down, and the class and text
# of what the Python rule raised (`test_an_order_the_handler_turns_down_is_python_s_text`).
TURNED_DOWN: list[tuple[dict[str, Any], tuple[str, str]]] = [
    (
        {"payload": {"modifier_id": "another"}},
        ("Failed", "staff task order does not reference a current approved program"),
    ),
    (
        {"marker": {"phase": "end"}},
        ("Failed", "staff task order does not reference a current approved program"),
    ),
    (
        {"payload": {"staff_id": 7}},
        ("Failed", "staff task order does not reference a current approved program"),
    ),
    (
        {"due_minute": 1},
        ("Failed", "staff task order does not reference a current approved program"),
    ),
    (
        {"marker": {"guard": {"operator": "lt", "field": "simulation.minute", "value": 0}}},
        ("Suppressed", "EffectProgram guard evaluated false"),
    ),
    (
        {"marker": {"guard": {"operator": "eq", "field": "target.capacity", "value": 1}}},
        ("Failed", "guard field 'target.capacity' has no facility target"),
    ),
]


def _away(world: Any) -> None:
    world.model_state["operational_events_v1"]["staff_status"]["staff_001"]["available"] = False


def _pending(world: Any, event_type: str) -> Any:
    return next(event for event in world.event_queue.pending() if event.event_type == event_type)


def _stopped(world: Any, event: Any) -> Any:
    """The world the Python rule left: the operation stopped (`interrupt_staff_operation`), then
    the event suppressed."""
    from sheltersim.domain.staff_operations import interrupt_staff_operation

    twin = copy.deepcopy(world)
    staff_id = event.payload["staff_id"]
    assert interrupt_staff_operation(twin, staff_id, event, current_event_id=event.event_id)
    return twin


def staff_order_texts_fixture() -> dict[str, Any]:
    """`staff_order_texts.json`: the gate's world, the order's worlds and the handlers' cases."""
    from sheltersim.core.world import WorldState
    from sheltersim.domain.staff_operations import order_staff_operation
    from sheltersim.events.effects import EffectProgramStructureHandler
    from sheltersim.events.handlers import (
        StaffOperationArrivalHandler,
        StaffOperationCompletionHandler,
    )

    fixture = Fixture()
    base = WorldState.from_scenario(_gate().generated(evacuees=20, minutes=120))
    # The Run's policies, graph and neighbors are the same for every world below; a controller
    # checks the world it is made for, which a worker who became unavailable mid-task fails.
    controller = _controller(base)
    fixture.world("generated", base, controller, full=True)
    structure = handler_of(EffectProgramStructureHandler())

    ordered = copy.deepcopy(base)
    event = ordered_program(ordered)
    fixture.world("ordered", ordered, controller)
    for change, (kind, text) in TURNED_DOWN:
        turned = _changed(event, **copy.deepcopy(change))
        if "due_minute" in change:
            turned = _changed(turned, due_minute=ordered.minute_index + change["due_minute"])
        fixture._add("turned_down", "ordered", structure, turned, outcome=[kind, text])

    stale = copy.deepcopy(ordered)
    stale.duration_minutes = stale.minute_index + 3
    fixture.world("ordered_stale", stale, controller)
    text = "staff task order would finish beyond the Run boundary"
    fixture._add("stale", "ordered_stale", structure, event, outcome=["Failed", text])

    travelling = copy.deepcopy(base)
    order_staff_operation(travelling, **order())
    arrival = _pending(travelling, "staff_operation_arrive")
    working = copy.deepcopy(travelling)
    StaffOperationArrivalHandler().apply(working, arrival)
    completion = _pending(working, "staff_operation_complete")
    for name, world, event, handler, text in (
        (
            "travelling",
            travelling,
            arrival,
            StaffOperationArrivalHandler(),
            "staff became unavailable before reaching the assigned task",
        ),
        (
            "working",
            working,
            completion,
            StaffOperationCompletionHandler(),
            "staff became unavailable before completing the assigned task",
        ),
    ):
        _away(world)
        fixture.world(name, world, controller)
        stopped = _stopped(world, event)
        fixture.world(f"{name}_stopped", stopped, controller)
        fixture._add(
            "unavailable",
            name,
            handler_of(handler),
            event,
            outcome=["Suppressed", text],
            after=f"{name}_stopped",
        )
    document = fixture.document()
    del document["canonical_sha256"]
    return document
