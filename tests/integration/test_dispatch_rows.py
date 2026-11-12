"""The Rust dispatch phase: the Python phase's order, records and errors.

The phase has no Python step left (lane L10), and since lane P9-1c no Python fallback: it is the
core's (native/core/src/rules/dispatch.rs), and a handler it does not know is run through its
`apply` by the host. Each case runs events through it and compares everything saved with what the
Python step saved from the same events: a digest of it, taken once from that step (45a9bc9) before
it went, pinned on every OS, as the gate's hashes are. See docs/rust-port/lanes/m3-dispatch.md.
"""

from __future__ import annotations

import hashlib
import json
from collections import Counter
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest
from support.paths import SCENARIO_100_AGENTS

from sheltersim import native
from sheltersim.core.engine import TickEngine
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.world import WorldState
from sheltersim.events.handlers import (
    AgentTravelCompletionHandler,
    SocialInteractionHandler,
)
from sheltersim.events.models import (
    EventApplicationError,
    EventKind,
    EventLogRecord,
    EventStatus,
    EventSuppressed,
    ScheduledEvent,
)
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.inspection.traces import RunArtifactWriter


class Sink:
    """What the controller writes to, which these tests don't read."""

    def __getattr__(self, name: str) -> Callable[..., None]:
        return lambda *arguments, **options: None


def _counts() -> Counter[str]:
    """The dispatch's counts, with each handler kernel's refusals as `refused.<label>`."""
    counts, reasons = native.rules("dispatch").stats()
    counted = Counter(counts)
    for reason, times in reasons.items():
        label = reason.split(":", 1)[0]
        counted[f"refused.{label}"] += times
    return counted


def _new_controller() -> OfflineSimulationController:
    world = WorldState.from_scenario(ScenarioSpec.load(SCENARIO_100_AGENTS))
    world.duration_minutes = 600
    return OfflineSimulationController(world, Sink())


def _event(name: str, sequence: int, event_type: str = "probe", **options: Any) -> ScheduledEvent:
    return ScheduledEvent(
        event_id=name,
        kind=options.pop("kind", EventKind.ENDOGENOUS),
        due_minute=options.pop("due", 0),
        priority=options.pop("priority", 0),
        sequence=sequence,
        event_type=event_type,
        payload=options.pop("payload", {}),
        source_id=options.pop("source_id", "probe-1"),
        causation_id=None,
        provenance=options.pop("provenance", {"source_type": "assumption"}),
    )


def _document(world: Any) -> Any:
    """What a walk writes, read in a world that may not be valid."""
    agent = world.agents["A001"]
    return (
        (agent.zone_id, agent.activity, agent.target_id),
        list(world.active_tasks),
        [event.event_id for event in world.event_queue.pending()],
        len(world.event_log),
        dict(world.id_counters),
        world._core.entities().verify(world),
    )


def _saved(controller: OfflineSimulationController) -> dict[str, Any]:
    world = controller.world
    return {
        "records": [item.decode() for item in world.event_log.items_bytes(0, len(world.event_log))],
        "pending": [event.to_dict() for event in world.event_queue.pending()],
        "counters": dict(world.id_counters),
        "world": world.canonical_json(),
    }


def _is_python_s(controller: OfflineSimulationController, python_saved: str) -> None:
    """All the controller's world saved is what the Python step saved: `python_saved`, the digest
    of `_saved`."""
    text = json.dumps(_saved(controller), sort_keys=True)
    assert hashlib.sha256(text.encode("utf-8")).hexdigest()[:16] == python_saved


def _dispatched(
    arrange: Callable[[OfflineSimulationController], Any],
    python_saved: str,
    minute: int = 0,
) -> OfflineSimulationController:
    """The arrangement run by the kernel: all it saved is what the Python step saved."""
    controller = _new_controller()
    arrange(controller)
    before = _counts()["phases_ran"]
    controller.world.minute_index = minute
    controller._run_dispatch_phase(controller.world, minute)
    assert _counts()["phases_ran"] == before + 1, "the kernel did not run"
    _is_python_s(controller, python_saved)
    return controller


class Recording:
    """A Python handler: says what it was given, and does what a test asks of it."""

    def __init__(self, seen: list[str], then: Callable[[Any, Any], Any] | None = None) -> None:
        self.seen = seen
        self.then = then

    def apply(self, world: Any, event: Any) -> Any:
        self.seen.append(event.event_id)
        return self.then(world, event) if self.then is not None else None


def test_events_run_by_minute_priority_and_sequence_and_are_recorded_in_that_order() -> None:
    orders: list[list[str]] = []

    def arrange(controller: OfflineSimulationController) -> None:
        seen: list[str] = []
        orders.append(seen)
        controller.event_dispatcher.handlers["probe"] = Recording(seen)
        world = controller.world
        for name, sequence, priority, due in (
            ("c", 903, 0, 0),
            ("a", 901, 5, 0),
            ("b", 902, 0, 0),
            ("late", 904, 0, 1),
            ("first", 905, -1, 0),
        ):
            world.schedule_event(_event(name, sequence, priority=priority, due=due))

    by_kernel = _dispatched(arrange, "f889ba9eceb028b3")
    assert orders == [["first", "b", "c", "a"]]
    assert [event.event_id for event in by_kernel.world.event_queue.pending()][:1] != ["first"]
    assert by_kernel.world.event_queue.find("late") is not None


def test_a_snapshot_entry_that_was_cancelled_is_skipped_with_no_record() -> None:
    def arrange(controller: OfflineSimulationController) -> None:
        def cancel(world: Any, event: Any) -> None:
            world.event_queue.remove("second")

        controller.event_dispatcher.handlers["probe"] = Recording([], cancel)
        controller.world.schedule_event(_event("first", 901))
        controller.world.schedule_event(_event("second", 902))

    by_kernel = _dispatched(arrange, "79bb05deffe0dbb1")
    assert by_kernel.world.event_log.find("second") is None


@pytest.mark.parametrize(
    ("same_values", "python_saved"), [(True, "94eaff10eddb7b43"), (False, "d68e579eb45e5653")]
)
def test_a_snapshot_entry_that_was_replaced_runs_only_if_it_is_the_same_event(
    same_values: bool, python_saved: str
) -> None:
    orders: list[list[str]] = []

    def arrange(controller: OfflineSimulationController) -> None:
        seen: list[str] = []
        orders.append(seen)

        def replace(world: Any, event: Any) -> None:
            if event.event_id == "first":
                due = 0 if same_values else 7
                world.event_queue.replace("second", _event("second", 902, due=due))

        controller.event_dispatcher.handlers["probe"] = Recording(seen, replace)
        controller.world.schedule_event(_event("first", 901))
        controller.world.schedule_event(_event("second", 902))

    by_kernel = _dispatched(arrange, python_saved)
    assert orders == [["first", "second"] if same_values else ["first"]]
    assert (by_kernel.world.event_log.find("second") is not None) is same_values


def test_an_event_type_without_a_handler_fails_with_python_s_text() -> None:
    by_kernel = _dispatched(
        lambda controller: controller.world.schedule_event(_event("x", 901)), "3363a241c396b9b0"
    )
    record = by_kernel.world.event_log.find("x")
    assert record.status is EventStatus.FAILED
    assert record.detail == "no handler registered for event type 'probe'"


@pytest.mark.parametrize(
    ("error", "status", "python_saved"),
    [
        (EventSuppressed, EventStatus.SUPPRESSED, "38d488adb5fe3afe"),
        (EventApplicationError, EventStatus.FAILED, "7f99fe7b25b8cd0b"),
    ],
)
def test_a_python_handler_s_expected_errors_are_its_outcome(
    error: type[Exception], status: EventStatus, python_saved: str
) -> None:
    def refuse(world: Any, event: Any) -> None:
        raise error("the reason")

    def arrange(controller: OfflineSimulationController) -> None:
        controller.event_dispatcher.handlers["probe"] = Recording([], refuse)
        controller.world.schedule_event(_event("x", 901))

    by_kernel = _dispatched(arrange, python_saved)
    record = by_kernel.world.event_log.find("x")
    assert (record.status, record.detail) == (status, "the reason")


def test_a_handler_s_other_error_ends_the_phase_and_does_not_run_it_twice() -> None:
    controller = _new_controller()
    seen: list[str] = []

    def fail(world: Any, event: Any) -> None:
        raise KeyError("a bug")

    controller.event_dispatcher.handlers["probe"] = Recording(seen, fail)
    controller.world.schedule_event(_event("x", 901))
    with pytest.raises(KeyError, match="a bug"):
        controller._run_dispatch_phase(controller.world, 0)
    assert seen == ["x"]


def test_a_handler_s_own_record_and_follow_ups_are_compared_and_appended_as_in_python() -> None:
    def transactional(world: Any, event: Any) -> tuple[Any, ...]:
        world.event_log.append(EventLogRecord(event, 0, EventStatus.APPLIED))
        return (EventLogRecord(_event("follow", 950), 0, EventStatus.APPLIED),)

    def arrange(controller: OfflineSimulationController) -> None:
        controller.world.id_counters["event"] = 1000
        controller.event_dispatcher.handlers["probe"] = Recording([], transactional)
        controller.world.schedule_event(_event("x", 901))

    by_kernel = _dispatched(arrange, "c13e322e9bf6693c")
    assert by_kernel.world.event_log.find("follow") is not None


def test_a_record_that_disagrees_with_the_outcome_raises_python_s_error() -> None:
    controller = _new_controller()

    def wrong(world: Any, event: Any) -> None:
        world.event_log.append(EventLogRecord(event, 0, EventStatus.FAILED))

    controller.event_dispatcher.handlers["probe"] = Recording([], wrong)
    controller.world.schedule_event(_event("x", 901))
    with pytest.raises(ValueError, match="transactional event record disagrees"):
        controller._run_dispatch_phase(controller.world, 0)


# ---- The table -----------------------------------------------------------------------------------


def _social_action(controller: OfflineSimulationController, sequence: int = 901) -> ScheduledEvent:
    world = controller.world
    first, second = "A001", "A002"
    world.agents[second].zone_id = world.agents[first].zone_id
    return ScheduledEvent(
        event_id=f"social-{sequence}",
        kind=EventKind.ENDOGENOUS,
        due_minute=0,
        priority=0,
        sequence=sequence,
        event_type="social_interaction",
        payload={
            "interaction_type": "reassurance_exchange",
            "agent_id": first,
            "participant_agent_id": second,
            "zone_id": world.agents[first].zone_id,
            "topic_id": "emotional_check_in",
            "action": "offer_reassurance",
            "reason_short": "autonomous_need_choice",
            "interaction_duration_minutes": 5,
        },
        source_id="social-policy-1",
        causation_id=None,
        provenance={
            "source_type": "assumption",
            "source_id": "ASM-EVENT-TRIGGERED-SOCIAL-INTERACTIONS-V1",
            "policy_version": "social-policy-1",
        },
    )


def test_a_social_action_runs_in_rust_with_python_s_results() -> None:
    before = _counts()
    by_kernel = _dispatched(
        lambda controller: controller.world.schedule_event(_social_action(controller)),
        "db670ea5268dbb27",
    )
    after = _counts()
    assert after["ran.social_interaction.apply"] > before["ran.social_interaction.apply"]
    assert after["settled_rows"] > before["settled_rows"]
    assert after["events_foreign"] == before["events_foreign"]
    assert by_kernel.world.event_log.find("social-901").status is EventStatus.APPLIED


def test_a_suppressed_social_action_has_python_s_detail_in_its_record() -> None:
    text = "social action participants are no longer co-located"

    def arrange(controller: OfflineSimulationController) -> None:
        event = _social_action(controller)
        world = controller.world
        zones = [zone for zone in world.zones if zone != world.agents["A001"].zone_id]
        world.agents["A002"].zone_id = zones[0]
        world.schedule_event(event)

    before = _counts()
    by_kernel = _dispatched(arrange, "f1b5249381ba1d0c")
    after = _counts()
    record = by_kernel.world.event_log.find("social-901")
    assert (record.status, record.detail) == (EventStatus.SUPPRESSED, text)
    assert after["outcomes_suppressed"] == before["outcomes_suppressed"] + 1
    assert after["refused.social_interaction.apply"] == before["refused.social_interaction.apply"]


def _social_action_with(controller: OfflineSimulationController, **change: Any) -> ScheduledEvent:
    event = _social_action(controller)
    return ScheduledEvent(
        event_id=event.event_id,
        kind=event.kind,
        due_minute=0,
        priority=0,
        sequence=event.sequence,
        event_type=event.event_type,
        payload={**event.payload_dict(), **change},
        source_id=event.source_id,
        causation_id=None,
        provenance=dict(event.provenance),
    )


def test_a_failed_social_action_has_python_s_detail_from_the_kernel() -> None:
    def arrange(controller: OfflineSimulationController) -> None:
        controller.world.schedule_event(_social_action_with(controller, reason_short="other"))

    before = _counts()
    by_kernel = _dispatched(arrange, "b0d258951174919c")
    after = _counts()
    assert after["refused.social_interaction.apply"] == before["refused.social_interaction.apply"]
    assert after["events_foreign"] == before["events_foreign"]
    record = by_kernel.world.event_log.find("social-901")
    assert record.status is EventStatus.FAILED
    assert record.detail == "social action reason code is not allowed"


class _Code(str):
    """A str whose comparison the kernel doesn't make: a payload is JSON, so no Run holds one."""


def test_an_event_no_row_holds_stops_the_phase_with_kernel_refusal_before_any_event() -> None:
    """A value outside the contract (a str subclass: no JSON payload holds one) leaves the event
    with no row, and the core runs only rows (H6 of design-p9.md, lane P9-1c): the phase raises
    KernelRefusal before its first event, with nothing changed."""
    controller = _new_controller()
    event = _social_action_with(controller, reason_short=_Code("other"))
    controller.world.schedule_event(event)
    before = _saved(controller)
    with pytest.raises(
        KernelRefusal, match="dispatch.run_phase refused: an event a row cannot hold"
    ):
        controller._run_dispatch_phase(controller.world, 0)
    assert controller.world.event_log.find("social-901") is None
    assert _saved(controller) == before


def test_a_patch_of_a_table_handler_s_apply_is_called(monkeypatch: pytest.MonkeyPatch) -> None:
    controller = _new_controller()
    event = _social_action(controller)
    controller.world.schedule_event(event)
    seen: list[str] = []
    monkeypatch.setattr(
        SocialInteractionHandler,
        "apply",
        lambda self, world, due: seen.append(due.event_id),
    )
    controller._run_dispatch_phase(controller.world, 0)
    assert seen == [event.event_id]


def test_a_fake_handler_of_a_table_s_event_type_is_called() -> None:
    controller = _new_controller()
    event = _social_action(controller)
    controller.world.schedule_event(event)
    seen: list[str] = []
    controller.event_dispatcher.handlers["social_interaction"] = Recording(seen)
    controller._run_dispatch_phase(controller.world, 0)
    assert seen == [event.event_id]


def test_an_effect_program_s_guard_is_python_s_to_read() -> None:
    controller = _new_controller()
    event = _social_action(controller)
    controller.world.schedule_event(
        ScheduledEvent(
            event_id=event.event_id,
            kind=event.kind,
            due_minute=0,
            priority=0,
            sequence=event.sequence,
            event_type=event.event_type,
            payload=event.payload_dict(),
            source_id=event.source_id,
            causation_id=None,
            provenance={**dict(event.provenance), "effect_program_v1": {"guard": None}},
        )
    )
    before = _counts()
    controller._run_dispatch_phase(controller.world, 0)
    assert _counts()["guard_passed"] == before["guard_passed"] + 1
    assert _counts()["events_foreign"] == before["events_foreign"]
    assert controller.world.event_log.find(event.event_id).status is EventStatus.APPLIED


def test_a_dispatcher_of_another_kind_is_refused_with_nothing_changed() -> None:
    """The phase has no Python step (lane L10): a dispatcher the kernel does not know is outside
    its contract, as the controller makes an EventDispatcher."""
    controller = _new_controller()

    class Other(type(controller.event_dispatcher)):
        pass

    other = Other(dict(controller.event_dispatcher.handlers))
    controller.event_dispatcher = other
    controller.world.schedule_event(_event("x", 901))
    saved = _saved(controller)
    before = _counts()
    with pytest.raises(
        KernelRefusal, match="^dispatch.run_phase refused: another kind of dispatcher$"
    ):
        controller._run_dispatch_phase(controller.world, 0)
    after = _counts()
    assert _saved(controller) == saved
    assert after["phases_refused"] == before["phases_refused"] + 1
    assert after["phases_ran"] == before["phases_ran"]


# ---- A walk --------------------------------------------------------------------------------------


def _walk(
    controller: OfflineSimulationController,
    purpose: str,
    facility_id: str | None,
    destination: str,
    agent_id: str = "A001",
) -> ScheduledEvent:
    world = controller.world
    agent = world.agents[agent_id]
    route = controller.space.route(agent.zone_id, destination, agent.id)
    assert route is not None and route.travel_minutes > 0
    assert controller._schedule_travel(
        agent_id=agent.id,
        route=route,
        purpose=purpose,
        facility_id=facility_id,
        minute=world.minute_index,
        validate_state=False,
    )
    return next(
        event
        for event in world.event_queue.pending()
        if event.event_type == "agent_travel_complete" and event.payload["agent_id"] == agent_id
    )


def _facility_elsewhere(controller: OfflineSimulationController, kind: str) -> Any:
    world = controller.world
    zone = world.agents["A001"].zone_id
    return next(
        facility
        for facility in world.facilities.values()
        if facility.kind == kind and facility.zone_id != zone
    )


def _zone_elsewhere(controller: OfflineSimulationController) -> str:
    world = controller.world
    zone = world.agents["A001"].zone_id
    return next(name for name in world.zones if name != zone)


def _return_walk(controller: OfflineSimulationController) -> ScheduledEvent:
    return _walk(controller, "return", None, _zone_elsewhere(controller))


def _toilet_walk(controller: OfflineSimulationController) -> ScheduledEvent:
    facility = _facility_elsewhere(controller, "toilet")
    return _walk(controller, "toilet_request", facility.id, facility.zone_id)


@pytest.mark.parametrize(
    ("walk", "python_saved"),
    [(_return_walk, "e7cc4860a63dc8ea"), (_toilet_walk, "d071ac8765838790")],
)
def test_a_walk_ends_in_rust_with_python_s_results(
    walk: Callable[[OfflineSimulationController], ScheduledEvent], python_saved: str
) -> None:
    def arrange(controller: OfflineSimulationController) -> None:
        walk(controller)

    before = _counts()
    by_kernel = _dispatched(arrange, python_saved, minute=_due_of(walk))
    after = _counts()
    assert after["ran.travel_completion.apply"] == before["ran.travel_completion.apply"] + 1
    assert after["refused.travel_completion.apply"] == before["refused.travel_completion.apply"]
    assert by_kernel.world.agents["A001"].target_id is None or walk is _toilet_walk


def _due_of(walk: Callable[[OfflineSimulationController], ScheduledEvent]) -> int:
    return walk(_new_controller()).due_minute


def test_a_walk_s_request_that_fails_is_recorded_with_its_text_as_in_python(
    monkeypatch: pytest.MonkeyPatch,
) -> None:
    def refuse(*arguments: Any, **options: Any) -> None:
        raise ValueError("the toilet refuses")

    def arrange(controller: OfflineSimulationController) -> None:
        _toilet_walk(controller)
        monkeypatch.setattr(controller.toilet_service, "request", refuse)

    before = _counts()
    by_kernel = _dispatched(arrange, "f70d12763e33305a", minute=_due_of(_toilet_walk))
    after = _counts()
    assert after["outcomes_failed"] == before["outcomes_failed"] + 1
    record = next(
        record
        for record in by_kernel.world.event_log.records
        if record.event.event_type == "agent_travel_complete"
    )
    assert (record.status, record.detail) == (EventStatus.FAILED, "the toilet refuses")
    assert by_kernel.world.agents["A001"].activity.value == "idle"


def _stop_walking(world: Any, event: Any) -> None:
    agent = world.agents["A001"]
    agent.activity = type(agent.activity).IDLE


# Each spoiled walk with the deleted Python rule's text, written down from it.
SPOILED_WALKS: list[tuple[Callable[[Any, Any], None], str]] = [
    (
        lambda world, event: world.active_tasks.pop(event.payload["task_id"]),
        "travel completion does not match an active task",
    ),
    (
        lambda world, event: setattr(world.agents["A001"], "target_id", "x"),
        "return travel completion does not match its target",
    ),
    (
        lambda world, event: setattr(
            world.agents["A001"], "activity", type(world.agents["A001"].activity).IDLE
        ),
        "travel completion Agent does not match its active task",
    ),
]


@pytest.mark.parametrize(("spoil", "text"), SPOILED_WALKS)
def test_a_walk_that_does_not_fit_changes_nothing_and_says_the_rule_s_text(
    spoil: Callable[[Any, Any], None], text: str
) -> None:
    controller = _new_controller()
    world = controller.world
    event = _return_walk(controller)
    world.minute_index = event.due_minute
    spoil(world, event)
    handler = controller.event_dispatcher.handlers["agent_travel_complete"]
    before = _document(world)
    with pytest.raises(EventApplicationError) as made:
        native.rules("travel_completion").apply(handler, world, event)
    assert str(made.value) == text
    assert _document(world) == before
    with pytest.raises(EventApplicationError) as made:
        AgentTravelCompletionHandler.apply(handler, world, event)
    assert str(made.value) == text
    assert _document(world) == before


@pytest.mark.parametrize(
    ("purpose", "text"),
    [
        ("sleep_bed", "sleep-bed travel does not match its live reservation"),
        ("presence_relocation", "presence relocation completion does not match its route"),
        ("another", "unsupported travel purpose 'another'"),
    ],
)
def test_a_walk_home_named_for_another_purpose_fails_with_the_rule_s_text_before_any_change(
    purpose: str, text: str
) -> None:
    controller = _new_controller()
    world = controller.world
    event = _return_walk(controller)
    payload = {**event.payload_dict(), "purpose": purpose}
    other = _event(
        "walk-other",
        event.sequence + 5000,
        "agent_travel_complete",
        due=event.due_minute,
        payload=payload,
    )
    handler = controller.event_dispatcher.handlers["agent_travel_complete"]
    world.minute_index = event.due_minute
    before = _document(world)
    with pytest.raises(EventApplicationError) as made:
        native.rules("travel_completion").apply(handler, world, other)
    assert _document(world) == before
    assert str(made.value) == text


def test_a_walk_with_a_value_the_writer_refuses_is_refused_by_the_phase_and_alone() -> None:
    """A payload value the event writer refuses (a str subclass: no JSON payload holds one). The
    handler's own entry, with no Python rule behind it (lane L10), refuses it with nothing changed;
    so does the phase, which runs only rows (lane P9-1c), before its first event."""

    class Text(str):
        pass

    def noted(controller: OfflineSimulationController) -> ScheduledEvent:
        event = _return_walk(controller)
        world = controller.world
        world.event_queue.remove(event.event_id)
        noted = ScheduledEvent(
            event_id=event.event_id,
            kind=event.kind,
            due_minute=event.due_minute,
            priority=event.priority,
            sequence=event.sequence,
            event_type=event.event_type,
            payload={**event.payload_dict(), "note": Text("a")},
            source_id=event.source_id,
            causation_id=event.causation_id,
            provenance=dict(event.provenance),
        )
        world.schedule_event(noted)
        world.minute_index = event.due_minute
        return noted

    alone = _new_controller()
    event = noted(alone)
    handler = alone.event_dispatcher.handlers["agent_travel_complete"]
    before = _document(alone.world)
    with pytest.raises(KernelRefusal, match="the event has no row"):
        AgentTravelCompletionHandler.apply(handler, alone.world, event)
    assert _document(alone.world) == before

    controller = _new_controller()
    event = noted(controller)
    before = _document(controller.world)
    with pytest.raises(
        KernelRefusal, match="dispatch.run_phase refused: an event a row cannot hold"
    ):
        controller._run_dispatch_phase(controller.world, event.due_minute)
    assert _document(controller.world) == before
    assert controller.world.event_log.find(event.event_id) is None


def test_a_walk_that_has_only_a_row_gets_its_object_only_if_python_reads_it() -> None:
    controller = _new_controller()
    state = native.state("events")
    event = _return_walk(controller)
    due = event.due_minute
    controller.world.event_queue.remove(event.event_id)
    state.schedule_row(controller.world, event)
    controller.world.minute_index = due
    before = _counts()
    controller._run_dispatch_phase(controller.world, due)
    after = _counts()
    assert after["ran.travel_completion.apply"] == before["ran.travel_completion.apply"] + 1
    assert after["settled_rows"] == before["settled_rows"] + 1
    _is_python_s(controller, "e7cc4860a63dc8ea")


# ---- A run ---------------------------------------------------------------------------------------


def _run(scenario: Any, directory: Path, minutes: int) -> dict[str, Any]:
    world = WorldState.from_scenario(scenario)
    artifacts = RunArtifactWriter(scenario, directory)
    artifacts.write_initial(world, execution_mode="offline")
    controller = OfflineSimulationController(world, artifacts)
    TickEngine(world, phase_handler=controller).run(minutes)
    return {
        "digest": world.state_digest(),
        "records": [raw.decode() for raw in world.event_log.items_bytes(0, len(world.event_log))],
        "counters": dict(world.id_counters),
    }


def test_a_run_through_the_kernel_is_what_the_python_step_made(
    generated_scenario: Any, tmp_path: Path
) -> None:
    """240 minutes through the kernel: what the Python step made of them, its digest taken from
    that step."""
    before = _counts()
    direct = _run(generated_scenario, tmp_path / "direct", 240)
    after = _counts()
    assert after["phases_ran"] - before["phases_ran"] == 240
    assert after["events_core"] > before["events_core"]
    assert after["events_foreign"] == before["events_foreign"]
    assert not [
        label for label in after if label.startswith("refused.") and after[label] != before[label]
    ]
    text = json.dumps(direct, sort_keys=True)
    assert hashlib.sha256(text.encode("utf-8")).hexdigest()[:16] == "9bea40ea3b5cd8b4"
