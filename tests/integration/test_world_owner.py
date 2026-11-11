"""A world's core is the world's own and holds its event queue and EventLog (Issue #152, lane 6-5).

The events of a world's queue and the records of its EventLog are parts of the core's World; the
Python EventQueue and EventLog are handles that keep the events' and records' objects. A queue or a
log that belongs to no world holds its own, moves in when it is assigned to a world, and moves out
again when the world lets it go. A copy that holds the EventLog of the world it was made from
(``_effect_candidate``) leaves it with that world, and a copy that takes a world's place gives its
core to the world. The core holds its world weakly: ``with_world`` refuses a world whose core is
another's, and when the world dies its objects go: the entity objects and their maps work on as
plain objects, and the queue and the log hold their own again.
"""

from __future__ import annotations

import copy
import gc
import weakref
from types import SimpleNamespace
from typing import Any

import pytest

from sheltersim import native
from sheltersim.core import state_mirror, txn_checks
from sheltersim.core.engine import TickEngine
from sheltersim.core.world import WorldState
from sheltersim.events.effects import _effect_candidate
from sheltersim.events.models import EventKind, EventLogRecord, EventStatus, ScheduledEvent
from sheltersim.events.queue import EventQueue, _PythonEventQueue
from sheltersim.experiments.initial_scenario import InitialScenarioBuilder, InitialScenarioRequest
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.experiments.timeline import _MetricSink


def _scenario() -> ScenarioSpec:
    request = InitialScenarioRequest(
        municipality="合成市", disaster="地震", evacuees=30, duration_days=0.25, seed=2026
    )
    return InitialScenarioBuilder().build(request).scenario


def _world() -> WorldState:
    return WorldState.from_scenario(_scenario())


def _event(name: str, sequence: int, due: int = 3) -> ScheduledEvent:
    return ScheduledEvent(
        event_id=name,
        kind=EventKind.ENDOGENOUS,
        due_minute=due,
        priority=0,
        sequence=sequence,
        event_type="record",
        payload={"value": sequence},
        source_id="source-1",
        causation_id=None,
        provenance={"source_type": "assumption"},
    )


def _record(world: WorldState, name: str) -> EventLogRecord:
    sequence = world.allocate_event_sequence()
    return EventLogRecord(_event(name, sequence, due=0), 0, EventStatus.APPLIED)


def _engine(world: WorldState) -> TickEngine:
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    return TickEngine(world, phase_handler=controller)


def _holds(world: WorldState) -> tuple[bool, bool]:
    return world.__dict__["_core"].holds_events(world)


def test_a_world_holds_its_queue_and_log_and_an_assigned_one_moves_in() -> None:
    world = _world()
    core = world.__dict__["_core"]
    assert core.is_owned_by(world)
    assert _holds(world) == (True, True)
    assert world.event_queue._store.attached and world.event_log._store.attached
    for name in ("first", "second"):
        world.schedule_event(_event(name, world.allocate_event_sequence(), due=10))
    old = world.event_queue
    pending = old.pending()
    new = EventQueue(list(pending))
    assert not new._store.attached
    world.event_queue = new
    # The new queue is the world's; the old one is its own again, with its events.
    assert new._store.attached and not old._store.attached
    assert _holds(world) == (True, True)
    assert old.pending() == pending and world.event_queue.pending() == pending
    old.remove(pending[0].event_id)
    assert world.event_queue.pending() == pending
    # A queue held in Python: the world holds none, and a rule's writer refuses it.
    world.event_queue = _PythonEventQueue()
    assert _holds(world) == (False, True)
    assert not new._store.attached
    with pytest.raises(native.Unsupported):
        native.state("events").allocate_sequence(world)
    world.event_queue = new
    assert _holds(world) == (True, True)
    native.state("events").schedule_row(
        world, _event("from-a-rule", world.allocate_event_sequence())
    )
    assert world.event_queue.find("from-a-rule") is not None


def test_a_queue_that_leaves_rust_leaves_the_world() -> None:
    world = _world()
    world.event_queue.schedule(_event("huge", world.allocate_event_sequence(), due=10**30))
    assert world.event_queue._store is None
    assert _holds(world) == (False, True)
    with pytest.raises(native.Unsupported):
        native.state("events").allocate_sequence(world)
    assert world.event_queue.find("huge") is not None


def test_a_copy_that_holds_the_worlds_log_leaves_it_with_the_world() -> None:
    world = _world()
    log = world.event_log
    candidate = _effect_candidate(world)
    assert candidate.event_log is log
    assert _holds(candidate) == (True, False)
    assert _holds(world) == (True, True)
    # The copy's rules write the world's log, as the shared object always did.
    sequence = candidate.allocate_event_sequence()
    event = _event("on-the-copy", sequence, due=0)
    native.state("events").record_row(candidate, event, 0, "applied")
    assert log.contains_id("on-the-copy") and len(log) == 1
    del candidate
    gc.collect()
    assert _holds(world) == (True, True)
    assert world.event_log is log and len(log) == 1


def test_a_copy_that_takes_the_worlds_place_gives_its_core_to_the_world() -> None:
    world = _world()
    log = world.event_log
    old_core = world.__dict__["_core"]
    txn_checks.refuse_transactions(True)
    try:
        with state_mirror.transaction_or_copy(world, "effect", _effect_candidate) as target:
            assert target is not world
            target.event_log.append(_record(target, "in-the-copy"))
    finally:
        txn_checks.refuse_transactions(False)
    core = world.__dict__["_core"]
    assert core is not old_core
    assert core.is_owned_by(world) and not old_core.is_owned_by(world)
    assert world.event_log is log and log.contains_id("in-the-copy")
    assert _holds(world) == (True, True)
    # The world works on: a transaction, and a Run.
    with state_mirror.transaction(world, "full"):
        world.event_log.append(_record(world, "in-a-transaction"))
    assert log.contains_id("in-a-transaction")
    _engine(world).run(steps=5)
    assert world.minute_index == 5
    # The core the world had goes with what it held: its stores hold nothing of it.
    old = weakref.ref(old_core)
    del old_core
    gc.collect()
    assert old() is None


def test_with_world_refuses_a_world_whose_core_is_anothers() -> None:
    world = _world()
    other = object.__new__(WorldState)
    other.__dict__.update(world.__dict__)
    with pytest.raises(RuntimeError, match="^the world's core is not this world's$"):
        with state_mirror.transaction(other, "full"):
            raise AssertionError("not reached")
    assert not world.__dict__["_core"].in_transaction
    with state_mirror.transaction(world, "full"):
        pass


def test_a_world_that_dies_lets_its_objects_go() -> None:
    world = _world()
    _engine(world).run(steps=3)
    agent_id = next(iter(world.agents))
    agent = world.agents[agent_id]
    agents = world.agents
    staff = next(iter(world.staff.values()))
    queue, log = world.event_queue, world.event_log
    log.append(_record(world, "kept"))
    pending, records = queue.pending(), log.records_from(0)
    values = dict(vars(agent))
    core = weakref.ref(world.__dict__["_core"])
    del world
    gc.collect()
    # Plain objects with their values, and maps that are plain dicts.
    assert agent._store is None and agent.needs._store is None and agents._store is None
    assert staff._store is None
    assert agents[agent_id] is agent
    assert {key: value for key, value in vars(agent).items() if key != "_store"} == {
        key: value for key, value in values.items() if key != "_store"
    }
    agent.stress = 0.5
    assert agent.stress == 0.5
    # The queue and the log hold their own again, with what they had.
    assert not queue._store.attached and not log._store.attached
    assert queue.pending() == pending
    assert log.records_from(0) == records and log.contains_id("kept")
    log.append(EventLogRecord(_event("after", log.max_sequence + 1, due=0), 0, EventStatus.APPLIED))
    assert log.contains_id("after")
    # The core, if a cache of the last Run still holds it, holds neither of them.
    held = core()
    if held is not None:
        stand_in = SimpleNamespace(event_queue=queue, event_log=log)
        assert held.holds_events(stand_in) == (False, False)


def test_a_records_bytes_are_one_object_and_a_rollback_takes_out_its_records() -> None:
    world = _world()
    log = world.event_log
    for name in ("a", "b", "c"):
        log.append(_record(world, name))
    first = log.items_bytes(0, 3)
    assert all(mine is theirs for mine, theirs in zip(first, log.items_bytes(0, 3), strict=True))
    records = log.records_from(0)
    pending = world.event_queue.pending()
    with state_mirror.transaction(world, "full") as txn:
        log.append(_record(world, "d"))
        world.event_queue.schedule(_event("new", world.allocate_event_sequence(), due=30))
        assert len(log) == 4 and log.items_bytes(3, 4)
        txn.rollback()
    assert len(log) == 3 and not log.contains_id("d")
    assert log.records_from(0) == records
    assert all(mine is theirs for mine, theirs in zip(first, log.items_bytes(0, 3), strict=True))
    assert world.event_queue.pending() == pending
    assert world.event_queue.find("new") is None


def test_a_run_goes_on_the_same_after_its_copies_died() -> None:
    """Copies of a world that die (a deepcopy, an effect's copy holding the world's log) leave the
    world's Run as it would have been."""

    def run(copies: bool) -> tuple[str, list[Any]]:
        world = _world()
        engine = _engine(world)
        for _ in range(4):
            engine.run(steps=15)
            if copies:
                copied = copy.deepcopy(world)
                candidate = _effect_candidate(world)
                assert _holds(copied) == (True, True)
                del copied, candidate
                gc.collect()
            assert _holds(world) == (True, True)
        return world.canonical_json(), [record.to_dict() for record in world.event_log.records]

    assert run(copies=True) == run(copies=False)
