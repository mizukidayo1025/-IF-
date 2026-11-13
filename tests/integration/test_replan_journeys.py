"""The Python entry of `_replan_journeys` (native/src/rules/replan.rs), whose core step is
`rules::tick::update::replan_journeys`.

The step's branches are checked without Python in `native/core/tests/replan_journeys.rs`, on the
worlds these tests built (`support/core_fixtures_movement.py`, #225). Here are the entry's: what it
turns down before the first stop (a KernelRefusal, nothing changed) and after it (a RuntimeError,
the stop written), and the driver over ticks that really replan.
"""

from __future__ import annotations

import hashlib
from typing import Any

import pytest
from support.core_fixtures_movement import (
    FIRST_LEG,
    controller_of,
    replan_world,
    walk_to_the_toilet,
    with_alternate_route,
)

from sheltersim import native
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.world import WorldState
from sheltersim.domain.movement import MOVEMENT_STATE_KEY as KEY
from sheltersim.events.models import EventStatus


def _bytes(world: WorldState) -> str:
    return world._canonical_text(world._canonical_data())


def _records(world: WorldState) -> list[Any]:
    return [
        (
            record.event.event_id,
            record.event.sequence,
            record.event.causation_id,
            record.realized_minute,
            record.status.value,
            record.detail,
            sorted(record.event.payload.items(), key=lambda item: item[0]),
        )
        for record in world.event_log.records
    ]


def _pending(world: WorldState) -> list[Any]:
    return [
        (
            event.event_id,
            event.sequence,
            event.due_minute,
            event.source_id,
            event.causation_id,
            sorted(event.payload.items(), key=lambda item: item[0]),
            sorted(event.provenance.items(), key=lambda item: item[0]),
        )
        for event in world.event_queue.pending()
    ]


# ---- What the kernel turns down ------------------------------------------------------------------


def test_a_refusal_before_the_first_stop_changes_nothing(generated_scenario: Any) -> None:
    world = replan_world(generated_scenario)
    with_alternate_route(world)
    walk_to_the_toilet(world)
    world.zone_edges[FIRST_LEG].closed = True
    before = (_bytes(world), _pending(world), _records(world))
    controller = controller_of(world)
    with pytest.raises(KernelRefusal, match="^replan.replan_journeys refused: "):
        controller._replan_journeys(world.model_state[KEY], ("A001",), "1")
    assert (_bytes(world), _pending(world), _records(world)) == before


def test_a_refusal_after_a_stop_is_an_error(generated_scenario: Any) -> None:
    """A stop is written, so what the kernel turns down then cannot leave the world as it was."""
    from sheltersim.domain.models import ZoneState

    world = replan_world(generated_scenario)
    with_alternate_route(world)
    old = dict(walk_to_the_toilet(world))
    world.zone_edges[FIRST_LEG].closed = True
    controller = controller_of(world)
    # A zone the Run's router was not built with: the stop puts the person there, and the new
    # route from it is refused.
    world.zones["annex"] = ZoneState("annex", "annex", None)
    world.model_state[KEY]["positions"]["A001"]["zone_id"] = "annex"
    with pytest.raises(RuntimeError, match="after a replan was written: an unknown zone"):
        controller._replan_journeys(world.model_state[KEY], ("A001",), 1)
    assert "A001" not in world.model_state[KEY]["journeys"]
    assert world.event_log.find(old["event_id"]).status is EventStatus.SUPPRESSED


def test_a_tick_with_a_closed_edge_replans_through_the_driver(generated_scenario: Any) -> None:
    """The driver over ticks that really replan, against the world the phases' Python steps left
    in the same ticks: its digest, recorded when both paths still gave it (#221)."""
    from sheltersim.core.engine import TickEngine

    world = replan_world(generated_scenario)
    world.minute_index = 0
    with_alternate_route(world)
    controller = controller_of(world)
    engine = TickEngine(world, phase_handler=controller)
    ticks = native.rules("tick").stats()[0]["ticks"]
    engine.step()
    walk_to_the_toilet(world, sorted(world.agents)[-1])
    world.zone_edges[FIRST_LEG].closed = True
    controller._refresh_space()
    for _ in range(10):
        engine.step()
    assert native.rules("tick").stats()[0]["ticks"] - ticks == 11
    assert world.metric_state.get("movement_replan_count", 0.0) >= 1.0
    assert hashlib.sha256(_bytes(world).encode("utf-8")).hexdigest()[:16] == "30719c65b82d33f9"
