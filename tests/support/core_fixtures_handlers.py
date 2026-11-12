"""The fixture of the core's tests of the handlers' rules (Issue #225, lane M4).

`native/core/tests/handler_rules.rs` checks the kernels that integration/test_handler_rules.py and
test_handler_rest_rules.py checked through their PyO3 entries: the toilet's queue and service, the
care receipts, a seek, the end of a request or of a staff member's support, the walk home, the
handlers of the operations and of the social events, and the draws of a claim. The worlds and the
inputs are built here as those files built them: the generated Scenario of `generated_scenario`
(the UI's preview) after 5 minutes, edited in Python.

`handler_rules.json` is a fixture of `support/core_texts.py`'s shape (`Fixture`): its worlds (a
whole entry, or a variant of another, `{"base", "changes"}`), its `cases` (one event for one
handler, with the outcome the Python rule raised or the world it left; `support/cases.rs`), and
`calls`: one call of a kernel by group, each the pytest test of the same name without `test_`:

- `world`: the world the kernel runs on; `after`: the world the Python rule left (the copy the
  pytest file edited as that rule did), or none where the world must stay as it was;
- the call's arguments, and what the pytest file expected of it (`error`: `[class, text]`, the
  class as `core::error::Error`'s variant; `expect`: facts of the world after, `[path, value]`
  or `[path]` for a key that is gone, the path into `canonical_json()`'s document, a list of rows
  by their ID).

Where the pytest file pinned a digest of the world, the core's test pins it too; those digests are
in the Rust file, with where they came from.
"""

from __future__ import annotations

import copy
import functools
import hashlib
import json
import tempfile
from pathlib import Path
from typing import Any

from support.core_texts import Fixture, _run, changes, handler_of
from support.generated import preview_scenario

# The minutes the pytest files ran the generated Scenario before each test.
MINUTES = 5
# The draws of `test_many_draws_of_a_claim_and_a_correction_are_python_s` and of the seed past 64
# bits.
CLAIM_DRAWS = 4000
BIG_SEED_DRAWS = 200
BIG_SEED = 2**70 + 3
TYPED_KINDS = (
    "caregiver_assistance",
    "dependent_assistance",
    "privacy_accommodation",
    "mobility_assistance",
    "language_access",
    "medical_assessment",
    "medication_support",
    "hygiene_support",
)
SOCIAL_SOURCE = "ASM-EVENT-TRIGGERED-SOCIAL-INTERACTIONS-V1"


class Calls(Fixture):
    """The worlds, the handlers' cases and the kernels' calls of `handler_rules.json`."""

    def __init__(self) -> None:
        super().__init__()
        self.calls: dict[str, list[Any]] = {}

    def call(self, group: str, **data: Any) -> None:
        self.calls.setdefault(group, []).append(json.loads(json.dumps(data)))

    def outcome(
        self, group: str, world: str, handler: Any, event: Any, kind: str, text: str
    ) -> None:
        """A case the Python rule turned down: `kind` is Suppressed or Failed."""
        self._add(group, world, handler, event, outcome=[kind, text])

    def document(self) -> dict[str, Any]:
        return {**super().document(), "calls": self.calls}


@functools.cache
def _scenario_document() -> str:
    scenario = preview_scenario(Path(tempfile.mkdtemp(prefix="handler-rules-")))
    return json.dumps(scenario.model_dump(mode="json"))


def _scenario(**registry: Any) -> Any:
    """A fresh `generated_scenario`, its policy registry changed as the test changed it."""
    from sheltersim.experiments.scenario import ScenarioSpec

    document = json.loads(_scenario_document())
    if registry:
        selected = document["model_configs"]["policy_registry"]
        selected.pop("suite_id", None)
        selected.update(registry)
    return ScenarioSpec.model_validate(document)


def _generated(**registry: Any) -> tuple[Any, Any]:
    """The world and controller of test_handler_rules.py's `_run(generated_scenario, 5)`."""
    return _run(_scenario(**registry), MINUTES)


# ---- What the pytest files edited ---------------------------------------------------------------


def _idle_person(world: Any) -> str:
    from sheltersim.domain.models import Activity

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
    raise AssertionError("nobody is idle")


def _toilet(world: Any) -> str:
    return next(key for key, item in sorted(world.facilities.items()) if "toilet" in item.kind)


def _queue_task_id(facility_id: str, agent_id: str, minute: int) -> str:
    text = f"{facility_id}\0{agent_id}\0{minute}".encode()
    return "toilet-queue-" + hashlib.sha256(text).hexdigest()[:24]


def _event(world: Any, event_type: str, payload: dict[str, Any], **fields: Any) -> Any:
    """test_handler_rest_rules.py's `_event`."""
    from sheltersim.events.models import EventKind, ScheduledEvent

    return ScheduledEvent(
        event_id=fields.pop("event_id", f"test-{event_type.replace('_', '-')}"),
        kind=fields.pop("kind", EventKind.EXOGENOUS),
        due_minute=world.minute_index,
        priority=0,
        sequence=fields.pop("sequence", 900_001),
        event_type=event_type,
        payload=payload,
        source_id=fields.pop("source_id", "SRC-TEST"),
        causation_id=fields.pop("causation_id", None),
        provenance=fields.pop("provenance", {"source_type": "assumption", "source_id": "SRC-TEST"}),
    )


def _completion_handler() -> dict[str, Any]:
    """A `ToiletServiceCompletionHandler()`: no step after the completion."""
    from sheltersim.events.handlers import ToiletServiceCompletionHandler

    return {**handler_of(ToiletServiceCompletionHandler()), "attrs": {"after_complete": None}}


# ---- integration/test_handler_rules.py: the toilet ----------------------------------------------


def _toilet_request(fixture: Calls, base: str, world: Any, controller: Any) -> None:
    from sheltersim.domain.facility_queues import facility_has_independent_queues
    from sheltersim.domain.models import ActiveTask, Activity

    twin = copy.deepcopy(world)
    agent_id, facility_id = _idle_person(world), _toilet(world)
    assert not facility_has_independent_queues(world, facility_id)
    minute = world.minute_index
    task_id = _queue_task_id(facility_id, agent_id, minute)
    place = len(twin.facilities[facility_id].queue_agent_ids) + 1
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
    after = fixture.world("toilet_requested", twin, controller)
    fixture.call(
        "a_toilet_request_puts_the_person_last_in_the_queue",
        world=base,
        agent_id=agent_id,
        facility_id=facility_id,
        receipt=[agent_id, facility_id, minute, task_id, place],
        after=after,
    )

    spoils = [
        (lambda edited, agent: "nobody", "unknown Agent 'nobody'"),
        (
            lambda edited, agent: setattr(edited.agents[agent], "activity", Activity.MOVING),
            "Agent '{agent}' cannot request a toilet while active",
        ),
        (
            lambda edited, agent: edited.facilities[_toilet(edited)].queue_agent_ids.append(agent),
            "Agent '{agent}' already has an active toilet request",
        ),
        (
            lambda edited, agent: setattr(
                edited.facilities[_toilet(edited)], "kind", "water_station"
            ),
            "'{facility}' is not a toilet Facility",
        ),
    ]
    for number, (spoil, text) in enumerate(spoils):
        edited = copy.deepcopy(world)
        agent = spoil(edited, agent_id) or agent_id
        name = fixture.world(f"toilet_refused_{number}", edited, controller)
        fixture.call(
            "what_a_toilet_request_refuses_is_python_s_text",
            world=name,
            agent_id=agent,
            facility_id=facility_id,
            error=["Value", text.format(agent=agent, facility=facility_id)],
        )

    edited = copy.deepcopy(world)
    edited.active_tasks[task_id] = ActiveTask(
        id=task_id,
        kind="toilet_queue",
        agent_id="somebody",
        resource_id=facility_id,
        start_minute=0,
        due_minute=0,
    )
    fixture.call(
        "a_queue_task_id_in_use_is_an_invariant_error",
        world=fixture.world("toilet_task_in_use", edited, controller),
        agent_id=agent_id,
        facility_id=facility_id,
        error=["Invariant", f"duplicate toilet queue task ID {task_id!r}"],
    )

    edited = copy.deepcopy(world)
    for facility in edited.facilities.values():
        facility.queue_agent_ids.clear()
    empty = fixture.world("toilet_queues_empty", edited, controller)
    edited.facilities[facility_id].queue_agent_ids.append(_idle_person(edited))
    one = fixture.world("toilet_queue_of_one", edited, controller)
    group = "the_facilities_to_serve_leave_out_only_an_empty_queue"
    fixture.call(group, world=empty, due=[])
    fixture.call(group, world=one, due=[facility_id])


def _queued(world: Any, controller: Any) -> tuple[str, str]:
    """test_handler_rules.py's `_queued`: a request through the toilet's kernel."""
    from sheltersim import native

    agent_id = _idle_person(world)
    facility_id = _toilet(world)
    world.facilities[facility_id].queue_agent_ids.clear()
    world.facilities[facility_id].active_agent_ids.clear()
    native.rules("toilet").request(
        controller.toilet_service, agent_id, facility_id, validate_state=False
    )
    return agent_id, facility_id


def _head(fixture: Calls) -> None:
    from sheltersim.core.rng import CounterRNG
    from sheltersim.domain.models import ActiveTask, Activity
    from sheltersim.events.models import EventKind, ScheduledEvent

    world, controller = _generated()
    agent_id, facility_id = _queued(world, controller)
    queue_task = next(task for task in world.active_tasks.values() if task.agent_id == agent_id)
    world.minute_index += 3
    minute = world.minute_index
    before = fixture.world("toilet_head_waits", world, controller)
    twin = copy.deepcopy(world)
    rng = CounterRNG(world.seed)
    manager = controller.toilet_service
    key = ("toilet-service", facility_id, agent_id, "minute", minute, manager.policy_version)
    key += ("duration",)
    draw = rng.random(*key)
    span = manager.max_service_minutes - manager.min_service_minutes + 1
    service = min(manager.min_service_minutes + int(draw * span), manager.max_service_minutes)
    sequence = twin.allocate_event_sequence()
    event_id = f"toilet-complete-{sequence}"
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
    after = fixture.world("toilet_head_started", twin, controller)
    fixture.call(
        "the_head_starts_its_service",
        world=before,
        facility_id=facility_id,
        event_id=event_id,
        after=after,
    )

    world, controller = _generated()
    _, facility_id = _queued(world, controller)
    world.duration_minutes = world.minute_index + 1
    fixture.call(
        "a_head_that_cannot_finish_stays_in_line",
        world=fixture.world("toilet_head_too_late", world, controller),
        facility_id=facility_id,
    )


def _in_service(world: Any, due: int) -> Any:
    """test_handler_rules.py's `_in_service`."""
    from sheltersim.domain.models import ActiveTask, Activity
    from sheltersim.events.models import EventKind, ScheduledEvent

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


def _completed(world: Any, event: Any) -> Any:
    """What the Python rule of a toilet's completion left (`complete_toilet_service` and the
    handler, gone in lane L10), on a copy: the slot, the task, the person and the count, and 0.2
    off the hygiene need."""
    from sheltersim.domain.models import Activity

    twin = copy.deepcopy(world)
    agent_id = event.payload["agent_id"]
    facility_id, task_id = event.payload["facility_id"], event.payload["task_id"]
    twin.facilities[facility_id].active_agent_ids.remove(agent_id)
    del twin.active_tasks[task_id]
    twin.agents[agent_id].activity = Activity.IDLE
    twin.agents[agent_id].target_id = None
    twin.agents[agent_id].needs.toilet_urgency = 0.0
    twin.metric_state["toilet_service_completed_count"] = (
        twin.metric_state.get("toilet_service_completed_count", 0.0) + 1.0
    )
    record = twin.model_state["body_state_v1"]["agents"][agent_id]
    record["hygiene_need"] = max(0.0, record["hygiene_need"] - 0.2)
    return twin


def _toilet_completion(fixture: Calls, world: Any, controller: Any) -> None:
    from sheltersim.domain.models import Activity

    handler = _completion_handler()
    for number, hygiene in enumerate((0.75, 0.2, 0.1, 0.0)):
        edited = copy.deepcopy(world)
        event = _in_service(edited, edited.minute_index)
        edited.model_state["body_state_v1"]["agents"][event.payload["agent_id"]]["hygiene_need"] = (
            hygiene
        )
        before = fixture.world(f"in_service_{number}", edited, controller)
        after = fixture.world(f"toilet_completed_{number}", _completed(edited, event), controller)
        fixture.applies("a_toilet_completion_is_python_s", before, handler, event, after)

    group = "a_facility_kind_beyond_ascii_is_folded_as_python_folds_it"
    for kind, is_toilet in (("TOILET_和式", True), ("給水所", False)):
        edited = copy.deepcopy(world)
        event = _in_service(edited, edited.minute_index)
        facility_id = event.payload["facility_id"]
        edited.facilities[facility_id].kind = kind
        before = fixture.world(f"toilet_kind_{kind}", edited, controller)
        if is_toilet:
            after = fixture.world("toilet_kind_completed", _completed(edited, event), controller)
            fixture.applies(group, before, handler, event, after)
        else:
            text = f"{facility_id!r} is not a toilet Facility"
            fixture.outcome(group, before, handler, event, "Failed", text)

    edited = copy.deepcopy(world)
    event = _in_service(edited, edited.minute_index)
    agent_id, facility_id = event.payload["agent_id"], event.payload["facility_id"]
    edited.agents[agent_id].activity = Activity.IDLE
    # `complete_toilet_service`'s ValueError, as the handler wraps it: the pytest file matched
    # its start.
    text = f"Agent {agent_id!r} is not using Facility {facility_id!r}"
    fixture.outcome(
        "a_toilet_completion_that_does_not_fit_is_python_s",
        fixture.world("in_service_idle", edited, controller),
        handler,
        event,
        "Failed",
        text,
    )


# ---- integration/test_handler_rules.py: care receipts --------------------------------------------


def _open_request(world: Any, agent_id: str, *, request_id: str = "support-t1") -> None:
    """test_handler_rules.py's `_open_request`."""
    from sheltersim.domain.models import Activity, SupportRequestState

    world.support_requests[request_id] = SupportRequestState(
        id=request_id,
        agent_id=agent_id,
        requested_minute=world.minute_index,
        timeout_minute=world.minute_index + 30,
    )
    world.agents[agent_id].support_request_id = request_id
    world.agents[agent_id].activity = Activity.SEEKING_HELP


def _transition(world: Any, *, status: str = "requested", **more: Any) -> dict[str, Any]:
    return {
        "request_id": "support-t1",
        "agent_id": sorted(world.agents)[0],
        "household_id": None,
        "support_kind": "emotional_support",
        "status": status,
        "source_ids": ["ASM-Z", "ASM-A", "ASM-Z"],
        **more,
    }


def _care_transitions(fixture: Calls, base: str, world: Any, controller: Any) -> None:
    from sheltersim import native

    fixture.call(
        "a_care_transition_is_python_s",
        world=base,
        transitions=[_transition(world)],
        expect=[
            [
                ["model_state", "shared_operations_v1", "support_requests", "support-t1", "status"],
                "requested",
            ],
            [["event_log", -1, "event", "event_type"], "care_support_transition"],
            [
                ["event_log", -1, "detail"],
                "care transition requested recorded after state application",
            ],
            [
                ["event_log", -1, "event", "payload", "source_ids"],
                ["ASM-A", "ASM-CARE-TRANSITION-PRODUCER-V1", "ASM-Z"],
            ],
        ],
    )
    group = "what_a_care_transition_refuses_changes_nothing"
    for more, why in (
        ({"support_kind": "unknown"}, "contract"),
        ({"request_id": "bad id"}, "contract"),
        ({"status": "unknown"}, "status"),
    ):
        transition = _transition(world, **more)
        # The refusal's text, which the pytest file matched by `why`.
        edited = copy.deepcopy(world)
        try:
            native.rules("care_receipts").append_transition(
                edited, **{**transition, "source_ids": tuple(transition["source_ids"])}
            )
        except native.Unsupported as refused:
            text = str(refused)
        else:
            raise AssertionError("the transition was not refused")
        assert why in text, text
        fixture.call(group, world=base, transitions=[transition], error=["Unsupported", text])
    fixture.call(
        "a_transition_out_of_order_has_the_python_text_and_changes_nothing",
        world=base,
        transitions=[_transition(world, status="completed")],
        error=["Value", "support lifecycle transition is out of order"],
    )
    fixture.call(
        "a_request_id_used_before_is_python_s_error",
        world=base,
        transitions=[
            _transition(world),
            _transition(world, status="interrupted"),
            _transition(world),
        ],
        error=["Value", "support request IDs cannot be reused across Run history"],
    )
    edited = copy.deepcopy(world)
    del edited.model_state["shared_operations_v1"]
    fixture.call(
        "without_the_namespace_nothing_happens",
        world=fixture.world("no_shared_operations", edited, controller),
        transitions=[_transition(world)],
    )


# ---- integration/test_handler_rules.py: seeks ---------------------------------------------------


def _seeks(fixture: Calls) -> None:
    """The worlds of the seeks: each runs `_apply_effect` of one proposal of its person, the
    request's next decision 7 minutes on."""
    people: list[Any] = [
        {},
        {"source_ids": ["S-B", "S-A", 3]},
        {"care_support_needs": ["language_access"]},
        {"care_support_needs": ["hygiene_support", "language_access"]},
        {"medication_support": True},
        {"medical_support": True},
        {"medical_support": 1},
        "not a person",
    ]
    for number, person in enumerate(people):
        world, controller = _generated()
        agent_id = _idle_person(world)
        configs = world.model_configs.setdefault("population_context", {})
        configs.setdefault("people", {})[agent_id] = person
        fixture.call(
            "what_a_seek_reads_of_a_person_is_python_s",
            world=fixture.world(f"seek_person_{number}", world, controller),
            action="seek_help",
            agent_id=agent_id,
            next_decision_at=world.minute_index + 7,
        )
    world, controller = _generated()
    agent_id = _idle_person(world)
    for action in ("seek_help", "seek_information"):
        fixture.call(
            "a_seek_is_python_s",
            world="generated",
            action=action,
            agent_id=agent_id,
            next_decision_at=world.minute_index + 7,
        )
    edited = copy.deepcopy(world)
    edited.agents[agent_id].support_cooldown_until = edited.minute_index + 1
    fixture.call(
        "a_seek_in_cooldown_counts_as_rejected",
        world=fixture.world("seek_in_cooldown", edited, controller),
        action="seek_help",
        agent_id=agent_id,
        next_decision_at=world.minute_index + 7,
    )
    assert controller.information_policy is not None
    edited = copy.deepcopy(world)
    state = edited.model_state["information_spread_v1"]
    state["query_pressure"][agent_id] = controller.information_policy.config.query_threshold
    fixture.call(
        "a_seek_reaches_the_information_rule_above_its_threshold",
        world=fixture.world("seek_query_pressure", edited, controller),
        action="seek_help",
        agent_id=agent_id,
        next_decision_at=world.minute_index + 7,
    )


# ---- integration/test_handler_rules.py: a request's end ------------------------------------------


def _timeout_event(world: Any, agent_id: str, request_id: str = "support-t1") -> Any:
    from sheltersim.events.models import EventKind, ScheduledEvent

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


def _timeouts(fixture: Calls, world: Any, controller: Any) -> None:
    from sheltersim.domain.models import Activity, SupportRequestStatus
    from sheltersim.events.handlers import SupportRequestTimeoutHandler

    handler = handler_of(SupportRequestTimeoutHandler(cooldown_minutes=30))
    edited = copy.deepcopy(world)
    agent_id = _idle_person(edited)
    _open_request(edited, agent_id)
    opened = fixture.world("request_open", edited, controller)
    event = _timeout_event(edited, agent_id)
    # The Python rule (`timeout_support_request`, gone in lane L10): unresolved, with a cooldown.
    twin = copy.deepcopy(edited)
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
    after = fixture.world("request_timed_out", twin, controller)
    fixture.applies("a_timeout_is_python_s", opened, handler, event, after)

    assigned = copy.deepcopy(edited)
    assigned.support_requests["support-t1"].status = SupportRequestStatus.ASSIGNED
    fixture.outcome(
        "a_timeout_of_what_has_left_pending_says_python_s_text",
        fixture.world("request_assigned", assigned, controller),
        handler,
        event,
        "Suppressed",
        "support request is no longer pending",
    )
    other = next(key for key in sorted(edited.agents) if key != agent_id)
    fixture.outcome(
        "a_timeout_of_another_person_s_request_is_python_s_error",
        opened,
        handler,
        _timeout_event(edited, other),
        "Failed",
        "support request 'support-t1' does not match its Agent",
    )


def _assigned(world: Any, kind: str) -> tuple[Any, str]:
    """test_handler_rules.py's `_assigned`: the receipts through the kernel, then the request
    assigned to the first staff member, whose support is due now."""
    from sheltersim import native
    from sheltersim.domain.models import ActiveTask, SupportRequestStatus
    from sheltersim.events.models import EventKind, ScheduledEvent

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


def _resolved(world: Any, event: Any) -> list[Any]:
    """What `complete_support_task` did, as facts of the world after: the task gone, the staff
    member free, the request resolved with the cooldown, the person idle, one more resolved
    request."""
    minute = world.minute_index
    agent_id, staff_id = event.payload["agent_id"], event.payload["staff_id"]
    return [
        [["active_tasks", "staff-support-test"]],
        [["staff", staff_id, "active_task_id"], None],
        [["staff", staff_id, "busy_until"], 0],
        [["staff", staff_id, "work_queue"], []],
        [["support_requests", "support-t1", "status"], "resolved"],
        [["support_requests", "support-t1", "resolved_minute"], minute],
        [["support_requests", "support-t1", "cooldown_until"], minute + 30],
        [["agents", agent_id, "activity"], "idle"],
        [["agents", agent_id, "target_id"], None],
        [["agents", agent_id, "support_request_id"], None],
        [["agents", agent_id, "support_cooldown_until"], minute + 30],
        [
            ["metric_state", "support_resolved_count"],
            world.metric_state.get("support_resolved_count", 0.0) + 1.0,
        ],
    ]


def _support_ends(fixture: Calls) -> None:
    from sheltersim.domain.models import Activity
    from sheltersim.events.handlers import StaffSupportCompletionHandler

    handler = handler_of(StaffSupportCompletionHandler(cooldown_minutes=30, require_colocated=True))
    group = "the_end_of_support_resolves_and_records_the_care"
    for kind in ("emotional_support", "hygiene_support", "language_access"):
        world, controller = _generated()
        if kind != "emotional_support":
            world.model_configs.get("policy_registry", {}).pop("care_outcome", None)
        event, agent_id = _assigned(world, kind)
        body = world.model_state["body_state_v1"]
        record = body["agents"][agent_id]
        for field in ("hygiene_need", "reassurance", "positive_affect", "negative_affect"):
            record[field] = 0.5
        world.agents[agent_id].stress = 0.5
        world.agents[agent_id].trust = 0.5
        sources = ["ASM-CARE-TRANSITION-PRODUCER-V1", "ASM-LIFE-STATE-FEEDBACK-V1"]
        # The deleted rule, with the body state's configured amounts.
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
        state_changes = [
            {
                "entity_id": agent_id,
                "entity_kind": entity,
                "field_path": field if entity == "agent" else f"body_state_v1.agents.{field}",
                "before": 0.5,
                "after": value,
                "unit": "normalized_0_1",
            }
            for (entity, field), value in after.items()
        ]
        facts = _resolved(world, event)
        for (entity, field), value in after.items():
            path = (
                ["agents", agent_id, field]
                if entity == "agent"
                else ["model_state", "body_state_v1", "agents", agent_id, field]
            )
            facts.append([path, value])
        if kind != "emotional_support":
            facts += [[["agents", agent_id, "stress"], 0.5], [["agents", agent_id, "trust"], 0.5]]
        fixture.call(
            group,
            world=fixture.world(f"support_due_{kind}", world, controller),
            handler=handler,
            event=event.to_dict(),
            expect=facts,
            record=[
                [["event", "payload", "support_kind"], kind],
                [["event", "payload", "status"], "completed"],
                [["event", "payload", "staff_id"], event.payload["staff_id"]],
                [["event", "payload", "state_changes"], state_changes],
                [
                    ["event", "payload", "outcome_observation"],
                    "observed_change" if after else "not_measured",
                ],
                [
                    ["event", "payload", "source_ids"],
                    [*sources, "ASM-STAFF-SUPPORT-LIFECYCLE-V1"],
                ],
                [["event", "payload", "causation_id"], event.event_id],
            ],
        )

    # Without the body state (a Run of no body model), emotional support relieves nothing and its
    # care is not measured: recorded once from the deleted rule.
    world, controller = _generated()
    event, agent_id = _assigned(world, "emotional_support")
    del world.model_state["body_state_v1"]
    world.agents[agent_id].stress = 0.5
    world.agents[agent_id].trust = 0.5
    fixture.call(
        "the_end_of_support_without_a_body_state_measures_nothing",
        world=fixture.world("support_due_without_body", world, controller),
        handler=handler,
        event=event.to_dict(),
        expect=[
            *_resolved(world, event),
            [["agents", agent_id, "stress"], 0.5],
            [["agents", agent_id, "trust"], 0.5],
        ],
        record=[
            [["event", "payload", "outcome_observation"], "not_measured"],
            [["event", "payload", "state_changes"], []],
            [
                ["event", "payload", "source_ids"],
                ["ASM-CARE-TRANSITION-PRODUCER-V1", "ASM-STAFF-SUPPORT-LIFECYCLE-V1"],
            ],
        ],
    )

    world, controller = _generated()
    event, agent_id = _assigned(world, "emotional_support")
    world.agents[agent_id].activity = Activity.IDLE
    fixture.outcome(
        "the_end_of_support_that_does_not_fit_is_python_s",
        fixture.world("support_due_idle", world, controller),
        handler,
        event,
        "Failed",
        "support completion does not match its assigned task",
    )


# ---- integration/test_handler_rules.py: what follows in the dispatch phase -----------------------


def _receipts_of_a_tick(fixture: Calls) -> None:
    from sheltersim.domain import care_receipts
    from sheltersim.domain.models import Activity, SupportRequestStatus
    from sheltersim.events.models import EventKind, ScheduledEvent
    from support.receipts import care_source_ids

    def requested(world: Any, agent_id: str) -> None:
        care_receipts.append_applied_care_transition(
            world,
            request_id="support-t1",
            agent_id=agent_id,
            household_id=None,
            support_kind="emotional_support",
            status="requested",
        )

    world, controller = _generated()
    agent_id = _idle_person(world)
    _open_request(world, agent_id)
    requested(world, agent_id)
    world.support_requests["support-t1"].status = SupportRequestStatus.UNRESOLVED
    event = _timeout_event(world, agent_id)
    twin = copy.deepcopy(world)
    # What the Python rule appended: the receipt's household and kind, the timeout as the cause.
    care_receipts.append_applied_care_transition(
        twin,
        request_id="support-t1",
        agent_id=agent_id,
        household_id=None,
        support_kind="emotional_support",
        status="interrupted",
        causation_id=event.event_id,
        source_ids=care_source_ids(twin, agent_id),
    )
    group = "a_timeout_records_the_interruption_of_a_care_request"
    before = fixture.world("request_unresolved", world, controller)
    without = {**event.to_dict(), "payload": {"agent_id": agent_id}}
    fixture.call(group, world=before, event=without)
    fixture.call(
        group,
        world=before,
        event=event.to_dict(),
        after=fixture.world("request_interrupted", twin, controller),
    )

    world, controller = _generated()
    agent_id = _idle_person(world)
    _open_request(world, agent_id)
    requested(world, agent_id)
    staff_id = sorted(world.staff)[0]
    request = world.support_requests["support-t1"]
    request.status = SupportRequestStatus.ASSIGNED
    request.assigned_staff_id = staff_id
    request.task_id = "staff-support-test"

    def arrival(**payload: Any) -> dict[str, Any]:
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
        ).to_dict()

    group = "a_staff_arrival_records_the_start_of_care"
    seeking = fixture.world("request_assigned_seeking", world, controller)
    # Nothing without the request, for another staff member, or for a request not there.
    for event in (
        arrival(staff_id=staff_id),
        arrival(request_id="support-t1", staff_id="someone-else"),
        arrival(request_id="no-such-request", staff_id=staff_id),
    ):
        fixture.call(group, world=seeking, event=event)
    idle = copy.deepcopy(world)
    idle.agents[agent_id].activity = Activity.IDLE
    fixture.call(
        group,
        world=fixture.world("request_assigned_idle", idle, controller),
        event=arrival(request_id="support-t1", staff_id=staff_id),
    )
    twin = copy.deepcopy(world)
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
        source_ids=care_source_ids(twin, agent_id),
    )
    fixture.call(
        group,
        world=seeking,
        event=arrival(request_id="support-t1", staff_id=staff_id),
        after=fixture.world("request_started", twin, controller),
    )

    world, controller = _generated()
    agent_id = _idle_person(world)
    _open_request(world, agent_id)
    fixture.call(
        "a_settled_request_without_a_receipt_adds_nothing",
        world=fixture.world("request_open_without_receipt", world, controller),
        event=_timeout_event(world, agent_id).to_dict(),
    )


def _arrival_and_home(fixture: Calls) -> None:
    world, controller = _generated()
    world.model_configs.pop("facility_geometry", None)
    person, toilet = _idle_person(world), _toilet(world)
    world.facilities[toilet].queue_agent_ids.append(person)
    fixture.call(
        "an_arrival_s_refused_toilet_request_is_its_text",
        world=fixture.world("toilet_queued_without_geometry", world, controller),
        agent_id=person,
        facility_id=toilet,
        destination=world.facilities[toilet].zone_id,
        error=["Failed", f"Agent {person!r} already has an active toilet request"],
    )

    world, controller = _generated()
    agent_id = _idle_person(world)
    agent = world.agents[agent_id]
    home = agent.profile.initial_zone_id
    event = _timeout_event(world, agent_id).to_dict()
    group = "the_journey_home_after_a_toilet_is_a_return_trip"
    # Nothing for someone at home or someone unknown.
    assert agent.zone_id == home
    fixture.call(group, world="generated", event=event)
    fixture.call(group, world="generated", event=_timeout_event(world, "nobody").to_dict())
    agent.zone_id = next(zone for zone in sorted(world.zones) if zone != home)
    away = fixture.world("away_from_home", world, controller)
    twin = copy.deepcopy(world)
    python_controller = _controller_of(twin)
    # What the Python rule did: the controller's route home and a "return" trip on it.
    route = python_controller._route(twin.agents[agent_id].zone_id, home, agent_id)
    assert route is not None
    python_controller._schedule_travel(
        agent_id=agent_id,
        route=route,
        purpose="return",
        facility_id=None,
        minute=twin.minute_index,
        causation_id=event["event_id"],
        validate_state=False,
    )
    fixture.call(
        group, world=away, event=event, after=fixture.world("walking_home", twin, controller)
    )


def _controller_of(world: Any) -> Any:
    from sheltersim.experiments.metrics import MetricsRecorder
    from sheltersim.experiments.runner import OfflineSimulationController
    from sheltersim.experiments.timeline import _MetricSink

    return OfflineSimulationController(world, _MetricSink(MetricsRecorder()))


# ---- integration/test_handler_rest_rules.py ------------------------------------------------------


def _rest_texts(fixture: Calls, base: str, world: Any) -> None:
    from sheltersim.events.handlers import (
        FacilityOutageHandler,
        InformationContactHandler,
        ProximityContactHandler,
        WaterRestorationHandler,
    )

    facility_id = sorted(world.facilities)[0]
    outage = handler_of(FacilityOutageHandler())
    for payload, kind, text in (
        ({"facility_id": facility_id}, "Failed", "facility outage payload fields do not match"),
        (
            {"facility_id": facility_id, "units_out": 0},
            "Suppressed",
            "zero-unit facility outage has no effect",
        ),
        (
            {"facility_id": facility_id, "units_out": -1},
            "Failed",
            "facility outage units_out cannot be negative",
        ),
        ({"facility_id": "nowhere", "units_out": 1}, "Failed", "facility 'nowhere' does not exist"),
        (
            {"facility_id": facility_id, "units_out": 10_000},
            "Failed",
            "facility outage exceeds currently working capacity",
        ),
        # A Scenario's units are any non-negative int: past 64 bits too, as Python compares them.
        (
            {"facility_id": facility_id, "units_out": 10**20},
            "Failed",
            "facility outage exceeds currently working capacity",
        ),
        (
            {"facility_id": facility_id, "units_out": -(10**20)},
            "Failed",
            "facility outage units_out cannot be negative",
        ),
    ):
        event = _event(world, "facility_outage", payload)
        fixture.outcome(
            "a_failed_outage_changes_nothing_and_says_python_s_text",
            base,
            outage,
            event,
            kind,
            text,
        )
    fixture.outcome(
        "a_stale_proximity_sample_is_failed_in_python_s_text",
        base,
        handler_of(ProximityContactHandler()),
        _event(world, "proximity_contact_update", {}),
        "Failed",
        "proximity contact provenance or time is invalid",
    )
    fixture.outcome(
        "a_contact_that_no_longer_applies_says_python_s_text",
        base,
        handler_of(InformationContactHandler()),
        _event(world, "information_contact_update", {"message_id": "nothing"}),
        "Failed",
        "information_contact_update payload fields do not match",
    )
    fixture.outcome(
        "a_restoration_of_no_outage_says_python_s_text",
        base,
        handler_of(WaterRestorationHandler()),
        _event(world, "water_restored", {"outage_id": "nothing"}),
        "Failed",
        "water restoration does not match its outage",
    )


def _assigned_care(world: Any, kind: str) -> tuple[Any, str]:
    """test_handler_rest_rules.py's `_assigned_care`."""
    from sheltersim.domain import care_receipts
    from sheltersim.domain.models import (
        ActiveTask,
        Activity,
        SupportRequestState,
        SupportRequestStatus,
    )
    from sheltersim.events.models import EventKind, ScheduledEvent

    agent_id = sorted(world.agents)[0]
    staff_id = sorted(world.staff)[0]
    person = world.staff[staff_id]
    agent = world.agents[agent_id]
    task_id = "staff-support-test"
    for key in list(world.active_tasks):
        task = world.active_tasks[key]
        if task.agent_id == agent_id or task.staff_id == staff_id:
            del world.active_tasks[key]
    agent.support_request_id = "support-t1"
    agent.activity = Activity.SEEKING_HELP
    agent.target_id = None
    agent.zone_id = person.zone_id
    agent.stress = 0.5
    world.support_requests["support-t1"] = SupportRequestState(
        "support-t1", agent_id, world.minute_index - 1, world.minute_index + 50
    )
    request = world.support_requests["support-t1"]
    common = {
        "request_id": "support-t1",
        "agent_id": agent_id,
        "household_id": None,
        "support_kind": kind,
    }
    care_receipts.append_applied_care_transition(world, **common, status="requested")
    care_receipts.append_applied_care_transition(
        world, **common, status="assigned", staff_id=staff_id
    )
    care_receipts.append_applied_care_transition(
        world, **common, status="started", staff_id=staff_id
    )
    request.status = SupportRequestStatus.ASSIGNED
    request.assigned_staff_id = staff_id
    request.task_id = task_id
    world.active_tasks[task_id] = ActiveTask(
        id=task_id,
        kind="staff_support",
        agent_id=agent_id,
        resource_id=None,
        start_minute=world.minute_index,
        due_minute=world.minute_index,
        staff_id=staff_id,
    )
    person.active_task_id = task_id
    person.work_queue[:] = [task_id]
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
            "task_id": task_id,
            "support_duration_minutes": 10,
        },
        source_id=task_id,
        provenance={"source_type": "assumption", "source_id": "ASM-TEST"},
    )
    return event, agent_id


def _cares(fixture: Calls) -> None:
    """The care a selected formula decides: each world `_assigned_care` left, by formula and kind,
    and the care with a dependent; the digests are the Rust file's."""
    from sheltersim.events.handlers import StaffSupportCompletionHandler
    from support.core_texts import _gate

    handler = handler_of(StaffSupportCompletionHandler(cooldown_minutes=30, require_colocated=True))
    selectable = _gate().selectable_models()
    models = [model for domain, model in selectable if domain == "care_outcome"]
    for model_id in models:
        for kind in TYPED_KINDS:
            world, controller = _generated(care_outcome=model_id)
            event, _ = _assigned_care(world, kind)
            fixture.call(
                "a_kind_a_selected_formula_decides_is_python_s",
                world=fixture.world(f"care_{model_id}_{kind}", world, controller),
                handler=handler,
                event=event.to_dict(),
                model_id=model_id,
                kind=kind,
            )

    world, controller = _generated(care_outcome="typed-care-support-response-v1")
    caregiver, dependent = sorted(world.agents)[:2]
    world.model_configs["population_context"] = {
        "people": {
            caregiver: {
                "household_id": "home-1",
                "care_for_ids": [dependent],
                "source_ids": ["SRC-CAREGIVER"],
            },
            dependent: {
                "household_id": "home-1",
                "caregiver_ids": [caregiver],
                "source_ids": ["SRC-DEPENDENT"],
            },
        }
    }
    event, _ = _assigned_care(world, "caregiver_assistance")
    fixture.call(
        "care_with_a_dependent_is_python_s",
        world=fixture.world("care_with_a_dependent", world, controller),
        handler=handler,
        event=event.to_dict(),
        people=[caregiver, dependent],
    )

    world, controller = _generated(care_outcome="typed-care-support-response-v1")
    event, agent_id = _assigned_care(world, "mobility_assistance")
    twin = copy.deepcopy(world)
    twin.model_configs["body_dynamics"]["mobility_assistance_stress_relief"] = 0.03 * 1.01
    fixture.call(
        "a_care_delta_1_percent_off_shows",
        worlds=[
            fixture.world("care_mobility", world, controller),
            fixture.world("care_mobility_1_percent_more", twin, controller),
        ],
        handler=handler,
        event=event.to_dict(),
        agent_id=agent_id,
    )

    for kind in TYPED_KINDS:
        world, controller = _generated(care_outcome="typed-care-support-response-v1")
        configs = world.model_configs["body_dynamics"]
        for name in [key for key in configs if key.startswith(kind) or "assistance" in key]:
            configs.pop(name)
        event, _ = _assigned_care(world, kind)
        fixture.call(
            "care_without_configured_amounts_uses_the_formula_s_defaults",
            world=fixture.world(f"care_defaults_{kind}", world, controller),
            handler=handler,
            event=event.to_dict(),
            kind=kind,
        )


def _ingest(fixture: Calls, world: Any, controller: Any) -> None:
    from support.receipts import facility_recovery_record

    world = copy.deepcopy(world)
    facility_id = sorted(world.facilities)[0]
    record = facility_recovery_record(
        world,
        facility_id=facility_id,
        component_id="component-1",
        status="degraded",
        previous_working_units=2,
        working_units=1,
        causation_id="cause-1",
    )[0]
    world.event_log.append(record)
    fixture.call(
        "ingest_of_a_record_of_this_tick_is_python_s",
        world=fixture.world("recovery_recorded", world, controller),
        event=record.event.to_dict(),
        realized_minute=record.realized_minute,
    )


def _social_event(world: Any, payload: dict[str, Any], **fields: Any) -> Any:
    from sheltersim.events.models import EventKind

    return _event(
        world,
        "social_interaction",
        payload,
        kind=EventKind.ENDOGENOUS,
        source_id="social-policy-test",
        provenance={
            "source_type": "assumption",
            "source_id": SOCIAL_SOURCE,
            "policy_version": "social-policy-test",
        },
        **fields,
    )


def _open_rest_request(world: Any, *, age: int = 0) -> str:
    """test_handler_rest_rules.py's `_open_request`."""
    from sheltersim.domain.models import Activity, SupportRequestState

    agent_id = sorted(world.agents)[0]
    agent = world.agents[agent_id]
    for key in list(world.active_tasks):
        if world.active_tasks[key].agent_id == agent_id:
            del world.active_tasks[key]
    agent.activity = Activity.SEEKING_HELP
    agent.target_id = None
    agent.support_request_id = "support-t1"
    world.support_requests["support-t1"] = SupportRequestState(
        "support-t1", agent_id, world.minute_index - age, world.minute_index + 50
    )
    return "support-t1"


def _socials(fixture: Calls, world: Any, controller: Any) -> None:
    """The social events whose fixed amounts few runs reach: the kernel writes only the stress and
    trust of the people the event names, each the value the pytest file wrote down from the
    deleted Python rule."""
    from sheltersim.events.handlers import SocialInteractionHandler
    from sheltersim.events.models import EventKind, EventLogRecord, EventStatus, ScheduledEvent

    handler = handler_of(SocialInteractionHandler())

    def case(group: str, name: str, edited: Any, event: Any, values: dict[str, Any]) -> None:
        before = fixture.world(name, edited, controller)
        twin = copy.deepcopy(edited)
        for agent_id, (stress, trust) in values.items():
            twin.agents[agent_id].stress = stress
            twin.agents[agent_id].trust = trust
        after = fixture.world(f"{event.payload['action']}_by_{name}", twin, controller)
        fixture.applies(group, before, handler, event, after)

    edited = copy.deepcopy(world)
    request_id = _open_rest_request(edited, age=4)
    agent = next(iter(sorted(edited.agents.items())))[1]
    agent.stress = 0.5
    agent.trust = 0.5
    for action, stress, trust in (
        ("acknowledge_complaint", 0.5 - 0.02, 0.5 + 0.01),
        ("explain_staff_process", 0.5 - 0.01, 0.5 + 0.01),
        ("share_practical_information", 0.5, 0.5 + 0.005),
    ):
        event = _social_event(
            edited,
            {
                "interaction_type": "complaint_response",
                "agent_id": agent.id,
                "request_id": request_id,
                "zone_id": agent.zone_id,
                "stress_threshold": 0.1,
                "complaint_wait_minutes": 3,
                "action": action,
                "reason_short": "long_wait",
            },
            causation_id=request_id,
        )
        case(
            "a_complaint_response_is_python_s",
            "complaint",
            edited,
            event,
            {agent.id: (stress, trust)},
        )

    edited = copy.deepcopy(world)
    request_id = _open_rest_request(edited)
    agent = edited.agents[edited.support_requests[request_id].agent_id]
    agent.stress = 0.5
    agent.trust = 0.5
    for action, stress, trust in (
        ("offer_reassurance", 0.5 - 0.03, 0.5 + 0.01),
        ("share_practical_information", 0.5, 0.5 + 0.005),
    ):
        event = _social_event(
            edited, {"agent_id": agent.id, "request_id": request_id, "action": action}
        )
        case(
            "an_untyped_social_event_is_python_s",
            "untyped",
            edited,
            event,
            {agent.id: (stress, trust)},
        )

    edited = copy.deepcopy(world)
    first, second = (edited.agents[key] for key in sorted(edited.agents)[:2])
    second.zone_id = first.zone_id
    presence = edited.model_state["population_presence_v1"]
    presence["people"][first.id]["household_id"] = "home-1"
    presence["people"][second.id]["household_id"] = "home-1"
    first.stress, second.stress, first.trust, second.trust = 0.5, 0.6, 0.5, 0.6
    contact = ScheduledEvent(
        event_id="contact-test",
        kind=EventKind.ENDOGENOUS,
        due_minute=edited.minute_index - 1,
        priority=0,
        sequence=900_010,
        event_type="information_contact_update",
        payload={"sender_agent_id": first.id, "recipient_agent_id": second.id},
        source_id="SRC-TEST",
        provenance={"source_type": "assumption", "source_id": "SRC-TEST"},
    )
    edited.event_log.append(
        EventLogRecord(
            event=contact, realized_minute=edited.minute_index - 1, status=EventStatus.APPLIED
        )
    )
    event = _social_event(
        edited,
        {
            "interaction_type": "cooperation",
            "agent_id": first.id,
            "participant_agent_id": second.id,
            "zone_id": first.zone_id,
            "stress_threshold": 0.1,
            "relationship": "same_group",
            "trigger_event_id": "contact-test",
            "action": "offer_mutual_support",
            "reason_short": "shared_group_contact",
        },
        causation_id="contact-test",
    )
    case(
        "cooperation_is_python_s",
        "cooperation",
        edited,
        event,
        {first.id: (0.5 - 0.02, 0.5 + 0.01), second.id: (0.6 - 0.02, 0.6 + 0.01)},
    )


def _operations(fixture: Calls) -> None:
    """test_handler_rest_rules.py's `_operation_world` of each kind: a staff member at work on an
    operation in a zone, due now."""
    from sheltersim.core.world import WorldState
    from sheltersim.domain.models import ActiveTask
    from sheltersim.domain.simulator_state import initialize_simulator_extensions
    from sheltersim.events.handlers import StaffOperationCompletionHandler
    from sheltersim.events.models import EventKind, ScheduledEvent
    from sheltersim.experiments.scenario import ScenarioSpec
    from support.paths import SMALL_REPLAY

    probe = json.loads(SMALL_REPLAY.read_text(encoding="utf-8"))
    probe["schema_version"] = 3
    probe["scheduled_events"] = []
    probe_world = WorldState.from_scenario(ScenarioSpec.model_validate(probe))
    zone_id = sorted(agent.zone_id for agent in probe_world.agents.values())[0]
    handler = handler_of(StaffOperationCompletionHandler())
    for kind in ("cleaning", "information_outreach", "patrol"):
        document = json.loads(SMALL_REPLAY.read_text(encoding="utf-8"))
        document["schema_version"] = 3
        document["duration_minutes"] = 200
        document["scheduled_events"] = []
        document["model_configs"]["staff_operations"] = {}
        document["model_configs"]["shared_operations"] = {
            "schema_version": 1,
            "policy_version": "shared-operations-v1",
        }
        world = WorldState.from_scenario(ScenarioSpec.model_validate(document))
        initialize_simulator_extensions(world)
        world.minute_index = 10
        # The pytest file's world has no controller: the Run's values are a copy's, before the
        # edits.
        controller = _controller_of(copy.deepcopy(world))
        staff_id = sorted(world.staff)[0]
        staff = world.staff[staff_id]
        task_id = "staff-operation-test"
        world.active_tasks[task_id] = ActiveTask(
            id=task_id,
            kind="staff_operation",
            agent_id=None,
            resource_id=None,
            start_minute=5,
            due_minute=10,
            staff_id=staff_id,
        )
        staff.active_task_id = task_id
        staff.work_queue[:] = [task_id]
        staff.busy_until = 10
        world.model_state["staff_operations_v1"]["staff"][staff_id].update(
            {
                "phase": "working",
                "task_id": task_id,
                "task_kind": kind,
                "target_id": None,
                "target_zone_id": zone_id,
                "reason": "test",
                "started_minute": 5,
                "arrival_minute": 5,
                "due_minute": 10,
                "causation_event_ids": ["cause-1"],
            }
        )
        event = ScheduledEvent(
            event_id="staff-operation-complete-test",
            kind=EventKind.COMPLETION,
            due_minute=10,
            priority=5,
            sequence=900_011,
            event_type="staff_operation_complete",
            payload={
                "staff_id": staff_id,
                "task_id": task_id,
                "task_kind": kind,
                "target_id": None,
                "target_zone_id": zone_id,
            },
            source_id="SRC-TEST",
            provenance={"source_type": "assumption", "source_id": "SRC-TEST"},
        )
        name = fixture.world(f"operation_{kind}", world, controller, full=kind == "cleaning")
        fixture.call(
            "a_finished_operation_is_python_s",
            world=name,
            handler=handler,
            event=event.to_dict(),
            kind=kind,
        )


def _draws(fixture: Calls, world: Any, controller: Any) -> None:
    from sheltersim.core.rng import CounterRNG
    from sheltersim.domain.models import InformationSpreadConfig
    from support.core_fixtures import INFORMATION_FIELDS

    def fields(config: Any) -> dict[str, Any]:
        return {**{name: getattr(config, name) for name in INFORMATION_FIELDS}, "present": True}

    believing = InformationSpreadConfig(
        rumor_belief_probability=0.0, rumor_trust_sensitivity=0.5, rumor_source_sensitivity=0.0
    )
    accepting = InformationSpreadConfig(
        correction_acceptance_probability=1.0,
        correction_trust_sensitivity=0.0,
        correction_source_sensitivity=0.0,
    )
    edited = copy.deepcopy(world)
    agent_id = sorted(edited.agents)[0]
    edited.agents[agent_id].trust = 1.0
    trusting = fixture.world("trusting", edited, controller)
    # The belief's Python rule: 1.0 when the draw is below the clamped probability, else 0.0.
    believing_probability = min(1.0, max(0.0, 0.0 + 2.0 * 0.5 * (1.0 - 0.5) + 0.0))
    rng = CounterRNG(edited.seed)
    beliefs, acceptances = [], []
    for number in range(CLAIM_DRAWS):
        message_id = f"claim-{number}"
        belief_draw = rng.random("rumor-belief-acceptance-v1", 7, agent_id, message_id)
        beliefs.append("1" if belief_draw < believing_probability else "0")
        drawn = rng.random("correction-acceptance-v1", 7, "SRC-TEST", agent_id, message_id)
        acceptances.append("1" if drawn < 0.95 else "0")
    fixture.call(
        "many_draws_of_a_claim_and_a_correction_are_python_s",
        world=trusting,
        agent_id=agent_id,
        believing=fields(believing),
        accepting=fields(accepting),
        beliefs="".join(beliefs),
        acceptances="".join(acceptances),
    )

    edited.seed = BIG_SEED
    rng = CounterRNG(edited.seed)
    beliefs = []
    for number in range(BIG_SEED_DRAWS):
        drawn = rng.random("rumor-belief-acceptance-v1", 7, agent_id, f"claim-{number}")
        beliefs.append("1" if drawn < 0.5 else "0")
    fixture.call(
        "a_claim_s_draw_keys_a_seed_past_64_bits_as_python_does",
        world=fixture.world("trusting_big_seed", edited, controller),
        agent_id=agent_id,
        believing=fields(believing),
        beliefs="".join(beliefs),
    )


# ---- The file -----------------------------------------------------------------------------------


def handler_rules_fixture() -> dict[str, Any]:
    """`handler_rules.json`: the generated Scenario at minute 5 (`generated`, whole), the small
    replay's world of an operation (`operation_cleaning`, whole), the worlds the pytest files
    edited of them, and the calls and cases of their tests."""
    fixture = Calls()
    world, controller = _generated()
    base = fixture.world("generated", world, controller, full=True)
    fixture.canonical_digest(base, world)
    _toilet_request(fixture, base, world, controller)
    _head(fixture)
    _toilet_completion(fixture, world, controller)
    _care_transitions(fixture, base, world, controller)
    _seeks(fixture)
    _timeouts(fixture, world, controller)
    _support_ends(fixture)
    _receipts_of_a_tick(fixture)
    _arrival_and_home(fixture)
    _rest_texts(fixture, base, world)
    _cares(fixture)
    _ingest(fixture, world, controller)
    _socials(fixture, world, controller)
    _operations(fixture)
    _draws(fixture, world, controller)
    return fixture.document()


__all__ = ["changes", "handler_rules_fixture"]
