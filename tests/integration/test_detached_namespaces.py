"""The namespaces their Rust rows alone hold, with no Python tree (L14 stages 1 and 2, #134).

core/state_mirror.py lists them in DETACHED_NAMESPACES. A world loads them straight into their
rows; the tick's kernels read the rows; whole-world readers (the canonical JSON, same_state,
validation, a Checkpoint capture and restore, deepcopy for a fork) read the rows too, and build no
tree. A read from Python builds the tree, a write through it reaches the row, and the next tick
drops the tree.
docs/rust-port/step4-plan.md section 5 has the stage and what stage 2 needs.
"""

from __future__ import annotations

import copy
from typing import Any

import pytest
from support.world import view_of, world_digest

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
from sheltersim.domain.facility_queues import FACILITY_QUEUES_STATE_KEY
from sheltersim.domain.population_lifecycle import current_household_id
from sheltersim.domain.psychological_state import PSYCHOLOGICAL_STATE_KEY
from sheltersim.domain.social_network import SOCIAL_NETWORK_STATE_KEY
from sheltersim.domain.temporary_spaces import TEMPORARY_SPACES_STATE_KEY
from sheltersim.events.effects import EFFECT_PROGRAM_STATE_KEY, schedule_effect_program
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.timeline import _MetricSink
from sheltersim.inspection.frames import world_frame

SOCIAL = "social_propagation_v1"
# Their rows are `V` (docs/rust-port/lanes/m6-population.md); #134 stage 2, lane a2.
POPULATION = (
    "population_presence_v1",
    "population_lifecycle_v1",
    "population_dynamics_v1",
    "geometry_overlay_v1",
)

# Detached in stage 2 by lane a1 (#134): the movement, the people's and staff's memory, the
# information, the independent queues and the programs.
LANE_A1 = frozenset(
    {
        "movement_v1",
        "agent_dynamics_v1",
        "information_spread_v1",
        "staff_dynamics_v1",
        FACILITY_QUEUES_STATE_KEY,
        EFFECT_PROGRAM_STATE_KEY,
    }
)


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


def _present(world: WorldState) -> frozenset[str]:
    """The detached namespaces this world has: the generated Scenario has all but two (below)."""
    return DETACHED_NAMESPACES & dict.keys(world.model_state)


def _rebuilds(world: WorldState) -> dict[str, int]:
    return {name: world._core.mirror(name).stats()["rebuilds"] for name in _present(world)}


def _none_rebuilt(world: WorldState) -> dict[str, int]:
    return dict.fromkeys(_present(world), 0)


def _all_detached(world: WorldState) -> bool:
    return all(
        _held(world, name) is state_mirror._DETACHED and world._core.mirror(name).is_detached
        for name in _present(world)
    )


@pytest.fixture
def ticked(generated_scenario: Any) -> tuple[WorldState, TickEngine]:
    return _run(generated_scenario, 20)


def test_a_run_holds_the_detached_namespaces_in_their_rows_alone(
    ticked: tuple[WorldState, TickEngine],
) -> None:
    world, _ = ticked
    assert DETACHED_NAMESPACES == {
        PSYCHOLOGICAL_STATE_KEY,
        SOCIAL,
        TEMPORARY_SPACES_STATE_KEY,
        SOCIAL_NETWORK_STATE_KEY,
        *POPULATION,
        *LANE_A1,
    }
    # The generated Scenario has them all but the independent queues and the programs (the
    # tests at the end); the social one is made at the first tick.
    assert DETACHED_NAMESPACES - _present(world) == {
        FACILITY_QUEUES_STATE_KEY,
        EFFECT_PROGRAM_STATE_KEY,
    }
    assert _all_detached(world)
    assert isinstance(world.model_state, NamespaceTable)
    # The kernels of 20 ticks and their checks read the rows: no tree was built.
    assert _rebuilds(world) == _none_rebuilt(world)


def test_whole_world_readers_read_the_rows_and_build_no_tree(
    ticked: tuple[WorldState, TickEngine],
) -> None:
    world, _ = ticked
    world.validate()
    document = world.to_dict()
    for name in _present(world):
        assert document["model_state"][name] == world._core.mirror(name).document()
    cache = CheckpointHistoryCache()
    checkpoint = Checkpoint.capture(world, history_cache=cache)
    restored = checkpoint.restore_world()
    assert _all_detached(restored)
    assert restored.same_state(world) and world.same_state(restored)
    assert restored.model_state == world.model_state
    assert world.state_digest() == restored.state_digest()
    assert _rebuilds(world) == _none_rebuilt(world)
    assert _rebuilds(restored) == _none_rebuilt(restored)


def test_saved_bytes_do_not_depend_on_whether_a_tree_is_held(
    ticked: tuple[WorldState, TickEngine],
) -> None:
    world, _ = ticked
    detached = Checkpoint.capture(world, history_cache=CheckpointHistoryCache())
    for name in _present(world):
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
    assert _all_detached(fork) and _rebuilds(world) == _none_rebuilt(world)
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


def test_read_only_part_is_one_field_of_the_document(
    ticked: tuple[WorldState, TickEngine],
) -> None:
    world, _ = ticked
    for name in _present(world):
        document = world._core.mirror(name).document()
        for key, value in document.items():
            assert state_mirror.read_only_part(world, name, key) == value
        assert state_mirror.read_only_part(world, name, "absent", []) == []
    assert state_mirror.read_only_part(world, "absent_v1", "agents", {}) == {}
    assert _all_detached(world) and _rebuilds(world) == _none_rebuilt(world)
    # An attached namespace gives its tree's value, as model_state[name].get(key) does.
    tree = world.model_state[SOCIAL_NETWORK_STATE_KEY]
    assert (
        state_mirror.read_only_part(world, SOCIAL_NETWORK_STATE_KEY, "relationships")
        is (tree["relationships"])
    )


def test_a_frame_reads_the_rows_and_is_the_frame_of_the_trees(
    ticked: tuple[WorldState, TickEngine],
) -> None:
    world, _ = ticked

    def frame() -> dict[str, Any]:
        return world_frame(world, run_id="r", branch_id="b", computed_until=world.minute_index)

    detached = frame()
    assert _rebuilds(world) == _none_rebuilt(world)
    for name in _present(world):
        world.model_state[name]
    assert not any(world._core.mirror(name).is_detached for name in _present(world))
    assert frame() == detached


def _households(world: WorldState) -> tuple[dict[str, Any], dict[str, Any]]:
    """Each person's household as `current_household_id` and a phase's view read it."""
    view = view_of(world, include_state_hash=False)
    return (
        {agent_id: current_household_id(world, agent_id) for agent_id in world.agents},
        {agent_id: agent.group_id for agent_id, agent in view.agents.items()},
    )


def test_the_household_readers_read_the_presence_row(
    ticked: tuple[WorldState, TickEngine],
) -> None:
    world, _ = ticked
    people = world.model_state["population_presence_v1"]["people"]
    first, second, third = sorted(people)[:3]
    people[first]["household_id"] = "moved-household"
    people[second]["household_id"] = None
    del people[third]
    with_tree = _households(world)
    state_mirror.detach_unread(world)
    assert _all_detached(world)
    before = _rebuilds(world)
    assert _households(world) == with_tree
    assert with_tree[0][first] == with_tree[1][first] == "moved-household"
    assert with_tree[0][second] is None and with_tree[0][third] is None
    assert _rebuilds(world) == before and _all_detached(world)


def test_the_after_tick_memo_stamps_a_detached_namespace_by_its_mirror(
    ticked: tuple[WorldState, TickEngine],
) -> None:
    world, _ = ticked
    world.validate_after_tick()
    world.validate_after_tick()
    unchanged = world.__dict__["_tick_memo"].facts()["unchanged"]
    assert unchanged["presence"] and unchanged["lifecycle"] and unchanged["dynamics"]
    assert unchanged["overlay"]
    assert _all_detached(world) and _rebuilds(world) == _none_rebuilt(world)
    # A write reaches the row and moves its generation: the next check reads the row again.
    world.model_state["population_lifecycle_v1"]["schema_version"] = 2
    state_mirror.detach_unread(world)
    with pytest.raises((StateInvariantError, ValueError)):
        world.validate_after_tick()


# ---- Lane a1 of stage 2 (#134) ------------------------------------------------------------------
#
# The kernels that read or wrote these namespaces' trees read and write their rows while they are
# detached, and their trees as before while they are attached. A Run with them detached is the Run
# with their trees, step for step, and builds no tree of them.


def _toilet_queues(scenario: Any, place: Any) -> WorldState:
    from sheltersim.domain.facilities import ToiletServiceManager

    world = WorldState.from_scenario(scenario)
    place(world, "toilet_01")
    service = ToiletServiceManager(world, min_service_minutes=2, max_service_minutes=2)
    service.request("A001", "toilet_01", queue_id="toilet_01-left-fifo")
    service.request("A002", "toilet_01", queue_id="toilet_01-left-fifo")
    service.request("A003", "toilet_01", queue_id="toilet_01-right-fifo")
    return world


def _with_toilets(scenario: Any, program: Any) -> WorldState:
    world = WorldState.from_scenario(scenario)
    schedule_effect_program(world, program(start_minute=5))
    return world


def _attached_and_detached(
    monkeypatch: pytest.MonkeyPatch, build: Any, minutes: int
) -> tuple[list[str], list[str], WorldState, dict[str, int]]:
    """world_digest after each tick, with lane a1's namespaces attached and then detached; and
    the trees the detached Run's ticks built, by namespace (its set-up writes from Python)."""

    def run() -> tuple[list[str], WorldState, dict[str, int]]:
        world = build()
        before = _rebuilds(world)
        engine = _engine(world)
        digests = []
        for _ in range(minutes):
            engine.step()
            digests.append(world_digest(world))
        built = {name: count - before.get(name, 0) for name, count in _rebuilds(world).items()}
        return digests, world, built

    with monkeypatch.context() as patch:
        patch.setattr(state_mirror, "DETACHED_NAMESPACES", DETACHED_NAMESPACES - LANE_A1)
        attached, attached_world, _ = run()
    assert not any(
        attached_world._core.mirror(name).is_detached
        for name in LANE_A1 & dict.keys(attached_world.model_state)
    )
    detached, world, built = run()
    return attached, detached, world, built


@pytest.mark.parametrize("case", ["generated", "toilet_queues", "program"])
def test_a_run_with_the_rows_alone_is_the_run_with_the_trees(
    case: str,
    monkeypatch: pytest.MonkeyPatch,
    generated_scenario: Any,
    generated_scenario_with_split_queues: Any,
    place_three_at_facility: Any,
    add_toilets_program: Any,
) -> None:
    builds = {
        "generated": (lambda: WorldState.from_scenario(generated_scenario), 360),
        "toilet_queues": (
            lambda: _toilet_queues(generated_scenario_with_split_queues, place_three_at_facility),
            16,
        ),
        "program": (lambda: _with_toilets(generated_scenario, add_toilets_program), 30),
    }
    build, minutes = builds[case]
    attached, detached, world, built = _attached_and_detached(monkeypatch, build, minutes)
    assert detached == attached
    present = LANE_A1 & _present(world)
    expected = {
        "generated": LANE_A1 - {FACILITY_QUEUES_STATE_KEY, EFFECT_PROGRAM_STATE_KEY},
        "toilet_queues": LANE_A1 - {EFFECT_PROGRAM_STATE_KEY},
        "program": LANE_A1 - {FACILITY_QUEUES_STATE_KEY},
    }
    assert present == expected[case]
    assert _all_detached(world)
    # The ticks' kernels read and wrote the rows: none of these trees was built.
    assert {name: built[name] for name in present} == dict.fromkeys(present, 0)


def test_the_queues_written_in_the_row_are_those_of_the_tree(
    generated_scenario_with_split_queues: Any, place_three_at_facility: Any
) -> None:
    """The enqueue, start, finish and stall of the FIFO change the row as the tree's writes did."""
    world = _toilet_queues(generated_scenario_with_split_queues, place_three_at_facility)
    assert _held(world, FACILITY_QUEUES_STATE_KEY) is state_mirror._DETACHED
    document = state_mirror.read_only_state(world, FACILITY_QUEUES_STATE_KEY)
    assert document["queues"]["toilet_01-left-fifo"]["queued_request_ids"] == [
        request_id
        for request_id, request in document["requests"].items()
        if request["agent_id"] in {"A001", "A002"}
    ]
    mirror = world._core.mirror(FACILITY_QUEUES_STATE_KEY)
    engine = _engine(world)
    for _ in range(4):
        engine.step()
    assert mirror.is_detached and mirror.stats()["rebuilds"] == 0
    # The tree built now from the row is the row's, and the row is whole.
    tree = world.model_state[FACILITY_QUEUES_STATE_KEY]
    assert mirror.verify() and tree == mirror.document()
    assert any(queue["active_request_ids"] for queue in tree["queues"].values())
