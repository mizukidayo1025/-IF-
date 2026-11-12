"""A walk in a queue that left its reservation fails with the travel rule's own text, unchanged.

`AgentTravelCompletionHandler.apply` raises `EventApplicationError` when a walk's event does not
match its task, its person, or what its purpose needs; the dispatcher saves the text as the event's
`detail`. The core's tests check each of these texts, in the rule's order of checks
(`native/core/tests/travel_completion_texts.rs`, #225). This file keeps the walks in a queue: their
worlds are runs of the generated Scenario until the first queue walk, too large to keep as the
core's fixtures. They go through the handler's `kernel_only` entry, whose kernel
(rules/travel_completion.rs) must raise the text itself, before any change (support/world.py
`kernel_says_python_s_text`). The Python rule is gone (lane L10): the text is written down from it
(docs/rust-port/lanes/l8-texts-events.md).
"""

from __future__ import annotations

from typing import Any

import pytest
from support.world import kernel_says_python_s_text

from sheltersim.core.engine import TickEngine
from sheltersim.core.world import WorldState
from sheltersim.events.models import EventApplicationError, ScheduledEvent
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.timeline import _MetricSink

_GONE = object()
QUEUE_PURPOSES = {
    "toilet_queue_position",
    "resource_queue_position",
    "toilet_queue_service",
    "resource_queue_service",
}


def _controller(scenario: Any) -> OfflineSimulationController:
    world = WorldState.from_scenario(scenario)
    world.duration_minutes = max(world.duration_minutes, 600)
    return OfflineSimulationController(world, _MetricSink(MetricsRecorder()))


def _variant(event: ScheduledEvent, payload: dict[str, Any]) -> Any:
    """The event with some payload values changed (`_GONE` drops one)."""
    changed = {**event.payload_dict()}
    for key, value in payload.items():
        if value is _GONE:
            changed.pop(key, None)
        else:
            changed[key] = value
    return ScheduledEvent(
        event_id=event.event_id,
        kind=event.kind,
        due_minute=event.due_minute,
        priority=event.priority,
        sequence=event.sequence,
        event_type=event.event_type,
        payload=changed,
        source_id=event.source_id,
        causation_id=event.causation_id,
        provenance=dict(event.provenance),
    )


def _says(handler: Any, world: WorldState, event: Any, text: str) -> None:
    """The handler fails the walk with `text` and changes nothing: the kernel says it."""
    expected = (EventApplicationError, text)
    assert kernel_says_python_s_text(handler, world, event, expected) == (
        "EventApplicationError",
        text,
    )


def _queue_walk(
    world: WorldState, engine: TickEngine, facilities: set[str] | None, ending: str
) -> Any:
    """The first walk in a queue the run schedules whose purpose ends so, at one of `facilities`."""
    for _ in range(300):
        engine.step()
        for event in world.event_queue.pending():
            if (
                event.event_type == "agent_travel_complete"
                and event.payload.get("purpose") in QUEUE_PURPOSES
                and event.payload["purpose"].endswith(ending)
                and (facilities is None or event.payload.get("facility_id") in facilities)
            ):
                return event
    pytest.fail("no walk in a queue was scheduled")


@pytest.mark.parametrize("ending", ["_position", "_service"])
@pytest.mark.parametrize("split", [False, True])
def test_a_queue_walk_that_left_its_reservation_fails_its_fifo_check(
    split: bool, ending: str, generated_scenario: Any, generated_scenario_with_split_queues: Any
) -> None:
    scenario = generated_scenario_with_split_queues if split else generated_scenario
    controller = _controller(scenario)
    world = controller.world
    engine = TickEngine(world, phase_handler=controller)
    facilities = {"toilet_01", "water_station_main"} if split else None
    event = _queue_walk(world, engine, facilities, ending)
    handler = controller.event_dispatcher.handlers["agent_travel_complete"]
    text = "physical queue travel does not match its FIFO reservation"
    agent_id = event.payload["agent_id"]
    facility = world.facilities[event.payload["facility_id"]]
    elsewhere = next(zone for zone in sorted(world.zones) if zone != facility.zone_id)
    _says(handler, world, _variant(event, {"destination_zone_id": elsewhere}), text)
    _says(handler, world, _variant(event, {"facility_id": _GONE}), text)
    world.agents[agent_id].target_id = "somewhere else"
    _says(handler, world, event, text)
    world.agents[agent_id].target_id = facility.id
    if split:
        assert isinstance(event.payload.get("request_id"), str)
        # A request the queue no longer holds.
        _says(handler, world, _variant(event, {"request_id": "nothing"}), text)
