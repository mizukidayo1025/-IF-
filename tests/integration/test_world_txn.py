"""A transaction in place stands for the copy through canonical JSON (Issue #152, lane 6-2).

``state_mirror.transaction(world, kind)`` keeps a snapshot of the world's core, puts its rows in
canonical order and makes the Python containers again (design-p5-p6.md §6, policy E5). Its start
must hold what ``WorldState.from_dict(world.to_dict())`` holds, in the same order: the same
canonical JSON, and every map iterated in the same order with the same types (the entities, the
rows' documents, the pending events and their payloads, the model_state trees). These tests compare
the two at several minutes of Runs that reach every namespace a gate Scenario has, and check that a
rollback gives back the world as it was: its state, its order, and every object and container it
held. Nothing in the tick uses the transaction yet (lane 6-3 will).
"""

from __future__ import annotations

import copy
from typing import Any

import pytest
from support.world_order import (
    Run,
    check_rows,
    fingerprint,
    first_difference,
    gate_runs,
    identities,
    write_everything,
)

from sheltersim import native
from sheltersim.core import state_mirror, txn_checks
from sheltersim.core.engine import TickEngine
from sheltersim.core.world import StateInvariantError, WorldState
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.timeline import _MetricSink

# ---- A transaction's start is the copy through canonical JSON -----------------------------------


@pytest.mark.parametrize("kind", ["full", "effect"])
def test_a_transaction_starts_from_what_a_copy_through_canonical_json_holds(
    add_toilets_program: Any, kind: str
) -> None:
    for label, run in gate_runs(add_toilets_program).items():
        for minute in (0, 25, 70, 160):
            world = run.to(minute)
            copied = WorldState.from_dict(world.to_dict())
            before = fingerprint(world)
            text = world.canonical_json()
            with state_mirror.transaction(world, kind) as txn:
                assert world.__dict__["_core"].in_transaction
                assert world.canonical_json() == copied.canonical_json(), (label, minute)
                mine, theirs = fingerprint(world), fingerprint(copied)
                assert mine == theirs, (label, minute, first_difference(mine, theirs))
                check_rows(world)
                txn.rollback()
            assert not world.__dict__["_core"].in_transaction
            after = fingerprint(world)
            assert after == before, (label, minute, first_difference(after, before))
            assert world.canonical_json() == text
            check_rows(world)


def test_a_namespace_no_row_holds_goes_through_canonical_json(add_toilets_program: Any) -> None:
    """A value V can't hold (a tuple) taints its key; the copy holds a list, and owns it again."""
    world = gate_runs(add_toilets_program)["generated-operations"].to(10)
    world.model_state["custom_pair_v1"] = {"pair": (1, 2), "b": 1, "a": {"z": 0, "y": 1}}
    mirror = world.__dict__["_core"].mirror("custom_pair_v1")
    assert mirror.tainted is not None
    copied = WorldState.from_dict(world.to_dict())
    before = fingerprint(world)
    with state_mirror.transaction(world, "full") as txn:
        assert fingerprint(world) == fingerprint(copied)
        assert mirror.tainted is None
        assert list(world.model_state["custom_pair_v1"]) == ["a", "b", "pair"]
        assert world.model_state["custom_pair_v1"]["pair"] == [1, 2]
        txn.rollback()
    assert fingerprint(world) == before
    assert world.model_state["custom_pair_v1"]["pair"] == (1, 2)
    assert mirror.tainted is not None


# ---- A rollback gives everything back ------------------------------------------------------------


def test_a_rollback_gives_back_the_world_its_order_and_every_object(
    add_toilets_program: Any,
) -> None:
    world = gate_runs(add_toilets_program)["generated-operations"].to(70)
    world.event_queue.pending()  # every pending event has its object
    saved = copy.deepcopy(world)
    before = fingerprint(world)
    held_objects = identities(world)
    text = world.canonical_json()
    log_length = len(world.event_log)
    old_maps = {name: world.__dict__[name] for name in state_mirror.ENTITY_FIELDS}
    with state_mirror.transaction(world, "full") as txn:
        new_maps = {name: world.__dict__[name] for name in state_mirror.ENTITY_FIELDS}
        assert all(new_maps[name] is not old_maps[name] for name in new_maps)
        write_everything(world)
        txn.rollback()
    assert world.same_state(saved)
    assert world.canonical_json() == text
    # A key first stored in the block keeps its slot, with no state, as a deleted key does.
    after = {
        key: value
        for key, value in fingerprint(world).items()
        if key in before or value != (None, False)
    }
    assert after == before, first_difference(after, before)
    now = identities(world)
    assert now == held_objects, first_difference(now, held_objects)
    assert len(world.event_log) == log_length
    assert world.event_log._store.audit(world.minute_index, world.duration_minutes) == 0
    check_rows(world)
    for name, kind in state_mirror.ENTITY_FIELDS.items():
        assert world.__dict__[name]._store is world.__dict__["_core"].entities()
        assert world.__dict__[name]._table == kind
        assert new_maps[name]._store is None
    # The world runs on as it would have.
    world.validate()
    assert world.to_dict() == saved.to_dict()


def test_an_error_in_the_block_rolls_back_and_passes_on(add_toilets_program: Any) -> None:
    world = gate_runs(add_toilets_program)["generated-operations"].to(40)
    text = world.canonical_json()
    held_objects = identities(world)
    with pytest.raises(KeyError, match="the block failed"):
        with state_mirror.transaction(world, "effect"):
            write_everything(world)
            raise KeyError("the block failed")
    assert world.canonical_json() == text
    assert identities(world) == held_objects
    check_rows(world)


def test_a_commit_keeps_the_writes_in_canonical_order(add_toilets_program: Any) -> None:
    world = gate_runs(add_toilets_program)["generated-operations"].to(70)
    world.validate_after_tick()
    assert "_tick_memo" in world.__dict__
    old_agents = world.agents
    with state_mirror.transaction(world, "full"):
        world.agents[next(iter(world.agents))].stress = 0.25
        world.metric_state["new_metric"] = 1.5
    assert "_tick_memo" not in world.__dict__
    assert not world.__dict__["_core"].in_transaction
    assert world.agents is not old_agents
    assert old_agents._store is None
    assert list(world.agents) == sorted(world.agents, key=lambda key: key.encode())
    assert world.agents[next(iter(world.agents))].stress == 0.25
    assert world.metric_state["new_metric"] == 1.5
    check_rows(world)
    world.validate()
    # The world runs on; a second transaction opens after the first.
    run = Run.__new__(Run)
    run.world = world
    run.engine = TickEngine(
        world, phase_handler=OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    )
    run.to(80)
    with state_mirror.transaction(world, "effect") as txn:
        txn.commit()
    check_rows(world)


def test_a_world_that_fails_its_validation_is_not_changed(
    add_toilets_program: Any, monkeypatch: pytest.MonkeyPatch
) -> None:
    world = gate_runs(add_toilets_program)["generated-operations"].to(30)
    text = world.canonical_json()
    held_objects = identities(world)
    calls = []
    original = WorldState.validate

    def failing(self: WorldState, *arguments: Any, **keywords: Any) -> Any:
        calls.append(len(calls))
        if len(calls) == 1:
            raise ValueError("the world is wrong")
        return original(self, *arguments, **keywords)

    monkeypatch.setattr(WorldState, "validate", failing)
    # The world's own check: its error as it is, as world.to_dict() raises it.
    with pytest.raises(ValueError, match="^the world is wrong$") as raised:
        with state_mirror.transaction(world, "full"):
            raise AssertionError("not reached")
    assert type(raised.value) is ValueError
    assert world.canonical_json() == text

    # The candidate's check: wrapped as from_dict wraps it, and the world given back.
    def failing_second(self: WorldState, *arguments: Any, **keywords: Any) -> Any:
        calls.append(len(calls))
        if len(calls) == 3:
            raise KeyError("missing")
        return original(self, *arguments, **keywords)

    calls.clear()
    calls.append(0)
    monkeypatch.setattr(WorldState, "validate", failing_second)
    with pytest.raises(StateInvariantError, match="^invalid WorldState document: 'missing'$"):
        with state_mirror.transaction(world, "effect"):
            raise AssertionError("not reached")
    monkeypatch.setattr(WorldState, "validate", original)
    assert world.canonical_json() == text
    assert identities(world) == held_objects
    assert not world.__dict__["_core"].in_transaction
    check_rows(world)


def test_transactions_do_not_nest_and_unsupported_worlds_are_refused(
    add_toilets_program: Any,
) -> None:
    world = gate_runs(add_toilets_program)["generated-operations"].to(5)
    with state_mirror.transaction(world, "full"):
        with pytest.raises(RuntimeError, match="a transaction is open on the world already"):
            with state_mirror.transaction(world, "full"):
                pass
    with pytest.raises(ValueError, match="unknown kind of transaction"):
        with state_mirror.transaction(world, "other"):
            pass
    text = world.canonical_json()
    dict.__setitem__(world.model_state, 7, {"a": 1})
    with pytest.raises(native.Unsupported, match="a key of model_state is not a str"):
        with state_mirror.transaction(world, "full"):
            pass
    dict.__delitem__(world.model_state, 7)
    world.__dict__["agents"] = dict(world.agents)
    with pytest.raises(native.Unsupported, match="a map of entities is not the rows' map"):
        with state_mirror.transaction(world, "full"):
            pass
    assert world.canonical_json() == text


# ---- The rules of a tick in transactions (lane 6-3) ----------------------------------------------


def _documents(world: WorldState) -> tuple[str, list[Any]]:
    return world.canonical_json(), [record.to_dict() for record in world.event_log.records]


def test_the_rules_of_a_tick_leave_what_their_copy_left(add_toilets_program: Any) -> None:
    """The effect handlers, the operational transaction and population run in place; a world the
    transaction refuses takes the copy, and both end where the copy-and-swap ended."""
    counts = native.rules("transaction").counts
    before = counts()
    in_place = _runs(add_toilets_program)["generated-operations"].to(160)
    between = counts()
    assert between["in_place"] > before["in_place"]
    assert between["copy"] == before["copy"]
    txn_checks.refuse_transactions(True)
    try:
        on_copy = _runs(add_toilets_program)["generated-operations"].to(160)
    finally:
        txn_checks.refuse_transactions(False)
    after = counts()
    assert after["copy"] - between["copy"] == between["in_place"] - before["in_place"]
    assert after["in_place"] == between["in_place"]
    assert _documents(in_place) == _documents(on_copy)
    _check_rows(in_place)


def test_the_poison_finds_an_object_kept_across_a_transaction(add_toilets_program: Any) -> None:
    world = _runs(add_toilets_program)["generated-operations"].to(30)
    kept = world.agents
    kept_type = type(kept)
    txn_checks.poison_replaced(True)
    try:
        with pytest.raises(RuntimeError, match="taken out of the world was used: IdMap.__len__"):
            with state_mirror.transaction(world, "effect"):
                assert world.agents is not kept
                len(kept)
        uses = txn_checks.poison_uses()
    finally:
        txn_checks.poison_replaced(False)
    assert uses["uses"] == 1
    assert uses["poisoned"] > 0
    assert "IdMap.__len__" in uses["first"]
    # The rollback gave the map back with its class.
    assert world.agents is kept
    assert type(kept) is kept_type
    _check_rows(world)
