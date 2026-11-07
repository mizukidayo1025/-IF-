"""The namespaces their Rust rows alone hold, with no Python tree (L14 stage 1).

core/state_mirror.py lists them in DETACHED_NAMESPACES: psychological_distress_v1,
social_propagation_v1 and temporary_spaces_v1. A world loads them straight into their rows; the
tick's kernels read the rows; whole-world readers (the canonical JSON, same_state, validation, a
Checkpoint capture and restore, deepcopy for a fork) read the rows too, and build no tree. A read
from Python builds the tree, a write through it reaches the row, and the next tick drops the tree.
docs/rust-port/step4-plan.md section 5 has the stage and what stage 2 needs.
"""

from __future__ import annotations

import copy
from typing import Any

import pytest

from sheltersim.core import state_mirror
from sheltersim.core.checkpoint import Checkpoint
from sheltersim.core.checkpoint_cache import CheckpointHistoryCache
from sheltersim.core.engine import TickEngine
from sheltersim.core.state_mirror import (
    DETACHED_NAMESPACES,
    DetachedNodeError,
    NamespaceTable,
    TrackedDict,
)
from sheltersim.core.world import StateInvariantError, WorldState
from sheltersim.domain.psychological_state import PSYCHOLOGICAL_STATE_KEY
from sheltersim.domain.temporary_spaces import TEMPORARY_SPACES_STATE_KEY
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.timeline import _MetricSink

SOCIAL = "social_propagation_v1"


def _engine(world: WorldState) -> TickEngine:
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    return TickEngine(world, phase_handler=controller)


def _run(scenario: Any, minutes: int) -> tuple[WorldState, TickEngine]:
    world = WorldState.from_scenario(scenario)
    engine = _engine(world)
    for _ in range(minutes):
        engine.step()
    return world, engine


def _held(world: WorldState, name: str) -> Any:
    return dict.get(world.model_state, name)


def _rebuilds(world: WorldState) -> dict[str, int]:
    return {name: world._core.mirror(name).stats()["rebuilds"] for name in DETACHED_NAMESPACES}


def _all_detached(world: WorldState) -> bool:
    return all(
        _held(world, name) is state_mirror._DETACHED and world._core.mirror(name).is_detached
        for name in DETACHED_NAMESPACES
    )


@pytest.fixture
def ticked(generated_scenario: Any) -> tuple[WorldState, TickEngine]:
    return _run(generated_scenario, 20)


def test_a_run_holds_the_three_namespaces_in_their_rows_alone(
    ticked: tuple[WorldState, TickEngine],
) -> None:
    world, _ = ticked
    assert DETACHED_NAMESPACES == {PSYCHOLOGICAL_STATE_KEY, SOCIAL, TEMPORARY_SPACES_STATE_KEY}
    # The generated Scenario has all three; the social one is made at the first tick.
    assert _all_detached(world)
    assert isinstance(world.model_state, NamespaceTable)
    # The kernels of 20 ticks and their checks read the rows: no tree was built.
    assert _rebuilds(world) == dict.fromkeys(DETACHED_NAMESPACES, 0)


def test_whole_world_readers_read_the_rows_and_build_no_tree(
    ticked: tuple[WorldState, TickEngine],
) -> None:
    world, _ = ticked
    world.validate()
    document = world.to_dict()
    for name in DETACHED_NAMESPACES:
        assert document["model_state"][name] == world._core.mirror(name).document()
    cache = CheckpointHistoryCache()
    checkpoint = Checkpoint.capture(world, history_cache=cache)
    restored = checkpoint.restore_world()
    assert _all_detached(restored)
    assert restored.same_state(world) and world.same_state(restored)
    assert restored.model_state == world.model_state
    assert world.state_digest() == restored.state_digest()
    assert _rebuilds(world) == dict.fromkeys(DETACHED_NAMESPACES, 0)
    assert _rebuilds(restored) == dict.fromkeys(DETACHED_NAMESPACES, 0)


def test_saved_bytes_do_not_depend_on_whether_a_tree_is_held(
    ticked: tuple[WorldState, TickEngine],
) -> None:
    world, _ = ticked
    detached = Checkpoint.capture(world, history_cache=CheckpointHistoryCache())
    for name in DETACHED_NAMESPACES:
        assert isinstance(world.model_state[name], TrackedDict)
    assert type(world.model_state) is NamespaceTable
    attached = Checkpoint.capture(world, history_cache=CheckpointHistoryCache())
    assert attached.state_hash == detached.state_hash
    assert attached.checkpoint_hash == detached.checkpoint_hash
    assert world.canonical_json() == WorldState.from_dict(world.to_dict()).canonical_json()


def test_a_fork_copies_the_rows_and_ticks_on_its_own(
    ticked: tuple[WorldState, TickEngine],
) -> None:
    world, _ = ticked
    before = world.to_dict()
    fork = copy.deepcopy(world)
    assert _all_detached(fork) and _rebuilds(world) == dict.fromkeys(DETACHED_NAMESPACES, 0)
    assert fork.same_state(world)
    fork_engine = _engine(fork)
    for _ in range(5):
        fork_engine.step()
    assert world.to_dict() == before and world.minute_index == 20
    assert fork.minute_index == 25 and _all_detached(fork)
    assert (
        fork.to_dict()["model_state"][PSYCHOLOGICAL_STATE_KEY]
        != before["model_state"][PSYCHOLOGICAL_STATE_KEY]
    )


def test_a_python_write_reaches_the_row_and_the_next_tick_drops_the_tree(
    ticked: tuple[WorldState, TickEngine],
) -> None:
    world, engine = ticked
    mirror = world._core.mirror(PSYCHOLOGICAL_STATE_KEY)
    agents = world.model_state[PSYCHOLOGICAL_STATE_KEY]["agents"]
    assert not mirror.is_detached and mirror.stats()["rebuilds"] == 1
    person = next(iter(agents))
    agents[person]["completed_days"] = 7
    assert mirror.document()["agents"][person]["completed_days"] == 7
    assert mirror.verify()
    engine.step()
    assert mirror.is_detached and _held(world, PSYCHOLOGICAL_STATE_KEY) is state_mirror._DETACHED
    assert (
        state_mirror.read_only_state(world, PSYCHOLOGICAL_STATE_KEY)["agents"][person][
            "completed_days"
        ]
        == 7
    )
    # A node kept from before the tick reads the past and refuses a write.
    with pytest.raises(DetachedNodeError):
        agents[person]["completed_days"] = 8


def test_a_whole_write_loads_the_row_without_a_tree(
    ticked: tuple[WorldState, TickEngine],
) -> None:
    world, _ = ticked
    state = state_mirror.read_only_state(world, TEMPORARY_SPACES_STATE_KEY)
    world.model_state[TEMPORARY_SPACES_STATE_KEY] = state
    assert _held(world, TEMPORARY_SPACES_STATE_KEY) is state_mirror._DETACHED
    assert world._core.mirror(TEMPORARY_SPACES_STATE_KEY).document() == state
    world.validate()


def test_read_only_state_is_a_new_document_a_write_to_it_reaches_nothing(
    ticked: tuple[WorldState, TickEngine],
) -> None:
    world, _ = ticked
    state = state_mirror.read_only_state(world, PSYCHOLOGICAL_STATE_KEY)
    assert type(state) is dict
    state["last_updated_minute"] = -5
    assert state_mirror.read_only_state(world, PSYCHOLOGICAL_STATE_KEY)["last_updated_minute"] != -5
    assert state_mirror.read_only_state(world, "absent_v1", {}) == {}
    assert _all_detached(world)


def _psychology_error(world: WorldState) -> str:
    with pytest.raises(StateInvariantError) as raised:
        world.validate()
    return str(raised.value)


def test_a_validation_error_keeps_its_words_with_the_row_alone(
    ticked: tuple[WorldState, TickEngine],
) -> None:
    world, _ = ticked
    agents = world.model_state[PSYCHOLOGICAL_STATE_KEY]["agents"]
    agents[next(iter(agents))]["long_term_distress"] = 1.5
    with_tree = _psychology_error(world)
    state_mirror.detach_unread(world)
    assert _held(world, PSYCHOLOGICAL_STATE_KEY) is state_mirror._DETACHED
    assert _psychology_error(world) == with_tree
    assert with_tree.startswith("invalid psychological distress state:")
    # The Python validator read a document of the row: the namespace stays detached.
    assert world._core.mirror(PSYCHOLOGICAL_STATE_KEY).is_detached


def test_a_temporary_space_error_keeps_its_words_with_the_row_alone(
    ticked: tuple[WorldState, TickEngine],
) -> None:
    world, _ = ticked
    world.model_state[TEMPORARY_SPACES_STATE_KEY]["policy_version"] = "other"
    with pytest.raises(StateInvariantError) as with_tree:
        world.validate()
    state_mirror.detach_unread(world)
    with pytest.raises(StateInvariantError) as detached:
        world.validate()
    assert str(detached.value) == str(with_tree.value) == "invalid simulator extension state"
    assert str(detached.value.__cause__) == str(with_tree.value.__cause__)
    assert world._core.mirror(TEMPORARY_SPACES_STATE_KEY).is_detached
