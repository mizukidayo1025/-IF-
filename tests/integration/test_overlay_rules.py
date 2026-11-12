"""rules/overlay.rs and rules/facility_queues.rs against their Python rules, on copies of a world.

The 7-day Run reaches almost none of these, so what is compared here and by the shadow over the
gate is all that is compared (docs/rust-port/lanes/m6-overlay.md). Values next to a limit or a
coefficient are chosen so a change of 1% shows.

The rules of temporary spaces, rooms, beds, installed facilities and changes of the floor plan are
checked without Python in `native/core/tests/overlay_rules.rs`, on the same world
(`support/core_fixtures_overlay.py`, #225). Here each of their Python entries keeps a test that it
still reaches the kernel and gives its answer, beside the checks against the Python validators
(decision D3) and the refusals of the boundary.
"""

from __future__ import annotations

import hashlib
import json
from copy import deepcopy
from typing import Any

import pytest
from support.core_fixtures_overlay import (
    ROOM_FIELDS,
    change_cases,
    room_record,
    space,
)
from support.world import world_digest

from sheltersim import native
from sheltersim.core import state_mirror
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.world import WorldState
from sheltersim.domain import (
    facility_queues,
    geometry_overlay,
    movement,
    queueing,
    temporary_spaces,
)
from sheltersim.domain.facilities import ToiletServiceManager
from sheltersim.domain.models import Activity
from sheltersim.domain.resource_service import ResourceServiceManager
from sheltersim.domain.simulator_state import initialize_simulator_extensions
from sheltersim.domain.temporary_spaces import create_temporary_spaces_state
from sheltersim.events.models import EventKind, ScheduledEvent

ROOM_ZONE = "sleeping"


@pytest.fixture(autouse=True)
def _trees(keep_trees: Any) -> None:
    """The resource FIFO tests pin the tree the kernels write beside the rows, key order and all:
    the service state and the ledger keep their trees here (support/world.py keep_trees)."""
    keep_trees("resource_service_v1", "resource_ledger_v1")


def _rules(name: str = "overlay") -> Any:
    return native.rules(name)


def _copy(world: WorldState) -> WorldState:
    return WorldState.from_dict(world.to_dict())


_VALUE_ROWS = frozenset(
    {
        "population_presence_v1",
        "population_lifecycle_v1",
        "population_dynamics_v1",
        "geometry_overlay_v1",
    }
)


def _bytes(world: WorldState) -> str:
    """The state as it is, valid or not: `to_dict` would check it.

    A detached namespace (test_detached_namespaces.py) is read as a tree built in the order of
    its typed row; these worlds come from JSON, whose keys are sorted. The namespaces whose row is
    a `V` (_VALUE_ROWS) keep the order of their keys, as the tree did, and are read as they are.
    """
    model_state = dict(world.model_state)
    for name in (state_mirror.DETACHED_NAMESPACES - _VALUE_ROWS) & model_state.keys():
        model_state[name] = json.loads(json.dumps(model_state[name], sort_keys=True))
    parts = (
        model_state,
        world.metric_state,
        world.id_counters,
        repr(world.agents),
        repr(world.facilities),
        repr(world.staff),
        repr(world.active_tasks),
        repr(world.zone_edges),
        repr(world.event_queue.pending()),
        # A record the log holds as its bytes reads back with its keys sorted.
        [json.dumps(record.to_dict(), sort_keys=True) for record in world.event_log.records],
    )
    return json.dumps(parts, ensure_ascii=False)


def _same_bytes(first: str, second: str) -> None:
    """Without pytest's diff of the two, which takes far too long on a whole world."""
    if first != second:
        place = next(
            (at for at, pair in enumerate(zip(first, second)) if pair[0] != pair[1]),
            min(len(first), len(second)),
        )
        pytest.fail(f"they differ at {place}: {first[place - 80 : place + 80]!r}")


def _same_world(first: WorldState, second: WorldState) -> None:
    _same_bytes(_bytes(first), _bytes(second))


def _plain(value: Any) -> Any:
    if hasattr(value, "model_dump"):
        return value.model_dump(mode="json")
    if isinstance(value, (set, frozenset)):
        return sorted(value)
    if isinstance(value, (tuple, list)):
        return [_plain(item) for item in value]
    if isinstance(value, dict):
        return {key: _plain(item) for key, item in value.items()}
    return value


# Digests of what the deleted Python rules of `kernel_only` kernels answered and left, recorded once
# from them (2d8fb46) on Linux: route points and lengths are floats, the same bits on every OS.
# The toilet's entries whose world has queue records, and the "serves" and "interrupted" of
# RESOURCE_FIFO, were recorded again when facility_queues_v1 was detached (#134): `_bytes` sorts a
# detached namespace's keys, and the old digests held the tree's order. The new ones came from the
# same worlds with the namespace still attached (whose old digests passed) and from the detached
# ones, and were the same.
RECORDED = {
    "walk toilet_01-left-fifo": "723961dac125c409eaa43b22c60ded783d384b6f2257dc5e7f68c840dc7fc7b8",
    "walk toilet_01-right-fifo": "a39a0e59ab284d406c24446027b37d51bc0d29584aadb83fa31fc9954e45fb0c",
    "toilet request queue": "c4f5d6d8b263779c3486b488bae35b72b18871808dbb01e9765ad8950ab2b526",
    "toilet request": "c4f5d6d8b263779c3486b488bae35b72b18871808dbb01e9765ad8950ab2b526",
    "toilet head": "98f439dc7b6571d37a8ef60ffa986f53a6c5a973178c820749048dcec5bad3a4",
    "toilet head full": "d76e169018dc0a5a440b675167cd8551c97159800df97b0362cffc890078f022",
    "toilet head late": "ab860141b3d1f42890b80543acf48495f59fe11a186d0ca7380296c3d13d218f",
    "toilet head unskilled": "6b7473565ba7c92d8106948bfbe766bfe9ed4a0c045d3241391fec25ffabc3c8",
    "toilet head staffed": "5508059c468028e6e62f649129e486e874bf243daca667c7ea13495235ec839e",
}


def _answer(world: WorldState, call: Any, *arguments: Any, **options: Any) -> tuple[Any, Any]:
    """The call on a copy of the world: what it returned, and the copy it left."""
    ours = _copy(world)
    result = call(ours, *arguments, **options)
    for name in ("geometry_overlay_v1", "temporary_spaces_v1", "facility_queues_v1"):
        mirror = ours.model_state._mirrors.get(name)
        if mirror is not None and name in ours.model_state:
            assert mirror.tainted is None
            assert mirror.verify()
    return result, ours


def _recorded(name: str, result: Any, after: WorldState) -> None:
    """The result and the world after it are the deleted rule's (RECORDED)."""
    text = json.dumps(_plain(result)) + "\n" + _bytes(after)
    assert hashlib.sha256(text.encode()).hexdigest() == RECORDED[name], name


def _same(world: WorldState, kernel: Any, rule: Any, *arguments: Any, **options: Any) -> Any:
    """The kernel and the Python rule on two copies: the same result and the same world."""
    theirs = _copy(world)
    ours = _copy(world)
    expected = rule(theirs, *arguments, **options)
    result = kernel(ours, *arguments, **options)
    assert json.dumps(_plain(result)) == json.dumps(_plain(expected))
    _same_world(ours, theirs)
    for name in ("geometry_overlay_v1", "temporary_spaces_v1", "facility_queues_v1"):
        mirror = ours.model_state._mirrors.get(name)
        if mirror is not None and name in ours.model_state:
            assert mirror.tainted is None
            assert mirror.verify()
    return result


def _refused(world: WorldState, kernel: Any, rule: Any, *arguments: Any, **options: Any) -> None:
    """Unsupported with nothing changed, where the Python rule raises its own text."""
    before = _bytes(world)
    with pytest.raises(native.Unsupported):
        kernel(world, *arguments, **options)
    _same_bytes(_bytes(world), before)
    with pytest.raises((ValueError, KeyError, TypeError, AttributeError)):
        rule(_copy(world), *arguments, **options)


def _raises_as_python(
    world: WorldState, kernel: Any, rule: Any, *arguments: Any, **options: Any
) -> None:
    """The Python rule's error, class and text, with nothing changed (native/src/core/errors.rs)."""
    before = _bytes(world)
    with pytest.raises(Exception) as ours:
        kernel(world, *arguments, **options)
    _same_bytes(_bytes(world), before)
    with pytest.raises(Exception) as theirs:
        rule(_copy(world), *arguments, **options)
    assert (type(ours.value), str(ours.value)) == (type(theirs.value), str(theirs.value))


def _world(scenario: Any) -> WorldState:
    world = WorldState.from_scenario(scenario)
    initialize_simulator_extensions(world)
    return world


@pytest.fixture
def world(generated_scenario: Any) -> WorldState:
    return _world(generated_scenario)


@pytest.fixture
def queued(generated_scenario_with_split_queues: Any) -> WorldState:
    return _world(generated_scenario_with_split_queues)


# ---- Temporary spaces ----------------------------------------------------------------------------


@pytest.fixture
def spaces(world: WorldState) -> WorldState:
    world.model_state[temporary_spaces.TEMPORARY_SPACES_STATE_KEY] = create_temporary_spaces_state()
    return world


def _history_row(event_id: str, space_id: str, zone: str, transition: str, **fields: Any) -> dict:
    return {
        "event_id": event_id,
        "space_id": space_id,
        "zone_id": zone,
        "minute": fields.get("minute", 0),
        "transition": transition,
        "causation_id": fields.get("causation_id"),
    }


def test_a_space_registers_and_expires(spaces: WorldState) -> None:
    zone = next(iter(spaces.zones))
    state = spaces.model_state[temporary_spaces.TEMPORARY_SPACES_STATE_KEY]
    spaces.minute_index = 4
    temporary_spaces.register_temporary_space(spaces, space("a", zone), event_id="e1")
    assert state["spaces"]["a"] == space("a", zone)
    assert state["history"] == [_history_row("e1", "a", zone, "activated", minute=4)]
    before = _bytes(spaces)
    assert not temporary_spaces.expire_temporary_space(
        spaces, "b", event_id="e2", causation_id=None
    )
    assert _bytes(spaces) == before
    spaces.minute_index = 9
    assert temporary_spaces.expire_temporary_space(spaces, "a", event_id="e2", causation_id="e1")
    assert state["spaces"]["a"]["active"] is False
    assert state["spaces"]["a"]["end_event_id"] == "e2"
    assert state["history"][1:] == [
        _history_row("e2", "a", zone, "expired", minute=9, causation_id="e1")
    ]
    before = _bytes(spaces)
    assert not temporary_spaces.expire_temporary_space(
        spaces, "a", event_id="e3", causation_id=None
    )
    assert _bytes(spaces) == before
    for name in ("temporary_spaces_v1",):
        mirror = spaces.model_state._mirrors.get(name)
        assert mirror is None or (mirror.tainted is None and mirror.verify())


def test_an_input_outside_the_contract_is_a_kernel_refusal(spaces: WorldState) -> None:
    with pytest.raises(KernelRefusal, match="overlay.capacity_metrics refused") as refused:
        temporary_spaces.capacity_metrics(spaces, "no-such-zone")
    # The kernel's own reason goes with it, and nothing stands behind it to call.
    assert isinstance(refused.value.__cause__, native.Unsupported)
    assert str(refused.value) == f"overlay.capacity_metrics refused: {refused.value.__cause__}"
    assert not hasattr(temporary_spaces.capacity_metrics, "__wrapped__")


@pytest.mark.parametrize("occupancy", [None, 0, 3, 10_000])
def test_capacity_adds_active_spaces_and_keeps_expired_ones_while_people_need_them(
    spaces: WorldState, occupancy: int | None
) -> None:
    zone = next(zone for zone in spaces.zones if zone != "corridor")
    state = spaces.model_state[temporary_spaces.TEMPORARY_SPACES_STATE_KEY]
    state["spaces"]["active"] = space("active", zone)
    state["spaces"]["old"] = space("old", zone, active=False, people=7)
    state["spaces"]["other"] = space("other", "corridor", people=9)
    spaces.zones[zone].capacity = 2
    current = (
        sum(agent.zone_id == zone for agent in spaces.agents.values())
        if occupancy is None
        else occupancy
    )
    retained = min(7, max(0, current - 2 - 5))
    expected = {
        "nominal_capacity": 2,
        "effective_capacity": 7 + retained,
        "temporary_people_capacity": 5,
        "retained_expired_capacity": retained,
        "temporary_sleeping_capacity": 2,
        "physical_room_capacity": 0,
        "active_room_admissions": 0,
        "retained_expired_room_admissions": 0,
        "effective_total_capacity": 7 + retained,
    }
    assert temporary_spaces.capacity_metrics(spaces, zone, occupancy=occupancy) == expected
    assert temporary_spaces.effective_zone_capacity(spaces, zone, occupancy=occupancy) == (
        7 + retained
    )
    assert temporary_spaces.effective_total_zone_capacity(spaces, zone, occupancy=occupancy) == (
        7 + retained
    )
    spaces.zones[zone].capacity = None
    assert temporary_spaces.capacity_metrics(spaces, zone, occupancy=occupancy) == {
        "nominal_capacity": None,
        "effective_capacity": None,
        "temporary_people_capacity": 0,
        "retained_expired_capacity": 0,
        "temporary_sleeping_capacity": 0,
        "physical_room_capacity": 0,
        "active_room_admissions": 0,
        "retained_expired_room_admissions": 0,
        "effective_total_capacity": None,
    }


def test_incoming_travelers_are_the_people_walking_to_the_zone_or_its_facilities(
    world: WorldState,
) -> None:
    from sheltersim.domain.models import Activity

    assert all(agent.activity is not Activity.MOVING for agent in world.agents.values())
    first, second = list(world.agents)[:2]
    world.agents[first].activity = Activity.MOVING
    world.agents[first].target_id = ROOM_ZONE
    toilet = next(key for key, item in world.facilities.items() if "toilet" in item.kind)
    world.agents[second].activity = Activity.MOVING
    world.agents[second].target_id = toilet
    targets = {first: ROOM_ZONE, second: world.facilities[toilet].zone_id}
    for zone in (ROOM_ZONE, world.facilities[toilet].zone_id, "corridor"):
        for excluding in (None, first, second):
            expected = sum(
                target == zone and agent_id != excluding for agent_id, target in targets.items()
            )
            assert temporary_spaces.incoming_travelers(world, zone, excluding=excluding) == expected


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("people_capacity", 0),
        ("people_capacity", 1001),
        ("sleeping_capacity", 6),
        ("sleeping_capacity", -1),
        ("start_minute", -1),
        ("end_minute", 0),
        ("start_event_id", ""),
        ("end_event_id", "end"),
        ("program_id", 1),
        ("source_ids", [""]),
        ("geometry_status", "room"),
        ("zone_id", "nowhere"),
        ("active", 1),
    ],
)
def test_the_check_of_spaces_takes_nothing_the_python_rule_refuses(
    spaces: WorldState, field: str, value: Any
) -> None:
    zone = next(iter(spaces.zones))
    state = spaces.model_state[temporary_spaces.TEMPORARY_SPACES_STATE_KEY]
    state["spaces"]["a"] = space("a", zone)
    assert _rules().validate_spaces(spaces) is True
    temporary_spaces.validate_temporary_spaces_state(spaces)
    state["spaces"]["a"][field] = value
    with pytest.raises(ValueError):
        temporary_spaces.validate_temporary_spaces_state(spaces)
    with pytest.raises(native.Unsupported):
        _rules().validate_spaces(spaces)


# ---- Rooms and beds ------------------------------------------------------------------------------


def _room_world(world: WorldState, *, people: int = 2, beds: int = 1, room_id: str = "room") -> str:
    state = world.model_state[geometry_overlay.GEOMETRY_OVERLAY_KEY]
    state["temporary_rooms"][room_id] = room_record(world, room_id, people=people, beds=beds)
    return room_id


def _person_in_room_zone(world: WorldState) -> str:
    return next(key for key, agent in world.agents.items() if agent.zone_id == ROOM_ZONE)


def _writes(world: WorldState, call: Any, change: Any) -> Any:
    """The rule on a copy of the world, and `change` on another: the same world after both.

    `change` writes what the deleted Python rule wrote (lane L10), so the copy shows what the
    kernel must write and nothing else.
    """
    ours = _copy(world)
    expected = _copy(world)
    result = call(ours)
    change(expected)
    _same_world(ours, expected)
    for name in ("geometry_overlay_v1", "temporary_spaces_v1", "facility_queues_v1"):
        mirror = ours.model_state._mirrors.get(name)
        if mirror is not None and name in ours.model_state:
            assert mirror.tainted is None
            assert mirror.verify()
    return result


def _raises_text(world: WorldState, call: Any, text: str) -> None:
    """A plain ValueError with the Python rule's text, and nothing changed."""
    before = _bytes(world)
    with pytest.raises(ValueError) as said:
        call(world)
    assert type(said.value) is ValueError
    assert str(said.value) == text
    _same_bytes(_bytes(world), before)


@pytest.mark.parametrize("held", [True, False])
def test_a_release_drops_the_person_s_bed_only(world: WorldState, held: bool) -> None:
    _room_world(world, people=4, beds=2)
    agent_id = _person_in_room_zone(world)
    if held:
        geometry_overlay.reserve_sleep_bed(world, agent_id)

    def dropped(expected: WorldState) -> None:
        expected.model_state[geometry_overlay.GEOMETRY_OVERLAY_KEY]["bed_reservations"].pop(
            agent_id, None
        )

    released = _writes(
        world, lambda ours: geometry_overlay.release_sleep_bed(ours, agent_id), dropped
    )
    assert released is held


def _admission(agent_id: str, room_id: str, minute: int, event_id: str) -> dict[str, Any]:
    return {
        "agent_id": agent_id,
        "room_id": room_id,
        "admitted_minute": minute,
        "event_id": event_id,
        "source_ids": sorted(
            {"ASM-TEMP-ROOM-TEST-V1", geometry_overlay.TEMPORARY_ROOM_INTAKE_SOURCE_ID}
        ),
    }


def test_a_room_takes_people_at_free_points_up_to_its_capacity(world: WorldState) -> None:
    from sheltersim.contracts.extensions import Point2D

    room = _room_world(world, people=2, beds=1)
    state = world.model_state[geometry_overlay.GEOMETRY_OVERLAY_KEY]
    record = state["temporary_rooms"][room]
    # The bed is the middle of the room, so it is the first point the rule tries.
    bed = Point2D.model_validate(record["bed_points_m"][0])
    assert geometry_overlay.room_admission_point(world, room) == bed
    assert geometry_overlay.available_temporary_room(world, ROOM_ZONE) == room
    first, second, third = list(world.agents)[:3]
    options = {"room_id": room, "minute": 0, "event_id": "e1"}

    def admit(agent_id: str) -> Any:
        return lambda ours: geometry_overlay.admit_to_temporary_room(
            ours, agent_id=agent_id, **options
        )

    def admitted(agent_id: str) -> Any:
        return lambda expected: expected.model_state[geometry_overlay.GEOMETRY_OVERLAY_KEY][
            "room_admissions"
        ].__setitem__(agent_id, _admission(agent_id, room, 0, "e1"))

    assert _writes(world, admit(first), admitted(first)) == bed
    admit(first)(world)
    # Someone standing there takes the point within half a metre, and not beyond it. Then the
    # nearest point of the 0.75 m grid that is clear of them: its first column and third row.
    min_x = min(point["x"] for point in record["polygon_m"])
    min_y = min(point["y"] for point in record["polygon_m"])
    beside = Point2D(x=min_x + 0.375, y=min_y + 0.375 + 0.75 + 0.75)
    position = world.model_state["movement_v1"]["positions"][first]
    for offset, expected in ((0.0, beside), (0.4999, beside), (0.5, bed), (0.6, bed)):
        position["point"] = {"x": bed.x + offset, "y": bed.y}
        assert geometry_overlay.room_admission_point(world, room) == expected
    assert _writes(world, admit(second), admitted(second)) == bed
    admit(second)(world)
    assert geometry_overlay.available_temporary_room(world, ROOM_ZONE) is None

    def released(agent_id: str) -> Any:
        return lambda expected: expected.model_state[geometry_overlay.GEOMETRY_OVERLAY_KEY][
            "room_admissions"
        ].pop(agent_id, None)

    release = geometry_overlay.release_temporary_room_admission
    assert _writes(world, lambda ours: release(ours, first), released(first)) is True
    assert _writes(world, lambda ours: release(ours, third), released(third)) is False
    record["active"] = False
    record["end_event_id"] = "end"
    _raises_text(world, admit(third), "temporary room admission is no longer available")


# ---- A facility placed during a Run --------------------------------------------------------------


def _new_toilet(world: WorldState, facility_id: str = "extra") -> str:
    from sheltersim.domain.models import FacilityState

    world.facilities[facility_id] = FacilityState(
        id=facility_id, kind="toilet", zone_id="toilet", capacity=1, working_units=1
    )
    return facility_id


# Where the Python rule placed four toilets in the generated toilet zone, in turn (lane L10). The
# zone has no room for a fifth.
_PLACED = (
    (13.62417562100671, 9.737087810503356),
    (14.524175621006712, 9.737087810503356),
    (13.62417562100671, 11.21867753513065),
    (14.524175621006712, 11.21867753513065),
)


def _placed(facility_id: str, point: Any, event_id: str, source_ids: list[str]) -> Any:
    import hashlib

    def change(expected: WorldState) -> None:
        state = expected.model_state[geometry_overlay.GEOMETRY_OVERLAY_KEY]
        common = {
            "event_id": event_id,
            "source_ids": sorted(
                {*source_ids, geometry_overlay.INSTALLED_FACILITY_PLACEMENT_SOURCE_ID}
            ),
            "minute": expected.minute_index,
        }
        state["facility_points"][facility_id] = {"point_m": {"x": point.x, "y": point.y}, **common}
        digest = hashlib.sha256(f"{event_id}:{facility_id}".encode()).hexdigest()[:24]
        state["history"].append(
            {
                "transition_id": f"install-{digest}",
                "op": "install_facility",
                "target_id": facility_id,
                "status": "applied",
                "causation_id": event_id,
                **common,
            }
        )

    return change


def test_a_placement_takes_the_next_free_slot_and_records_it(world: WorldState) -> None:
    from sheltersim.contracts.extensions import Point2D

    for index, (x, y) in enumerate(_PLACED):
        facility_id = _new_toilet(world, f"extra_{index}")
        point = Point2D(x=x, y=y)
        assert geometry_overlay.installed_facility_point(world, facility_id) == point
        placed = _writes(
            world,
            lambda ours, facility_id=facility_id, index=index: (
                geometry_overlay.place_installed_facility(
                    ours, facility_id, event_id=f"e{index}", source_ids=("ASM-A", "ASM-A")
                )
            ),
            _placed(facility_id, point, f"e{index}", ["ASM-A"]),
        )
        assert placed == point
        geometry_overlay.place_installed_facility(
            world, facility_id, event_id=f"e{index}", source_ids=("ASM-A",)
        )
    assert geometry_overlay.installed_facility_placement_enabled(world) is True
    world.model_configs["geometry_overlay"]["schema_version"] = 3
    assert geometry_overlay.installed_facility_placement_enabled(world) is False


# ---- Changes of the floor plan -------------------------------------------------------------------


def _event(
    world: WorldState,
    op: str,
    target: str,
    parameters: Any,
    end: int | None,
    event_id: str = "change-1",
) -> Any:
    return ScheduledEvent(
        event_id=event_id,
        kind=EventKind.ENDOGENOUS,
        due_minute=world.minute_index,
        priority=0,
        sequence=900,
        event_type="effect_program_geometry",
        payload={
            "program_id": "program",
            "effect_id": "effect",
            "modifier_id": "modifier",
            "target_id": target,
            "op": op,
            "parameters": parameters,
            "end_minute": end,
        },
        source_id="ASM-SOURCE",
        causation_id="cause",
        provenance={"effect_program_v1": {"assumption_ids": ["ASM-ASSUME", "ASM-SOURCE", ""]}},
    )


def _applied(world: WorldState, event: Any, *, expire: bool = False) -> None:
    """What the deleted Python rule of apply_geometry_change wrote for an approved change."""
    from sheltersim.contracts.operations import TemporaryRoomGeometry

    payload = event.payload
    op, target, modifier = payload["op"], payload["target_id"], payload["modifier_id"]
    parameters = payload["parameters"]
    state = world.model_state[geometry_overlay.GEOMETRY_OVERLAY_KEY]
    marker = event.provenance.get("effect_program_v1", {})
    named = (event.source_id, *parameters.get("source_ids", ()), *marker.get("assumption_ids", ()))
    common = {
        "event_id": event.event_id,
        "source_ids": sorted({item for item in named if item}),
        "minute": world.minute_index,
    }
    history_target = target
    if op in {"move_facility", "transfer_equipment"}:
        if op == "transfer_equipment":
            world.facilities[target].zone_id = parameters["to_zone_id"]
        point = {key: float(value) for key, value in parameters["point_m"].items()}
        state["facility_points"][target] = {"point_m": point, **common}
    elif op == "set_partition":
        if expire:
            state["partitions"].pop(modifier, None)
        else:
            state["partitions"][modifier] = {
                "zone_id": target,
                "polygon_m": [
                    {key: float(value) for key, value in point.items()}
                    for point in parameters["polygon_m"]
                ],
                **common,
                "end_minute": payload["end_minute"],
            }
    elif op == "set_route_access":
        edge = world.zone_edges[target]
        record = state["route_modifiers"].setdefault(
            target, {"base_closed": edge.closed, "modifiers": {}}
        )
        if expire:
            record["modifiers"].pop(modifier, None)
        else:
            record["modifiers"][modifier] = {
                "accessible": parameters["accessible"],
                **common,
                "end_minute": payload["end_minute"],
            }
        edge.closed = record["base_closed"] or any(
            not item["accessible"] for item in record["modifiers"].values()
        )
        if not record["modifiers"]:
            del state["route_modifiers"][target]
    else:
        room = TemporaryRoomGeometry.model_validate(parameters)
        history_target = room.room_id
        if expire:
            state["temporary_rooms"][room.room_id]["active"] = False
            state["temporary_rooms"][room.room_id]["end_event_id"] = event.event_id
        else:
            state["temporary_rooms"][room.room_id] = {
                **room.model_dump(mode="json"),
                "start_minute": world.minute_index,
                "end_minute": payload["end_minute"],
                "active": True,
                "start_event_id": event.event_id,
                "end_event_id": None,
                "program_id": payload["program_id"],
                "effect_id": payload["effect_id"],
                "source_ids": common["source_ids"],
            }
    state["history"].append(
        {
            "transition_id": event.event_id,
            "op": op,
            "target_id": history_target,
            "status": "expired" if expire else "applied",
            "causation_id": event.causation_id,
            **common,
        }
    )
    del state["history"][:-20000]


# What the Python rule said of each of _change_cases, in order: None where it passed (lane L10).
_CHANGE_TEXTS = (
    None,
    None,
    "expiry_required: route-access changes require a bounded end",
    "target_missing: route does not exist",
    None,
    "unsupported_move_expiry: same-zone moves are permanent; approve an explicit later move",
    "area_invalid: cross-zone facility moves are unsupported",
    "target_missing: facility has no surveyed position",
    None,
    "expiry_required: partitions require an explicit bounded end",
    "target_missing: zone does not exist",
    None,
    "expiry_required: temporary rooms need an in-run expiry",
    "target_mismatch: temporary room zone must match its selected zone",
    None,
    "unsupported_geometry_operator",
)


def test_each_change_is_checked_and_applied_as_the_python_rule_did(world: WorldState) -> None:
    validate = geometry_overlay.validate_geometry_change
    apply = geometry_overlay.apply_geometry_change
    cases = change_cases(world)
    for (op, target, parameters, end), text in zip(cases, _CHANGE_TEXTS, strict=True):
        event = _event(world, op, target, parameters, end)
        if text is not None:
            # The check comes first, so applying says the same.
            _raises_text(world, lambda ours: validate(ours, op, target, parameters, end), text)
            _raises_text(world, lambda ours: apply(ours, event), text)
            continue
        assert validate(world, op, target, parameters, end) is None
        _writes(world, lambda ours: apply(ours, event), lambda expected: _applied(expected, event))


def _square(x: float, y: float, size: float) -> list[dict[str, float]]:
    return [
        {"x": x, "y": y},
        {"x": x + size, "y": y},
        {"x": x + size, "y": y + size},
        {"x": x, "y": y + size},
    ]


def _sweep_cases(world: WorldState) -> list[tuple[str, str, Any, int | None]]:
    """Many changes, most of them wrong, over the zones of the plan: each check of the rule."""
    geometry = world.model_configs["facility_geometry"]
    zones = {zone["zone_id"]: zone["polygon"] for zone in geometry["zones"]}
    points = geometry["facility_points"]
    cases: list[tuple[str, str, Any, int | None]] = []
    for zone_id, polygon in zones.items():
        left, top = min(p["x"] for p in polygon), min(p["y"] for p in polygon)
        right, bottom = max(p["x"] for p in polygon), max(p["y"] for p in polygon)
        for fx in (0.05, 0.3, 0.5, 0.97):
            for fy in (0.05, 0.5, 0.95):
                x, y = left + (right - left) * fx, top + (bottom - top) * fy
                for size in (0.4, 1.5, 30.0):
                    cases.append(("set_partition", zone_id, {"polygon_m": _square(x, y, size)}, 50))
                room = room_record(world, "sweep-room")
                room["zone_id"] = zone_id
                room["polygon_m"] = _square(x, y, 1.2)
                room["bed_points_m"] = [{"x": x + 0.6, "y": y + 0.6}]
                room["route_edge_ids"] = [f"route_corridor_{zone_id}", f"route_{zone_id}_corridor"]
                plain = {key: room[key] for key in ROOM_FIELDS}
                cases.append(("temporary_room_geometry", zone_id, plain, 50))
                for facility_id in points:
                    move = {"point_m": {"x": x, "y": y}}
                    cases.append(("move_facility", facility_id, move, None))
                    source = world.facilities[facility_id].zone_id
                    transfer = {
                        "schema_version": 1,
                        "equipment_id": facility_id,
                        "from_zone_id": source,
                        "to_zone_id": zone_id,
                        "point_m": {"x": x, "y": y},
                    }
                    cases.append(("transfer_equipment", facility_id, transfer, None))
    for facility_id, point in points.items():
        zone_id = world.facilities[facility_id].zone_id
        around = _square(point["x"] - 0.2, point["y"] - 0.2, 0.4)
        cases.append(("set_partition", zone_id, {"polygon_m": around}, 50))
        room = {key: room_record(world, "sweep-room")[key] for key in ROOM_FIELDS}
        room.update(zone_id=zone_id, polygon_m=_square(point["x"] - 0.6, point["y"] - 0.6, 1.2))
        room["bed_points_m"] = [{"x": point["x"] + 0.3, "y": point["y"] + 0.3}]
        room["route_edge_ids"] = [f"route_corridor_{zone_id}", f"route_{zone_id}_corridor"]
        cases.append(("temporary_room_geometry", zone_id, room, 50))
        elsewhere = "sleeping" if zone_id != "sleeping" else "food"
        transfer = {
            "schema_version": 1,
            "equipment_id": facility_id,
            "from_zone_id": zone_id,
            "to_zone_id": elsewhere,
            "point_m": point,
        }
        cases.append(("transfer_equipment", facility_id, transfer, None))
    room = {key: room_record(world, "sweep-room")[key] for key in ROOM_FIELDS}
    cases.append(("temporary_room_geometry", ROOM_ZONE, {**room, "room_id": "room"}, 50))
    for routes in ([], ["route_corridor_sleeping"], ["route_food_corridor"], ["no_route"]):
        cases.append(("temporary_room_geometry", ROOM_ZONE, {**room, "route_edge_ids": routes}, 50))
    cases.append(("temporary_room_geometry", ROOM_ZONE, {**room, "zone_id": "food"}, 50))
    cases.append(("temporary_room_geometry", ROOM_ZONE, room, world.duration_minutes))
    cases.append(("temporary_room_geometry", "no_zone", {**room, "zone_id": "no_zone"}, 50))
    flat = [{"x": 1.0, "y": 3.0}, {"x": 2.0, "y": 3.0}, {"x": 3.0, "y": 3.0}]
    cases.append(("temporary_room_geometry", ROOM_ZONE, {**room, "polygon_m": flat}, 50))
    cases.append(("set_partition", ROOM_ZONE, {"polygon_m": flat}, 50))
    for facility_id, point in points.items():
        transfer = {
            "schema_version": 1,
            "equipment_id": facility_id,
            "from_zone_id": world.facilities[facility_id].zone_id,
            "to_zone_id": "sleeping",
            "point_m": point,
        }
        cases.append(("transfer_equipment", facility_id, {**transfer, "equipment_id": "x"}, None))
        nowhere = {**transfer, "to_zone_id": "nowhere"}
        cases.append(("transfer_equipment", facility_id, nowhere, None))
        cases.append(("transfer_equipment", facility_id, transfer, 50))
        cases.append(("transfer_equipment", "no_facility", transfer, None))
        cases.append(("transfer_equipment", facility_id, {"schema_version": 2}, None))
    for edge_id in ("route_corridor_sleeping", "route_sleeping_corridor", "route_corridor_food"):
        for accessible in (True, False):
            cases.append(("set_route_access", edge_id, {"accessible": accessible}, 50))
    cases.append(("set_route_access", "route_corridor_food", {"accessible": "yes"}, 50))
    cases.append(("no_such_operator", ROOM_ZONE, {}, 50))
    return cases


def _sweep_worlds(world: WorldState) -> list[WorldState]:
    """The world, and copies with what the checks look at: a room, people, tasks, a closure."""
    worlds = [world]
    roomy = _copy(world)
    _room_world(roomy)
    geometry_overlay.reserve_sleep_bed(roomy, _person_in_room_zone(roomy))
    worlds.append(roomy)
    busy = _copy(world)
    for facility in busy.facilities.values():
        facility.queue_agent_ids.append(next(iter(busy.agents)))
        break
    tasks = busy.model_state.get("staff_operations_v1", {}).get("staff")
    if isinstance(tasks, dict) and tasks:
        first = next(iter(tasks))
        if isinstance(tasks[first], dict):
            tasks[first]["target_id"] = list(busy.facilities)[1]
    worlds.append(busy)
    closed = _copy(world)
    closed.zone_edges["route_corridor_food"].closed = True
    closed.zone_edges["route_corridor_toilet"].closed = True
    worlds.append(closed)
    unstaffed = _copy(world)
    for staff in unstaffed.staff.values():
        staff.zone_id = "staff"
    worlds.append(unstaffed)
    without = _copy(world)
    del without.model_state[geometry_overlay.GEOMETRY_OVERLAY_KEY]
    worlds.append(without)
    unplanned = _copy(world)
    unplanned.model_configs["facility_geometry"] = None
    worlds.append(unplanned)
    points = world.model_configs["facility_geometry"]["facility_points"]
    for obstacles in ([], [{"id": "crate", "polygon": _square(2.0, 4.0, 1.0)}]):
        footprinted = _copy(world)
        raw = deepcopy(footprinted.model_configs["spatial_constraints"])
        raw["equipment_footprints"] = [
            {
                "facility_id": facility_id,
                "polygon": _square(
                    points[facility_id]["x"] - 0.3, points[facility_id]["y"] - 0.3, 0.6
                ),
                "area_m2": 0.36,
            }
            for facility_id in ("toilet_01", "meal_distribution_main", "water_station_main")
        ]
        raw["obstacles"] = obstacles
        footprinted.model_configs["spatial_constraints"] = raw
        worlds.append(footprinted)
    reserved = _copy(roomy)
    rooms = reserved.model_state[geometry_overlay.GEOMETRY_OVERLAY_KEY]["temporary_rooms"]
    rooms["room"]["active"] = False
    worlds.append(reserved)
    return worlds


# What the Python rule said over the sweep, by the first line of its text (lane L10). "(refused)"
# is a transfer of a facility with a footprint where the plan has obstacles: the Python rule raised
# AttributeError there (`item.facility_id` of an obstacle), and the kernel refuses it.
_SWEEP_TEXTS = {
    "(passed)": 2318,
    "(refused)": 52,
    "1 validation error for EquipmentTransferParameters": 672,
    "1 validation error for RouteAccessParameters": 8,
    "1 validation error for TemporaryRoomGeometry": 112,
    "5 validation errors for EquipmentTransferParameters": 56,
    "area_invalid: cross-zone facility moves are unsupported": 4536,
    "area_invalid: partition covers a facility": 56,
    "area_invalid: partition requires a simple positive-area polygon inside its zone": 1576,
    "area_invalid: temporary room covers a facility": 52,
    "area_invalid: temporary room overlaps another active room": 2,
    "area_invalid: temporary room polygon must lie inside its zone": 432,
    "area_invalid: transfer point must lie inside the destination zone": 54,
    "clear_width_insufficient: moved service point requires half a metre from walls": 324,
    "clear_width_insufficient: partition obstructs a declared passage": 435,
    "clear_width_insufficient: transferred equipment must remain half a metre from walls": 1720,
    "expiry_required: temporary rooms need an in-run expiry": 8,
    "facility_in_use: an active staff task targets this equipment": 85,
    "facility_in_use: an active staff task targets this facility": 96,
    "facility_in_use: equipment queues and services must be empty": 85,
    "facility_in_use: facility move would enter an active temporary room": 7,
    "facility_in_use: queues and services must be empty": 96,
    "geometry_overlay_required: declare the versioned geometry overlay before approving effects": (
        1802
    ),
    "geometry_unavailable": 1802,
    "precondition_failed: equipment source/destination zone does not match": 56,
    "room_exists: temporary room ID already exists": 2,
    "room_in_use: temporary room would cover a current person": 8,
    "room_in_use: temporary room would cover a current staff member": 30,
    "route_in_use: closure would disconnect an active temporary room": 2,
    "route_in_use: partition covers a current person": 16,
    "route_in_use: partition covers a staff standing point": 45,
    "route_unavailable: equipment zones have no open directed route": 120,
    "route_unavailable: temporary room access route is closed or inaccessible": 12,
    "route_unavailable: temporary room access route must connect to its zone": 16,
    "route_unavailable: temporary room requires directed ingress and egress routes": 8,
    "target_mismatch: equipment_id must match the selected facility": 56,
    "target_mismatch: temporary room zone must match its selected zone": 8,
    "target_missing: equipment must identify a positioned facility": 56,
    "target_missing: temporary room zone has no surveyed polygon": 8,
    "unsupported_geometry_operator": 8,
    "unsupported_reopening: this operation cannot override a prior graph closure": 1,
    "unsupported_transfer_expiry: equipment transfers are permanent": 56,
}
_SWEEP_STAFF = {
    ("meal_service", "corridor"): 96,
    ("meal_service", "intake"): 96,
    ("meal_service", "medical"): 96,
    ("meal_service", "quiet"): 96,
    ("meal_service", "sleeping"): 95,
    ("meal_service", "toilet"): 84,
    ("water_service", "corridor"): 96,
    ("water_service", "intake"): 96,
    ("water_service", "medical"): 96,
    ("water_service", "quiet"): 96,
    ("water_service", "sleeping"): 95,
    ("water_service", "toilet"): 84,
}
# sha256 of the JSON list of each outcome in turn: None, ["KernelRefusal"], or the class name and
# the first line of the text.
_SWEEP_DIGEST = "cb84542131179619151bc20a61f2556923bd9af9d1a969d906a4c4d3af01ac32"


def test_every_refusal_of_a_change_is_the_python_rule_s_error(world: WorldState) -> None:
    """Each check raises the Python rule's ValueError, class and text, in its order; where the rule
    passed, the kernel does too (docs/rust-port/lanes/l8-texts-rest.md)."""
    import hashlib
    from collections import Counter

    validate = geometry_overlay.validate_geometry_change
    texts: Counter[str] = Counter()
    outcomes: list[Any] = []
    cases = _sweep_cases(world)
    for variant in _sweep_worlds(world):
        for op, target, parameters, end in cases:
            try:
                validate(variant, op, target, deepcopy(parameters), end)
            except KernelRefusal as refusal:
                assert "raises on an obstacle" in str(refusal), (op, target, end)
                texts["(refused)"] += 1
                outcomes.append(["KernelRefusal"])
                continue
            except ValueError as error:
                said = str(error).split("\n")[0]
                texts[said] += 1
                outcomes.append([type(error).__name__, said])
                continue
            texts["(passed)"] += 1
            outcomes.append(None)
    staff = {
        (skill, zone): texts.pop(
            f"staff_unavailable: no staff with {skill} works in {zone!r}, so the moved station "
            "would not be served"
        )
        for skill, zone in _SWEEP_STAFF
    }
    assert staff == _SWEEP_STAFF
    assert dict(texts) == _SWEEP_TEXTS
    assert hashlib.sha256(json.dumps(outcomes).encode()).hexdigest() == _SWEEP_DIGEST


@pytest.mark.parametrize(
    "corrupt",
    [
        lambda state: state.__setitem__("schema_version", 2),
        lambda state: state.__setitem__("extra", 1),
        lambda state: state["facility_points"].__setitem__("nobody", {"point_m": {}}),
        lambda state: state["temporary_rooms"]["room"].__setitem__("active", 1),
        lambda state: state["temporary_rooms"]["room"].__setitem__("zone_id", "nowhere"),
        lambda state: state["temporary_rooms"]["room"].__setitem__("people_capacity", 0),
        lambda state: state["temporary_rooms"]["room"].__setitem__("start_minute", 99),
        lambda state: state["temporary_rooms"]["room"].__setitem__("end_event_id", "x"),
        lambda state: state["temporary_rooms"]["room"].__setitem__("route_edge_ids", ["no"]),
        lambda state: state["bed_reservations"].__setitem__("nobody", {}),
        lambda state: state["room_admissions"].__setitem__("nobody", {}),
        lambda state: state["history"].append({"transition_id": "x"}),
        lambda state: state["partitions"].__setitem__("p", {"zone_id": "nowhere"}),
        lambda state: state["route_modifiers"].__setitem__("no", {}),
    ],
)
def test_the_check_of_the_overlay_takes_nothing_the_python_rule_refuses(
    world: WorldState, corrupt: Any
) -> None:
    _room_world(world)
    state = world.model_state[geometry_overlay.GEOMETRY_OVERLAY_KEY]
    assert _rules().validate_overlay(world) is True
    geometry_overlay.validate_geometry_overlay_state(world)
    corrupt(state)
    with pytest.raises(ValueError):
        geometry_overlay.validate_geometry_overlay_state(world)
    with pytest.raises(native.Unsupported):
        _rules().validate_overlay(world)


def test_a_bed_is_checked_against_its_target(world: WorldState) -> None:
    _room_world(world, people=4, beds=2)
    agent_id = _person_in_room_zone(world)
    geometry_overlay.reserve_sleep_bed(world, agent_id)
    assert _rules().validate_overlay(world) is True
    state = world.model_state[geometry_overlay.GEOMETRY_OVERLAY_KEY]
    state["bed_reservations"][agent_id]["target_id"] = "bed-000000000000000000000000"
    with pytest.raises(ValueError):
        geometry_overlay.validate_geometry_overlay_state(world)
    with pytest.raises(native.Unsupported):
        _rules().validate_overlay(world)


# ---- The temporary-room branch of a decision -----------------------------------------------------


def _controller(world: WorldState) -> Any:
    from sheltersim.experiments.runner import OfflineSimulationController

    class _Sink:
        def __getattr__(self, name: str) -> Any:
            return lambda *arguments, **options: None

    return OfflineSimulationController(world, _Sink())


def _sleep_proposal(world: WorldState, agent_id: str) -> Any:
    from types import SimpleNamespace

    from sheltersim.policies.behavior import ActionKind

    return SimpleNamespace(
        agent_id=agent_id,
        action=ActionKind.SLEEP,
        target_id=None,
        next_decision_at=world.minute_index + 5,
    )


@pytest.mark.parametrize(
    ("at_bed", "digest"), [(False, "a4f07608e1e04866"), (True, "2732f2f9b97629f9")]
)
def test_sleep_in_a_room_is_the_python_step_s(world: WorldState, at_bed: bool, digest: str) -> None:
    """The result and the world (`world_digest`) of the Python step, recorded from it on 45a9bc9
    before it went (lane L10), pinned on every OS."""
    room = room_record(world, "room")
    world.model_state[geometry_overlay.GEOMETRY_OVERLAY_KEY]["temporary_rooms"]["room"] = room
    agent_id = _person_in_room_zone(world)
    if at_bed:
        world.model_state["movement_v1"]["positions"][agent_id]["point"] = dict(
            room["bed_points_m"][0]
        )
    ours = _copy(world)
    kernel = native.rules("proposals").apply_effect
    result = kernel(_controller(ours), _sleep_proposal(ours, agent_id))
    assert tuple(result) == ("bed-ec7532def0b0c01715ae0abb", 5, True)
    assert ours.model_state[geometry_overlay.GEOMETRY_OVERLAY_KEY]["bed_reservations"]
    assert world_digest(ours) == digest


def test_sleep_past_the_run_s_end_releases_the_bed(world: WorldState) -> None:
    _room_world(world)
    agent_id = _person_in_room_zone(world)
    # The walk would end after the Run: no trip, and what the person held is gone.
    geometry_overlay.reserve_sleep_bed(world, agent_id)
    world.minute_index = world.duration_minutes - 1
    ours = _copy(world)
    result = native.rules("proposals").apply_effect(
        _controller(ours), _sleep_proposal(ours, agent_id)
    )
    assert tuple(result) == (None, 364, True)
    assert not ours.model_state[geometry_overlay.GEOMETRY_OVERLAY_KEY]["bed_reservations"]
    assert world_digest(ours) == "bf284ac247d7f46c"


# ---- Independent queues --------------------------------------------------------------------------


def _queue_ids(world: WorldState) -> list[str]:
    return sorted(world.model_state[facility_queues.FACILITY_QUEUES_STATE_KEY]["queues"])


def _line_up(world: WorldState, facility_id: str, queue_id: str, agents: list[str]) -> list[str]:
    zone = world.facilities[facility_id].zone_id
    service = ToiletServiceManager(world, min_service_minutes=2, max_service_minutes=2)
    tasks = []
    for agent_id in agents:
        world.agents[agent_id].zone_id = zone
        position = world.model_state["movement_v1"]["positions"].get(agent_id)
        if position is not None:
            position["zone_id"] = zone
        receipt = service.request(agent_id, facility_id, queue_id=queue_id, validate_state=False)
        tasks.append(receipt.queue_task_id)
    return tasks


def test_the_row_of_the_queues_is_its_tree(queued: WorldState) -> None:
    mirror = queued._core.mirror(facility_queues.FACILITY_QUEUES_STATE_KEY)
    assert mirror.has_row
    _line_up(queued, "toilet_01", "toilet_01-left-fifo", ["A001", "A002"])
    assert mirror.tainted is None
    assert mirror.verify()
    assert mirror.row_json() == native.canonical_json(
        queued.model_state[facility_queues.FACILITY_QUEUES_STATE_KEY]
    )
    queued.model_state[facility_queues.FACILITY_QUEUES_STATE_KEY]["queues"]["toilet_01-left-fifo"][
        "last_stall"
    ] = 7
    assert mirror.tainted is not None
    with pytest.raises(native.Unsupported):
        _rules("facility_queues").queue_agent_ids(queued, "toilet_01-left-fifo")
    # No Python rule is left behind the kernel.
    with pytest.raises(KernelRefusal, match="facility_queues.queue_agent_ids refused"):
        facility_queues.queue_agent_ids(queued, "toilet_01-left-fifo")


def _request(request_id: str, agent_id: str, queue: str = "toilet_01-left-fifo") -> dict[str, Any]:
    return {
        "request_id": request_id,
        "queue_id": queue,
        "service_point_id": queue.removesuffix("-fifo"),
        "facility_id": "toilet_01",
        "agent_id": agent_id,
        "service_kind": "toilet",
        "requested_minute": 0,
        "status": "queued",
        "queue_task_id": request_id,
        "active_task_id": request_id,
    }


def test_reads_of_the_queues(queued: WorldState) -> None:
    tasks = _line_up(queued, "toilet_01", "toilet_01-left-fifo", ["A001", "A002"])
    _line_up(queued, "toilet_01", "toilet_01-right-fifo", ["A003"])
    state = queued.model_state[facility_queues.FACILITY_QUEUES_STATE_KEY]
    queue = "toilet_01-left-fifo"
    assert facility_queues.queue_agent_ids(queued, queue) == ["A001", "A002"]
    assert facility_queues.queue_agent_ids(queued, "toilet_01-right-fifo") == ["A003"]
    assert facility_queues.queue_agent_ids(queued, "none") == []
    # Every facility of this Scenario has a signed point, and so a queue.
    for facility in queued.facilities:
        assert facility_queues.facility_has_independent_queues(queued, facility) is True
    assert facility_queues.facility_has_independent_queues(queued, "none") is False
    assert facility_queues.queue_ids_for_facility(queued, "toilet_01") == (
        "toilet_01-left-fifo",
        "toilet_01-right-fifo",
    )
    assert facility_queues.queue_ids_for_facility(queued, "medical_desk") == ("medical_desk",)
    assert facility_queues.queue_ids_for_facility(queued, "none") == ()
    rules = _rules("facility_queues")
    # The Python reader (validate_* keeps it, D3) and its kernel agree.
    assert rules.queue_head_agent_ids(queued, "toilet_01") == facility_queues.queue_head_agent_ids(
        queued, "toilet_01"
    )
    request = facility_queues.request_for_task(queued, tasks[0])
    assert request == _request(tasks[0], "A001")
    # The state's own record, which a caller writes to.
    assert request is state["requests"][tasks[0]]
    assert facility_queues.request_for_task(queued, "none") is None
    assert facility_queues.request_for_agent(queued, "A002") is state["requests"][tasks[1]]
    assert (
        facility_queues.request_for_agent(queued, "A002", queue_id=queue)
        is (state["requests"][tasks[1]])
    )
    assert (
        facility_queues.request_for_agent(queued, "A002", queue_id="toilet_01-right-fifo") is None
    )
    assert facility_queues.request_for_agent(queued, "A004") is None
    created = facility_queues.create_facility_queue_state(queued)
    assert created == {
        "schema_version": 1,
        "policy_version": facility_queues.FACILITY_QUEUES_POLICY_VERSION,
        "queues": {
            point.queue_id: {
                "facility_id": point.facility_id,
                "service_point_id": point.id,
                "queued_request_ids": [],
                "active_request_ids": [],
                "last_stall": None,
            }
            for point in facility_queues.service_points(queued)
        },
        "requests": {},
    }
    assert list(created["queues"]) == sorted(created["queues"])
    assert created["queues"]["toilet_01-left-fifo"]["service_point_id"] == "toilet_01-left"
    assert facility_queues.independent_queues_required(queued.model_configs) is True
    assert facility_queues.is_legacy_queue_checkpoint(queued) is False


def test_a_legacy_checkpoint_is_one_with_the_marker_and_without_the_state(
    queued: WorldState, world: WorldState
) -> None:
    marker = dict(facility_queues.FACILITY_QUEUES_LEGACY_MODE)
    queued.model_state.pop(facility_queues.FACILITY_QUEUES_STATE_KEY)
    assert facility_queues.is_legacy_queue_checkpoint(queued) is False
    queued.model_state[facility_queues.FACILITY_QUEUES_LEGACY_MODE_KEY] = marker
    assert facility_queues.is_legacy_queue_checkpoint(queued) is True
    queued.model_state[facility_queues.FACILITY_QUEUES_LEGACY_MODE_KEY] = {**marker, "mode": "x"}
    assert facility_queues.is_legacy_queue_checkpoint(queued) is False
    # One point per facility, each its facility's queue: no independent queues to migrate.
    assert facility_queues.independent_queues_required(world.model_configs) is False
    world.model_state[facility_queues.FACILITY_QUEUES_LEGACY_MODE_KEY] = marker
    assert facility_queues.is_legacy_queue_checkpoint(world) is False
    assert facility_queues.independent_queues_required({}) is False
    assert facility_queues.independent_queues_required({"facility_geometry": []}) is False


def test_what_the_queue_rules_turn_down_is_the_python_rule_s_error(world: WorldState) -> None:
    with pytest.raises(ValueError) as said:
        facility_queues.create_facility_queue_state(world)
    assert (type(said.value), str(said.value)) == (
        ValueError,
        "facility_queues_v1 requires independent explicit service points",
    )
    # The parse's own error, as the Python rule let it through.
    with pytest.raises(ValueError) as said:
        geometry_overlay.parsed_facility_geometry({"schema_version": 99})
    with pytest.raises(ValueError) as ours:
        facility_queues.independent_queues_required({"facility_geometry": {"schema_version": 99}})
    assert type(ours.value) is type(said.value)
    assert not isinstance(ours.value, KernelRefusal)
    assert str(ours.value) == str(said.value)


def test_the_shortest_accessible_queue(queued: WorldState) -> None:
    choose = facility_queues.choose_queue_id
    # A tie goes to the smaller queue ID.
    assert choose(queued, "toilet_01", "A005") == "toilet_01-left-fifo"
    _line_up(queued, "toilet_01", "toilet_01-left-fifo", ["A001"])
    assert choose(queued, "toilet_01", "A005") == "toilet_01-right-fifo"
    _line_up(queued, "toilet_01", "toilet_01-right-fifo", ["A002", "A003"])
    assert choose(queued, "toilet_01", "A005") == "toilet_01-left-fifo"
    # Without the access its point asks for, a person doesn't count that queue.
    geometry = queued.model_configs["facility_geometry"]
    for point in geometry["service_points"]:
        if point["queue_id"] == "toilet_01-left-fifo":
            point["access"] = {
                "accessibility": "restricted",
                "required_access_tags": ["key_holder"],
                "required_staff_skills": [],
            }
    assert choose(queued, "toilet_01", "A005") == "toilet_01-right-fifo"
    # A person's needs and mobility are the tags a point may ask for.
    assert queued.agents["A005"].profile.mobility.value == "assisted"
    for tags, chosen in (
        (["mobility_support", "assisted"], "toilet_01-left-fifo"),
        (["assisted", "key_holder"], "toilet_01-right-fifo"),
    ):
        for point in geometry["service_points"]:
            if point["queue_id"] == "toilet_01-left-fifo":
                point["access"]["required_access_tags"] = tags
        assert choose(queued, "toilet_01", "A005") == chosen
    assert choose(queued, "water_station_main", "A005") == "water_station_main-left-fifo"
    assert choose(queued, "none", "A005") is None
    queued.model_state.pop(facility_queues.FACILITY_QUEUES_STATE_KEY)
    assert choose(queued, "toilet_01", "A005") is None


def test_writes_of_the_queues(queued: WorldState) -> None:
    key = facility_queues.FACILITY_QUEUES_STATE_KEY
    queue = "toilet_01-left-fifo"
    tasks = _line_up(queued, "toilet_01", queue, ["A001", "A002"])
    queued.minute_index = 7
    options = {
        "queue_id": queue,
        "request_id": "fresh",
        "task_id": "fresh-task",
        "agent_id": "A003",
        "service_kind": "toilet",
        "requested_minute": 0,
    }
    ours = _copy(queued)
    assert facility_queues.enqueue(ours, **options) is None
    state = ours.model_state[key]
    assert state["requests"]["fresh"] == {
        **_request("fresh", "A003"),
        "queue_task_id": "fresh-task",
        "active_task_id": "fresh-task",
    }
    assert state["queues"][queue]["queued_request_ids"] == [*tasks, "fresh"]
    not_installed = _copy(queued)
    not_installed.model_state.pop(key)
    for world, changed in (
        (queued, {"request_id": tasks[0]}),
        (queued, {"queue_id": "none"}),
        (not_installed, {}),
    ):
        before = _bytes(world)
        with pytest.raises(ValueError) as said:
            facility_queues.enqueue(world, **{**options, **changed})
        assert (type(said.value), str(said.value)) == (
            ValueError,
            "service queue is not installed or request ID is already in use",
        )
        _same_bytes(_bytes(world), before)

    ours = _copy(queued)
    facility_queues.mark_stall(ours, queue, tasks[0], "staff")
    assert ours.model_state[key]["queues"][queue]["last_stall"] == {
        "minute": 7,
        "request_id": tasks[0],
        "reason": "staff",
    }
    for world in (queued, not_installed):
        before = _bytes(world)
        assert facility_queues.mark_stall(world, "none", tasks[0], "staff") is None
        _same_bytes(_bytes(world), before)

    # Starting the head clears the stall; the record is the state's own.
    facility_queues.start_service(ours, tasks[0], "active")
    state = ours.model_state[key]
    assert state["requests"][tasks[0]] == {
        **_request(tasks[0], "A001"),
        "status": "active",
        "active_task_id": "active",
    }
    assert state["queues"][queue]["queued_request_ids"] == [tasks[1]]
    assert state["queues"][queue]["active_request_ids"] == [tasks[0]]
    assert state["queues"][queue]["last_stall"] is None
    assert facility_queues.request_for_task(ours, "active") is state["requests"][tasks[0]]
    started = deepcopy(ours)

    for status in ("completed", "abandoned"):
        world = deepcopy(started)
        world.model_state[key]["queues"][queue]["last_stall"] = {
            "minute": 7,
            "request_id": tasks[1],
            "reason": "staff",
        }
        result = facility_queues.finish_service(world, tasks[1], status=status)
        assert result == {**_request(tasks[1], "A002"), "status": status}
        state = world.model_state[key]
        assert tasks[1] not in state["requests"]
        assert state["queues"][queue]["queued_request_ids"] == []
        assert state["queues"][queue]["active_request_ids"] == [tasks[0]]
        assert state["queues"][queue]["last_stall"] is None
    # An active request ends by its active task's ID.
    world = deepcopy(started)
    assert facility_queues.finish_service(world, "active", status="completed") == {
        **_request(tasks[0], "A001"),
        "status": "completed",
        "active_task_id": "active",
    }
    assert world.model_state[key]["queues"][queue]["active_request_ids"] == []
    for world in (started, not_installed):
        before = _bytes(world)
        assert facility_queues.finish_service(world, "none", status="x") is None
        _same_bytes(_bytes(world), before)


@pytest.mark.parametrize(
    ("transition", "wait", "cause", "position"),
    [("queued", None, None, 2), ("service_started", 0, "cause", 2), ("completed", 3, "c", None)],
)
def test_a_queue_transition_is_an_applied_record_of_the_log(
    queued: WorldState, transition: str, wait: int | None, cause: str | None, position: int | None
) -> None:
    key = facility_queues.FACILITY_QUEUES_STATE_KEY
    queue = "toilet_01-left-fifo"
    tasks = _line_up(queued, "toilet_01", queue, ["A001", "A002"])
    queued.minute_index = 4
    request = dict(queued.model_state[key]["requests"][tasks[1]])
    if position is None:
        request["request_id"] = "gone"
    sequence = queued.id_counters["event"]
    logged = len(queued.event_log)
    facility_queues.record_queue_transition(
        queued,
        request,
        transition=transition,
        task_id=tasks[1],
        wait_minutes=wait,
        causation_id=cause,
    )
    assert queued.id_counters["event"] == sequence + 1
    assert len(queued.event_log) == logged + 1
    record = queued.event_log.records[-1]
    event_id = f"facility-queue-transition-{sequence}"
    assert (record.status.value, record.realized_minute, record.detail) == ("applied", 4, None)
    event = record.event
    assert (event.event_id, event.kind, event.due_minute, event.priority, event.sequence) == (
        event_id,
        EventKind.ENDOGENOUS,
        4,
        0,
        sequence,
    )
    assert event.event_type == "facility_queue_transition"
    assert event.source_id == facility_queues.FACILITY_QUEUES_POLICY_VERSION
    assert event.causation_id == cause
    assert event.provenance == {
        "source_type": "assumption",
        "source_id": "ASM-FACILITY-QUEUES-V1",
        "policy_version": facility_queues.FACILITY_QUEUES_POLICY_VERSION,
    }
    assert event.payload == {
        "schema_version": 1,
        "record_type": "operations",
        "event_type": "facility_queue_transition",
        "event_id": event_id,
        "request_id": request["request_id"],
        "queue_id": queue,
        "service_point_id": "toilet_01-left",
        "facility_id": "toilet_01",
        "agent_id": "A002",
        "service_kind": "toilet",
        "transition": transition,
        "minute": 4,
        "requested_minute": 0,
        "task_id": tasks[1],
        "queue_position": position,
        "service_capacity_concurrent": 1,
        "wait_minutes": wait,
        "causation_id": cause,
    }


def test_the_check_of_the_queues_takes_nothing_the_python_rule_refuses(queued: WorldState) -> None:
    rules = _rules("facility_queues")
    _line_up(queued, "toilet_01", "toilet_01-left-fifo", ["A001"])
    assert rules.validate(queued) is True
    facility_queues.validate_facility_queue_state(queued)
    state = queued.model_state[facility_queues.FACILITY_QUEUES_STATE_KEY]
    request_id = next(iter(state["requests"]))
    row = state["queues"]["toilet_01-left-fifo"]

    def refuses() -> bool:
        with pytest.raises(ValueError):
            facility_queues.validate_facility_queue_state(queued)
        try:
            return rules.validate(queued) is False
        except native.Unsupported:
            return True

    state["requests"][request_id]["requested_minute"] = 10_000
    assert refuses()
    state["requests"][request_id]["requested_minute"] = 0
    state["requests"][request_id]["service_kind"] = "water"
    assert refuses()
    state["requests"][request_id]["service_kind"] = "toilet"
    state["requests"][request_id]["status"] = "active"
    assert refuses()
    state["requests"][request_id]["status"] = "queued"
    row["active_request_ids"].append(request_id)
    assert refuses()
    row["active_request_ids"].clear()
    row["last_stall"] = {"minute": -1, "request_id": request_id, "reason": "staff"}
    assert refuses()
    row["last_stall"] = None
    assert rules.validate(queued) is True
    state["requests"]["unlisted"] = dict(state["requests"][request_id], request_id="unlisted")
    assert refuses()


def test_a_fifo_keeps_its_order_and_only_a_wait_ends_it(queued: WorldState) -> None:
    prepare = queueing.prepare_independent_facility_queue
    queue = "toilet_01-left-fifo"
    tasks = _line_up(queued, "toilet_01", queue, ["A001", "A002"])
    for queue_id, minute in ((queue, queued.minute_index), ("none", queued.minute_index)):
        before = _bytes(queued)
        prepare(queued, queue_id, minute=minute, resource=False)
        _same_bytes(_bytes(queued), before)
    queued.minute_index = 119
    before = _bytes(queued)
    prepare(queued, queue, minute=119, resource=False)
    _same_bytes(_bytes(queued), before)
    # Past the toilet's default wait, with a low need: this person leaves the queue.
    queued.agents["A002"].needs.toilet_urgency = 0.1
    queued.active_tasks[tasks[1]].start_minute = 0
    queued.minute_index = 121
    center = movement._zone_center(movement._geometry(queued), queued.agents["A002"].zone_id)
    queued.model_state["movement_v1"]["positions"]["A002"]["point"] = {"x": center.x, "y": center.y}
    sequence = queued.id_counters["event"]
    logged = len(queued.event_log)
    prepare(queued, queue, minute=121, resource=False)
    state = queued.model_state["facility_queues_v1"]
    assert state["queues"][queue]["queued_request_ids"] == [tasks[0]]
    assert tasks[1] not in state["requests"]
    assert queued.facilities["toilet_01"].queue_agent_ids == ["A001"]
    assert tasks[1] not in queued.active_tasks
    agent = queued.agents["A002"]
    # At the zone's anchor already: no walk.
    assert (agent.activity, agent.target_id, agent.next_decision_at) == (Activity.IDLE, None, 136)
    assert queued.metric_state["facility_queue_abandon_count"] == 1.0
    assert queued.id_counters["event"] == sequence + 2
    # The request's end is logged first, under the next sequence; then the abandonment.
    transition, abandon = queued.event_log.records[logged:]
    assert transition.event.event_id == f"facility-queue-transition-{sequence + 1}"
    assert {
        name: transition.event.payload[name]
        for name in ("transition", "agent_id", "request_id", "queue_position", "wait_minutes")
    } == {
        "transition": "abandoned",
        "agent_id": "A002",
        "request_id": tasks[1],
        "queue_position": None,
        "wait_minutes": 121,
    }
    assert transition.event.causation_id == tasks[1]
    assert abandon.event.event_id == f"facility-queue-abandon-{sequence}"
    assert abandon.event.payload == {
        "facility_id": "toilet_01",
        "agent_id": "A002",
        "task_id": tasks[1],
        "service": "toilet",
        "start_minute": 0,
        "wait_minutes": 121,
        "retry_at_minute": 136,
        "reason": "need_weighted_wait_threshold",
    }
    assert abandon.detail == (
        "Agent left a prolonged low-urgency queue and will reconsider after a bounded delay"
    )


# The deleted Python rules' walks to service, slots and approaches of each FIFO, recorded from
# them: the results, and the whole world after a walk to service, by sha256.
FIFO_WALKS = {
    "toilet_01-left-fifo": "e9cd3a9fc6b9775b9ca03f45bbe8fb3c32218a1bc6bde5f704f1630c39cce095",
    "toilet_01-right-fifo": "e1eae6eeec33fb20dd747df6cf6913b369f7da1fe5a2826157698925488573c6",
}


@pytest.mark.parametrize("queue_id", ["toilet_01-left-fifo", "toilet_01-right-fifo"])
def test_walks_of_a_fifo_are_the_python_rule_s(queued: WorldState, queue_id: str) -> None:
    import hashlib

    from sheltersim.domain import movement

    queues = _rules("queues")
    _line_up(queued, "toilet_01", queue_id, ["A001", "A002", "A003"])
    minute = queued.minute_index
    options = {"resource": False, "minute": minute, "queue_id": queue_id}
    first, after = _answer(queued, queues.progress_physical_queue_positions, "toilet_01", **options)
    _recorded(f"walk {queue_id}", first, after)
    journey_world = _copy(queued)
    queues.progress_physical_queue_positions(journey_world, "toilet_01", **options)
    journeys = journey_world.model_state["movement_v1"]["journeys"]
    assert all(journey.get("queue_id") == queue_id for journey in journeys.values())
    assert first in (True, False)
    answers = []
    for extra in ({"queue_id": queue_id}, {}):
        walked = _copy(queued)
        answers.append(
            repr(
                movement.progress_physical_queue_head_to_service(
                    walked, "toilet_01", resource=False, minute=minute, **extra
                )
            )
        )
        answers.append(hashlib.sha256(_bytes(walked).encode()).hexdigest())
    geometry = geometry_overlay.effective_geometry(queued)
    for agent_id in ("A001", "A002", "A003"):
        answers.append(
            repr(movement._queue_slot_position(queued, geometry, agent_id, "toilet_01", queue_id))
        )
    point = next(item for item in geometry.service_points if item.queue_id == queue_id)
    answers.append(repr(movement.facility_approach_route(queued, "A001", "toilet_01", point.id)))
    assert hashlib.sha256("\n".join(answers).encode()).hexdigest() == FIFO_WALKS[queue_id]
    assert movement.facility_approach_route(queued, "A001", "toilet_01", "none") is None


def _resource_queues(world: WorldState) -> tuple[str, str] | None:
    for queue_id, row in sorted(
        world.model_state[facility_queues.FACILITY_QUEUES_STATE_KEY]["queues"].items()
    ):
        if world.facilities[row["facility_id"]].kind in {"water_station", "meal_distribution"}:
            return row["facility_id"], queue_id
    return None


# The water and meal service has no Python rule left (lane L10, `kernel_only`). What its rules
# made of each case, recorded from them: the results and worlds, by sha256 (on every OS, as the
# gate's hashes).
# Recorded again in #152 P8b-1, where the ledger's audit records appended to its tree took the
# row's key order (the tree follows the row); nothing else changed. Recorded again in #152 P8 c3,
# where the service's rules write the row of `resource_service_v1` and its tree takes the row's
# key order: with sorted keys, each case is the same as before.
RESOURCE_FIFO = {
    "serves": "55c19613d7506346",
    "interrupted": "ac8f36a0687bd7e1",
    "cancelled-operator_cancelled": "4b945a95e866d703",
    "cancelled-facility_stopped": "4b945a95e866d703",
    "cancelled-staff_unavailable": "204b6edeaad9429f",
    "cancelled-place": "b4995ef04bb778fd",
    "completed": "989cad4a9fe17255",
}


def _is_the_python_rules(answers: list[str], case: str) -> None:
    import hashlib

    digest = hashlib.sha256("\n".join(answers).encode()).hexdigest()[:16]
    assert digest == RESOURCE_FIFO[case]


def _rows_follow(world: WorldState) -> None:
    for name in ("resource_service_v1", "resource_ledger_v1", "facility_queues_v1"):
        mirror = world.model_state._mirrors.get(name)
        if mirror is not None and name in world.model_state:
            assert mirror.tainted is None
            assert mirror.verify()


def test_a_resource_fifo_serves_as_the_python_rule_does(queued: WorldState) -> None:
    found = _resource_queues(queued)
    assert found is not None
    facility_id, queue_id = found
    kind = queued.facilities[facility_id].kind
    resource = "water_ml" if kind == "water_station" else "food_meal_units"
    zone = queued.facilities[facility_id].zone_id
    manager = ResourceServiceManager(queued)
    agent_id = "A001"
    queued.agents[agent_id].zone_id = zone
    queued.model_state["movement_v1"]["positions"][agent_id]["zone_id"] = zone
    kernel = _rules("service")
    answers: list[str] = []
    # The request.
    ours = _copy(queued)
    our_receipt = kernel.request(
        ResourceServiceManager(ours), agent_id, facility_id, validate_state=False
    )
    assert (our_receipt.agent_id, our_receipt.facility_id, our_receipt.queue_position) == (
        agent_id,
        facility_id,
        1,
    )
    answers += [repr(our_receipt), _bytes(ours)]
    _rows_follow(ours)
    manager.request(agent_id, facility_id, validate_state=False)
    # Each stop and the start of a service, on a world that walks no further.
    queued.model_state["movement_v1"]["positions"][agent_id]["point"] = queued.model_configs[
        "facility_geometry"
    ]["facility_points"][facility_id]
    minute = queued.minute_index

    def serve(world: WorldState, quantity: int | None = None) -> Any:
        ours = _copy(world)
        each = ResourceServiceManager(ours)
        if quantity is not None:
            key = (
                "water_ml_per_service" if resource == "water_ml" else "food_meal_units_per_service"
            )
            each.config[key] = quantity
        result = kernel.serve_independent_head(each, facility_id, resource, queue_id, minute)
        answers.extend([repr(tuple(result)), _bytes(ours)])
        _rows_follow(ours)
        return result

    started = serve(queued)
    assert started[0] is not None or started[1] is None or started[1][1]
    # No staff.
    without = _copy(queued)
    for staff in without.staff.values():
        staff.zone_id = "corridor"
    assert serve(without)[1][1] == "staff"
    # No stock.
    assert serve(queued, quantity=10**12)[1][1] == "stock"
    # The Run's end.
    late = _copy(queued)
    late.minute_index = late.duration_minutes - 1
    minute = late.minute_index
    assert serve(late)[1][1] == "run_end"
    minute = queued.minute_index
    # Its stall records: the head's kept, or the first queue's new.
    stalls = [(agent_id, "staff")]
    previous = {"agent_id": agent_id, "reason": "staff"}
    for wanted, known in (
        (stalls, None),
        (stalls, previous),
        ([], None),
        (stalls, {"agent_id": "A", "reason": "stock"}),
    ):
        ours = _copy(queued)
        kernel.record_independent_stall(
            ResourceServiceManager(ours), facility_id, list(wanted), known
        )
        answers.append(_bytes(ours))
        _rows_follow(ours)
    _is_the_python_rules(answers, "serves")


def _serving(queued: WorldState) -> tuple[str, str]:
    """Someone in service at an independent queue: the task's ID and the staff member's."""
    facility_id, _ = _resource_queues(queued)
    zone = queued.facilities[facility_id].zone_id
    for agent_id in ("A001", "A002"):
        queued.agents[agent_id].zone_id = zone
        queued.model_state["movement_v1"]["positions"][agent_id]["zone_id"] = zone
    manager = ResourceServiceManager(queued)
    manager.request("A001", facility_id, validate_state=False)
    for staff in queued.staff.values():
        staff.zone_id = zone
    queue_state = queued.model_state[facility_queues.FACILITY_QUEUES_STATE_KEY]
    request = next(iter(queue_state["requests"].values()))
    point = next(
        item
        for item in queued.model_configs["facility_geometry"]["service_points"]
        if item["queue_id"] == request["queue_id"]
    )
    queued.model_state["movement_v1"]["positions"]["A001"]["point"] = point["point_m"]
    for _ in range(30):
        manager.arbitrate(queued.minute_index, validate_state=False)
        for task in queued.active_tasks.values():
            if task.kind in ("resource_service_water", "resource_service_food"):
                return task.id, task.staff_id
        queued.minute_index += 1
    pytest.fail("the service never started")


def test_a_service_interrupted_goes_back_first_in_its_fifo_as_the_python_rule_says(
    queued: WorldState,
) -> None:
    _, staff_id = _serving(queued)
    facility_id, _ = _resource_queues(queued)
    # Someone else waits, so going back first in line shows.
    ResourceServiceManager(queued).request("A002", facility_id, validate_state=False)
    cause = ScheduledEvent(
        event_id="staff-away",
        kind=EventKind.ENDOGENOUS,
        due_minute=queued.minute_index,
        priority=0,
        sequence=900_000,
        event_type="staff_unavailable",
        payload={"staff_id": staff_id},
        source_id="ASM-TEST",
    )
    ours = _copy(queued)
    _rules("handlers").interrupt_for_staff(
        ResourceServiceManager(ours), staff_id, causation_event=cause
    )
    _rows_follow(ours)
    _is_the_python_rules([_bytes(ours)], "interrupted")
    records = [
        record.event.payload
        for record in ours.event_log.records
        if record.event.event_type == "facility_queue_transition"
    ]
    assert [item["transition"] for item in records[-2:]] == ["interrupted", "requeued"]
    assert records[-1]["queue_position"] == 1


@pytest.mark.parametrize("reason", ["operator_cancelled", "facility_stopped", "staff_unavailable"])
def test_a_cancelled_service_is_the_python_rule_s(queued: WorldState, reason: str) -> None:
    task_id, _ = _serving(queued)
    event = ScheduledEvent(
        event_id="cancel-it",
        kind=EventKind.ENDOGENOUS,
        due_minute=queued.minute_index,
        priority=0,
        sequence=900_000,
        event_type="resource_service_cancel",
        payload={"task_id": task_id, "reason": reason},
        source_id="ASM-TEST",
    )
    ours = _copy(queued)
    _rules("handlers").cancel(ResourceServiceManager(ours), event)
    _rows_follow(ours)
    _is_the_python_rules([_bytes(ours)], f"cancelled-{reason}")
    assert task_id in ours.model_state["resource_service_v1"]["cancelled_task_ids"]
    assert task_id not in ours.active_tasks


def test_a_cancelled_place_in_a_queue_is_the_python_rule_s(queued: WorldState) -> None:
    facility_id, _ = _resource_queues(queued)
    zone = queued.facilities[facility_id].zone_id
    queued.agents["A001"].zone_id = zone
    queued.model_state["movement_v1"]["positions"]["A001"]["zone_id"] = zone
    receipt = ResourceServiceManager(queued).request("A001", facility_id, validate_state=False)
    event = ScheduledEvent(
        event_id="cancel-place",
        kind=EventKind.ENDOGENOUS,
        due_minute=queued.minute_index,
        priority=0,
        sequence=900_000,
        event_type="resource_service_cancel",
        payload={"task_id": receipt.queue_task_id, "reason": "operator_cancelled"},
        source_id="ASM-TEST",
    )
    ours = _copy(queued)
    _rules("handlers").cancel(ResourceServiceManager(ours), event)
    _rows_follow(ours)
    _is_the_python_rules([_bytes(ours)], "cancelled-place")
    assert "A001" not in ours.facilities[facility_id].resource_queue_agent_ids


def test_a_completed_service_leaves_its_fifo_as_the_python_rule_does(queued: WorldState) -> None:
    found = _resource_queues(queued)
    assert found is not None
    facility_id, _ = found
    zone = queued.facilities[facility_id].zone_id
    for agent_id in ("A001", "A002"):
        queued.agents[agent_id].zone_id = zone
        position = queued.model_state["movement_v1"]["positions"][agent_id]
        position["zone_id"] = zone
    manager = ResourceServiceManager(queued)
    manager.request("A001", facility_id, validate_state=False)
    for staff in queued.staff.values():
        staff.zone_id = zone
    geometry = queued.model_configs["facility_geometry"]
    queue_state = queued.model_state[facility_queues.FACILITY_QUEUES_STATE_KEY]
    request = next(iter(queue_state["requests"].values()))
    point = next(
        item for item in geometry["service_points"] if item["queue_id"] == request["queue_id"]
    )
    queued.model_state["movement_v1"]["positions"]["A001"]["point"] = point["point_m"]
    for _ in range(30):
        manager.arbitrate(queued.minute_index, validate_state=False)
        pending = [
            event
            for event in queued.event_queue.pending()
            if event.event_type == "resource_service_complete"
        ]
        if pending:
            break
        queued.minute_index += 1
    assert pending, "the service never started"
    event = pending[0]
    queued.minute_index = event.due_minute
    ours = _copy(queued)
    _rules("service").complete(ResourceServiceManager(ours), event)
    _rows_follow(ours)
    _is_the_python_rules([_bytes(ours)], "completed")
    assert not ours.model_state[facility_queues.FACILITY_QUEUES_STATE_KEY]["requests"]


def _toilet(world: WorldState) -> ToiletServiceManager:
    return ToiletServiceManager(world, min_service_minutes=2, max_service_minutes=4)


def test_a_toilet_fifo_is_the_python_rule_s(queued: WorldState) -> None:
    from sheltersim.core.rng import CounterRNG

    kernel = _rules("toilet")
    queue = "toilet_01-left-fifo"
    zone = queued.facilities["toilet_01"].zone_id
    for agent_id in ("A001", "A002"):
        queued.agents[agent_id].zone_id = zone
        queued.model_state["movement_v1"]["positions"][agent_id]["zone_id"] = zone

    def same_request(name: str, world: WorldState, agent_id: str, **options: Any) -> None:
        def request(ours: WorldState) -> Any:
            receipt = kernel.request(
                _toilet(ours), agent_id, "toilet_01", validate_state=False, **options
            )
            return repr(receipt)

        result, after = _answer(world, request)
        # In the given queue, else the shortest accessible one: last in it, queued there.
        requests = after.model_state["facility_queues_v1"]["requests"]
        (task_id,) = [key for key, row in requests.items() if row["agent_id"] == agent_id]
        if "queue_id" in options:
            assert requests[task_id]["queue_id"] == options["queue_id"]
        assert after.agents[agent_id].activity is Activity.QUEUED
        _recorded(name, result, after)

    same_request("toilet request queue", queued, "A001", queue_id=queue)
    same_request("toilet request", queued, "A001")
    _toilet(queued).request("A001", "toilet_01", queue_id=queue, validate_state=False)
    _toilet(queued).request("A002", "toilet_01", queue_id=queue, validate_state=False)
    rng = CounterRNG(queued.seed)

    def same_head(name: str, world: WorldState, minute: int) -> Any:
        def serve(ours: WorldState) -> Any:
            return kernel.serve_independent_head(_toilet(ours), "toilet_01", queue, minute, rng)

        result, after = _answer(world, serve)
        _recorded(name, result, after)
        return after, result

    # Each stop: the walk, a point with no place, the Run's end, no skilled staff; a stop marks
    # its stall on the head's request.
    same_head("toilet head", queued, queued.minute_index)
    point = next(
        item
        for item in queued.model_configs["facility_geometry"]["service_points"]
        if item["queue_id"] == queue
    )
    queued.model_state["movement_v1"]["positions"]["A001"]["point"] = dict(point["point_m"])
    full = _copy(queued)
    full.facilities["toilet_01"].working_units = 0
    stopped, result = same_head("toilet head full", full, full.minute_index)
    assert result is None
    late = _copy(queued)
    late.minute_index = late.duration_minutes - 1
    assert same_head("toilet head late", late, late.minute_index)[1] is None
    skilled = _copy(queued)
    for item in skilled.model_configs["facility_geometry"]["service_points"]:
        if item["queue_id"] == queue:
            item["access"] = {
                "accessibility": "accessible",
                "required_access_tags": [],
                "required_staff_skills": ["no_such_skill"],
            }
    assert same_head("toilet head unskilled", skilled, skilled.minute_index)[1] is None
    # With skilled staff here the service starts and holds the person.
    staffed = _copy(skilled)
    member = next(iter(staffed.staff.values()))
    member.zone_id = zone
    member.skills = [*member.skills, "no_such_skill"]
    served, started = same_head("toilet head staffed", staffed, staffed.minute_index)
    assert started is not None
    # And the completion lets them go.
    event = served.event_queue.find(started)
    assert event.payload["staff_id"] == member.id
    served.minute_index = event.due_minute
    from sheltersim.events.handlers import ToiletServiceCompletionHandler

    ours = _copy(served)
    kernel.complete(ToiletServiceCompletionHandler(), ours, event)
    # What the completion's Python rule did (gone in lane L10): the slot, the task and the staff
    # member's reservation are freed, and the person is done.
    person = event.payload["agent_id"]
    assert ours.staff[member.id].active_task_id is None
    assert ours.staff[member.id].work_queue == []
    assert ours.staff[member.id].busy_until == 0
    assert event.payload["task_id"] not in ours.active_tasks
    assert person not in ours.facilities["toilet_01"].active_agent_ids
    assert ours.agents[person].activity is Activity.IDLE
    assert ours.agents[person].needs.toilet_urgency == 0.0
