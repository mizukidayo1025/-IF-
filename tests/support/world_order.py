"""Worlds of the gate's Scenarios at several minutes, and what tells two worlds apart.

For the tests of a world's copies (Issue #152): a transaction in place against the copy
through canonical JSON (integration/test_world_txn.py, lane 6-2) and deepcopy and the
previews' copy against the old copies (integration/test_world_fork.py, lane 6-6). Nothing
here has state: a Run is made by the test that uses it.
"""

from __future__ import annotations

import dataclasses
import enum
from typing import Any

from sheltersim import native
from sheltersim.core import state_mirror
from sheltersim.core.engine import TickEngine
from sheltersim.core.world import WorldState
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
from support.paths import SCENARIOS

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


def generated(**fields: Any) -> ScenarioSpec:
    request = InitialScenarioRequest(
        municipality="合成市", disaster="地震", evacuees=30, duration_days=0.25, seed=2026, **fields
    )
    return InitialScenarioBuilder().build(request).scenario


def fixture_scenario(name: str, *, minutes: int, agents: int) -> ScenarioSpec:
    document = ScenarioSpec.load(SCENARIOS / f"{name}.json").model_dump(mode="json")
    document["duration_minutes"] = minutes
    document["profiles"] = document["profiles"][:agents]
    document["scheduled_events"] = [
        event for event in document.get("scheduled_events", []) if event["due_minute"] < minutes
    ]
    return ScenarioSpec.model_validate(document)


class Run:
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


def gate_runs(add_toilets_program: Any) -> dict[str, Run]:
    return {
        "generated-operations": Run(
            generated(operational_conditions=OPERATIONS),
            add_toilets_program(start_minute=20),
        ),
        "water-outage": Run(fixture_scenario("water_outage", minutes=240, agents=25)),
        "multi-operational": Run(fixture_scenario("multi_operational", minutes=240, agents=25)),
    }


# ---- What is compared ---------------------------------------------------------------------------


def shape(value: Any, depth: int = 0) -> Any:
    """The value with its type and the order of every map in it."""
    if depth > 60:
        raise AssertionError("a value nested too deep")
    if isinstance(value, enum.Enum):
        return ("enum", type(value).__name__, value.value)
    if value is None or isinstance(value, (bool, int, float, str)):
        return (type(value).__name__, value)
    if isinstance(value, dict):
        items = [(key, shape(item, depth + 1)) for key, item in dict.items(value)]
        return (type(value).__name__, items)
    if isinstance(value, (list, tuple)):
        return (type(value).__name__, [shape(item, depth + 1) for item in value])
    if hasattr(value, "__dict__"):
        # An object's private attributes are not its state (SupportRequestState's
        # _write_listener, which a manager leaves None and a copy does not have).
        items = [
            (key, shape(item, depth + 1))
            for key, item in vars(value).items()
            if not key.startswith("_")
        ]
        return (type(value).__name__, items)
    if dataclasses.is_dataclass(value):
        items = [
            (f.name, shape(getattr(value, f.name), depth + 1)) for f in dataclasses.fields(value)
        ]
        return (type(value).__name__, items)
    if hasattr(value, "items"):
        return (
            type(value).__name__,
            [(key, shape(item, depth + 1)) for key, item in value.items()],
        )
    raise AssertionError(f"no shape for {type(value).__name__}")


def fingerprint(world: WorldState) -> dict[str, Any]:
    """Every map of the world in its iteration order, with the types of its values."""
    state = world.__dict__
    core = state["_core"]
    store = core.entities()
    out: dict[str, Any] = {}
    for item in dataclasses.fields(WorldState):
        name = item.name
        value = state[name]
        if name in state_mirror.ENTITY_FIELDS:
            out[name] = shape(value)
            out[f"{name} rows"] = store.keys(name)
        elif name == "event_queue":
            out[name] = [shape(event) for event in value._store.events()]
            out["pending"] = [event.event_id for event in value.pending()]
        elif name == "event_log":
            out[name] = len(value)
        elif name == "model_state":
            items = []
            for key in dict.keys(value):
                held = dict.__getitem__(value, key)
                if held is state_mirror._DETACHED:
                    items.append((key, "detached", shape(value._mirrors[key].document())))
                else:
                    items.append((key, shape(held)))
            out[name] = (type(value).__name__, items)
        else:
            out[name] = shape(value)
    for name in core.owned_names():
        mirror = core.mirror(name)
        out[f"row {name}"] = (
            shape(mirror.document()) if mirror.has_row else mirror.tainted,
            mirror.is_detached,
        )
    return out


def first_difference(mine: Any, theirs: Any, path: str = "") -> str:
    if isinstance(mine, dict) and isinstance(theirs, dict):
        for key in list(mine) + [key for key in theirs if key not in mine]:
            if mine.get(key) != theirs.get(key):
                return first_difference(mine.get(key), theirs.get(key), f"{path}/{key}")
    if isinstance(mine, (list, tuple)) and isinstance(theirs, (list, tuple)):
        for index, (first, second) in enumerate(zip(mine, theirs, strict=False)):
            if first != second:
                return first_difference(first, second, f"{path}[{index}]")
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


def identities(world: WorldState) -> dict[str, Any]:
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


def check_rows(world: WorldState) -> None:
    core = world.__dict__["_core"]
    assert core.entities().verify(world) is None
    for name in core.owned_names():
        assert core.mirror(name).verify(), name


def write_everything(world: WorldState) -> None:
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
