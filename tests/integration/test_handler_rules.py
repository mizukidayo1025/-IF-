"""The handlers' kernels against their Python rules, on a world and a copy of it made before.

rules/care_receipts.rs, rules/toilet.rs, rules/support.rs, `after_arrival` of
rules/travel_completion.rs, the seeks of rules/proposals.rs and the dispatch table's new entries:
the branches the gate's runs may not reach and the values that change the result
(docs/rust-port/lanes/m6-handlers.md). A rule with no Python body left is compared with what that
rule gave, recorded from it before it went.
"""

from __future__ import annotations

import copy
import hashlib
from typing import Any

import pytest
from support.world import kernel_says_python_s_text, world_digest

from sheltersim import native
from sheltersim.core.engine import TickEngine
from sheltersim.core.rng import CounterRNG
from sheltersim.core.world import StateInvariantError, WorldState
from sheltersim.domain import care_receipts
from sheltersim.domain.facilities import ToiletRequestReceipt, ToiletServiceManager
from sheltersim.domain.facility_queues import facility_has_independent_queues
from sheltersim.domain.models import ActiveTask, Activity, SupportRequestState, SupportRequestStatus
from sheltersim.events.handlers import (
    AgentTravelCompletionHandler,
    StaffSupportCompletionHandler,
    SupportRequestTimeoutHandler,
    ToiletServiceCompletionHandler,
)
from sheltersim.events.models import (
    EventApplicationError,
    EventKind,
    EventStatus,
    EventSuppressed,
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


def _queue_task_id(facility_id: str, agent_id: str, minute: int) -> str:
    """The deleted rule's ID of a queue task: its SHA-256 of the three, joined by NUL."""
    text = f"{facility_id}\0{agent_id}\0{minute}".encode()
    return "toilet-queue-" + hashlib.sha256(text).hexdigest()[:24]


def test_a_toilet_request_puts_the_person_last_in_the_queue(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    twin = copy.deepcopy(world)
    agent_id = _idle_person(world)
    facility_id = _toilet(world)
    assert not facility_has_independent_queues(world, facility_id)
    minute = world.minute_index
    kernel = native.rules("toilet").request
    receipt = kernel(controller.toilet_service, agent_id, facility_id, validate_state=False)
    # The deleted rule: a queue task of the request's minute, the person last in the facility's
    # list, queued with the facility as target; the receipt has the place.
    task_id = _queue_task_id(facility_id, agent_id, minute)
    place = len(twin.facilities[facility_id].queue_agent_ids) + 1
    assert receipt == ToiletRequestReceipt(agent_id, facility_id, minute, task_id, place)
    twin.facilities[facility_id].queue_agent_ids.append(agent_id)
    twin.active_tasks[task_id] = ActiveTask(
        id=task_id,
        kind="toilet_queue",
        agent_id=agent_id,
        resource_id=facility_id,
        start_minute=minute,
        due_minute=minute,
    )
    twin.agents[agent_id].activity = Activity.QUEUED
    twin.agents[agent_id].target_id = facility_id
    assert world.agents[agent_id].activity is Activity.QUEUED
    _same_world(world, twin)


# What the request refuses, in the rule's order and with its texts: a walk's arrival saves the
# text as its event's detail (`_after_arrival`).
REFUSED_REQUESTS: list[tuple[Any, str]] = [
    (lambda world, agent_id: "nobody", "unknown Agent 'nobody'"),
    (
        lambda world, agent_id: setattr(world.agents[agent_id], "activity", Activity.MOVING),
        "Agent '{agent}' cannot request a toilet while active",
    ),
    (
        lambda world, agent_id: world.facilities[_toilet(world)].queue_agent_ids.append(agent_id),
        "Agent '{agent}' already has an active toilet request",
    ),
    (
        lambda world, agent_id: setattr(world.facilities[_toilet(world)], "kind", "water_station"),
        "'{facility}' is not a toilet Facility",
    ),
]


@pytest.mark.parametrize(("spoil", "text"), REFUSED_REQUESTS)
def test_what_a_toilet_request_refuses_is_python_s_text(
    generated_scenario: Any, spoil: Any, text: str
) -> None:
    world, controller = _run(generated_scenario, 5)
    agent_id = _idle_person(world)
    facility_id = _toilet(world)
    agent_id = spoil(world, agent_id) or agent_id
    said = text.format(agent=agent_id, facility=facility_id)
    before = _bytes(world)
    with pytest.raises(ValueError) as made:
        native.rules("toilet").request(
            controller.toilet_service, agent_id, facility_id, validate_state=False
        )
    assert type(made.value) is ValueError
    assert str(made.value) == said
    assert _bytes(world) == before
    with pytest.raises(ValueError) as made:
        controller.toilet_service.request(agent_id, facility_id, validate_state=False)
    assert str(made.value) == said


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


def test_a_queue_task_id_in_use_is_an_invariant_error(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    agent_id, facility_id = _idle_person(world), _toilet(world)
    task_id = _queue_task_id(facility_id, agent_id, world.minute_index)
    world.active_tasks[task_id] = ActiveTask(
        id=task_id,
        kind="toilet_queue",
        agent_id="somebody",
        resource_id=facility_id,
        start_minute=0,
        due_minute=0,
    )
    before = _bytes(world)
    with pytest.raises(StateInvariantError) as made:
        controller.toilet_service.request(agent_id, facility_id, validate_state=False)
    assert str(made.value) == f"duplicate toilet queue task ID {task_id!r}"
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


def test_the_head_starts_its_service(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    agent_id, facility_id = _queued(world, controller)
    queue_task = next(task for task in world.active_tasks.values() if task.agent_id == agent_id)
    world.minute_index += 3
    minute = world.minute_index
    twin = copy.deepcopy(world)
    rng = CounterRNG(world.seed)
    manager = controller.toilet_service
    assert native.rules("toilet").has_room(manager, facility_id) is True
    event_id = native.rules("toilet").start_head(manager, facility_id, minute, rng)
    # The deleted rule: a uniform duration of whole minutes drawn at the head's key, a service
    # task and its completion of the next sequence, the person in service, and the wait counted.
    key = ("toilet-service", facility_id, agent_id, "minute", minute, manager.policy_version)
    key += ("duration",)
    draw = rng.random(*key)
    span = manager.max_service_minutes - manager.min_service_minutes + 1
    service = min(manager.min_service_minutes + int(draw * span), manager.max_service_minutes)
    sequence = twin.allocate_event_sequence()
    assert event_id == f"toilet-complete-{sequence}"
    task_id = f"toilet-service-{sequence}"
    wait = minute - queue_task.start_minute
    facility = twin.facilities[facility_id]
    facility.queue_agent_ids.pop(0)
    facility.active_agent_ids.append(agent_id)
    del twin.active_tasks[queue_task.id]
    twin.active_tasks[task_id] = ActiveTask(
        id=task_id,
        kind="toilet_service",
        agent_id=agent_id,
        resource_id=facility_id,
        start_minute=minute,
        due_minute=minute + service,
    )
    twin.agents[agent_id].activity = Activity.IN_SERVICE
    twin.agents[agent_id].target_id = facility_id
    twin.schedule_event(
        ScheduledEvent(
            event_id=event_id,
            kind=EventKind.COMPLETION,
            due_minute=minute + service,
            priority=0,
            sequence=sequence,
            event_type="toilet_service_complete",
            payload={"facility_id": facility_id, "agent_id": agent_id, "task_id": task_id},
            source_id=task_id,
            causation_id=queue_task.id,
            provenance={
                "source_type": "assumption",
                "source_id": "ASM-TOILET-SERVICE-DURATION-V1",
                "policy_version": manager.policy_version,
                "duration_distribution": "discrete_uniform_inclusive",
                "minimum_minutes": manager.min_service_minutes,
                "maximum_minutes": manager.max_service_minutes,
                "service_minutes": service,
                "queue_wait_minutes": wait,
                "requested_at": queue_task.start_minute,
                "service_started_at": minute,
                "rng_key": list(key),
                "rng_draw": draw,
            },
        )
    )
    metrics = twin.metric_state
    metrics["toilet_wait_count"] = metrics.get("toilet_wait_count", 0.0) + 1.0
    metrics["toilet_wait_total_minutes"] = metrics.get("toilet_wait_total_minutes", 0.0) + wait
    metrics["toilet_wait_max_minutes"] = max(
        metrics.get("toilet_wait_max_minutes", 0.0), float(wait)
    )
    assert world.agents[agent_id].activity is Activity.IN_SERVICE
    assert world.metric_state["toilet_wait_max_minutes"] >= 3.0
    _same_world(world, twin)


def test_a_head_that_cannot_finish_stays_in_line(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    agent_id, facility_id = _queued(world, controller)
    world.duration_minutes = world.minute_index + 1
    before = _bytes(world)
    rng = CounterRNG(world.seed)
    result = native.rules("toilet").start_head(
        controller.toilet_service, facility_id, world.minute_index, rng
    )
    assert result is None
    assert world.agents[agent_id].activity is Activity.QUEUED
    assert _bytes(world) == before


def test_another_random_source_is_asked_with_the_head_s_key(generated_scenario: Any) -> None:
    """A KeyedRandom that is not the world's CounterRNG is asked as the rule asked it: `random`
    with the head's key, whose draw must lie in 0..1."""
    world, controller = _run(generated_scenario, 5)
    agent_id, facility_id = _queued(world, controller)
    twin = copy.deepcopy(world)
    manager, minute = controller.toilet_service, world.minute_index
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


@pytest.mark.parametrize("hygiene", [0.75, 0.2, 0.1, 0.0])
def test_a_toilet_completion_is_python_s(generated_scenario: Any, hygiene: float) -> None:
    world, _ = _run(generated_scenario, 5)
    event = _in_service(world, world.minute_index)
    agent_id = event.payload["agent_id"]
    world.model_state["body_state_v1"]["agents"][agent_id]["hygiene_need"] = hygiene
    twin = copy.deepcopy(world)
    native.rules("toilet").complete(ToiletServiceCompletionHandler(), world, event)
    # The Python rule (`complete_toilet_service` and the handler, gone in lane L10): the slot, the
    # task, the person and the count; a person not in a queue has no queue record to finish.
    facility_id, task_id = event.payload["facility_id"], event.payload["task_id"]
    twin.facilities[facility_id].active_agent_ids.remove(agent_id)
    del twin.active_tasks[task_id]
    twin.agents[agent_id].activity = Activity.IDLE
    twin.agents[agent_id].target_id = None
    twin.agents[agent_id].needs.toilet_urgency = 0.0
    twin.metric_state["toilet_service_completed_count"] = (
        twin.metric_state.get("toilet_service_completed_count", 0.0) + 1.0
    )
    twin.model_state["body_state_v1"]["agents"][agent_id]["hygiene_need"] = max(0.0, hygiene - 0.2)
    assert world.agents[agent_id].activity is Activity.IDLE
    assert world.agents[agent_id].needs.toilet_urgency == 0.0
    # 0.2 is the model's relief: a change of 1% shows here.
    assert world.model_state["body_state_v1"]["agents"][agent_id]["hygiene_need"] == max(
        0.0, hygiene - 0.2
    )
    _same_world(world, twin)


@pytest.mark.parametrize(("kind", "is_toilet"), [("TOILET_和式", True), ("給水所", False)])
def test_a_facility_kind_beyond_ascii_is_folded_as_python_folds_it(
    generated_scenario: Any, kind: str, is_toilet: bool
) -> None:
    """A Scenario's facility kind is any str: `"toilet" in kind.casefold()`, Python's fold."""
    world, _ = _run(generated_scenario, 5)
    event = _in_service(world, world.minute_index)
    facility_id, agent_id = event.payload["facility_id"], event.payload["agent_id"]
    world.facilities[facility_id].kind = kind
    handler = ToiletServiceCompletionHandler()
    if is_toilet:
        handler.apply(world, event)
        assert world.agents[agent_id].activity is Activity.IDLE
        return
    expected = (EventApplicationError, f"{facility_id!r} is not a toilet Facility")
    kernel_says_python_s_text(handler, world, event, expected)


def test_a_toilet_completion_that_does_not_fit_is_python_s(generated_scenario: Any) -> None:
    world, _ = _run(generated_scenario, 5)
    event = _in_service(world, world.minute_index)
    agent_id = event.payload["agent_id"]
    world.agents[agent_id].activity = Activity.IDLE
    before = _bytes(world)
    # `complete_toilet_service`'s ValueError, as the handler wraps it.
    with pytest.raises(EventApplicationError, match=f"Agent '{agent_id}' is not using Facility"):
        native.rules("toilet").complete(ToiletServiceCompletionHandler(), world, event)
    assert _bytes(world) == before


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


def test_a_care_transition_is_python_s(generated_scenario: Any) -> None:
    world, _ = _run(generated_scenario, 5)
    care_receipts.append_applied_care_transition(world, **_transition(world))
    # What the Python rule made of it (it has none left, lane L10), by the digest of the world.
    assert hashlib.sha256(_bytes(world).encode()).hexdigest()[:16] == "48233b1d7f135f38"
    head = world.model_state["shared_operations_v1"]["support_requests"]["support-t1"]
    assert (head["status"], head["latest_event_id"]) == (
        "requested",
        world.event_log.records[-1].event.event_id,
    )
    last = world.event_log.records[-1]
    assert last.event.event_type == "care_support_transition"
    assert last.detail == "care transition requested recorded after state application"
    # Sorted and without repeats, with the producer's own source.
    assert last.event.payload["source_ids"] == (
        "ASM-A",
        "ASM-CARE-TRANSITION-PRODUCER-V1",
        "ASM-Z",
    )


@pytest.mark.parametrize(
    ("more", "why"),
    [
        ({"support_kind": "unknown"}, "contract"),
        ({"request_id": "bad id"}, "contract"),
        ({"status": "unknown"}, "status"),
    ],
)
def test_what_a_care_transition_refuses_changes_nothing(
    generated_scenario: Any, more: dict[str, Any], why: str
) -> None:
    world, _ = _run(generated_scenario, 5)
    before = _bytes(world)
    counters = dict(world.id_counters)
    with pytest.raises(native.Unsupported, match=why):
        native.rules("care_receipts").append_transition(world, **_transition(world, **more))
    assert _bytes(world) == before
    assert world.id_counters == counters


def test_a_transition_out_of_order_has_the_python_text_and_changes_nothing(
    generated_scenario: Any,
) -> None:
    """The Python rule appended the event and took its sequence first; nothing of it is kept."""
    world, _ = _run(generated_scenario, 5)
    before = _bytes(world)
    counters = dict(world.id_counters)
    with pytest.raises(ValueError) as raised:
        care_receipts.append_applied_care_transition(
            world, **_transition(world, status="completed")
        )
    assert str(raised.value) == "support lifecycle transition is out of order"
    assert _bytes(world) == before
    assert world.id_counters == counters


def test_a_request_id_used_before_is_python_s_error(generated_scenario: Any) -> None:
    world, _ = _run(generated_scenario, 5)
    kernel = native.rules("care_receipts").append_transition
    kernel(world, **_transition(world))
    kernel(world, **_transition(world, status="interrupted"))
    for call in (kernel, care_receipts.append_applied_care_transition):
        with pytest.raises(ValueError) as raised:
            call(world, **_transition(world))
        assert str(raised.value) == "support request IDs cannot be reused across Run history"


def test_without_the_namespace_nothing_happens(generated_scenario: Any) -> None:
    world, _ = _run(generated_scenario, 5)
    del world.model_state["shared_operations_v1"]
    counters = dict(world.id_counters)
    assert native.rules("care_receipts").append_transition(world, **_transition(world)) is None
    assert world.id_counters == counters


@pytest.mark.parametrize(
    ("person", "digest"),
    [
        ({}, "b97278e44a1578bb"),
        ({"source_ids": ["S-B", "S-A", 3]}, "77396644fe0730ff"),
        ({"care_support_needs": ["language_access"]}, "15ff1c01a90eedaa"),
        ({"care_support_needs": ["hygiene_support", "language_access"]}, "1b1b58c7ce93057d"),
        ({"medication_support": True}, "cddd80dc1bf49308"),
        ({"medical_support": True}, "1b05e9d54a04d73b"),
        ({"medical_support": 1}, "76415054a7336834"),
        ("not a person", "a149ba7dc717f136"),
    ],
)
def test_what_a_seek_reads_of_a_person_is_python_s(
    generated_scenario: Any, person: Any, digest: str
) -> None:
    world, controller = _run(generated_scenario, 5)
    agent_id = _idle_person(world)
    configs = world.model_configs.setdefault("population_context", {})
    configs.setdefault("people", {})[agent_id] = person
    proposal = _proposal(ActionKind.SEEK_HELP, agent_id, world)
    _seek(world, controller, proposal, ("support-9a4dff703809248129a56411", 12, True), digest)


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


def _seek(
    world: WorldState, controller: Any, proposal: Any, expected: tuple[Any, ...], digest: str
) -> Any:
    """The kernel on the world: the result and the world (`world_digest`) of the Python step,
    recorded from it on 45a9bc9 before it went (lane L10), pinned on every OS."""
    result = native.rules("proposals").apply_effect(controller, proposal)
    assert tuple(result) == expected
    assert type(result[2]) is bool
    assert world._core.entities().verify(world) is None
    assert world_digest(world) == digest
    return result


@pytest.mark.parametrize(
    ("action", "digest"),
    [
        (ActionKind.SEEK_HELP, "5d20d2e22d7a9ee7"),
        (ActionKind.SEEK_INFORMATION, "9f3389377a05f25c"),
    ],
)
def test_a_seek_is_python_s(generated_scenario: Any, action: ActionKind, digest: str) -> None:
    world, controller = _run(generated_scenario, 5)
    agent_id = _idle_person(world)
    proposal = _proposal(action, agent_id, world)
    result = _seek(
        world, controller, proposal, ("support-9a4dff703809248129a56411", 12, True), digest
    )
    assert world.agents[agent_id].activity is Activity.SEEKING_HELP
    assert result[0] in world.support_requests


def test_a_seek_in_cooldown_counts_as_rejected(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    agent_id = _idle_person(world)
    world.agents[agent_id].support_cooldown_until = world.minute_index + 1
    proposal = _proposal(ActionKind.SEEK_HELP, agent_id, world)
    _seek(world, controller, proposal, (None, 12, False), "f53692cd3b447cc5")
    assert world.metric_state["support_request_rejected_count"] >= 1.0


def test_a_seek_reaches_the_information_rule_above_its_threshold(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    if controller.information_policy is None:
        pytest.skip("this Scenario has no information policy")
    agent_id = _idle_person(world)
    state = world.model_state["information_spread_v1"]
    state["query_pressure"][agent_id] = controller.information_policy.config.query_threshold
    proposal = _proposal(ActionKind.SEEK_HELP, agent_id, world)
    _seek(
        world,
        controller,
        proposal,
        ("support-9a4dff703809248129a56411", 12, True),
        "dbe1888452f64656",
    )


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


def test_a_timeout_is_python_s(generated_scenario: Any) -> None:
    world, _ = _run(generated_scenario, 5)
    agent_id = _idle_person(world)
    _open_request(world, agent_id)
    event = _timeout_event(world, agent_id)
    twin = copy.deepcopy(world)
    handler = SupportRequestTimeoutHandler(cooldown_minutes=30)
    native.rules("support").timeout(handler, world, event)
    # The Python rule (`timeout_support_request`, gone in lane L10): unresolved, with a cooldown.
    minute = twin.minute_index
    pending = twin.support_requests["support-t1"]
    pending.status = SupportRequestStatus.UNRESOLVED
    pending.resolved_minute = minute
    pending.cooldown_until = minute + 30
    person = twin.agents[agent_id]
    person.support_request_id = None
    person.support_cooldown_until = minute + 30
    person.activity = Activity.IDLE
    person.target_id = None
    twin.metric_state["support_unresolved_count"] = (
        twin.metric_state.get("support_unresolved_count", 0.0) + 1.0
    )
    request = world.support_requests["support-t1"]
    assert request.status is SupportRequestStatus.UNRESOLVED
    assert request.cooldown_until == world.minute_index + 30
    assert world.agents[agent_id].activity is Activity.IDLE
    _same_world(world, twin)


def test_a_timeout_of_what_has_left_pending_says_python_s_text(generated_scenario: Any) -> None:
    world, _ = _run(generated_scenario, 5)
    agent_id = _idle_person(world)
    _open_request(world, agent_id)
    world.support_requests["support-t1"].status = SupportRequestStatus.ASSIGNED
    event = _timeout_event(world, agent_id)
    handler = SupportRequestTimeoutHandler(cooldown_minutes=30)
    before = _bytes(world)
    # An assigned request goes on in service: the Python rule's suppression.
    with pytest.raises(EventSuppressed) as by_kernel:
        native.rules("support").timeout(handler, world, event)
    assert str(by_kernel.value) == "support request is no longer pending"
    assert _bytes(world) == before


def test_a_timeout_of_another_person_s_request_is_python_s_error(generated_scenario: Any) -> None:
    world, _ = _run(generated_scenario, 5)
    agent_id = _idle_person(world)
    _open_request(world, agent_id)
    other = next(key for key in sorted(world.agents) if key != agent_id)
    handler = SupportRequestTimeoutHandler(cooldown_minutes=30)
    text = "support request 'support-t1' does not match its Agent"
    expected = (EventApplicationError, text)
    kernel_says_python_s_text(handler, world, _timeout_event(world, other), expected)


# ---- a staff member's support ends ---------------------------------------------------------------


def _assigned(world: WorldState, kind: str) -> tuple[Any, str]:
    agent_id = _idle_person(world)
    _open_request(world, agent_id)
    kernel = native.rules("care_receipts").append_transition
    common = {"request_id": "support-t1", "agent_id": agent_id, "household_id": None}
    common["support_kind"] = kind
    staff_id = sorted(world.staff)[0]
    person = world.staff[staff_id]
    kernel(world, **common, status="requested")
    kernel(world, **common, status="assigned", staff_id=staff_id)
    kernel(world, **common, status="started", staff_id=staff_id)
    request = world.support_requests["support-t1"]
    request.status = SupportRequestStatus.ASSIGNED
    request.assigned_staff_id = staff_id
    request.task_id = "staff-support-test"
    world.agents[agent_id].zone_id = person.zone_id
    world.active_tasks["staff-support-test"] = ActiveTask(
        id="staff-support-test",
        kind="staff_support",
        agent_id=agent_id,
        resource_id=None,
        start_minute=world.minute_index,
        due_minute=world.minute_index,
        staff_id=staff_id,
    )
    person.active_task_id = "staff-support-test"
    person.work_queue[:] = ["staff-support-test"]
    person.busy_until = world.minute_index
    event = ScheduledEvent(
        event_id="staff-support-complete-test",
        kind=EventKind.COMPLETION,
        due_minute=world.minute_index,
        priority=0,
        sequence=900_002,
        event_type="staff_support_complete",
        payload={
            "request_id": "support-t1",
            "agent_id": agent_id,
            "staff_id": staff_id,
            "task_id": "staff-support-test",
            "support_duration_minutes": 10,
        },
        source_id="staff-support-test",
        provenance={"source_type": "assumption", "source_id": "ASM-TEST"},
    )
    return event, agent_id


def _change(entity_kind: str, agent_id: str, path: str, before: float, after: float) -> dict:
    return {
        "entity_id": agent_id,
        "entity_kind": entity_kind,
        "field_path": path,
        "before": before,
        "after": after,
        "unit": "normalized_0_1",
    }


def _resolved(world: WorldState, event: ScheduledEvent, before: dict[str, Any]) -> None:
    """What `complete_support_task` did: the task gone, the staff member free, the request
    resolved with the cooldown, the person idle, and one more resolved request."""
    minute = world.minute_index
    agent_id, staff_id = event.payload["agent_id"], event.payload["staff_id"]
    assert "staff-support-test" not in world.active_tasks
    staff = world.staff[staff_id]
    assert (staff.active_task_id, staff.busy_until, list(staff.work_queue)) == (None, 0, [])
    request = world.support_requests["support-t1"]
    assert request.status is SupportRequestStatus.RESOLVED
    assert (request.resolved_minute, request.cooldown_until) == (minute, minute + 30)
    agent = world.agents[agent_id]
    assert (agent.activity, agent.target_id) == (Activity.IDLE, None)
    assert (agent.support_request_id, agent.support_cooldown_until) == (None, minute + 30)
    resolved = before.get("support_resolved_count", 0.0) + 1.0
    assert world.metric_state["support_resolved_count"] == resolved
    assert world._core.entities().verify(world) is None
    for name in ("shared_operations_v1", "body_state_v1"):
        if name in world.model_state:
            assert world._core.mirror(name).verify()


@pytest.mark.parametrize("kind", ["emotional_support", "hygiene_support", "language_access"])
def test_the_end_of_support_resolves_and_records_the_care(
    generated_scenario: Any, kind: str
) -> None:
    world, _ = _run(generated_scenario, 5)
    if kind != "emotional_support":
        # The selected formula's tables decide these kinds.
        world.model_configs.get("policy_registry", {}).pop("care_outcome", None)
    event, agent_id = _assigned(world, kind)
    body = world.model_state["body_state_v1"]
    record = body["agents"][agent_id]
    for field in ("hygiene_need", "reassurance", "positive_affect", "negative_affect"):
        record[field] = 0.5
    world.agents[agent_id].stress = 0.5
    world.agents[agent_id].trust = 0.5
    metrics = dict(world.metric_state)
    handler = StaffSupportCompletionHandler(cooldown_minutes=30, require_colocated=True)
    records = native.rules("support").complete_support(handler, world, event)
    assert [item.status for item in records] == [EventStatus.APPLIED]
    _resolved(world, event, metrics)
    # The deleted rule, with the body state's configured amounts.
    agent = world.agents[agent_id]
    sources = ["ASM-CARE-TRANSITION-PRODUCER-V1", "ASM-LIFE-STATE-FEEDBACK-V1"]
    if kind == "emotional_support":
        relief = body["config"]["complaint_reassurance_gain"]
        assert agent_id not in body["config"].get("complaint_reassurance_gain_by_agent", {})
        after = {
            ("agent", "stress"): max(0.0, 0.5 - relief),
            ("agent", "trust"): min(1.0, 0.5 + relief * 0.25),
            ("model_state", "reassurance"): min(1.0, 0.5 + relief),
            ("model_state", "positive_affect"): min(1.0, 0.5 + relief * 0.5),
            ("model_state", "negative_affect"): max(0.0, 0.5 - relief * 0.5),
        }
    elif kind == "hygiene_support":
        relief = body["config"]["hygiene_support_relief"]
        assert agent_id not in body["config"].get("hygiene_support_relief_by_agent", {})
        after = {("model_state", "hygiene_need"): max(0.0, 0.5 - relief)}
    else:
        after = {}
        sources = ["ASM-CARE-TRANSITION-PRODUCER-V1"]
    changes = [
        _change(
            entity,
            agent_id,
            field if entity == "agent" else f"body_state_v1.agents.{field}",
            0.5,
            value,
        )
        for (entity, field), value in after.items()
    ]
    for (entity, field), value in after.items():
        assert (getattr(agent, field) if entity == "agent" else record[field]) == value
    if kind != "emotional_support":
        assert (agent.stress, agent.trust) == (0.5, 0.5)
    payload = records[0].to_dict()["event"]["payload"]
    assert payload["support_kind"] == kind
    assert (payload["status"], payload["staff_id"]) == ("completed", event.payload["staff_id"])
    assert payload["state_changes"] == changes
    assert payload["outcome_observation"] == ("observed_change" if after else "not_measured")
    assert payload["source_ids"] == [*sources, "ASM-STAFF-SUPPORT-LIFECYCLE-V1"]
    assert payload["causation_id"] == event.event_id


def test_the_end_of_support_without_a_body_state_measures_nothing(generated_scenario: Any) -> None:
    """The rule read and wrote the body only when it was a dict: without the body state (a Run of
    no body model), emotional support relieves nothing and its care is not measured. Recorded
    once from the deleted rule, which the kernel then left to it."""
    world, _ = _run(generated_scenario, 5)
    event, agent_id = _assigned(world, "emotional_support")
    del world.model_state["body_state_v1"]
    world.agents[agent_id].stress = 0.5
    world.agents[agent_id].trust = 0.5
    metrics = dict(world.metric_state)
    handler = StaffSupportCompletionHandler(cooldown_minutes=30, require_colocated=True)
    records = native.rules("support").complete_support(handler, world, event)
    assert [item.status for item in records] == [EventStatus.APPLIED]
    _resolved(world, event, metrics)
    assert (world.agents[agent_id].stress, world.agents[agent_id].trust) == (0.5, 0.5)
    payload = records[0].to_dict()["event"]["payload"]
    assert payload["outcome_observation"] == "not_measured"
    assert payload["state_changes"] == []
    assert payload["source_ids"] == [
        "ASM-CARE-TRANSITION-PRODUCER-V1",
        "ASM-STAFF-SUPPORT-LIFECYCLE-V1",
    ]


def test_the_end_of_support_that_does_not_fit_is_python_s(generated_scenario: Any) -> None:
    world, _ = _run(generated_scenario, 5)
    event, agent_id = _assigned(world, "emotional_support")
    world.agents[agent_id].activity = Activity.IDLE
    handler = StaffSupportCompletionHandler(cooldown_minutes=30, require_colocated=True)
    expected = (EventApplicationError, "support completion does not match its assigned task")
    kernel_says_python_s_text(handler, world, event, expected)


# ---- what follows in the dispatch phase ----------------------------------------------------------


def _unchanged(world: WorldState) -> tuple[str, int, int]:
    return _bytes(world), len(world.event_log.records), len(world.event_queue.pending())


def test_a_timeout_records_the_interruption_of_a_care_request(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    agent_id = _idle_person(world)
    _open_request(world, agent_id)
    care_receipts.append_applied_care_transition(
        world,
        request_id="support-t1",
        agent_id=agent_id,
        household_id=None,
        support_kind="emotional_support",
        status="requested",
    )
    world.support_requests["support-t1"].status = SupportRequestStatus.UNRESOLVED
    event = _timeout_event(world, agent_id)
    # Without a request in the event there is nothing to record.
    before = _unchanged(world)
    timeout = _timeout_event(world, agent_id).to_dict()
    without = ScheduledEvent.from_saved({**timeout, "payload": {"agent_id": agent_id}})
    DispatchPhase._record_request_interrupted(controller, world, without)
    assert _unchanged(world) == before
    twin = copy.deepcopy(world)
    DispatchPhase._record_request_interrupted(controller, world, event)
    assert _unchanged(world) != before
    # What the Python rule appended: the receipt's household and kind, the timeout as the cause.
    care_receipts.append_applied_care_transition(
        twin,
        request_id="support-t1",
        agent_id=agent_id,
        household_id=None,
        support_kind="emotional_support",
        status="interrupted",
        causation_id=event.event_id,
        source_ids=care_receipts.care_source_ids_for_agent(twin, agent_id),
    )
    _same_world(world, twin)


def test_a_staff_arrival_records_the_start_of_care(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    agent_id = _idle_person(world)
    _open_request(world, agent_id)
    care_receipts.append_applied_care_transition(
        world,
        request_id="support-t1",
        agent_id=agent_id,
        household_id=None,
        support_kind="emotional_support",
        status="requested",
    )
    staff_id = sorted(world.staff)[0]
    request = world.support_requests["support-t1"]
    request.status = SupportRequestStatus.ASSIGNED
    request.assigned_staff_id = staff_id
    request.task_id = "staff-support-test"

    def arrival(**payload: Any) -> ScheduledEvent:
        return ScheduledEvent(
            event_id="staff-arrival-test",
            kind=EventKind.COMPLETION,
            due_minute=world.minute_index,
            priority=0,
            sequence=900_003,
            event_type="agent_travel_complete",
            payload={"purpose": "staff_support_travel", **payload},
            source_id="staff-support-test",
            provenance={"source_type": "assumption", "source_id": "ASM-TEST"},
        )

    before = _unchanged(world)
    # Nothing without the request, for another staff member, or for a person not seeking help.
    for event in (
        arrival(staff_id=staff_id),
        arrival(request_id="support-t1", staff_id="someone-else"),
        arrival(request_id="no-such-request", staff_id=staff_id),
    ):
        DispatchPhase._record_staff_support_started(controller, world, event)
        assert _unchanged(world) == before
    world.agents[agent_id].activity = Activity.IDLE
    idle = _unchanged(world)
    DispatchPhase._record_staff_support_started(
        controller, world, arrival(request_id="support-t1", staff_id=staff_id)
    )
    assert _unchanged(world) == idle
    world.agents[agent_id].activity = Activity.SEEKING_HELP
    twin = copy.deepcopy(world)
    DispatchPhase._record_staff_support_started(
        controller, world, arrival(request_id="support-t1", staff_id=staff_id)
    )
    assert _unchanged(world) != before
    # What the Python rule appended: the assigned staff member, the request's task as the cause.
    care_receipts.append_applied_care_transition(
        twin,
        request_id="support-t1",
        agent_id=agent_id,
        household_id=None,
        support_kind="emotional_support",
        status="started",
        staff_id=staff_id,
        causation_id="staff-support-test",
        source_ids=care_receipts.care_source_ids_for_agent(twin, agent_id),
    )
    _same_world(world, twin)


def test_a_settled_request_without_a_receipt_adds_nothing(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    agent_id = _idle_person(world)
    _open_request(world, agent_id)
    event = _timeout_event(world, agent_id)
    before = _bytes(world)
    native.rules("support").record_interrupted(controller, world, event)
    assert _bytes(world) == before


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


def test_an_arrival_s_refused_toilet_request_is_its_text(generated_scenario: Any) -> None:
    """The request's own ValueError, as #90 saves it: the walk fails with the request's text."""
    world, controller = _run(generated_scenario, 5)
    world.model_configs.pop("facility_geometry", None)
    handler = AgentTravelCompletionHandler(controller.toilet_service, validate_request_state=False)
    person, toilet = _idle_person(world), _toilet(world)
    world.facilities[toilet].queue_agent_ids.append(person)
    before = _bytes(world)
    with pytest.raises(EventApplicationError) as made:
        native.rules("travel_completion").after_arrival(
            handler,
            world,
            None,
            person,
            "toilet_request",
            toilet,
            world.facilities[toilet].zone_id,
            None,
        )
    assert str(made.value) == f"Agent {person!r} already has an active toilet request"
    assert _bytes(world) == before


def test_the_journey_home_after_a_toilet_is_a_return_trip(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    agent_id = _idle_person(world)
    agent = world.agents[agent_id]
    home = agent.profile.initial_zone_id
    event = _timeout_event(world, agent_id)
    # Nothing for someone at home or someone unknown.
    before = _unchanged(world)
    assert agent.zone_id == home
    DispatchPhase._schedule_return_to_initial_zone(controller, world, event)
    stranger = _timeout_event(world, "nobody")
    DispatchPhase._schedule_return_to_initial_zone(controller, world, stranger)
    assert _unchanged(world) == before
    agent.zone_id = next(zone for zone in sorted(world.zones) if zone != home)
    twin = copy.deepcopy(world)
    python_controller = _controller(twin)
    DispatchPhase._schedule_return_to_initial_zone(controller, world, event)
    assert agent.activity is Activity.MOVING
    # What the Python rule did: the controller's route home and a "return" trip on it.
    route = python_controller._route(twin.agents[agent_id].zone_id, home, agent_id)
    assert route is not None
    python_controller._schedule_travel(
        agent_id=agent_id,
        route=route,
        purpose="return",
        facility_id=None,
        minute=twin.minute_index,
        causation_id=event.event_id,
        validate_state=False,
    )
    _same_world(world, twin)
