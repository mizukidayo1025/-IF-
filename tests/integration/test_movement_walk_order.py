"""The branches of `advance_movement` that depend on the walk's order.

The gate and the 7-day run never reach them (docs/rust-port/lanes/l3-movement.md), so these tests
are all that checks them: a queue trip that replans, and a person placed in a slot in the walk.
`advance_movement` has no Python rule left (`kernel_only`). Each test's answers, the result and
the whole world after each call (floats by their bits, with the event numbers, the suppressed
records and the metrics), are chained into one digest and compared with the deleted Python rule's,
recorded once from it at 2d8fb46 (ANSWERS, at the end).
"""

from __future__ import annotations

import hashlib
import random
from collections.abc import Iterator
from typing import Any

import pytest

from sheltersim import native
from sheltersim.core.rng import CounterRNG
from sheltersim.core.world import WorldState
from sheltersim.domain import movement
from sheltersim.domain.facilities import ToiletServiceManager
from sheltersim.domain.models import ActiveTask, Activity
from sheltersim.domain.movement import MOVEMENT_STATE_KEY as KEY
from sheltersim.events.models import EventKind, EventStatus, ScheduledEvent

TOILET = "toilet_01"
SLOW = 0.2


def _bytes(world: WorldState) -> str:
    return world._canonical_text(world._canonical_data())


def _records(world: WorldState) -> list[Any]:
    """Each record, with the event's payload by its sorted keys, as the log's bytes hold it."""
    return [
        (
            record.event.event_id,
            record.event.sequence,
            record.realized_minute,
            record.status,
            record.detail,
            sorted(record.event.payload.items(), key=lambda item: item[0]),
        )
        for record in world.event_log.records
    ]


def _pending(world: WorldState) -> list[str]:
    return [repr(event) for event in world.event_queue.pending()]


def _order_of_maps(world: WorldState) -> list[list[str]]:
    state = world.model_state[KEY]
    return [list(state[name]) for name in ("positions", "journeys", "edge_usage_minutes")]


def _answer(world: WorldState, result: tuple[str, ...]) -> bytes:
    """What the rule returned and left, as the deleted rule's answers were recorded."""
    return repr(
        (
            result,
            _bytes(world),
            sorted(world.id_counters.items()),
            sorted(world.metric_state.items()),
            _pending(world),
            _records(world),
            _order_of_maps(world),
            sorted((key, agent.activity, agent.target_id) for key, agent in world.agents.items()),
        )
    ).encode()


# Each test's answers, chained in the order of its calls of `_same_after`.
_CHAINS: dict[str, Any] = {}
_NODE = [""]


@pytest.fixture(autouse=True)
def _answers_are_the_deleted_rule_s(request: pytest.FixtureRequest) -> Iterator[None]:
    """After each test, its chained answers against the deleted Python rule's (ANSWERS).

    The worlds hold floats of the platform's libm (the floor plan's lengths and points), recorded
    on Linux. Lane L13 pins them on every OS; until then the checks in each test are what macOS
    and Windows check.
    """
    _NODE[0] = request.node.name
    yield
    chain = _CHAINS.pop(request.node.name, None)
    if chain is not None:
        assert chain.hexdigest() == ANSWERS[request.node.name]


def _same_after(world: WorldState, minute: int) -> tuple[str, ...]:
    """The rule on the world, its answer chained for the test's check, the row still the tree's."""
    result = movement.advance_movement(world, minute)
    _CHAINS.setdefault(_NODE[0], hashlib.sha256()).update(_answer(world, result) + b"\n")
    mirror = world._core.mirror(KEY)
    assert mirror.tainted is None
    assert mirror.verify()
    assert mirror.row_json() == native.canonical_json(world.model_state[KEY])
    return result


def _toilet_area(world: WorldState) -> Any:
    geometry = movement._geometry(world)
    return next(item for item in geometry.zones if item.zone_id == "toilet")


def _stand_in_the_toilet_area(world: WorldState, agent_ids: tuple[str, ...]) -> None:
    speeds = world.model_configs["movement_model"]["speed_m_per_minute_by_mobility"]
    for mobility in speeds:
        speeds[mobility] = SLOW
    routes = world.model_configs["facility_geometry"]["routes"]
    entrance = next(route for route in routes if route["edge_id"] == "route_corridor_toilet")
    for agent_id in agent_ids:
        world.agents[agent_id].zone_id = "toilet"
        world.agents[agent_id].next_decision_at = 100
        world.model_state[KEY]["positions"][agent_id].update(
            zone_id="toilet", point=dict(entrance["points"][-1]), position_kind="physical"
        )


def _queues_of_the_toilet(world: WorldState) -> list[str]:
    return sorted(
        queue_id
        for queue_id, row in world.model_state["facility_queues_v1"]["queues"].items()
        if row["facility_id"] == TOILET
    )


def _two_trips_in_two_queues(scenario: Any) -> WorldState:
    world = WorldState.from_scenario(scenario)
    _stand_in_the_toilet_area(world, ("A001", "A002"))
    first, second, *_ = _queues_of_the_toilet(world)
    service = ToiletServiceManager(world, min_service_minutes=2, max_service_minutes=2)
    service.request("A001", TOILET, queue_id=first)
    service.request("A002", TOILET, queue_id=second)
    service.arbitrate(0, CounterRNG(world.seed))
    assert set(world.model_state[KEY]["journeys"]) == {"A001", "A002"}
    world.minute_index = 1
    return world


def _head_and_follower_in_one_queue(scenario: Any) -> tuple[WorldState, dict[str, float]]:
    """The head walks to the service point and the follower to its slot; the head stands on it."""
    world = WorldState.from_scenario(scenario)
    _stand_in_the_toilet_area(world, ("A001", "A002"))
    queue_id = _queues_of_the_toilet(world)[0]
    service = ToiletServiceManager(world, min_service_minutes=2, max_service_minutes=2)
    service.request("A001", TOILET, queue_id=queue_id)
    service.request("A002", TOILET, queue_id=queue_id)
    service.arbitrate(0, CounterRNG(world.seed))
    head = world.model_state[KEY]["journeys"]["A001"]
    head["purpose"] = head["purpose"].removesuffix("_position") + "_service"
    assert not movement.progress_physical_queue_positions(
        world, facility_id=TOILET, resource=False, minute=0, queue_id=queue_id
    )
    assert set(world.model_state[KEY]["journeys"]) == {"A001", "A002"}
    slot = world.model_state[KEY]["journeys"]["A002"]["route_points"][-1]
    world.model_state[KEY]["positions"]["A001"]["point"] = dict(slot)
    world.minute_index = 1
    return world, dict(slot)


def _without_independent_queues(scenario: Any) -> Any:
    """The generated Scenario with no `facility_queues`: each facility has its own queue."""
    document = scenario.model_dump(mode="json")
    document["model_configs"].pop("facility_queues", None)
    document["model_configs"]["facility_geometry"]["service_points"] = []
    return type(scenario).model_validate(document)


def _trips_of_the_facility_s_own_queue(scenario: Any, people: tuple[str, ...]) -> WorldState:
    """People in a queue without independent FIFOs, as arbitration leaves them."""
    world = WorldState.from_scenario(_without_independent_queues(scenario))
    assert "facility_queues_v1" not in world.model_state
    _stand_in_the_toilet_area(world, people)
    service = ToiletServiceManager(world, min_service_minutes=2, max_service_minutes=2)
    for agent_id in people:
        service.request(agent_id, TOILET)
    service.arbitrate(0, CounterRNG(world.seed))
    assert world.model_state[KEY]["journeys"]
    world.minute_index = 1
    return world


def _move_the_toilet(world: WorldState, point: dict[str, float] | None = None) -> None:
    """What an equipment move leaves: a trip planned before it must replan."""
    where = point or world.model_configs["facility_geometry"]["facility_points"][TOILET]
    world.model_state["geometry_overlay_v1"]["facility_points"][TOILET] = {
        "point_m": dict(where),
        "minute": 5,
        "source_ids": [],
        "event_id": "moved",
    }


def _obstacle_on_the_walk(world: WorldState, agent_id: str) -> None:
    """A small square across the first leg of this person's room walk."""
    points = world.model_state[KEY]["journeys"][agent_id]["route_points"]
    x = (points[0]["x"] + points[1]["x"]) / 2
    y = (points[0]["y"] + points[1]["y"]) / 2
    constraints = world.model_configs.setdefault("spatial_constraints", {})
    constraints.setdefault("obstacles", []).append(
        {
            "id": f"block-{agent_id}",
            "polygon": [
                {"x": x - 0.3, "y": y - 0.3},
                {"x": x + 0.3, "y": y - 0.3},
                {"x": x + 0.3, "y": y + 0.3},
                {"x": x - 0.3, "y": y + 0.3},
            ],
            "source_ids": ["ASM-TEST"],
        }
    )


def _schedule(world: WorldState, agent_id: str, task_id: str, event_id: str, due: int) -> None:
    world.active_tasks[task_id] = ActiveTask(
        id=task_id,
        kind="travel_return",
        agent_id=agent_id,
        resource_id=None,
        start_minute=0,
        due_minute=due,
    )
    sequence = world.allocate_event_sequence()
    world.schedule_event(
        ScheduledEvent(
            event_id=event_id,
            kind=EventKind.COMPLETION,
            due_minute=due,
            priority=0,
            sequence=sequence,
            event_type="agent_travel_complete",
            payload={"agent_id": agent_id, "task_id": task_id},
            source_id=task_id,
            causation_id=task_id,
            provenance={"source_type": "assumption", "source_id": "ASM-TEST"},
        )
    )


# ---- A queue trip that replans -------------------------------------------------------------------


def test_two_replans_take_their_event_numbers_in_the_order_of_the_walk(
    generated_scenario_with_split_queues: Any,
) -> None:
    world = _two_trips_in_two_queues(generated_scenario_with_split_queues)
    before = {key: value["event_id"] for key, value in world.model_state[KEY]["journeys"].items()}
    _move_the_toilet(world)
    assert _same_after(world, 1) == ()
    journeys = world.model_state[KEY]["journeys"]
    first, second = (int(journeys[key]["event_id"].rsplit("-", 1)[1]) for key in ("A001", "A002"))
    assert first + 1 == second
    assert world.metric_state["physical_queue_replan_count"] == 2.0
    assert world.metric_state["movement_replan_count"] == 2.0
    assert list(journeys) == ["A001", "A002"]
    suppressed = [row for row in world.event_log.records if row.status is EventStatus.SUPPRESSED]
    assert [row.event.event_id for row in suppressed] == [before["A001"], before["A002"]]


def test_a_replan_for_a_wall_on_the_walk(generated_scenario_with_split_queues: Any) -> None:
    world = _two_trips_in_two_queues(generated_scenario_with_split_queues)
    _obstacle_on_the_walk(world, "A002")
    before = dict(world.model_state[KEY]["journeys"]["A002"])
    assert _same_after(world, 1) == ()
    after = world.model_state[KEY]["journeys"]["A002"]
    assert after["event_id"] != before["event_id"]
    assert after["route_points"] != before["route_points"]


def test_a_replan_for_a_facility_that_changed_zone(
    generated_scenario_with_split_queues: Any,
) -> None:
    world = _two_trips_in_two_queues(generated_scenario_with_split_queues)
    world.facilities[TOILET].zone_id = "corridor"
    assert _same_after(world, 1) == ()


def _queue_trip(
    world: WorldState,
    agent_id: str,
    purpose: str = "toilet_queue_position",
    *,
    started: int = 0,
    due: int = 300,
    points: list[dict[str, float]] | None = None,
) -> dict[str, Any]:
    """A walk of this person, by hand, with its event pending and their queue's task."""
    state = world.model_state[KEY]
    task_id = next(
        key
        for key, task in world.active_tasks.items()
        if task.agent_id == agent_id and task.kind == "toilet_queue"
    )
    event_id = f"trip-of-{agent_id}"
    if world.event_queue.find(event_id) is None:
        _schedule(world, agent_id, f"holder-{agent_id}", event_id, due)
        del world.active_tasks[f"holder-{agent_id}"]
    start = state["positions"][agent_id]["point"]
    target = world.model_configs["facility_geometry"]["facility_points"][TOILET]
    state["journeys"].pop(agent_id, None)
    state["journeys"][agent_id] = {
        "event_id": event_id,
        "task_id": task_id,
        "purpose": purpose,
        "facility_id": TOILET,
        "destination_zone_id": "toilet",
        "route_zone_ids": ["toilet", "toilet"],
        "route_edge_ids": [],
        "route_points": points or [dict(start), dict(target)],
        "started_minute": started,
        "due_minute": due,
    }
    return state["journeys"][agent_id]


def _own_queue_with_trips(scenario: Any, people: tuple[str, ...]) -> WorldState:
    """Everyone in the facility's own queue on a queue trip that must replan once it has moved."""
    world = _trips_of_the_facility_s_own_queue(scenario, people)
    world.model_state[KEY]["journeys"].clear()
    for key in [event.event_id for event in world.event_queue.pending()]:
        if key.startswith("physical-queue-trip-"):
            world.event_queue.remove(key)
    for agent_id in people:
        _queue_trip(world, agent_id)
    return world


def test_a_replan_with_no_room_route_is_blocked_and_counted(generated_scenario: Any) -> None:
    world = _own_queue_with_trips(generated_scenario, ("A001", "A002"))
    _move_the_toilet(world, {"x": -1000.0, "y": -1000.0})
    assert _same_after(world, 1) == ()
    assert world.model_state[KEY]["journeys"] == {}
    assert world.metric_state["physical_queue_replan_blocked_count"] == 2.0
    assert world.metric_state["movement_replan_blocked_count"] == 2.0


def test_a_replan_past_the_end_of_the_run_is_blocked_in_its_schedule(
    generated_scenario_with_split_queues: Any,
) -> None:
    world = _two_trips_in_two_queues(generated_scenario_with_split_queues)
    _obstacle_on_the_walk(world, "A001")
    _obstacle_on_the_walk(world, "A002")
    world.duration_minutes = 3
    assert _same_after(world, 1) == ()
    assert world.metric_state["physical_queue_route_blocked_count"] == 2.0
    # The replan's own count goes up too: the Python rule counts it when the schedule gave up.
    assert world.metric_state["physical_queue_replan_count"] == 2.0


def test_a_replan_to_where_the_person_stands_takes_no_trip(generated_scenario: Any) -> None:
    world = _own_queue_with_trips(generated_scenario, ("A001", "A002"))
    state = world.model_state[KEY]
    for agent_id in ("A001", "A002"):
        state["journeys"][agent_id]["purpose"] = "toilet_queue_service"
    _move_the_toilet(world, dict(state["positions"]["A001"]["point"]))
    assert _same_after(world, 1) == ()
    assert state["journeys"] == {}
    assert world.metric_state["physical_queue_replan_count"] == 2.0


def test_someone_on_their_way_on_a_queue_trip_is_queued_by_the_replan(
    generated_scenario: Any,
) -> None:
    world = _own_queue_with_trips(generated_scenario, ("A001", "A002"))
    world.agents["A001"].activity = Activity.MOVING
    world.agents["A001"].target_id = "elsewhere"
    _move_the_toilet(world)
    assert _same_after(world, 1) == ()
    assert world.agents["A001"].activity is Activity.QUEUED
    assert world.agents["A001"].target_id == TOILET


def test_someone_on_their_way_whose_replan_is_blocked_stands_still(
    generated_scenario: Any,
) -> None:
    world = _own_queue_with_trips(generated_scenario, ("A001", "A002"))
    world.agents["A001"].activity = Activity.MOVING
    _move_the_toilet(world, {"x": -1000.0, "y": -1000.0})
    assert _same_after(world, 1) == ()
    assert world.agents["A001"].activity is Activity.MOVING
    assert "A001" not in world.model_state[KEY]["journeys"]


def test_a_replan_when_the_facility_is_gone_only_ends_the_trip(generated_scenario: Any) -> None:
    world = _own_queue_with_trips(generated_scenario, ("A001",))
    journey = world.model_state[KEY]["journeys"]["A001"]
    journey["facility_id"] = "another_facility"
    # The move is of the name on the journey, which is no facility of the world.
    world.model_state["geometry_overlay_v1"]["facility_points"]["another_facility"] = {
        "point_m": {"x": 0.0, "y": 0.0},
        "minute": 5,
        "source_ids": [],
        "event_id": "moved",
    }
    assert _same_after(world, 1) == ()
    assert world.model_state[KEY]["journeys"] == {}
    assert "movement_replan_count" not in world.metric_state


def test_a_replan_of_the_facility_s_own_queue(generated_scenario: Any) -> None:
    world = _own_queue_with_trips(generated_scenario, ("A001", "A002"))
    _move_the_toilet(world)
    before = world.model_state[KEY]["journeys"]["A001"]["event_id"]
    assert _same_after(world, 1) == ()
    assert world.model_state[KEY]["journeys"]["A001"]["event_id"] != before


@pytest.mark.parametrize("people", [("A001", "A002"), ("A002", "A001")])
def test_a_slot_read_with_the_one_ahead_before_or_after_their_walk(
    generated_scenario: Any, people: tuple[str, str]
) -> None:
    """Whoever is ahead in the queue, walked already or not when the follower replans."""
    world = _own_queue_with_trips(generated_scenario, people)
    state = world.model_state[KEY]
    head, follower = people
    state["journeys"][head]["purpose"] = "toilet_queue_service"
    _obstacle_on_the_walk(world, follower)
    state["positions"][head]["progress"] = 0.9
    state["positions"][follower]["progress"] = 0.0
    assert _same_after(world, 1) == ()


def test_the_one_ahead_moving_changes_the_follower_s_destination(
    generated_scenario_with_split_queues: Any,
) -> None:
    """The dependence with a coordinate in it: the follower takes the slot the head walked off."""
    world, slot = _head_and_follower_in_one_queue(generated_scenario_with_split_queues)
    state = world.model_state[KEY]
    assert state["journeys"]["A002"]["route_points"][-1] == slot
    world.model_state["geometry_overlay_v1"]["facility_points"][TOILET] = {
        "point_m": dict(world.model_configs["facility_geometry"]["facility_points"][TOILET]),
        "minute": 5,
        "source_ids": [],
        "event_id": "moved",
    }
    assert _same_after(world, 1) == ()
    assert set(state["journeys"]) == {"A001", "A002"}
    assert state["journeys"]["A002"]["route_points"][-1] != slot


def test_a_replan_seen_by_the_slot_of_someone_later_in_the_walk(
    generated_scenario: Any,
) -> None:
    """A001 replans and so stands elsewhere; A002, later in the walk, is placed in a slot."""
    world = _own_queue_with_trips(generated_scenario, ("A001", "A002"))
    state = world.model_state[KEY]
    _move_the_toilet(world)
    # A002 is queued, with a walk of another purpose: the step places them in their slot.
    assert world.agents["A002"].activity is Activity.QUEUED
    state["journeys"]["A002"]["purpose"] = "return"
    state["journeys"]["A002"]["facility_id"] = None
    state["journeys"]["A002"]["started_minute"] = 0
    assert _same_after(world, 1) == ()
    assert "A002" not in state["journeys"]
    assert state["positions"]["A002"]["position_kind"] == "physical"


# ---- A person placed in a slot during the walk ----------------------------------------------


@pytest.mark.parametrize("queue_order", [("A001", "A002", "A003"), ("A003", "A002", "A001")])
def test_people_placed_in_slots_read_who_has_walked(
    generated_scenario: Any, queue_order: tuple[str, str, str]
) -> None:
    """Each is queued with a trip whose task went: the walk places them, one after another."""
    world = _own_queue_with_trips(generated_scenario, queue_order)
    state = world.model_state[KEY]
    base = state["positions"][queue_order[0]]["point"]
    for offset, agent_id in enumerate(queue_order):
        state["positions"][agent_id]["point"] = {"x": base["x"], "y": base["y"] + offset * 0.1}
        state["journeys"][agent_id]["task_id"] = f"gone-{agent_id}"
    assert _same_after(world, 1) == ()
    assert state["journeys"] == {}


def test_a_queued_person_past_their_trip_keeps_their_arrival(generated_scenario: Any) -> None:
    """The completion was applied: `finish_movement` of a queue trip, in the walk."""
    world = _own_queue_with_trips(generated_scenario, ("A001", "A002"))
    state = world.model_state[KEY]
    world.event_queue.remove(state["journeys"]["A001"]["event_id"])
    assert _same_after(world, 1) == ()
    assert "A001" not in state["journeys"]


# ---- Walks that replan twice in a tick -----------------------------------------------------------


def test_one_person_replans_in_two_ticks_running(
    generated_scenario_with_split_queues: Any,
) -> None:
    world = _two_trips_in_two_queues(generated_scenario_with_split_queues)
    _move_the_toilet(world)
    assert _same_after(world, 1) == ()
    first = dict(world.model_state[KEY]["journeys"]["A001"])
    world.minute_index = 2
    world.model_state["geometry_overlay_v1"]["facility_points"][TOILET]["minute"] = 7
    assert _same_after(world, 2) == ()
    second = world.model_state[KEY]["journeys"]["A001"]
    assert first["event_id"] != second["event_id"]
    # And a minute already done does nothing, whatever it would replan.
    assert _same_after(world, 2) == ()


def test_a_queue_replan_and_a_walker_who_must_replan_in_one_walk(
    generated_scenario_with_split_queues: Any,
) -> None:
    """One replans in the walk and takes an event; the other is returned to the controller."""
    world = _two_trips_in_two_queues(generated_scenario_with_split_queues)
    state = world.model_state[KEY]
    edge = world.zone_edges["route_sleeping_corridor"]
    edge.closed = True
    _schedule(world, "A003", "walk-3", "walk-event-3", 40)
    world.agents["A003"].activity = Activity.MOVING
    world.agents["A003"].zone_id = "sleeping"
    state["journeys"]["A003"] = {
        "event_id": "walk-event-3",
        "task_id": "walk-3",
        "purpose": "return",
        "facility_id": None,
        "destination_zone_id": "corridor",
        "route_zone_ids": ["sleeping", "corridor"],
        "route_edge_ids": ["route_sleeping_corridor"],
        "route_points": [],
        "started_minute": 0,
        "due_minute": 40,
    }
    _move_the_toilet(world)
    assert _same_after(world, 1) == ("A003",)
    assert world.metric_state["physical_queue_replan_count"] == 2.0


# ---- Refusals ------------------------------------------------------------------------------------


def test_a_trip_with_no_queue_task_is_python_s_error_before_any_change(
    generated_scenario: Any,
) -> None:
    """The deleted rule's ValueError, which the kernel raises before it writes anything."""
    world = _own_queue_with_trips(generated_scenario, ("A001", "A002"))
    state = world.model_state[KEY]
    task_id = state["journeys"]["A001"]["task_id"]
    world.active_tasks[task_id].resource_id = "another"
    _move_the_toilet(world)
    before = _bytes(world)
    with pytest.raises(ValueError) as raised:
        movement.advance_movement(world, 1)
    assert str(raised.value) == "physical queue reservation has no matching active queue task"
    assert _bytes(world) == before


# ---- At random -----------------------------------------------------------------------------------


def _random_trips(world: WorldState, rng: random.Random, people: tuple[str, ...]) -> None:
    """Everyone in a queue, each with a trip of a kind drawn at random, some of them stale."""
    state = world.model_state[KEY]
    for key in [event.event_id for event in world.event_queue.pending()]:
        if key.startswith("physical-queue-trip-"):
            world.event_queue.remove(key)
    area = _toilet_area(world)
    xs = [point.x for point in area.polygon]
    ys = [point.y for point in area.polygon]

    def somewhere() -> dict[str, float]:
        return {
            "x": rng.uniform(min(xs) + 0.2, max(xs) - 0.2),
            "y": rng.uniform(min(ys) + 0.2, max(ys) - 0.2),
        }

    state["journeys"].clear()
    tasks = {
        task.agent_id: key
        for key, task in world.active_tasks.items()
        if task.kind == "toilet_queue"
    }
    for agent_id in rng.sample(people, len(people)):
        kind = rng.choice(("position", "service", "other", "none", "stale", "gone"))
        state["positions"][agent_id]["point"] = somewhere()
        state["positions"][agent_id]["progress"] = rng.choice((0.0, 0.25, 1.0))
        if kind == "none":
            continue
        event_id = f"trip-{agent_id}"
        purpose = {
            "position": "toilet_queue_position",
            "service": "toilet_queue_service",
            "stale": "toilet_queue_position",
            "gone": "toilet_queue_position",
            "other": "return",
        }[kind]
        task_id = tasks[agent_id]
        if kind == "other":
            task_id = f"other-{agent_id}"
        _schedule(world, agent_id, f"holder-{agent_id}", event_id, 200 + rng.randrange(50))
        del world.active_tasks[f"holder-{agent_id}"]
        if kind == "other":
            world.active_tasks[task_id] = ActiveTask(
                id=task_id,
                kind="travel_return",
                agent_id=agent_id,
                resource_id=None,
                start_minute=0,
                due_minute=300,
            )
        if kind == "stale":
            world.event_queue.remove(event_id)
        if kind == "gone":
            task_id = f"task-that-went-{agent_id}"
        state["journeys"][agent_id] = {
            "event_id": event_id,
            "task_id": task_id,
            "purpose": purpose,
            "facility_id": TOILET if kind != "other" else None,
            "destination_zone_id": "toilet",
            "route_zone_ids": ["toilet", "toilet"],
            "route_edge_ids": [],
            "route_points": [somewhere(), somewhere(), somewhere()],
            "started_minute": rng.choice((0, 7)),
            "due_minute": 300,
        }


def _random_walk(scenario: Any, seed: int) -> WorldState:
    rng = random.Random(seed)
    people = tuple(rng.sample(("A001", "A002", "A003", "A004", "A005", "A006"), 6))
    world = _trips_of_the_facility_s_own_queue(scenario, people)
    _random_trips(world, rng, people)
    point = world.model_configs["facility_geometry"]["facility_points"][TOILET]
    _move_the_toilet(world, {"x": point["x"] - rng.uniform(0.0, 0.4), "y": point["y"]})
    if rng.random() < 0.5:
        area = _toilet_area(world)
        xs = [item.x for item in area.polygon]
        ys = [item.y for item in area.polygon]
        x, y = rng.uniform(min(xs), max(xs)), rng.uniform(min(ys), max(ys))
        world.model_configs.setdefault("spatial_constraints", {}).setdefault(
            "obstacles", []
        ).append(
            {
                "id": "random-block",
                "polygon": [
                    {"x": x - 0.2, "y": y - 0.2},
                    {"x": x + 0.2, "y": y - 0.2},
                    {"x": x + 0.2, "y": y + 0.2},
                    {"x": x - 0.2, "y": y + 0.2},
                ],
                "source_ids": ["ASM-TEST"],
            }
        )
    for minute in (1, 2, 3):
        world.minute_index = minute
        _same_after(world, minute)
    return world


@pytest.mark.parametrize("seed", range(40))
def test_random_walks_are_python_s_bit_for_bit(generated_scenario: Any, seed: int) -> None:
    _random_walk(generated_scenario, seed)


def test_the_random_walks_reach_the_branches(generated_scenario: Any) -> None:
    """Or the test above compares little: replans of each ending, and people placed in slots."""
    counts: dict[str, float] = {}
    for seed in range(12):
        world = _random_walk(generated_scenario, seed)
        for name in (
            "physical_queue_replan_count",
            "physical_queue_replan_blocked_count",
            "movement_replan_count",
        ):
            counts[name] = counts.get(name, 0.0) + world.metric_state.get(name, 0.0)
    assert counts["physical_queue_replan_count"] > 0
    assert counts["physical_queue_replan_blocked_count"] > 0


# The deleted Python rule's answers of each test, chained in the order of its calls: recorded
# once from the Python rule at 2d8fb46, on Linux.
ANSWERS = {
    "test_a_queue_replan_and_a_walker_who_must_replan_in_one_walk": (
        "dbd027987b782fe7b335db4a66ad06a64a090e63ad931dfb3aeb62d4bff39a26"
    ),
    "test_a_queued_person_past_their_trip_keeps_their_arrival": (
        "c1e3a73b53bbff45afb0e2f91329434f67ab4b0e0fa251fc377d17ab2bd852f1"
    ),
    "test_a_replan_for_a_facility_that_changed_zone": (
        "b7315fc57f7560c1a721cfceec20aede946a0b79f1f9184f2007643c89fb6f7c"
    ),
    "test_a_replan_for_a_wall_on_the_walk": (
        "fd6ba4178a7121170be3ae1105b5d61db80ae48ddc34dfbe99ee01adf6255c53"
    ),
    "test_a_replan_of_the_facility_s_own_queue": (
        "ff6a37ca560d199a1314a703f81b5481761fec6bb605428a74b12769b2aad304"
    ),
    "test_a_replan_past_the_end_of_the_run_is_blocked_in_its_schedule": (
        "c63af15a6ee52ce0d69818f5b51e6ef82a7527d20e2de8da48be464c2a00d3a3"
    ),
    "test_a_replan_seen_by_the_slot_of_someone_later_in_the_walk": (
        "6eb1ed14a325123f25fbcd4cfdc085db21c0baeb94c9163dc28afff96c2b4419"
    ),
    "test_a_replan_to_where_the_person_stands_takes_no_trip": (
        "d7a46d640910241b232e56200c6564ad1df8df136f5a438a57cc44db98b966ee"
    ),
    "test_a_replan_when_the_facility_is_gone_only_ends_the_trip": (
        "e38534325228edf42453d58d35fd4c14e7a7c80cc0feee0bd0aee33c0ecaaf8a"
    ),
    "test_a_replan_with_no_room_route_is_blocked_and_counted": (
        "964f0525338befd392753aca62e48496ee07dff59c155f96198bc3d87cc1e02f"
    ),
    "test_a_slot_read_with_the_one_ahead_before_or_after_their_walk[people0]": (
        "a7b6df0115fe8b80e9fe523093b424ea436b642c5f2151baa5a5e185345619a4"
    ),
    "test_a_slot_read_with_the_one_ahead_before_or_after_their_walk[people1]": (
        "93004fcf3e625ef5c417fd0b628bf112ace6468b38cb4162a154aaba6e7f528f"
    ),
    "test_one_person_replans_in_two_ticks_running": (
        "88d59727ec86dfd525037157fd09a8fc5f5af44679bc4331ec8fdcfe83acde43"
    ),
    "test_people_placed_in_slots_read_who_has_walked[queue_order0]": (
        "660dd9e12fd09c0568a89c14cc31cb130a8910688a3bc895a931dea1ef0eb00f"
    ),
    "test_people_placed_in_slots_read_who_has_walked[queue_order1]": (
        "85b36c90eab3d90c218f8ad0d6039481d0a9dec709b0da4fdc5e7cfb69215f56"
    ),
    "test_random_walks_are_python_s_bit_for_bit[0]": (
        "e9d4ef400329a888a7114dd91f166d4f35ab039cd8684ca90fd81e4d8b471861"
    ),
    "test_random_walks_are_python_s_bit_for_bit[10]": (
        "41a29120050a35086846e504893b2d93c3a8b870c20a77b87b2103395a9d7d12"
    ),
    "test_random_walks_are_python_s_bit_for_bit[11]": (
        "afba7b48474a5c2c4089f3102d10146839145f854a6cfe769a692c1831e57310"
    ),
    "test_random_walks_are_python_s_bit_for_bit[12]": (
        "5222b0dde2605e22dad2b104456874e37b130154b48d24eb8be0fda7daee3523"
    ),
    "test_random_walks_are_python_s_bit_for_bit[13]": (
        "576fced6fdb71527e0910fcc1aa4f3ebb2ad78e73d706b06dde3692949696e7b"
    ),
    "test_random_walks_are_python_s_bit_for_bit[14]": (
        "af6d3f9fc3b254102ccafdf332ca4f1701d21cd6ef4454f5a98a318dbdcacdbb"
    ),
    "test_random_walks_are_python_s_bit_for_bit[15]": (
        "dafe25035bd94d22292227bba35ac99fc13728b157fd09c53817b9a2bd67bd66"
    ),
    "test_random_walks_are_python_s_bit_for_bit[16]": (
        "448ab96cfcfc2adb3e96d169503671601734ebf03077e4f2eda22f42e7f1e18c"
    ),
    "test_random_walks_are_python_s_bit_for_bit[17]": (
        "40099415f018c7eb115effd54bb919d2f71937c380538a6c73c7dce37a1af89e"
    ),
    "test_random_walks_are_python_s_bit_for_bit[18]": (
        "88b55191ac1ca920159f181aed9834d9267a811d155cea1f0fdab7eb3f43b26f"
    ),
    "test_random_walks_are_python_s_bit_for_bit[19]": (
        "204b2d9d6b2e3dd0c3929aa2a7337baf9e4e29f20dea5f6ca49ea1d9f3f530a6"
    ),
    "test_random_walks_are_python_s_bit_for_bit[1]": (
        "d9cc5928b92ee549cc6e0fae6921ddeb0b5b6613fc7c248cc1738604b3cbd152"
    ),
    "test_random_walks_are_python_s_bit_for_bit[20]": (
        "c0e9fe977196d02a7041f8f5c0c825907b32bbcadccec8c1f4c84ca7546eb579"
    ),
    "test_random_walks_are_python_s_bit_for_bit[21]": (
        "f0b4bb8f44fadcd975b87312e6ff43a9232b63e7d93abdd1ed49644a7e1c615e"
    ),
    "test_random_walks_are_python_s_bit_for_bit[22]": (
        "6df12f936985a435d8833555c28233a969588938e8cc715d3df4ffb61d36ad9f"
    ),
    "test_random_walks_are_python_s_bit_for_bit[23]": (
        "1151817dfde20df79ef213239f43d954ab93bb3a48134567295b41d9cdaf66b4"
    ),
    "test_random_walks_are_python_s_bit_for_bit[24]": (
        "c89c30b48fcc981f5a1ee16365ff8a613c3b007bccb6cd872f7a1f3d6f27f874"
    ),
    "test_random_walks_are_python_s_bit_for_bit[25]": (
        "4150b2a3cf2df1fe845ede054a6fa97047a44455b4499ae0277de9c62c8e19f0"
    ),
    "test_random_walks_are_python_s_bit_for_bit[26]": (
        "db7e90da71b47baccfbf90cedea30c421ba25967bd2f91cc79d6af98722c69a5"
    ),
    "test_random_walks_are_python_s_bit_for_bit[27]": (
        "7b5e4f0f8a4129064c83a3f03ef866a799199719695b61fccaae3660bcfbfde9"
    ),
    "test_random_walks_are_python_s_bit_for_bit[28]": (
        "7402de92d046ae586656b0fae89d5e688f244545f724ba033e25f794557c7563"
    ),
    "test_random_walks_are_python_s_bit_for_bit[29]": (
        "207f03e80db3befcb0aab2897c75ba0d9fdfc14c0cef0b595b42afed8e27f2d6"
    ),
    "test_random_walks_are_python_s_bit_for_bit[2]": (
        "5126596a7f18bce3cd934226ecb5688d4b0b80ed3e1ff4d7bef73572485ce5c6"
    ),
    "test_random_walks_are_python_s_bit_for_bit[30]": (
        "016d7e72a6c825ea562fb9eee65bfbd3047652c9370bfbbe55da0fba14350054"
    ),
    "test_random_walks_are_python_s_bit_for_bit[31]": (
        "1eaea0056d3cf6d609b86e711c3c055bee66c08a4d0bd1693ccea133e201cde9"
    ),
    "test_random_walks_are_python_s_bit_for_bit[32]": (
        "872a83a5ef21504539415ef05fe1542d8bfe10fc9cb8f67893447a2107039e57"
    ),
    "test_random_walks_are_python_s_bit_for_bit[33]": (
        "4facff7c31c5a58b81fa55cad55df077e58119056c685d27f3ce74ad84f82957"
    ),
    "test_random_walks_are_python_s_bit_for_bit[34]": (
        "fcfac9525839b7471b616f9e94b08cc73c13ede02807399bc33e8bd65c959978"
    ),
    "test_random_walks_are_python_s_bit_for_bit[35]": (
        "69bfaa1957d8f79ee55c402902a8db8c020c26a0b12748be5be96cc2b48e564d"
    ),
    "test_random_walks_are_python_s_bit_for_bit[36]": (
        "b5e3a1fb3623d630a25d804bd9d81011aa3597d5293b86f71ea5fc493b40b7ec"
    ),
    "test_random_walks_are_python_s_bit_for_bit[37]": (
        "9c3916e7d636737f0eb42c1d81dce25a4c4d2312754fdae7812fa9043fd20cdc"
    ),
    "test_random_walks_are_python_s_bit_for_bit[38]": (
        "04d6806f8a192a59f1bafc1ef3eae2222b835c96d8b23fab4cdd897f93c9d54c"
    ),
    "test_random_walks_are_python_s_bit_for_bit[39]": (
        "2e2501024a9bab81e0c0bbce98c43b544c5b3e75d5393be7b14957c48bb853bc"
    ),
    "test_random_walks_are_python_s_bit_for_bit[3]": (
        "46a41a0fc0c74b77898cd7eab677df0a50b374876fe09205535b7dc5a2dc6c5b"
    ),
    "test_random_walks_are_python_s_bit_for_bit[4]": (
        "cedcc8a93dc4127d3647b7ad39304c4ff1fe67455acf40ba1d1b7c0424bc6315"
    ),
    "test_random_walks_are_python_s_bit_for_bit[5]": (
        "c555e309667ec6658d4502eaed664254097e769b2c39f97072bdcda97fa47db6"
    ),
    "test_random_walks_are_python_s_bit_for_bit[6]": (
        "f0c69e33592db044fe0304d650a9f938cbf14e776aa36b16303178e9ef64d08e"
    ),
    "test_random_walks_are_python_s_bit_for_bit[7]": (
        "9f23142f43447c157d89fc70e139a49e544333d92917a951c988d3281a7f2567"
    ),
    "test_random_walks_are_python_s_bit_for_bit[8]": (
        "9bf6510a56e69ebee18829dd0bf99cb034987f4f0d005a4e1124b5c22eae385f"
    ),
    "test_random_walks_are_python_s_bit_for_bit[9]": (
        "bc5fda3f7f9842b5de54f599f4da3f0a117287ea5195530176446fdc36774074"
    ),
    "test_someone_on_their_way_on_a_queue_trip_is_queued_by_the_replan": (
        "ff6a37ca560d199a1314a703f81b5481761fec6bb605428a74b12769b2aad304"
    ),
    "test_someone_on_their_way_whose_replan_is_blocked_stands_still": (
        "f1ea36b5ff324391edeaa2eca3a4a65494ea3ea5993a7695b42e29227285dbd1"
    ),
    "test_the_one_ahead_moving_changes_the_follower_s_destination": (
        "79d8fef9bb8afad8067aadc169f257880d1f1c2446aee46aa550aeeae17eaf76"
    ),
    "test_the_random_walks_reach_the_branches": (
        "e645a90957f99fdd442d5bffa517e0dee590fdbfe96a60ec741e5e432ac85363"
    ),
    "test_two_replans_take_their_event_numbers_in_the_order_of_the_walk": (
        "ef386ff9e30219c0d309b514aa9a8069920b6d57ef263795f49c49e4f39a1c05"
    ),
}
