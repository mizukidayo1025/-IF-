"""The texts of the water and meal service's outcomes, and of a request its queue turns down:
one test of each Python entry of their rules.

The rules' texts, case by case, are tested in the core without Python
(native/core/tests/resource_service_texts.rs, Issue #225). These tests keep each entry of the PyO3
crate reaching the kernel and saying the Python rule's text: `ResourceServiceManager.complete`,
`ResourceServiceCancellationHandler.apply`, `interrupt_for_staff`, `request`, the toilet
completion's and the ledger handler's wraps, and the propose step's request at once (whose inputs
the core reads inside its tick). The kernel must raise the class and text the Python rule raised
there itself (not a refusal) and leave the world as it was, or, for a request, write what the
Python rule wrote (docs/rust-port/lanes/l8-texts-events.md). These rules have no Python rule left
(lane L10, `kernel_only`): the texts below were the Python rules'.
"""

from __future__ import annotations

import copy
import sys
from types import SimpleNamespace
from typing import Any

import pytest
from support.world import NATIVE_OUTCOME, world_digest

from sheltersim import native
from sheltersim.core.engine import TickEngine
from sheltersim.core.world import WorldState
from sheltersim.domain.models import Activity
from sheltersim.domain.resource_service import ResourceServiceManager
from sheltersim.events.handlers import (
    ResourceLedgerHandler,
    ResourceServiceCancellationHandler,
    ToiletServiceCompletionHandler,
)
from sheltersim.events.models import (
    EventApplicationError,
    EventKind,
    EventSuppressed,
    ScheduledEvent,
)
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.timeline import _MetricSink
from sheltersim.policies.behavior import ActionKind

SERVICE = "resource_service_v1"
LEDGER = "resource_ledger_v1"
OUTCOMES = (EventSuppressed, EventApplicationError)


def _bytes(world: WorldState) -> str:
    return world._canonical_text(world._canonical_data())


def _says(
    world: WorldState, call: Any, rule: Any, expected: type, text: str, change: Any = None
) -> None:
    """`call(world)` raises `expected` with `text` itself, as `rule(copy)` does.

    `call` runs the kernel (through its Python entry); `rule` is the Python rule alone. The world
    `call` ran on must end as the copy the rule ran on does: unchanged for an event's outcome. A
    rule with no Python rule left (`kernel_only`, lane L10) passes None: `text` was its rule's, and
    `change` makes on the copy what the rule wrote before it raised, if anything.
    """
    twin = copy.deepcopy(world)
    if rule is not None:
        with pytest.raises(expected) as said:
            rule(twin)
        assert type(said.value) is expected
        assert str(said.value) == text
    elif change is not None:
        change(twin)
    before = _bytes(world)
    with pytest.raises(expected) as made:
        call(world)
    assert getattr(made.value, NATIVE_OUTCOME, False), f"the kernel refused: {made.value}"
    assert type(made.value) is expected
    assert str(made.value) == text
    assert _bytes(world) == _bytes(twin)
    if expected is not ValueError:
        assert _bytes(world) == before


def _event(event_type: str, payload: dict[str, Any], **fields: Any) -> ScheduledEvent:
    return ScheduledEvent(
        event_id=fields.pop("event_id", f"test-{event_type.replace('_', '-')}"),
        kind=fields.pop("kind", EventKind.EXOGENOUS),
        due_minute=fields.pop("due_minute", 0),
        priority=0,
        sequence=fields.pop("sequence", 900_001),
        event_type=event_type,
        payload=payload,
        source_id=fields.pop("source_id", "SRC-TEST"),
        causation_id=None,
        provenance={"source_type": "assumption", "source_id": "SRC-TEST"},
    )


def _started(resource_service_scenario: Any, **overrides: Any) -> tuple[WorldState, ScheduledEvent]:
    """A1 being served water at minute 0: the world and its pending completion."""
    world = WorldState.from_scenario(resource_service_scenario(**overrides))
    manager = ResourceServiceManager(world)
    world.agents["A001"].zone_id = world.facilities["water_station_1"].zone_id
    manager.request("A001", "water_station_1", minute=0)
    started = manager.arbitrate(0)
    assert len(started) == 1
    world.validate()
    return world, started[0]


def _completion(completion: ScheduledEvent, **changes: Any) -> ScheduledEvent:
    document = completion.to_dict()
    payload = {**document["payload"], **changes.pop("payload", {})}
    return ScheduledEvent(**{**document, **changes, "payload": payload})


# ---- complete -----------------------------------------------------------------------------------


def _complete_says(world: WorldState, event: ScheduledEvent, expected: type, text: str) -> None:
    _says(world, lambda state: ResourceServiceManager(state).complete(event), None, expected, text)


def test_a_completion_of_a_cancelled_task_is_suppressed_with_python_s_text(
    resource_service_scenario: Any,
) -> None:
    world, completion = _started(resource_service_scenario)
    world.model_state[SERVICE]["cancelled_task_ids"].append("resource-service-task-gone")
    event = _completion(completion, payload={"task_id": "resource-service-task-gone"})
    _complete_says(
        world,
        event,
        EventSuppressed,
        "resource service task 'resource-service-task-gone' was cancelled",
    )


def _cancel_says(world: WorldState, event: ScheduledEvent, expected: type, text: str) -> None:
    _says(
        world,
        lambda state: ResourceServiceCancellationHandler(ResourceServiceManager(state)).apply(
            state, event
        ),
        None,
        expected,
        text,
    )


def _cancel(payload: dict[str, Any]) -> ScheduledEvent:
    return _event("resource_service_cancel", payload, kind=EventKind.INTERVENTION)


def test_a_cancellation_of_a_cancelled_task_is_suppressed_with_python_s_text(
    resource_service_scenario: Any,
) -> None:
    world, completion = _started(resource_service_scenario)
    task_id = completion.payload["task_id"]
    world.model_state[SERVICE]["cancelled_task_ids"].append(task_id)
    _cancel_says(
        world,
        _cancel({"task_id": task_id, "reason": "facility_stopped"}),
        EventSuppressed,
        f"resource service task {task_id!r} was already cancelled",
    )


# ---- interrupt_for_staff ------------------------------------------------------------------------


def test_an_interruption_of_a_cancelled_task_is_the_cancellation_s_text(
    resource_service_scenario: Any,
) -> None:
    """The cancellation inside raises its own text (the Python rule's had taken a sequence)."""
    world, completion = _started(resource_service_scenario)
    task_id = completion.payload["task_id"]
    staff_id = completion.payload["staff_id"]
    world.model_state[SERVICE]["cancelled_task_ids"].append(task_id)
    cause = _event("staff_unavailable", {"staff_id": staff_id, "until_minute": 5})
    text = f"resource service task {task_id!r} was already cancelled"
    before = _bytes(world)
    with pytest.raises(EventSuppressed) as made:
        native.rules("handlers").interrupt_for_staff(
            ResourceServiceManager(world), staff_id, causation_event=cause
        )
    assert getattr(made.value, NATIVE_OUTCOME, False)
    assert str(made.value) == text
    # The kernel decides before it writes: in the staff handler both run on a copy it drops.
    assert _bytes(world) == before


# ---- A request the queue turns down ------------------------------------------------------------


def _request_says(
    world: WorldState,
    agent_id: Any,
    facility_id: Any,
    text: str,
    change: Any = None,
    **options: Any,
) -> None:
    _says(
        world,
        lambda state: ResourceServiceManager(state).request(agent_id, facility_id, **options),
        None,
        ValueError,
        text,
        change,
    )


def _took_a_task_id(state: WorldState) -> None:
    """The Python rule's write before it found no queue: the counter after the task's ID."""
    state.id_counters["resource_task"] = state.id_counters.get("resource_task", 0) + 1


def _restricted(scenario: Any) -> WorldState:
    """The split water station's two points need a tag nobody has."""
    document = scenario.model_dump(mode="json")
    for point in document["model_configs"]["facility_geometry"]["service_points"]:
        if point["facility_id"] == "water_station_main":
            point["access"] = {
                **point["access"],
                "accessibility": "restricted",
                "required_access_tags": ["staff_key"],
            }
    return WorldState.from_scenario(type(scenario).model_validate(document))


def _place_at(world: WorldState, agent_id: str, facility_id: str) -> None:
    from sheltersim.domain.agent_dynamics import learn_facility
    from sheltersim.domain.movement import MOVEMENT_STATE_KEY

    facility = world.facilities[facility_id]
    world.agents[agent_id].zone_id = facility.zone_id
    state = world.model_state.get(MOVEMENT_STATE_KEY)
    if state is not None:
        state["positions"][agent_id]["zone_id"] = facility.zone_id
    if "agent_dynamics_v1" in world.model_state:
        learn_facility(world, agent_id, facility_id)


def test_no_open_independent_queue_is_python_s_refusal(
    generated_scenario_with_split_queues: Any,
) -> None:
    world = _restricted(generated_scenario_with_split_queues)
    _place_at(world, "A001", "water_station_main")
    assert not world.active_tasks
    _request_says(
        world,
        "A001",
        "water_station_main",
        "no accessible independent resource service queue is available",
        _took_a_task_id,
        validate_state=False,
    )


def _controller(world: WorldState) -> OfflineSimulationController:
    return OfflineSimulationController(world, _MetricSink(MetricsRecorder()))


@pytest.mark.parametrize(
    ("why", "digest"),
    [
        ("queue", "3a9569d6ad20ffc0"),
        ("active", "4a025222d590125b"),
        ("exclusive", "8074303070235945"),
    ],
)
def test_a_request_at_once_the_queue_turns_down_keeps_python_s_text(
    generated_scenario_with_split_queues: Any, why: str, digest: str
) -> None:
    """The propose step records `str(exc)` as the request's detail, and counts the rejection: the
    world of the Python step (`world_digest`, recorded from it on 45a9bc9 before it went)."""
    world = _restricted(generated_scenario_with_split_queues)
    _place_at(world, "A001", "water_station_main")
    texts = {
        "queue": "no accessible independent resource service queue is available",
        "active": "Agent 'A001' cannot request resource service while active",
        "exclusive": "Agent 'A001' already has an exclusive task",
    }
    if why == "active":
        world = WorldState.from_scenario(generated_scenario_with_split_queues)
        _place_at(world, "A001", "water_station_main")
        world.agents["A001"].activity = Activity.SEEKING_HELP
    elif why == "exclusive":
        world = WorldState.from_scenario(generated_scenario_with_split_queues)
        _place_at(world, "A001", "water_station_main")
        world.facilities["toilet_01"].queue_agent_ids.append("A001")
    proposal = SimpleNamespace(
        action=ActionKind.FETCH_WATER,
        agent_id="A001",
        next_decision_at=world.minute_index + 7,
        target_id=None,
    )
    result = native.rules("proposals").apply_effect(_controller(world), proposal)
    assert list(result) == [None, 7, False]
    history = world.model_state[SERVICE]["request_history"]
    assert history[-1]["outcome"] == "rejected"
    assert history[-1]["reason"] == "request_rejected"
    assert history[-1]["detail"] == texts[why]
    assert world.metric_state["resource_service_request_rejected_count"] == 1.0
    assert world_digest(world) == digest


# ---- The wraps: a toilet visit's completion, and the ledger --------------------------------------


@pytest.fixture(scope="module")
def visiting() -> tuple[WorldState, str]:
    """A generated world at the first minute someone is being served at a toilet."""
    from pathlib import Path

    root = str(Path(__file__).resolve().parents[2] / "benchmarks")
    if root not in sys.path:
        sys.path.insert(0, root)
    import gate_scenarios

    world = WorldState.from_scenario(gate_scenarios.generated(evacuees=20, minutes=120))
    engine = TickEngine(world, phase_handler=_controller(world))
    for _ in range(120):
        engine.step()
        served = [key for key, task in world.active_tasks.items() if task.kind == "toilet_service"]
        if served:
            return world, served[0]
    pytest.fail("nobody was served at a toilet")


def test_a_toilet_completion_wraps_python_s_text(visiting: tuple[WorldState, str]) -> None:
    world = copy.deepcopy(visiting[0])
    task = world.active_tasks[visiting[1]]
    water = next(key for key, item in world.facilities.items() if item.kind == "water_station")
    payload = {"facility_id": water, "agent_id": task.agent_id, "task_id": task.id}
    event = _event(
        "toilet_service_complete",
        payload,
        kind=EventKind.COMPLETION,
        due_minute=task.due_minute,
        sequence=world.id_counters["event"] + 50,
    )
    handler = ToiletServiceCompletionHandler()
    _says(
        world,
        lambda state: handler.apply(state, event),
        None,
        EventApplicationError,
        f"{water!r} is not a toilet Facility",
    )


def test_the_ledger_handler_wraps_python_s_text(resource_service_scenario: Any) -> None:
    world, _ = _started(resource_service_scenario)
    payload = {"warehouse_id": "warehouse-h02", "resource_id": "water_ml", "quantity": 900}
    event = _event("resource_loss", payload, sequence=world.id_counters["event"] + 50)
    handler = ResourceLedgerHandler("loss")
    _says(
        world,
        lambda state: handler.apply(state, event),
        None,
        EventApplicationError,
        "resource loss exceeds unreserved remaining inventory",
    )
