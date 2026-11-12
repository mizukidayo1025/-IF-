"""An operator's staff order, and a worker unavailable mid-task: the Python rules' results in Rust.

`order_staff_operation` and its checks (`validate_staff_operation_order`) run in
rules/staff_order.rs for an EffectProgram's `staff_task_order`: each failed check has the Python
rule's text, raised before any change, and an order that passes writes what the Python rule
writes. `EffectProgramStructureHandler` has no Python rule left (`kernel_only`): its outcomes are
compared with the class and text that rule gave, written down from it, and its order with the
order the rule made on a copy of the world. The arrival and the completion of a task whose worker
became unavailable stop the operation and then are suppressed, as the Python rules do
(docs/rust-port/lanes/l8-texts-events.md).

The handler's own refusals of an order, a stale order, and both stops of a worker who became
unavailable are checked in the core (`native/core/tests/staff_order_texts.rs`, #225). Here stay the
comparisons with `order_staff_operation`, which is still Python, the contract's text, and one test
through each handler's entry.
"""

from __future__ import annotations

import copy
import sys
from pathlib import Path
from typing import Any

import pytest
from support.world import NATIVE_OUTCOME, kernel_says_python_s_text

from sheltersim import native
from sheltersim.core.world import WorldState
from sheltersim.domain.staff_operations import interrupt_staff_operation, order_staff_operation
from sheltersim.events.effects import (
    _PARAMETER_MODELS,
    DEFAULT_PRIMITIVE_REGISTRY,
    EffectProgram,
    EffectProgramStructureHandler,
    SupportMode,
    TargetSelector,
    TimedAction,
    schedule_effect_program,
)
from sheltersim.events.handlers import (
    StaffOperationArrivalHandler,
    StaffOperationCompletionHandler,
)
from sheltersim.events.models import EventApplicationError, EventSuppressed


def _gate() -> Any:
    root = str(Path(__file__).resolve().parents[2] / "benchmarks")
    if root not in sys.path:
        sys.path.insert(0, root)
    import gate_scenarios

    return gate_scenarios


@pytest.fixture(scope="module")
def scenario() -> Any:
    """Staff_001 (zone `staff`) has support, information and intake; the others meals and water."""
    return _gate().generated(evacuees=20, minutes=120)


@pytest.fixture
def world(scenario: Any) -> WorldState:
    return WorldState.from_scenario(scenario)


def _copy(world: WorldState) -> WorldState:
    """A copy, as the test changed it: `from_dict` would check what a test took away."""
    return copy.deepcopy(world)


def _text(world: WorldState) -> str:
    return world._canonical_text(world._canonical_data())


def _order(**changes: Any) -> dict[str, Any]:
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


def _ordered_both_ways(world: WorldState, order: dict[str, Any]) -> str | None:
    """Rust and Python on copies: the same text before any change, or the same task and world."""
    by_python, by_rust = _copy(world), _copy(world)
    before = _text(by_rust)
    try:
        expected: Any = order_staff_operation(by_python, **order)
    except EventApplicationError as said:
        with pytest.raises(EventApplicationError) as made:
            native.rules("staff_order").order(by_rust, **order)
        assert getattr(made.value, NATIVE_OUTCOME, False), f"not Rust's own: {made.value}"
        assert str(made.value) == str(said)
        assert _text(by_rust) == before
        return str(said)
    assert native.rules("staff_order").order(by_rust, **order) == expected
    assert _text(by_rust) == _text(by_python)
    return None


# ---- The checks, in the Python rule's order ---------------------------------------------------


def test_an_order_without_its_state_is_python_s_text(world: WorldState) -> None:
    del world.model_state["shared_operations_v1"]
    assert (
        _ordered_both_ways(world, _order())
        == "staff task order requires staff and shared operation state"
    )


def test_an_order_for_unknown_staff_is_python_s_text(world: WorldState) -> None:
    assert (
        _ordered_both_ways(world, _order(staff_id="nobody"))
        == "staff task order references unknown staff"
    )


def test_an_order_for_a_busy_or_unavailable_worker_is_python_s_text(world: WorldState) -> None:
    text = "staff task order requires an idle, available, on-shift worker"
    busy = _copy(world)
    order_staff_operation(busy, **_order())
    assert _ordered_both_ways(busy, _order(causation_event_id="order-2")) == text
    away = _copy(world)
    away.model_state["operational_events_v1"]["staff_status"]["staff_001"]["available"] = False
    assert _ordered_both_ways(away, _order()) == text


def test_an_order_of_an_unknown_task_kind_is_python_s_text(world: WorldState) -> None:
    assert (
        _ordered_both_ways(world, _order(task_kind="juggling"))
        == "staff task order uses an unregistered task kind"
    )


def test_an_order_for_a_worker_without_the_skill_names_who_has_it(world: WorldState) -> None:
    said = _ordered_both_ways(world, _order(staff_id="staff_002"))
    assert said is not None and said.startswith("staff_lacks_task_skill: staff_002")
    assert "staff_001" in said


@pytest.mark.parametrize("kind", ["cleaning", "maintenance"])
def test_an_order_no_one_is_qualified_for_says_so(world: WorldState, kind: str) -> None:
    target = "toilet_01" if kind == "maintenance" else None
    said = _ordered_both_ways(
        world, _order(task_kind=kind, target_zone_id="toilet", target_id=target)
    )
    assert said is not None and said.startswith("no_qualified_staff: ")


def test_an_order_to_an_unknown_zone_is_python_s_text(world: WorldState) -> None:
    assert (
        _ordered_both_ways(world, _order(target_zone_id="nowhere"))
        == "staff task order references an unknown target zone"
    )


def _repairer(world: WorldState) -> WorldState:
    world.staff["staff_002"].skills = ("repair", *world.staff["staff_002"].skills)
    return world


@pytest.mark.parametrize(
    ("target", "zone"), [(None, "toilet"), ("no-such-facility", "toilet"), ("toilet_01", "food")]
)
def test_a_maintenance_order_off_its_facility_is_python_s_text(
    world: WorldState, target: str | None, zone: str
) -> None:
    order = _order(
        staff_id="staff_002", task_kind="maintenance", target_zone_id=zone, target_id=target
    )
    assert (
        _ordered_both_ways(_repairer(world), order)
        == "maintenance order must target a facility in its selected zone"
    )


def test_a_maintenance_order_with_nothing_to_repair_is_python_s_text(world: WorldState) -> None:
    order = _order(
        staff_id="staff_002",
        task_kind="maintenance",
        target_zone_id="toilet",
        target_id="toilet_01",
    )
    assert (
        _ordered_both_ways(_repairer(world), order)
        == "maintenance order has no currently repairable facility unit"
    )


def test_a_maintenance_order_is_the_same_in_both(world: WorldState) -> None:
    world = _repairer(world)
    world.facilities["toilet_01"].working_units = 0
    if "infrastructure_v1" in world.model_state:
        world.model_state["infrastructure_v1"]["base_working_units"]["toilet_01"] = 0
    order = _order(
        staff_id="staff_002",
        task_kind="maintenance",
        target_zone_id="toilet",
        target_id="toilet_01",
    )
    assert _ordered_both_ways(world, order) is None


def test_only_a_maintenance_order_takes_a_facility(world: WorldState) -> None:
    assert (
        _ordered_both_ways(world, _order(target_id="toilet_01"))
        == "only maintenance orders accept a target facility"
    )


def test_a_support_order_where_no_one_is_is_python_s_text(world: WorldState) -> None:
    assert (
        _ordered_both_ways(world, _order(target_zone_id="quiet"))
        == "staff support order requires residents in its target zone"
    )


def test_outreach_without_the_body_state_is_python_s_text(world: WorldState) -> None:
    del world.model_state["body_state_v1"]
    assert (
        _ordered_both_ways(world, _order(task_kind="information_outreach"))
        == "information outreach requires the body-state policy"
    )


def test_intake_without_arrivals_is_python_s_text(world: WorldState) -> None:
    assert (
        _ordered_both_ways(world, _order(task_kind="intake", target_zone_id="intake"))
        == "intake order requires pending arrivals"
    )


def test_an_order_with_no_route_is_python_s_text(world: WorldState) -> None:
    world.zone_edges["route_corridor_sleeping"].closed = True
    assert _ordered_both_ways(world, _order()) == "staff task order has no reachable route"


def test_an_order_past_the_run_is_python_s_text(world: WorldState) -> None:
    world.duration_minutes = world.minute_index + 3
    assert (
        _ordered_both_ways(world, _order())
        == "staff task order would finish beyond the Run boundary"
    )


# ---- What an order writes ---------------------------------------------------------------------


@pytest.mark.parametrize(
    "order",
    [
        _order(),
        _order(source_id="SRC-ORDER", reason="さらに巡回"),
        _order(task_kind="information_outreach"),
    ],
)
def test_an_order_writes_what_the_python_rule_writes(
    world: WorldState, order: dict[str, Any]
) -> None:
    assert _ordered_both_ways(world, order) is None


def test_an_order_without_a_walk_writes_what_the_python_rule_writes(world: WorldState) -> None:
    world.staff["staff_001"].zone_id = "sleeping"
    assert _ordered_both_ways(world, _order()) is None


def test_a_policy_selected_formula_gives_the_order_its_source(world: WorldState) -> None:
    world.model_configs.setdefault("policy_registry", {})["staff_operations"] = (
        "staff-travel-first-v1"
    )
    assert _ordered_both_ways(world, _order()) is None


# ---- Through the EffectProgram's handler --------------------------------------------------------


def _ordered_program(world: WorldState) -> Any:
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
    from sheltersim.events.models import ScheduledEvent

    document = event.to_dict()
    payload = {**document["payload"], **changes.pop("payload", {})}
    marker = {**document["provenance"]["effect_program_v1"], **changes.pop("marker", {})}
    provenance = {**document["provenance"], "effect_program_v1": marker}
    return ScheduledEvent(**{**document, **changes, "payload": payload, "provenance": provenance})


def test_an_order_whose_parameters_the_contract_turns_down_is_python_s_text(
    world: WorldState,
) -> None:
    event = _ordered_program(world)
    parameters = {**event.payload["parameters"], "task_kind": "juggling"}
    changed = _changed(event, payload={"parameters": parameters})
    # The contract's own text, as the rule wrapped its error.
    with pytest.raises((TypeError, ValueError)) as contract:
        _PARAMETER_MODELS["staff_task_order"].model_validate(changed.payload.get("parameters"))
    text = f"staff task order parameters are invalid: {contract.value}"
    said = kernel_says_python_s_text(
        EffectProgramStructureHandler(), world, changed, (EventApplicationError, text)
    )
    assert said == ("EventApplicationError", text)


def test_an_order_through_the_handler_is_the_order_of_its_parameters(world: WorldState) -> None:
    """The handler's rule, after its checks and the guard, ordered the operation on a copy of the
    world with the event's parameters, its ID as the cause and its source."""
    event = _ordered_program(world)
    expected, by_rust = _copy(world), _copy(world)
    parameters = _PARAMETER_MODELS["staff_task_order"].model_validate(event.payload["parameters"])
    order_staff_operation(
        expected,
        staff_id=event.payload["staff_id"],
        task_kind=parameters.task_kind,
        target_zone_id=parameters.target_zone_id,
        target_id=parameters.target_id,
        reason=parameters.reason,
        causation_event_id=event.event_id,
        source_id=event.source_id,
    )
    EffectProgramStructureHandler().apply(by_rust, event)
    assert by_rust.staff["staff_001"].active_task_id is not None
    assert _text(by_rust) == _text(expected)


# ---- A worker who became unavailable ------------------------------------------------------------


def _canonical(world: WorldState) -> str:
    return world._canonical_text(world._canonical_data())


def _pending(world: WorldState, event_type: str) -> Any:
    return next(event for event in world.event_queue.pending() if event.event_type == event_type)


def _stops_and_suppresses(handler: Any, world: WorldState, event: Any, text: str) -> None:
    """Rust's own outcome, after the stop the Python rule made, here on a copy made before.

    The rule (`arrive_staff_operation`, `complete_staff_operation`, gone in lane L10) stopped the
    operation with `interrupt_staff_operation` and then suppressed the event with `text`.
    """
    twin = copy.deepcopy(world)
    before = _canonical(world)
    with pytest.raises(EventSuppressed) as made:
        handler.apply(world, event)
    assert getattr(made.value, NATIVE_OUTCOME, False), f"the kernel refused: {made.value}"
    assert str(made.value) == text
    staff_id = event.payload["staff_id"]
    assert interrupt_staff_operation(twin, staff_id, event, current_event_id=event.event_id)
    assert _canonical(world) != before
    assert _canonical(world) == _canonical(twin)
    record = world.model_state["staff_operations_v1"]["staff"]["staff_001"]
    assert record["phase"] == "idle"
    assert world.staff["staff_001"].active_task_id is None


def _away(world: WorldState) -> None:
    world.model_state["operational_events_v1"]["staff_status"]["staff_001"]["available"] = False


def test_a_completion_of_a_worker_who_became_unavailable_stops_the_task(world: WorldState) -> None:
    """The entries of both handlers; the core checks the arrival's stop too."""
    order_staff_operation(world, **_order())
    StaffOperationArrivalHandler().apply(world, _pending(world, "staff_operation_arrive"))
    event = _pending(world, "staff_operation_complete")
    _away(world)
    _stops_and_suppresses(
        StaffOperationCompletionHandler(),
        world,
        event,
        "staff became unavailable before completing the assigned task",
    )
