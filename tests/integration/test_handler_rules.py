"""The handlers' kernels through their PyO3 entries, and what stays Python's around them.

rules/care_receipts.rs, rules/toilet.rs, rules/support.rs, `after_arrival` of
rules/travel_completion.rs, the seeks of rules/proposals.rs and the dispatch table's new entries
(docs/rust-port/lanes/m6-handlers.md). The rules themselves are checked by the core's tests
(native/core/tests/handler_rules.rs, #225), on the worlds this file built: here each entry goes
through to its kernel once, and the values that only Python holds (a request's minute, the world's
validation, a test's random source, a patched handler or manager) are read as the rule read them.
"""

from __future__ import annotations

import copy
from typing import Any

import pytest
from support.world import kernel_says_python_s_text

from sheltersim import native
from sheltersim.core.engine import TickEngine
from sheltersim.core.rng import CounterRNG
from sheltersim.core.world import WorldState
from sheltersim.domain import care_receipts
from sheltersim.domain.facilities import ToiletServiceManager
from sheltersim.domain.models import ActiveTask, Activity, SupportRequestState, SupportRequestStatus
from sheltersim.events.handlers import (
    AgentTravelCompletionHandler,
    SupportRequestTimeoutHandler,
    ToiletServiceCompletionHandler,
)
from sheltersim.events.models import (
    EventApplicationError,
    EventKind,
    ScheduledEvent,
)
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.phases.dispatch import DispatchPhase
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.timeline import _MetricSink
from sheltersim.policies.behavior import ActionKind


def _controller(world: WorldState) -> OfflineSimulationController:
    return OfflineSimulationController(world, _MetricSink(MetricsRecorder()))


def _run(scenario: Any, minutes: int) -> tuple[WorldState, OfflineSimulationController]:
    world = WorldState.from_scenario(scenario)
    controller = _controller(world)
    engine = TickEngine(world, phase_handler=controller)
    for _ in range(minutes):
        engine.step()
    return world, controller


def _bytes(world: WorldState) -> str:
    return world._canonical_text(world._canonical_data())


def _same_world(world: WorldState, twin: WorldState) -> None:
    assert _bytes(world) == _bytes(twin)
    assert world.id_counters == twin.id_counters
    assert [repr(event) for event in world.event_queue.pending()] == [
        repr(event) for event in twin.event_queue.pending()
    ]
    assert [record.to_dict() for record in world.event_log.records] == [
        record.to_dict() for record in twin.event_log.records
    ]
    assert world._core.entities().verify(world) is None
    for name in ("shared_operations_v1", "body_state_v1"):
        assert world._core.mirror(name).verify()


def _idle_person(world: WorldState) -> str:
    busy = {task.agent_id for task in world.active_tasks.values()}
    journeys = world.model_state["movement_v1"]["journeys"]
    for agent_id in sorted(world.agents):
        agent = world.agents[agent_id]
        queued = any(
            agent_id in facility.queue_agent_ids or agent_id in facility.active_agent_ids
            for facility in world.facilities.values()
        )
        if (
            agent_id not in busy
            and agent_id not in journeys
            and not queued
            and agent.support_request_id is None
            and agent.support_cooldown_until <= world.minute_index
            and agent.activity in (Activity.IDLE, Activity.RESTING, Activity.SLEEPING)
        ):
            return agent_id
    pytest.fail("nobody is idle")


def _toilet(world: WorldState) -> str:
    return next(key for key, item in sorted(world.facilities.items()) if "toilet" in item.kind)


def _manager_on(controller: Any, world: WorldState) -> ToiletServiceManager:
    manager = ToiletServiceManager.__new__(ToiletServiceManager)
    manager.__dict__.update(controller.toilet_service.__dict__)
    manager.world = world
    return manager


# ---- ToiletServiceManager.request ----------------------------------------------------------------


@pytest.mark.parametrize(
    ("minute", "text"),
    [
        ("5", "toilet request minute must be an integer"),
        (True, "toilet request minute must be an integer"),
        (6, "toilet request minute must match WorldState"),
    ],
)
def test_a_request_s_minute_is_the_world_s(generated_scenario: Any, minute: Any, text: str) -> None:
    world, controller = _run(generated_scenario, 5)
    assert world.minute_index == 5
    before = _bytes(world)
    with pytest.raises(ValueError) as made:
        controller.toilet_service.request(
            _idle_person(world), _toilet(world), minute=minute, validate_state=False
        )
    assert (type(made.value), str(made.value)) == (ValueError, text)
    assert _bytes(world) == before


def test_a_request_with_the_world_s_validation_validates_it(
    generated_scenario: Any, monkeypatch: Any
) -> None:
    world, controller = _run(generated_scenario, 5)
    person, toilet = _idle_person(world), _toilet(world)
    calls = []
    validate = WorldState.validate

    def counted(self: Any, *arguments: Any, **options: Any) -> Any:
        calls.append(self)
        return validate(self, *arguments, **options)

    monkeypatch.setattr(WorldState, "validate", counted)
    native.rules("toilet").request(controller.toilet_service, person, toilet, validate_state=True)
    assert calls == [world, world]
    assert world.agents[person].activity.value == "queued"


def test_the_facilities_to_serve_leave_out_only_an_empty_queue(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    manager = controller.toilet_service
    for facility in world.facilities.values():
        facility.queue_agent_ids.clear()
    assert native.rules("toilet").facilities_to_serve(manager) == []
    facility_id = _toilet(world)
    world.facilities[facility_id].queue_agent_ids.append(_idle_person(world))
    assert native.rules("toilet").facilities_to_serve(manager) == [world.facilities[facility_id]]


def _queued(world: WorldState, controller: Any) -> tuple[str, str]:
    agent_id = _idle_person(world)
    facility_id = _toilet(world)
    world.facilities[facility_id].queue_agent_ids.clear()
    world.facilities[facility_id].active_agent_ids.clear()
    native.rules("toilet").request(
        controller.toilet_service, agent_id, facility_id, validate_state=False
    )
    return agent_id, facility_id


def test_another_random_source_is_asked_with_the_head_s_key(generated_scenario: Any) -> None:
    """A KeyedRandom that is not the world's CounterRNG is asked as the rule asked it: `random`
    with the head's key, whose draw must lie in 0..1."""
    world, controller = _run(generated_scenario, 5)
    agent_id, facility_id = _queued(world, controller)
    twin = copy.deepcopy(world)
    manager, minute = controller.toilet_service, world.minute_index
    assert native.rules("toilet").has_room(manager, facility_id) is True
    asked: list[tuple[Any, ...]] = []

    class Other(CounterRNG):
        def random(self, *key: Any) -> float:
            asked.append(key)
            return super().random(*key)

    start = native.rules("toilet").start_head
    event_id = start(manager, facility_id, minute, Other(world.seed))
    key = ("toilet-service", facility_id, agent_id, "minute", minute, manager.policy_version)
    assert asked == [(*key, "duration")]
    # The same draw as the CounterRNG's, so the same service.
    same = start(_manager_on(controller, twin), facility_id, minute, CounterRNG(world.seed))
    assert event_id == same
    _same_world(world, twin)

    class Broken(CounterRNG):
        def random(self, *key: Any) -> float:
            return 1.0

    world, controller = _run(generated_scenario, 5)
    _, facility_id = _queued(world, controller)
    before = _bytes(world)
    with pytest.raises(ValueError) as made:
        start(controller.toilet_service, facility_id, world.minute_index, Broken(1))
    assert str(made.value) == "keyed RNG draw must be between zero and one"
    assert _bytes(world) == before


# ---- ToiletServiceCompletionHandler --------------------------------------------------------------


def _in_service(world: WorldState, due: int) -> ScheduledEvent:
    agent_id = _idle_person(world)
    facility_id = _toilet(world)
    agent = world.agents[agent_id]
    agent.activity = Activity.IN_SERVICE
    agent.target_id = facility_id
    world.facilities[facility_id].active_agent_ids.append(agent_id)
    task_id = "toilet-service-test"
    world.active_tasks[task_id] = ActiveTask(
        id=task_id,
        kind="toilet_service",
        agent_id=agent_id,
        resource_id=facility_id,
        start_minute=world.minute_index,
        due_minute=due,
    )
    return ScheduledEvent(
        event_id="toilet-complete-test",
        kind=EventKind.COMPLETION,
        due_minute=due,
        priority=0,
        sequence=900_000,
        event_type="toilet_service_complete",
        payload={"facility_id": facility_id, "agent_id": agent_id, "task_id": task_id},
        source_id=task_id,
        provenance={"source_type": "assumption", "source_id": "ASM-TEST"},
    )


# ---- care receipts -------------------------------------------------------------------------------


def _open_request(world: WorldState, agent_id: str, *, request_id: str = "support-t1") -> None:
    world.support_requests[request_id] = SupportRequestState(
        id=request_id,
        agent_id=agent_id,
        requested_minute=world.minute_index,
        timeout_minute=world.minute_index + 30,
    )
    world.agents[agent_id].support_request_id = request_id
    world.agents[agent_id].activity = Activity.SEEKING_HELP


def _transition(world: WorldState, *, status: str = "requested", **more: Any) -> dict[str, Any]:
    return {
        "request_id": "support-t1",
        "agent_id": sorted(world.agents)[0],
        "household_id": None,
        "support_kind": "emotional_support",
        "status": status,
        "source_ids": ("ASM-Z", "ASM-A", "ASM-Z"),
        **more,
    }


def test_a_request_id_used_before_is_python_s_error(generated_scenario: Any) -> None:
    world, _ = _run(generated_scenario, 5)
    kernel = native.rules("care_receipts").append_transition
    kernel(world, **_transition(world))
    kernel(world, **_transition(world, status="interrupted"))
    for call in (kernel, care_receipts.append_applied_care_transition):
        with pytest.raises(ValueError) as raised:
            call(world, **_transition(world))
        assert str(raised.value) == "support request IDs cannot be reused across Run history"


# ---- seek_help and seek_information --------------------------------------------------------------


def _proposal(action: ActionKind, agent_id: str, world: WorldState) -> Any:
    class Proposal:
        pass

    proposal = Proposal()
    proposal.action = action
    proposal.agent_id = agent_id
    proposal.next_decision_at = world.minute_index + 7
    proposal.target_id = None
    return proposal


def test_a_seek_s_entry_opens_a_request(generated_scenario: Any) -> None:
    """`_apply_effect`'s entry: a seek opens the person's request."""
    world, controller = _run(generated_scenario, 5)
    agent_id = _idle_person(world)
    proposal = _proposal(ActionKind.SEEK_HELP, agent_id, world)
    result = native.rules("proposals").apply_effect(controller, proposal)
    assert tuple(result) == ("support-9a4dff703809248129a56411", 12, True)
    assert type(result[2]) is bool
    assert world.agents[agent_id].activity is Activity.SEEKING_HELP
    assert result[0] in world.support_requests


# ---- the timeout ---------------------------------------------------------------------------------


def _timeout_event(world: WorldState, agent_id: str, request_id: str = "support-t1") -> Any:
    return ScheduledEvent(
        event_id="support-timeout-test",
        kind=EventKind.COMPLETION,
        due_minute=world.minute_index,
        priority=20,
        sequence=900_001,
        event_type="support_request_timeout",
        payload={"request_id": request_id, "agent_id": agent_id},
        source_id=request_id,
        provenance={"source_type": "assumption", "source_id": "ASM-TEST"},
    )


def test_a_timeout_of_another_person_s_request_is_python_s_error(generated_scenario: Any) -> None:
    world, _ = _run(generated_scenario, 5)
    agent_id = _idle_person(world)
    _open_request(world, agent_id)
    other = next(key for key in sorted(world.agents) if key != agent_id)
    handler = SupportRequestTimeoutHandler(cooldown_minutes=30)
    text = "support request 'support-t1' does not match its Agent"
    expected = (EventApplicationError, text)
    kernel_says_python_s_text(handler, world, _timeout_event(world, other), expected)


# ---- what follows in the dispatch phase ----------------------------------------------------------


def _requested(world: WorldState, agent_id: str) -> None:
    care_receipts.append_applied_care_transition(
        world,
        request_id="support-t1",
        agent_id=agent_id,
        household_id=None,
        support_kind="emotional_support",
        status="requested",
    )


def _last_care(world: WorldState) -> tuple[str, Any, Any]:
    event = world.event_log.records[-1].event
    return event.event_type, event.payload["status"], event.payload["causation_id"]


def test_a_timeout_s_entry_records_the_interruption_of_a_care_request(
    generated_scenario: Any,
) -> None:
    world, controller = _run(generated_scenario, 5)
    agent_id = _idle_person(world)
    _open_request(world, agent_id)
    _requested(world, agent_id)
    world.support_requests["support-t1"].status = SupportRequestStatus.UNRESOLVED
    event = _timeout_event(world, agent_id)
    DispatchPhase._record_request_interrupted(controller, world, event)
    assert _last_care(world) == ("care_support_transition", "interrupted", event.event_id)


def test_a_staff_arrival_s_entry_records_the_start_of_care(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    agent_id = _idle_person(world)
    _open_request(world, agent_id)
    _requested(world, agent_id)
    staff_id = sorted(world.staff)[0]
    request = world.support_requests["support-t1"]
    request.status = SupportRequestStatus.ASSIGNED
    request.assigned_staff_id = staff_id
    request.task_id = "staff-support-test"
    arrival = ScheduledEvent(
        event_id="staff-arrival-test",
        kind=EventKind.COMPLETION,
        due_minute=world.minute_index,
        priority=0,
        sequence=900_003,
        event_type="agent_travel_complete",
        payload={
            "purpose": "staff_support_travel",
            "request_id": "support-t1",
            "staff_id": staff_id,
        },
        source_id="staff-support-test",
        provenance={"source_type": "assumption", "source_id": "ASM-TEST"},
    )
    DispatchPhase._record_staff_support_started(controller, world, arrival)
    assert _last_care(world) == ("care_support_transition", "started", "staff-support-test")


def test_a_patched_handler_of_the_table_goes_to_python(
    monkeypatch: pytest.MonkeyPatch, generated_scenario: Any
) -> None:
    world, controller = _run(generated_scenario, 5)
    calls: list[str] = []

    def apply(self: Any, target: Any, event: Any) -> None:
        calls.append(event.event_id)

    monkeypatch.setattr(ToiletServiceCompletionHandler, "apply", apply)
    event = _in_service(world, world.minute_index)
    world.schedule_event(event)
    controller._run_dispatch_phase(world, world.minute_index)
    assert calls == [event.event_id]


def test_a_manager_s_own_request_is_what_an_arrival_calls(generated_scenario: Any) -> None:
    """A `request` set on the manager itself (a test's) is what the arrival calls, with the rule's
    arguments; its ValueError is the walk's EventApplicationError with the same text."""
    world, controller = _run(generated_scenario, 5)
    world.model_configs.pop("facility_geometry", None)
    handler = AgentTravelCompletionHandler(controller.toilet_service, validate_request_state=False)
    calls: list[tuple[Any, ...]] = []

    def request(*arguments: Any, **options: Any) -> None:
        calls.append((arguments, options))

    controller.toilet_service.request = request  # type: ignore[method-assign]
    person, toilet = _idle_person(world), _toilet(world)
    zone = world.facilities[toilet].zone_id
    arrive = native.rules("travel_completion").after_arrival
    arrive(handler, world, None, person, "toilet_request", toilet, zone, None)
    assert calls == [((person, toilet), {"minute": world.minute_index, "validate_state": False})]

    def refuse(*_arguments: Any, **_options: Any) -> None:
        raise ValueError("the toilet refuses")

    controller.toilet_service.request = refuse  # type: ignore[method-assign]
    with pytest.raises(EventApplicationError) as made:
        arrive(handler, world, None, person, "toilet_request", toilet, zone, None)
    assert str(made.value) == "the toilet refuses"


def test_the_journey_home_s_entry_walks_the_person_home(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    agent_id = _idle_person(world)
    agent = world.agents[agent_id]
    home = agent.profile.initial_zone_id
    agent.zone_id = next(zone for zone in sorted(world.zones) if zone != home)
    DispatchPhase._schedule_return_to_initial_zone(
        controller, world, _timeout_event(world, agent_id)
    )
    assert agent.activity is Activity.MOVING
