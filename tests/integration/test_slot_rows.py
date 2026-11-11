"""The rows of the slots are the truth and the attached trees follow them (Issue #152, P8-0c).

A kernel of the core writes a slot's row alone (``World::row_mut``), which marks the slot; when its
borrow of the world ends, before any Python runs, the tree follows the row
(``NamespaceMirror::follow_row`` in native/src/core/world_core.rs): the root and every container
whose place the row still has stay the same objects, a dict gets the keys whose values changed and
new keys at its end, a list keeps the items still there. These tests write rows as such a kernel
does with ``apply_slot_ops`` and check what Python sees, and that --verify-rows finds a tree that
differs from its row.
"""

from __future__ import annotations

import json
from typing import Any

import pytest
import sheltersim_native
from support.paths import SCENARIO_100_AGENTS

from sheltersim.core.engine import TickEngine
from sheltersim.core.state_mirror import (
    check_slot_rows,
    detach,
    slot_row_mismatches,
    transaction,
    watch_entity_rows,
)
from sheltersim.core.world import WorldState
from sheltersim.experiments.scenario import ScenarioSpec

PROBE = "slot_probe_v1"


def _world() -> WorldState:
    return WorldState.from_scenario(ScenarioSpec.load(SCENARIO_100_AGENTS))


def _ops(world: WorldState, *ops: tuple[Any, ...]) -> list[int]:
    return sheltersim_native.apply_slot_ops(world, list(ops))


def _mirror(world: WorldState, name: str) -> Any:
    return world.__dict__["_core"].mirror(name)


def _row(world: WorldState, name: str) -> Any:
    return json.loads(_mirror(world, name).row_json())


def test_a_metric_the_core_counts_reaches_the_same_tree() -> None:
    world = _world()
    tree = world.metric_state
    before = dict(tree)
    _ops(world, ("metric", "probe_count"), ("metric", "probe_count"))
    assert world.metric_state is tree
    assert tree["probe_count"] == 2.0 and type(tree["probe_count"]) is float
    assert list(tree) == [*before, "probe_count"]
    assert _mirror(world, "metric_state").verify()
    # The tree is still the slot's: a Python write reaches the row.
    tree["probe_count"] = 7.0
    assert _row(world, "metric_state")["probe_count"] == 7.0


def test_event_sequences_the_core_allocates_reach_id_counters() -> None:
    world = _world()
    counters = world.id_counters
    first = world.id_counters.get("event", 0)
    assert _ops(world, ("allocate",), ("allocate",)) == [first, first + 1]
    assert world.id_counters is counters
    assert counters["event"] == first + 2
    assert world.allocate_event_sequence() == first + 2
    assert _mirror(world, "id_counters").verify()


def test_nested_containers_keep_their_identity_and_take_the_new_values() -> None:
    world = _world()
    world.model_state[PROBE] = {
        "a": {"x": 1, "y": [1, 2]},
        "b": [{"k": 1}, {"k": 2}],
        "gone": 1,
        "same": {"z": None},
    }
    root = world.model_state[PROBE]
    a, y, b, b1, same = root["a"], root["a"]["y"], root["b"], root["b"][1], root["same"]
    new = {
        "same": {"z": None},
        "a": {"x": 2, "y": [1, 2, 3]},
        "b": [{"k": 1}, {"k": 5}],
        "new": True,
    }
    _ops(world, ("value", PROBE, new))
    assert world.model_state[PROBE] is root
    assert root == new
    assert root["a"] is a and root["a"]["y"] is y and root["b"] is b and root["b"][1] is b1
    assert root["same"] is same
    assert a["x"] == 2 and y == [1, 2, 3] and b1 == {"k": 5}
    # Only changed keys are written, a key that left goes, a new one comes last.
    assert list(root) == ["a", "b", "same", "new"]
    assert _mirror(world, PROBE).verify()
    # The nodes are still the tree's, so a Python write to one reaches the row.
    a["w"] = "x"
    y.append(4)
    assert _row(world, PROBE)["a"] == {"x": 2, "y": [1, 2, 3, 4], "w": "x"}


def test_a_history_keeps_the_items_still_there_and_drops_the_oldest() -> None:
    world = _world()
    world.model_state[PROBE] = {"history": [{"i": 1}, {"i": 2}, {"i": 3}]}
    history = world.model_state[PROBE]["history"]
    oldest, second, third = history
    _ops(world, ("value", PROBE, {"history": [{"i": 2}, {"i": 3}, {"i": 4}, {"i": 5}]}))
    assert world.model_state[PROBE]["history"] is history
    assert history[0] is second and history[1] is third
    assert history == [{"i": 2}, {"i": 3}, {"i": 4}, {"i": 5}]
    # The item that left is a plain container now: writing it changes nothing of the namespace.
    oldest["i"] = 99
    assert _row(world, PROBE)["history"][0] == {"i": 2}
    # Changed in place, item by item, when no run is kept from the front.
    _ops(world, ("value", PROBE, {"history": [{"i": 2}, {"i": 30}]}))
    assert history[0] is second and history[1] is third and history == [{"i": 2}, {"i": 30}]
    assert _mirror(world, PROBE).verify()


def test_an_atom_takes_the_rows_exact_type() -> None:
    world = _world()
    world.model_state[PROBE] = {"n": 1, "f": 0.0, "b": True, "s": "x"}
    root = world.model_state[PROBE]
    _ops(world, ("value", PROBE, {"n": 1.0, "f": -0.0, "b": 1, "s": "x"}))
    assert type(root["n"]) is float and type(root["b"]) is int
    assert str(root["f"]) == "-0.0"
    assert _mirror(world, PROBE).verify()


def test_a_root_of_another_kind_is_refused() -> None:
    world = _world()
    world.model_state[PROBE] = {"a": 1}
    with pytest.raises(RuntimeError, match="changed the kind of a tree's root"):
        _ops(world, ("value", PROBE, [1]))


def test_the_tree_follows_even_when_the_borrow_ends_with_an_error() -> None:
    world = _world()
    tree = world.metric_state
    with pytest.raises(RuntimeError, match="the ops failed"):
        _ops(world, ("metric", "probe_count"), ("fail",))
    assert tree["probe_count"] == 1.0
    assert _mirror(world, "metric_state").verify()


def test_a_detached_slot_has_no_tree_and_its_next_read_is_the_row() -> None:
    world = _world()
    world.model_state[PROBE] = {"a": 1}
    old = world.model_state[PROBE]
    assert detach(world, PROBE)
    _ops(world, ("value", PROBE, {"a": 2}))
    assert old == {"a": 1}
    assert world.model_state[PROBE] == {"a": 2}


def test_a_rollback_gives_back_the_tree_and_a_commit_keeps_the_write() -> None:
    world = _world()
    with transaction(world, "full") as txn:
        _ops(world, ("metric", "probe_count"))
        assert world.metric_state["probe_count"] == 1.0
        txn.rollback()
    assert "probe_count" not in world.metric_state
    assert "probe_count" not in _row(world, "metric_state")
    with transaction(world, "full"):
        _ops(world, ("metric", "probe_count"))
    assert world.metric_state["probe_count"] == 1.0
    assert _mirror(world, "metric_state").verify()


def test_verify_rows_counts_a_tree_that_differs_from_its_row() -> None:
    world = _world()
    watch_entity_rows(True)
    try:
        TickEngine(world).run(3)
        found = slot_row_mismatches()
        assert found["checked"] == 3 and found["trees"] > 3
        assert found["mismatches"] == 0 and found["first"] is None
        # A write past the tree's overrides does not reach the row: the check finds it.
        dict.__setitem__(world.metric_state, "probe_count", 5.0)
        check_slot_rows(world, "after")
        found = slot_row_mismatches()
        assert found["mismatches"] == 1 and found["first"] == "after: metric_state"
    finally:
        watch_entity_rows(False)
