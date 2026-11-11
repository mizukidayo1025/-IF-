"""The rows of the slots are the truth and the attached trees follow them (Issue #152, P8-0c).

A kernel of the core writes a slot's row alone (``World::row_mut``), which marks the slot; when its
borrow of the world ends, before any Python runs, the tree follows the row
(``NamespaceMirror::follow_row`` in native/src/core/world_core.rs): the root and every container
whose place the row still has stay the same objects, a dict gets the keys whose values changed and
new keys at its end and then has its keys in the row's order, a list keeps the items still there.
These tests write rows as such a kernel does with ``apply_slot_ops`` and check what Python sees,
and that --verify-rows finds a tree that differs from its row, in its values or its key order.
"""

from __future__ import annotations

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
    """The row as a plain document, in its own order."""
    return _mirror(world, name).document()


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


def test_a_map_of_floats_writes_only_what_changed_or_else_is_patched_whole() -> None:
    """`metric_state` takes the changed values and the new keys at its end without the whole row
    made in Python (lane P8 a3); a tree whose keys are not the row's in order is patched whole."""
    world = _world()
    tree = world.metric_state
    tree["probe_a"] = 1.0
    tree["probe_b"] = 2.0
    # A value only the tree has, of another type: the row's float comes back.
    dict.__setitem__(tree, "probe_a", 5)
    _ops(world, ("metric", "probe_c"))
    assert world.metric_state is tree
    assert type(tree["probe_a"]) is float and tree["probe_a"] == 1.0
    assert list(tree)[-3:] == ["probe_a", "probe_b", "probe_c"]
    assert _mirror(world, "metric_state").verify() and _mirror(world, "metric_state").verify_order()
    # A key only the tree moved to its end: the general patch puts it back in the row's order.
    dict.__setitem__(tree, "probe_a", dict.pop(tree, "probe_a"))
    _ops(world, ("metric", "probe_c"))
    assert list(tree)[-3:] == ["probe_a", "probe_b", "probe_c"] and tree["probe_c"] == 2.0
    assert _mirror(world, "metric_state").verify() and _mirror(world, "metric_state").verify_order()


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
    # A key that left goes, a new one comes last, and the keys end in the row's order.
    assert list(root) == ["same", "a", "b", "new"]
    assert _mirror(world, PROBE).verify()
    # The nodes are still the tree's, so a Python write to one reaches the row.
    a["w"] = "x"
    y.append(4)
    assert _row(world, PROBE)["a"] == {"x": 2, "y": [1, 2, 3, 4], "w": "x"}


def test_a_key_taken_out_and_put_back_in_one_borrow_comes_last() -> None:
    world = _world()
    world.model_state[PROBE] = {"a": {"x": 1}, "b": {"y": 2}, "c": 3}
    root = world.model_state[PROBE]
    a, b = root["a"], root["b"]
    _ops(
        world,
        ("value", PROBE, {"b": {"y": 2}, "c": 3}),
        ("value", PROBE, {"b": {"y": 2}, "c": 3, "a": {"x": 1}}),
    )
    assert world.model_state[PROBE] is root
    assert list(root) == ["b", "c", "a"]
    assert root["a"] is a and root["b"] is b
    assert list(_row(world, PROBE)) == ["b", "c", "a"]
    assert _mirror(world, PROBE).verify_order()


def test_keys_follow_the_rows_order_at_every_level_with_the_same_nodes() -> None:
    world = _world()
    world.model_state[PROBE] = {"m": {"k1": 1, "k2": {"z": 0}, "k3": 3}, "n": [{"p": 1, "q": 2}]}
    root = world.model_state[PROBE]
    m, k2, item = root["m"], root["m"]["k2"], root["n"][0]
    new = {"n": [{"q": 2, "p": 1}], "m": {"k3": 3, "k1": 1, "k2": {"z": 0}}}
    _ops(world, ("value", PROBE, new))
    assert list(root) == ["n", "m"]
    assert list(m) == ["k3", "k1", "k2"] and list(item) == ["q", "p"]
    assert root["m"] is m and m["k2"] is k2 and root["n"][0] is item
    assert _mirror(world, PROBE).verify() and _mirror(world, PROBE).verify_order()
    # The nodes are still the tree's: a write reaches the row, at the end of the row's order.
    m["k4"] = 4
    k2["w"] = 1
    assert list(_row(world, PROBE)["m"]) == ["k3", "k1", "k2", "k4"]
    assert _row(world, PROBE)["m"]["k2"] == {"z": 0, "w": 1}


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
        # Keys put in another order past the overrides: the same canonical JSON, another order.
        dict.__delitem__(world.metric_state, "probe_count")
        world.metric_state["probe_a"] = 1.0
        world.metric_state["probe_b"] = 2.0
        assert _mirror(world, "metric_state").verify_order()
        dict.__setitem__(world.metric_state, "probe_a", dict.pop(world.metric_state, "probe_a"))
        assert _mirror(world, "metric_state").verify()
        watch_entity_rows(True)
        check_slot_rows(world, "moved")
        assert slot_row_mismatches()["first"] == "moved: metric_state (key order)"
    finally:
        watch_entity_rows(False)
