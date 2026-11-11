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
import dataclasses
import enum
from typing import Any

import pytest
from support.paths import SCENARIOS

from sheltersim import native
from sheltersim.core import state_mirror
from sheltersim.core.engine import TickEngine
from sheltersim.core.world import StateInvariantError, WorldState
from sheltersim.domain.models import (
    ActiveTask,
    NeedState,
    SupportRequestState,
    SupportRequestStatus,
)
from sheltersim.events.effects import schedule_effect_program
from sheltersim.events.models import EventKind, EventLogRecord, EventStatus, ScheduledEvent
from sheltersim.experiments.initial_scenario import InitialScenarioBuilder, InitialScenarioRequest
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.experiments.timeline import _MetricSink

# Outages, an absence and a shipment the operator entered on the form, as the gate's.
OPERATIONS: dict[str, Any] = {
    "equipment": [{"facility_id": "intake_desk", "capacity": 2, "working_units": 1}],
    "initial_resources": {
        "status": "entered",
        "food_meal_units": 120,
        "water_ml": 60_000,
        "source_kind": "operator",
    },
    "scheduled": [
        {
            "kind": "facility_outage",
            "facility_id": "toilet_01",
            "start_minute": 0,
            "end_minute": 120,
            "units_out": 1,
        },
        {
            "kind": "water_outage",
            "facility_id": "water_station_main",
            "start_minute": 30,
            "end_minute": 90,
        },
        {
            "kind": "staff_unavailable",
            "staff_id": "staff_002",
            "start_minute": 5,
            "end_minute": 30,
            "repeat_every_minutes": 60,
        },
        {
            "kind": "resource_delivery",
            "resource_id": "water_ml",
            "quantity": 100_000,
            "due_minute": 150,
            "warehouse_id": "main_warehouse",
        },
    ],
}


def _generated(**fields: Any) -> ScenarioSpec:
    request = InitialScenarioRequest(
        municipality="合成市", disaster="地震", evacuees=30, duration_days=0.25, seed=2026, **fields
    )
    return InitialScenarioBuilder().build(request).scenario


def _fixture(name: str, *, minutes: int, agents: int) -> ScenarioSpec:
    document = ScenarioSpec.load(SCENARIOS / f"{name}.json").model_dump(mode="json")
    document["duration_minutes"] = minutes
    document["profiles"] = document["profiles"][:agents]
    document["scheduled_events"] = [
        event for event in document.get("scheduled_events", []) if event["due_minute"] < minutes
    ]
    return ScenarioSpec.model_validate(document)


class _Run:
    """A world and the engine that moves it on."""

    def __init__(self, scenario: ScenarioSpec, program: Any = None) -> None:
        self.world = WorldState.from_scenario(scenario)
        if program is not None:
            schedule_effect_program(self.world, program)
        sink = _MetricSink(MetricsRecorder())
        controller = OfflineSimulationController(self.world, sink)
        self.engine = TickEngine(self.world, phase_handler=controller)

    def to(self, minute: int) -> WorldState:
        if minute > self.world.minute_index:
            self.engine.run(steps=minute - self.world.minute_index)
        return self.world


def _runs(add_toilets_program: Any) -> dict[str, _Run]:
    return {
        "generated-operations": _Run(
            _generated(operational_conditions=OPERATIONS),
            add_toilets_program(start_minute=20),
        ),
        "water-outage": _Run(_fixture("water_outage", minutes=240, agents=25)),
        "multi-operational": _Run(_fixture("multi_operational", minutes=240, agents=25)),
    }


# ---- What is compared ---------------------------------------------------------------------------


def _shape(value: Any, depth: int = 0) -> Any:
    """The value with its type and the order of every map in it."""
    if depth > 60:
        raise AssertionError("a value nested too deep")
    if isinstance(value, enum.Enum):
        return ("enum", type(value).__name__, value.value)
    if value is None or isinstance(value, (bool, int, float, str)):
        return (type(value).__name__, value)
    if isinstance(value, dict):
        items = [(key, _shape(item, depth + 1)) for key, item in dict.items(value)]
        return (type(value).__name__, items)
    if isinstance(value, (list, tuple)):
        return (type(value).__name__, [_shape(item, depth + 1) for item in value])
    if hasattr(value, "__dict__"):
        # An object's private attributes are not its state (SupportRequestState's
        # _write_listener, which a manager leaves None and a copy does not have).
        items = [
            (key, _shape(item, depth + 1))
            for key, item in vars(value).items()
            if not key.startswith("_")
        ]
        return (type(value).__name__, items)
    if dataclasses.is_dataclass(value):
        items = [
            (f.name, _shape(getattr(value, f.name), depth + 1)) for f in dataclasses.fields(value)
        ]
        return (type(value).__name__, items)
    if hasattr(value, "items"):
        return (
            type(value).__name__,
            [(key, _shape(item, depth + 1)) for key, item in value.items()],
        )
    raise AssertionError(f"no shape for {type(value).__name__}")


def _fingerprint(world: WorldState) -> dict[str, Any]:
    """Every map of the world in its iteration order, with the types of its values."""
    state = world.__dict__
    core = state["_core"]
    store = core.entities()
    out: dict[str, Any] = {}
    for item in dataclasses.fields(WorldState):
        name = item.name
        value = state[name]
        if name in state_mirror.ENTITY_FIELDS:
            out[name] = _shape(value)
            out[f"{name} rows"] = store.keys(name)
        elif name == "event_queue":
            out[name] = [_shape(event) for event in value._store.events()]
            out["pending"] = [event.event_id for event in value.pending()]
        elif name == "event_log":
            out[name] = len(value)
        elif name == "model_state":
            items = []
            for key in dict.keys(value):
                held = dict.__getitem__(value, key)
                if held is state_mirror._DETACHED:
                    items.append((key, "detached", _shape(value._mirrors[key].document())))
                else:
                    items.append((key, _shape(held)))
            out[name] = (type(value).__name__, items)
        else:
            out[name] = _shape(value)
    for name in core.owned_names():
        mirror = core.mirror(name)
        out[f"row {name}"] = (
            _shape(mirror.document()) if mirror.has_row else mirror.tainted,
            mirror.is_detached,
        )
    return out


def _first_difference(mine: Any, theirs: Any, path: str = "") -> str:
    if isinstance(mine, dict) and isinstance(theirs, dict):
        for key in list(mine) + [key for key in theirs if key not in mine]:
            if mine.get(key) != theirs.get(key):
                return _first_difference(mine.get(key), theirs.get(key), f"{path}/{key}")
    if isinstance(mine, (list, tuple)) and isinstance(theirs, (list, tuple)):
        for index, (first, second) in enumerate(zip(mine, theirs, strict=False)):
            if first != second:
                return _first_difference(first, second, f"{path}[{index}]")
    return f"{path}: {str(mine)[:200]} != {str(theirs)[:200]}"


_ATOMS = (type(None), bool, int, float, str, enum.Enum)


def _nodes(value: Any, out: list[int]) -> None:
    """The identities of the containers of a tree (an atom's identity is no state)."""
    if isinstance(value, _ATOMS):
        return
    out.append(id(value))
    if isinstance(value, dict):
        for item in dict.values(value):
            _nodes(item, out)
    elif isinstance(value, list):
        for item in value:
            _nodes(item, out)


def _identities(world: WorldState) -> dict[str, Any]:
    """The objects and containers the world holds, by their identity."""
    state = world.__dict__
    core = state["_core"]
    out: dict[str, Any] = {key: id(value) for key, value in state.items()}
    for name in state_mirror.ENTITY_FIELDS:
        for key, entity in dict.items(state[name]):
            out[f"{name}[{key}]"] = id(entity)
            out[f"{name}[{key}] fields"] = [
                id(item) for item in vars(entity).values() if not isinstance(item, _ATOMS)
            ]
    table = state["model_state"]
    for key, held in dict.items(table):
        nodes: list[int] = []
        _nodes(held, nodes)
        out[f"model_state[{key}]"] = nodes
    for name in state_mirror.OWNED_FIELDS:
        nodes = []
        _nodes(state[name], nodes)
        out[name] = nodes
    out["pending"] = [id(event) for event in state["event_queue"].pending()]
    out["stamps"] = {
        name: core.mirror(name).generation_of(
            state[name]
            if name in state_mirror.OWNED_FIELDS
            else dict.get(table, name, state_mirror._MISSING)
        )
        for name in core.owned_names()
        if name in state_mirror.OWNED_FIELDS or name in table
    }
    return out


def _check_rows(world: WorldState) -> None:
    core = world.__dict__["_core"]
    assert core.entities().verify(world) is None
    for name in core.owned_names():
        assert core.mirror(name).verify(), name


# ---- A transaction's start is the copy through canonical JSON -----------------------------------


@pytest.mark.parametrize("kind", ["full", "effect"])
def test_a_transaction_starts_from_what_a_copy_through_canonical_json_holds(
    add_toilets_program: Any, kind: str
) -> None:
    for label, run in _runs(add_toilets_program).items():
        for minute in (0, 25, 70, 160):
            world = run.to(minute)
            copied = WorldState.from_dict(world.to_dict())
            before = _fingerprint(world)
            text = world.canonical_json()
            with state_mirror.transaction(world, kind) as txn:
                assert world.__dict__["_core"].in_transaction
                assert world.canonical_json() == copied.canonical_json(), (label, minute)
                mine, theirs = _fingerprint(world), _fingerprint(copied)
                assert mine == theirs, (label, minute, _first_difference(mine, theirs))
                _check_rows(world)
                txn.rollback()
            assert not world.__dict__["_core"].in_transaction
            after = _fingerprint(world)
            assert after == before, (label, minute, _first_difference(after, before))
            assert world.canonical_json() == text
            _check_rows(world)


def test_a_namespace_no_row_holds_goes_through_canonical_json(add_toilets_program: Any) -> None:
    """A value V can't hold (a tuple) taints its key; the copy holds a list, and owns it again."""
    world = _runs(add_toilets_program)["generated-operations"].to(10)
    world.model_state["custom_pair_v1"] = {"pair": (1, 2), "b": 1, "a": {"z": 0, "y": 1}}
    mirror = world.__dict__["_core"].mirror("custom_pair_v1")
    assert mirror.tainted is not None
    copied = WorldState.from_dict(world.to_dict())
    before = _fingerprint(world)
    with state_mirror.transaction(world, "full") as txn:
        assert _fingerprint(world) == _fingerprint(copied)
        assert mirror.tainted is None
        assert list(world.model_state["custom_pair_v1"]) == ["a", "b", "pair"]
        assert world.model_state["custom_pair_v1"]["pair"] == [1, 2]
        txn.rollback()
    assert _fingerprint(world) == before
    assert world.model_state["custom_pair_v1"]["pair"] == (1, 2)
    assert mirror.tainted is not None


# ---- A rollback gives everything back ------------------------------------------------------------


def _write_everything(world: WorldState) -> None:
    """A write of every kind a rule makes: entities from Python and from Rust, keys in and out,
    trees, rows, fields, scalars, the queue and the log."""
    agent_id, other_id = list(world.agents)[:2]
    agent = world.agents[agent_id]
    agent.stress = 0.123
    agent.needs.hunger = 0.9
    world.agents[other_id].needs = NeedState(
        toilet_urgency=0.1, hunger=0.2, fatigue=0.3, hydration_need=0.4
    )
    native.state("entities").apply_row_ops(
        world, "agents", [("write", agent_id, "trust", 0.01), ("write", other_id, "stress", 0.5)]
    )
    facility_id = next(iter(world.facilities))
    world.facilities[facility_id].queue_agent_ids = [agent_id]
    world.facilities[facility_id].active_agent_ids.append(other_id)
    staff_id = next(iter(world.staff))
    world.staff[staff_id].work_queue.append("task-x")
    world.support_requests["request-new"] = SupportRequestState(
        id="request-new",
        agent_id=agent_id,
        requested_minute=world.minute_index,
        timeout_minute=world.minute_index + 30,
        status=SupportRequestStatus.PENDING,
    )
    world.active_tasks["task-new"] = ActiveTask(
        id="task-new",
        kind="travel",
        agent_id=None,
        resource_id=None,
        start_minute=0,
        due_minute=world.minute_index + 5,
    )
    del world.facilities[facility_id]
    world.zones[next(iter(world.zones))].capacity = 999
    world.zone_edges = {}
    table = world.model_state
    table["brand_new_v1"] = {"b": [1, {"y": 2, "x": 3}], "a": None}
    positions = table["movement_v1"]["positions"]
    positions[next(iter(positions))]["zone_id"] = "elsewhere"
    del table["operational_events_v1"]
    world.metric_state["new_metric"] = 1.5
    resource = next(iter(world.resources))
    world.resources[resource] += 7
    world.id_counters["event"] = world.id_counters.get("event", 0) + 5
    world.model_configs["movement_model"]["speed_m_per_minute_by_mobility"]["independent"] = 1.0
    world.minute_index += 1
    world.rng_state = {"replaced": True}
    sequence = world.allocate_event_sequence()
    pending = world.event_queue.pending()
    world.event_queue.remove(pending[0].event_id)
    event = ScheduledEvent(
        event_id=f"txn-{sequence}",
        kind=EventKind.EXOGENOUS,
        due_minute=world.minute_index + 3,
        priority=0,
        sequence=sequence,
        event_type="txn_test",
        payload={"b": 1, "a": 2},
    )
    world.schedule_event(event)
    record_event = ScheduledEvent(
        event_id=f"txn-done-{sequence}",
        kind=EventKind.EXOGENOUS,
        due_minute=world.minute_index,
        priority=0,
        sequence=sequence + 1_000_000,
        event_type="txn_test",
        payload={},
    )
    world.event_log.append(
        EventLogRecord(
            event=record_event,
            realized_minute=world.minute_index,
            status=EventStatus.APPLIED,
            detail=None,
        )
    )


def test_a_rollback_gives_back_the_world_its_order_and_every_object(
    add_toilets_program: Any,
) -> None:
    world = _runs(add_toilets_program)["generated-operations"].to(70)
    world.event_queue.pending()  # every pending event has its object
    saved = copy.deepcopy(world)
    before = _fingerprint(world)
    identities = _identities(world)
    text = world.canonical_json()
    log_length = len(world.event_log)
    old_maps = {name: world.__dict__[name] for name in state_mirror.ENTITY_FIELDS}
    with state_mirror.transaction(world, "full") as txn:
        new_maps = {name: world.__dict__[name] for name in state_mirror.ENTITY_FIELDS}
        assert all(new_maps[name] is not old_maps[name] for name in new_maps)
        _write_everything(world)
        txn.rollback()
    assert world.same_state(saved)
    assert world.canonical_json() == text
    # A key first stored in the block keeps its slot, with no state, as a deleted key does.
    after = {
        key: value
        for key, value in _fingerprint(world).items()
        if key in before or value != (None, False)
    }
    assert after == before, _first_difference(after, before)
    now = _identities(world)
    assert now == identities, _first_difference(now, identities)
    assert len(world.event_log) == log_length
    assert world.event_log._store.audit(world.minute_index, world.duration_minutes) == 0
    _check_rows(world)
    for name, kind in state_mirror.ENTITY_FIELDS.items():
        assert world.__dict__[name]._store is world.__dict__["_core"].entities()
        assert world.__dict__[name]._table == kind
        assert new_maps[name]._store is None
    # The world runs on as it would have.
    world.validate()
    assert world.to_dict() == saved.to_dict()


def test_an_error_in_the_block_rolls_back_and_passes_on(add_toilets_program: Any) -> None:
    world = _runs(add_toilets_program)["generated-operations"].to(40)
    text = world.canonical_json()
    identities = _identities(world)
    with pytest.raises(KeyError, match="the block failed"):
        with state_mirror.transaction(world, "effect"):
            _write_everything(world)
            raise KeyError("the block failed")
    assert world.canonical_json() == text
    assert _identities(world) == identities
    _check_rows(world)


def test_a_commit_keeps_the_writes_in_canonical_order(add_toilets_program: Any) -> None:
    world = _runs(add_toilets_program)["generated-operations"].to(70)
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
    _check_rows(world)
    world.validate()
    # The world runs on; a second transaction opens after the first.
    run = _Run.__new__(_Run)
    run.world = world
    run.engine = TickEngine(
        world, phase_handler=OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    )
    run.to(80)
    with state_mirror.transaction(world, "effect") as txn:
        txn.commit()
    _check_rows(world)


def test_a_world_that_fails_its_validation_is_not_changed(
    add_toilets_program: Any, monkeypatch: pytest.MonkeyPatch
) -> None:
    world = _runs(add_toilets_program)["generated-operations"].to(30)
    text = world.canonical_json()
    identities = _identities(world)
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
    assert _identities(world) == identities
    assert not world.__dict__["_core"].in_transaction
    _check_rows(world)


def test_transactions_do_not_nest_and_unsupported_worlds_are_refused(
    add_toilets_program: Any,
) -> None:
    world = _runs(add_toilets_program)["generated-operations"].to(5)
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
