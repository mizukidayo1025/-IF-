"""The test-only tracked mirror names the minute, phase and namespace of a write that goes astray.

Rust will own model_state and unported Python will write to it through tracked containers
(docs/rust-port/lanes/p1-tracked-mirror.md). A whole Run on these containers needs the Python rules
the kernels leave them to, so as lane L10 deletes those rules, a Run is no longer compared here.
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

import pytest
from support.tracked_mirror import Drift, Session, TrackedDict, TrackedList, mirror_of

from sheltersim.core.engine import TickEngine, TickPhase
from sheltersim.core.world import WorldState


def test_a_world_keeps_tracked_documents_through_copy_and_swap(generated_scenario) -> None:
    session = Session()
    with session:
        world = WorldState.from_scenario(generated_scenario)
        candidate = WorldState.from_dict(world.to_dict())
        assert isinstance(candidate.model_state, TrackedDict)
        candidate.model_state["probe"] = {"values": []}
        candidate.model_state["probe"]["values"].append(1)
        world.__dict__.clear()
        world.__dict__.update(candidate.__dict__)

        # The swap brought the candidate's tree, with the shadow its writes built.
        assert isinstance(world.model_state["probe"]["values"], TrackedList)
        assert mirror_of(world.model_state).shadow["probe"] == {"values": [1]}
        assert mirror_of(world.model_state).difference() is None


def _engine_with(scenario: Any, write: Callable[[WorldState], None]) -> TickEngine:
    def handler(phase: TickPhase, world: WorldState, minute: int, _rng: Any) -> None:
        if minute == 3 and phase is TickPhase.UPDATE_AGENT_STATE:
            write(world)
        # Taken out again before the end of the tick, which validates the namespaces.
        if phase is TickPhase.COLLECT_METRICS:
            world.model_state.pop("probe", None)

    return TickEngine(WorldState.from_scenario(scenario), phase_handler=handler)


def _alias_write(world: WorldState) -> None:
    record = world.model_state["probe"] = {"count": 0}
    record["count"] = 1


def test_a_write_through_a_plain_alias_is_named_by_minute_phase_and_namespace(
    generated_scenario,
) -> None:
    plain = Session(tracked=False, record=True)
    with plain:
        _engine_with(generated_scenario, _alias_write).run(steps=5)

    session = Session(expect=plain.stream)
    with session, pytest.raises(Drift, match="minute 3 phase update_agent_state model_state/probe"):
        _engine_with(generated_scenario, _alias_write).run(steps=5)

    assert any(
        "test_tracked_mirror.py" in site
        for site in session.findings["plain_alias_changed_after_insert"]
    )


def test_a_write_past_the_mutators_is_named_by_minute_phase_and_namespace(
    generated_scenario,
) -> None:
    session = Session()
    with session, pytest.raises(Drift, match="minute 3 phase update_agent_state model_state"):
        _engine_with(
            generated_scenario, lambda world: dict.__setitem__(world.model_state, "probe", 1)
        ).run(steps=5)
