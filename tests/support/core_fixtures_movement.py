"""The worlds of the movement rules' tests, for pytest and the core's tests (Issue #225, lane M9).

`native/core/tests/replan_journeys.rs`, `movement_walk_order.rs` and `movement_rules.rs` call the
core's step of `_replan_journeys` (`rules::tick::update::replan_journeys`), its walk of
`advance_movement` (`rules::movement::advance_through`) and the other rules of `domain/movement.py`
on the worlds that the pytest files of the same names (`integration/test_replan_journeys.py`,
`test_movement_walk_order.py` and `test_movement_rules.py`) built in Python:
the generated Scenario (`support/generated.py`, the Project screen's), edited and set going as
those files did (a trip scheduled through the controller, people put in a queue by the toilet's
service, a temporary room by its EffectProgram), up to the call of the rule. The helpers that build
them are here; the pytest entries that stay use some of them.

Each builder writes one file under `native/core/tests/data/` (`replan_journeys.json`,
`movement_walk_order.json`, `movement_rules.json`): each test's world just before the rule's call
(`export_world`), the first whole and the others as the changes from the one nearest
(`core_texts.Fixture`), and under `inputs`, by the test's name, its world's name and what it needs
besides: the people and minutes it gives the rule, and the values the pytest read or computed in
Python and compared with. A world is exported with the controller the pytest made of it; where the
pytest made none (`advance_movement` and the other rules read no Run), with a controller of the
Scenario's world at minute 0 (`reader`), which `load::loaded` reads.
"""

from __future__ import annotations

import copy
import json
import math
import pathlib
import tempfile
from dataclasses import replace
from typing import Any

from support.core_texts import Fixture
from support.generated import split_queues

KEY = "movement_v1"
OVERLAY = "geometry_overlay_v1"
TOILET = "toilet_01"
ALTERNATE = "route_sleeping_toilet_alt"
FIRST_LEG = "route_sleeping_corridor"
SLOW = 0.2


def generated() -> Any:
    """The `generated_scenario` of `support/generated.py`, made through the Project screen's
    preview as the fixture makes it."""
    from support.generated import preview_scenario

    return preview_scenario(pathlib.Path(tempfile.mkdtemp(prefix="core-fixture-movement-")))


def controller_of(world: Any) -> Any:
    from sheltersim.experiments.metrics import MetricsRecorder
    from sheltersim.experiments.runner import OfflineSimulationController
    from sheltersim.experiments.timeline import _MetricSink

    return OfflineSimulationController(world, _MetricSink(MetricsRecorder()))


def reader(scenario: Any) -> Any:
    """A controller of the Scenario's world at minute 0, for the export of a world the test made
    no controller of: `advance_movement` and the rules of `domain/movement.py` read no Run, and a
    controller made on the test's world would initialize and check it first."""
    from sheltersim.core.world import WorldState

    return controller_of(WorldState.from_scenario(scenario))


def run(scenario: Any, minutes: int) -> tuple[Any, Any]:
    """The world and its controller after `minutes` ticks of `TickEngine`."""
    from sheltersim.core.engine import TickEngine
    from sheltersim.core.world import WorldState

    world = WorldState.from_scenario(scenario)
    controller = controller_of(world)
    engine = TickEngine(world, phase_handler=controller)
    for _ in range(minutes):
        engine.step()
    return world, controller


# ---- The worlds of _replan_journeys --------------------------------------------------------------


def replan_world(scenario: Any, *, slow: bool = True) -> Any:
    """The world of a replan test: the Scenario at minute 1, everyone slow."""
    from sheltersim.core.world import WorldState

    world = WorldState.from_scenario(scenario)
    if slow:
        speeds = world.model_configs["movement_model"]["speed_m_per_minute_by_mobility"]
        for mobility in speeds:
            speeds[mobility] = SLOW
    world.minute_index = 1
    return world


def with_alternate_route(world: Any) -> None:
    """A second sleeping-to-toilet edge that closing the first leg does not block."""
    from sheltersim.domain.models import ZoneEdgeState

    geometry = world.model_configs["facility_geometry"]
    legs = {route["edge_id"]: route["points"] for route in geometry["routes"]}
    world.zone_edges[ALTERNATE] = ZoneEdgeState(
        id=ALTERNATE, from_zone_id="sleeping", to_zone_id="toilet", travel_minutes=20
    )
    geometry["routes"].append(
        {
            "edge_id": ALTERNATE,
            "points": [*legs[FIRST_LEG], *legs["route_corridor_toilet"][1:]],
            "width_m": 1.8,
            "accessible": True,
        }
    )
    for opening in world.model_configs["spatial_constraints"]["openings"]:
        if {FIRST_LEG, "route_corridor_toilet"} & set(opening["edge_ids"]):
            opening["edge_ids"].append(ALTERNATE)


def walk_to_the_toilet(world: Any, agent_id: str = "A001") -> dict[str, Any]:
    controller = controller_of(world)
    route = controller._route(world.agents[agent_id].zone_id, "toilet", agent_id)
    assert route is not None
    assert controller._schedule_travel(
        agent_id=agent_id,
        route=route,
        purpose="toilet_request",
        facility_id=TOILET,
        minute=world.minute_index,
        validate_state=False,
    )
    return world.model_state[KEY]["journeys"][agent_id]


def _in_the_toilet_area(world: Any, agent_id: str = "A001") -> dict[str, Any]:
    """Walking within the room of the toilet, as a trip to the facility is."""
    world.agents[agent_id].zone_id = "toilet"
    routes = world.model_configs["facility_geometry"]["routes"]
    entrance = next(route for route in routes if route["edge_id"] == "route_corridor_toilet")
    position = world.model_state[KEY]["positions"][agent_id]
    position.update(zone_id="toilet", point=dict(entrance["points"][-1]), position_kind="physical")
    controller = controller_of(world)
    room = controller._facility_approach(agent_id, TOILET)
    assert room is not None
    assert controller._schedule_travel(
        agent_id=agent_id,
        route=room[0],
        route_points=room[1],
        purpose="toilet_request",
        facility_id=TOILET,
        minute=world.minute_index,
        validate_state=False,
    )
    return world.model_state[KEY]["journeys"][agent_id]


def _room_with_a_bed(world: Any) -> dict[str, float]:
    """A temporary room in the sleeping area, by the effect a Run applies."""
    from sheltersim.events.effects import (
        EffectProgram,
        EffectProgramGeometryHandler,
        SupportMode,
        TargetSelector,
        TimedAction,
        schedule_effect_program,
    )
    from sheltersim.events.handlers import EventDispatcher
    from sheltersim.events.models import EventStatus

    geometry = world.model_configs["facility_geometry"]
    sleeping = next(zone["polygon"] for zone in geometry["zones"] if zone["zone_id"] == "sleeping")
    left = min(point["x"] for point in sleeping)
    bottom = max(point["y"] for point in sleeping)
    bed = {"x": left + 1.5, "y": bottom - 2.4}
    action = TimedAction(
        effect_id="room",
        op="temporary_room_geometry",
        target_selector=TargetSelector(kind="zone", target_id="sleeping"),
        start_minute=1,
        end_minute=300,
        parameters={
            "schema_version": 1,
            "room_id": "room",
            "zone_id": "sleeping",
            "polygon_m": [
                {"x": left + 0.5, "y": bottom - 4.4},
                {"x": left + 2.5, "y": bottom - 4.4},
                {"x": left + 2.5, "y": bottom - 0.4},
                {"x": left + 0.5, "y": bottom - 0.4},
            ],
            "people_capacity": 2,
            "sleeping_capacity": 1,
            "bed_points_m": [bed],
            "route_edge_ids": ["route_corridor_sleeping", "route_sleeping_corridor"],
            "source_ids": ["ASM-TEMP-ROOM-REPLAN-TEST-V1"],
        },
        units="metre_2d",
        source_id="ASM-TEMP-ROOM-REPLAN-TEST-V1",
        assumption_ids=[],
        policy_dependencies=["geometry_overlay_v1"],
        reversible=True,
    )
    program = EffectProgram(
        program_id="program-room",
        narrative="A room for the test",
        source_text="test source text",
        simulation_start_minute=1,
        effects=[action],
        assumptions=[],
        support_mode=SupportMode.NATIVE,
        compiler_provenance={"compiler_kind": "offline_rule", "compiler_version": "test-v1"},
    )
    schedule_effect_program(world, program)
    dispatcher = EventDispatcher({"effect_program_geometry": EffectProgramGeometryHandler()})
    assert dispatcher.dispatch_due(world, 1)[0].status is EventStatus.APPLIED
    return bed


def _walk_to_a_bed(world: Any) -> dict[str, Any]:
    """A001 on the walk that `sleep` starts."""
    from sheltersim.domain.geometry_overlay import reserve_sleep_bed
    from sheltersim.policies.space import ZoneRoute

    controller = controller_of(world)
    reservation = reserve_sleep_bed(world, "A001")
    assert reservation is not None and reservation["travel_minutes"] > 0
    zone = world.agents["A001"].zone_id
    assert controller._schedule_travel(
        agent_id="A001",
        route=ZoneRoute(zone, zone, (zone, zone), (), reservation["travel_minutes"]),
        route_points=reservation["route_points"],
        purpose="sleep_bed",
        facility_id=None,
        minute=world.minute_index,
        validate_state=False,
    )
    return world.model_state[KEY]["journeys"]["A001"]


def _assigned_support(scenario: Any) -> tuple[Any, str]:
    from sheltersim.domain.care_receipts import append_applied_care_transition
    from sheltersim.domain.population_lifecycle import current_household_id
    from support.receipts import care_source_ids

    world = replan_world(scenario, slow=False)
    world.minute_index = 0
    controller = controller_of(world)
    receipt = controller.staff_service.request("A001", validate_state=False)
    append_applied_care_transition(
        world,
        request_id=receipt.request_id,
        agent_id="A001",
        household_id=current_household_id(world, "A001"),
        support_kind="emotional_support",
        status="requested",
        source_ids=care_source_ids(world, "A001"),
    )
    assert controller.staff_service.assign_pending(0, validate_state=False) == (receipt.request_id,)
    request = world.support_requests[receipt.request_id]
    append_applied_care_transition(
        world,
        request_id=request.id,
        agent_id="A001",
        household_id=current_household_id(world, "A001"),
        support_kind="emotional_support",
        status="assigned",
        staff_id=request.assigned_staff_id,
        causation_id=request.task_id,
        source_ids=care_source_ids(world, "A001"),
    )
    assert world.model_state[KEY]["journeys"]["A001"]["purpose"] == "staff_support_travel"
    world.minute_index = 1
    return world, receipt.request_id


def _alternate_to_the_staff(world: Any) -> None:
    from sheltersim.domain.models import ZoneEdgeState

    geometry = world.model_configs["facility_geometry"]
    legs = {route["edge_id"]: route["points"] for route in geometry["routes"]}
    world.zone_edges["route_sleeping_staff_alt"] = ZoneEdgeState(
        id="route_sleeping_staff_alt", from_zone_id="sleeping", to_zone_id="staff", travel_minutes=5
    )
    geometry["routes"].append(
        {
            "edge_id": "route_sleeping_staff_alt",
            "points": [*legs[FIRST_LEG], *legs["route_corridor_staff"][1:]],
            "width_m": 1.8,
            "accessible": True,
        }
    )
    for opening in world.model_configs["spatial_constraints"]["openings"]:
        if {FIRST_LEG, "route_corridor_staff"} & set(opening["edge_ids"]):
            opening["edge_ids"].append("route_sleeping_staff_alt")


def _replan_worlds(scenario: Any, export: Any) -> None:
    """Each replan test's world just before `_replan_journeys`, and what the test gives it besides
    (the people, in order) and read before the call (`old`)."""

    def add(name: str, world: Any, people: tuple[str, ...] = ("A001",), **more: Any) -> None:
        export(name, world, controller_of(world), people=list(people), **more)

    world = replan_world(scenario)
    with_alternate_route(world)
    old = dict(walk_to_the_toilet(world))
    world.zone_edges[FIRST_LEG].closed = True
    add("a_walk_takes_the_other_route", world, old=old)

    world = replan_world(scenario)
    walk_to_the_toilet(world)
    world.zone_edges[FIRST_LEG].closed = True
    add("a_walk_with_no_route_stops_and_is_counted", world)

    world = replan_world(scenario)
    with_alternate_route(world)
    for agent_id in ("A001", "A002", "A003"):
        world.agents[agent_id].zone_id = "sleeping"
        walk_to_the_toilet(world, agent_id)
    world.zone_edges[FIRST_LEG].closed = True
    add(
        "people_replan_in_the_order_given_and_take_their_event_numbers",
        world,
        ("A003", "A001", "A002"),
    )

    world = replan_world(scenario)
    with_alternate_route(world)
    walk_to_the_toilet(world)
    world.zone_edges[FIRST_LEG].closed = True
    add("someone_with_no_journey_or_gone_is_passed_over", world, ("A002", "nobody", "A001"))

    world = replan_world(scenario)
    with_alternate_route(world)
    walk_to_the_toilet(world)
    world.model_state[KEY]["positions"]["A001"]["zone_id"] = "corridor"
    world.zone_edges[FIRST_LEG].closed = True
    add("a_walk_stopped_in_another_zone_is_where_its_position_says", world)

    world = replan_world(scenario)
    with_alternate_route(world)
    walk_to_the_toilet(world)
    world.zone_edges[FIRST_LEG].closed = True
    world.duration_minutes = 4
    add("the_trip_past_the_end_of_the_run_is_not_made", world)

    world = replan_world(scenario)
    old = dict(_in_the_toilet_area(world))
    world.model_state[KEY]["positions"]["A001"]["point"] = dict(old["route_points"][0])
    world.model_state[KEY]["positions"]["A001"]["point"]["x"] += 0.1
    add("a_room_walk_has_a_new_path", world, old=old)

    world = replan_world(scenario)
    old = dict(_in_the_toilet_area(world))
    world.model_state[KEY]["positions"]["A001"]["point"] = dict(old["route_points"][-1])
    add("a_room_walk_that_is_already_there_is_placed", world, old=old)

    world = replan_world(scenario)
    _in_the_toilet_area(world)
    point = world.model_configs["facility_geometry"]["facility_points"][TOILET]
    world.model_configs["spatial_constraints"]["obstacles"].append(
        {
            "id": "around-the-toilet",
            "polygon": [
                {"x": point["x"] - 0.5, "y": point["y"] - 0.5},
                {"x": point["x"] + 0.5, "y": point["y"] - 0.5},
                {"x": point["x"] + 0.5, "y": point["y"] + 0.5},
                {"x": point["x"] - 0.5, "y": point["y"] + 0.5},
            ],
            "source_ids": ["ASM-TEST"],
        }
    )
    add("a_room_walk_with_no_path_is_counted", world)

    world = replan_world(scenario)
    _in_the_toilet_area(world)
    world.model_state[KEY]["journeys"]["A001"]["route_points"] = []
    del world.model_configs["facility_geometry"]
    world.model_configs.pop("spatial_constraints", None)
    world.model_configs.pop("geometry_overlay", None)
    world.model_state.pop(OVERLAY, None)
    add("without_a_floor_plan_a_room_walk_is_counted", world)

    world = replan_world(scenario)
    _room_with_a_bed(world)
    old = dict(_walk_to_a_bed(world))
    world.model_state[KEY]["positions"]["A001"]["point"]["x"] += 0.2
    add("a_walk_to_a_bed_has_a_new_path", world, old=old)

    world = replan_world(scenario)
    bed = _room_with_a_bed(world)
    _walk_to_a_bed(world)
    world.model_state[KEY]["positions"]["A001"]["point"] = dict(bed)
    add("a_walk_to_a_bed_that_arrived_sleeps_there", world)

    world = replan_world(scenario)
    _room_with_a_bed(world)
    _walk_to_a_bed(world)
    world.duration_minutes = 3
    add("a_walk_to_a_bed_past_the_end_of_the_run_gives_it_up", world)

    world = replan_world(scenario)
    _room_with_a_bed(world)
    _walk_to_a_bed(world)
    overlay = world.model_state[OVERLAY]
    overlay["bed_reservations"].clear()
    overlay["temporary_rooms"].clear()
    add("a_walk_to_a_room_that_went_gives_it_up", world)

    world, request_id = _assigned_support(scenario)
    _alternate_to_the_staff(world)
    world.zone_edges[FIRST_LEG].closed = True
    add("support_with_another_route_is_rescheduled", world, request=request_id)

    world, request_id = _assigned_support(scenario)
    world.model_state[KEY]["positions"]["A001"]["zone_id"] = "staff"
    add("support_where_the_staff_already_are_starts_there", world, request=request_id)

    world, request_id = _assigned_support(scenario)
    world.zone_edges[FIRST_LEG].closed = True
    add("support_with_no_route_goes_back_to_the_queue", world, request=request_id)

    world, request_id = _assigned_support(scenario)
    world.zone_edges[FIRST_LEG].closed = True
    world.support_requests[request_id].timeout_minute = 1
    add("support_past_its_timeout_is_left_unresolved", world, request=request_id)

    world, request_id = _assigned_support(scenario)
    _alternate_to_the_staff(world)
    world.zone_edges[FIRST_LEG].closed = True
    world.duration_minutes = 3
    add("support_past_the_end_of_the_run_goes_back_to_the_queue", world, request=request_id)

    world, request_id = _assigned_support(scenario)
    del world.active_tasks[world.support_requests[request_id].task_id]
    add("support_that_lost_its_task_is_python_s_error", world)

    world, request_id = _assigned_support(scenario)
    world.active_tasks[world.support_requests[request_id].task_id].due_minute += 1
    add("support_with_a_moved_completion_is_python_s_error", world)

    world = replan_world(scenario)
    with_alternate_route(world)
    old = dict(walk_to_the_toilet(world))
    world.event_queue.remove(old["event_id"])
    world.zone_edges[FIRST_LEG].closed = True
    add("a_walk_whose_event_is_gone_stops_without_a_record", world, old=old)

    world = replan_world(scenario)
    controller = controller_of(world)
    start = world.agents["A001"].zone_id
    route = controller._route(start, "toilet", "A001")
    assert controller._schedule_travel(
        agent_id="A001",
        route=route,
        purpose="return",
        facility_id=None,
        minute=world.minute_index,
        validate_state=False,
    )
    world.model_state[KEY]["journeys"]["A001"]["destination_zone_id"] = start
    add("a_walk_to_its_own_zone_has_no_route_and_is_counted", world)

    world = replan_world(scenario)
    old = dict(_in_the_toilet_area(world))
    world.model_state[KEY]["positions"]["A001"]["point"] = dict(old["route_points"][0])
    world.model_state[KEY]["positions"]["A001"]["point"]["x"] += 0.1
    world.duration_minutes = 2
    add("a_room_walk_past_the_end_of_the_run_is_not_made_nor_blocked", world)

    world, request_id = _assigned_support(scenario)
    _alternate_to_the_staff(world)
    world.zone_edges[FIRST_LEG].closed = True
    world.zones["staff"].capacity = 0
    add("support_whose_walk_a_full_zone_refuses_goes_back_to_the_queue", world, request=request_id)

    world = replan_world(scenario)
    with_alternate_route(world)
    walk_to_the_toilet(world)
    world.zone_edges[FIRST_LEG].closed = True
    world.zone_edges[ALTERNATE].max_agents_in_transit = 5
    add("edge_capacities_are_counted_on_the_new_route", world)


# ---- The worlds of advance_movement's walk order ------------------------------------------------


def _stand_in_the_toilet_area(world: Any, agent_ids: tuple[str, ...]) -> None:
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


def _queues_of_the_toilet(world: Any) -> list[str]:
    return sorted(
        queue_id
        for queue_id, row in world.model_state["facility_queues_v1"]["queues"].items()
        if row["facility_id"] == TOILET
    )


def _toilet_service(world: Any) -> Any:
    from sheltersim.domain.facilities import ToiletServiceManager

    return ToiletServiceManager(world, min_service_minutes=2, max_service_minutes=2)


def two_trips_in_two_queues(scenario: Any) -> Any:
    """A001 and A002 on queue trips to the toilet, each in a queue of their own."""
    from sheltersim.core.rng import CounterRNG
    from sheltersim.core.world import WorldState

    world = WorldState.from_scenario(scenario)
    _stand_in_the_toilet_area(world, ("A001", "A002"))
    first, second, *_ = _queues_of_the_toilet(world)
    service = _toilet_service(world)
    service.request("A001", TOILET, queue_id=first)
    service.request("A002", TOILET, queue_id=second)
    service.arbitrate(0, CounterRNG(world.seed))
    assert set(world.model_state[KEY]["journeys"]) == {"A001", "A002"}
    world.minute_index = 1
    return world


def _head_and_follower_in_one_queue(scenario: Any) -> tuple[Any, dict[str, float]]:
    """The head walks to the service point and the follower to its slot; the head stands on it."""
    from sheltersim.core.rng import CounterRNG
    from sheltersim.core.world import WorldState
    from sheltersim.domain import movement

    world = WorldState.from_scenario(scenario)
    _stand_in_the_toilet_area(world, ("A001", "A002"))
    queue_id = _queues_of_the_toilet(world)[0]
    service = _toilet_service(world)
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


def trips_of_the_facility_s_own_queue(scenario: Any, people: tuple[str, ...]) -> Any:
    """People in a queue without independent FIFOs, as arbitration leaves them."""
    from sheltersim.core.rng import CounterRNG
    from sheltersim.core.world import WorldState

    world = WorldState.from_scenario(_without_independent_queues(scenario))
    assert "facility_queues_v1" not in world.model_state
    _stand_in_the_toilet_area(world, people)
    service = _toilet_service(world)
    for agent_id in people:
        service.request(agent_id, TOILET)
    service.arbitrate(0, CounterRNG(world.seed))
    assert world.model_state[KEY]["journeys"]
    world.minute_index = 1
    return world


def move_the_toilet(world: Any, point: dict[str, float] | None = None) -> None:
    """What an equipment move leaves: a trip planned before it must replan."""
    where = point or world.model_configs["facility_geometry"]["facility_points"][TOILET]
    world.model_state[OVERLAY]["facility_points"][TOILET] = {
        "point_m": dict(where),
        "minute": 5,
        "source_ids": [],
        "event_id": "moved",
    }


def _obstacle_on_the_walk(world: Any, agent_id: str) -> None:
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


def add_task_and_event(
    world: Any, agent_id: str, task_id: str, event_id: str, due: int, kind: str = "travel"
) -> None:
    """A task of this person and its pending completion."""
    from sheltersim.domain.models import ActiveTask
    from sheltersim.events.models import EventKind, ScheduledEvent

    world.active_tasks[task_id] = ActiveTask(
        id=task_id, kind=kind, agent_id=agent_id, resource_id=None, start_minute=0, due_minute=due
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


def _queue_trip(world: Any, agent_id: str, due: int = 300) -> None:
    """A walk of this person, by hand, with its event pending and their queue's task."""
    state = world.model_state[KEY]
    task_id = next(
        key
        for key, task in world.active_tasks.items()
        if task.agent_id == agent_id and task.kind == "toilet_queue"
    )
    event_id = f"trip-of-{agent_id}"
    if world.event_queue.find(event_id) is None:
        add_task_and_event(world, agent_id, f"holder-{agent_id}", event_id, due, "travel_return")
        del world.active_tasks[f"holder-{agent_id}"]
    start = state["positions"][agent_id]["point"]
    target = world.model_configs["facility_geometry"]["facility_points"][TOILET]
    state["journeys"].pop(agent_id, None)
    state["journeys"][agent_id] = {
        "event_id": event_id,
        "task_id": task_id,
        "purpose": "toilet_queue_position",
        "facility_id": TOILET,
        "destination_zone_id": "toilet",
        "route_zone_ids": ["toilet", "toilet"],
        "route_edge_ids": [],
        "route_points": [dict(start), dict(target)],
        "started_minute": 0,
        "due_minute": due,
    }


def _own_queue_with_trips(scenario: Any, people: tuple[str, ...]) -> Any:
    """Everyone in the facility's own queue on a queue trip that must replan once it has moved."""
    world = trips_of_the_facility_s_own_queue(scenario, people)
    world.model_state[KEY]["journeys"].clear()
    for key in [event.event_id for event in world.event_queue.pending()]:
        if key.startswith("physical-queue-trip-"):
            world.event_queue.remove(key)
    for agent_id in people:
        _queue_trip(world, agent_id)
    return world


def toilet_area(world: Any) -> Any:
    from sheltersim.domain import movement

    return next(item for item in movement._geometry(world).zones if item.zone_id == "toilet")


def _walk_order_worlds(scenario: Any, export: Any) -> None:
    """Each walk order test's world just before its first `advance_movement`, and what it read."""
    from sheltersim.domain.models import Activity

    split = split_queues(scenario)
    own = _without_independent_queues(scenario)
    readers = {"split": reader(split), "own": reader(own)}

    def add(name: str, world: Any, **more: Any) -> None:
        which = "own" if "facility_queues_v1" not in world.model_state else "split"
        export(name, world, readers[which], **more)

    world = two_trips_in_two_queues(split)
    old = {key: value["event_id"] for key, value in world.model_state[KEY]["journeys"].items()}
    move_the_toilet(world)
    add("two_replans_take_their_event_numbers_in_the_order_of_the_walk", world, old=old)

    world = two_trips_in_two_queues(split)
    _obstacle_on_the_walk(world, "A002")
    add("a_replan_for_a_wall_on_the_walk", world)

    world = two_trips_in_two_queues(split)
    world.facilities[TOILET].zone_id = "corridor"
    add("a_replan_for_a_facility_that_changed_zone", world)

    world = _own_queue_with_trips(scenario, ("A001", "A002"))
    move_the_toilet(world, {"x": -1000.0, "y": -1000.0})
    add("a_replan_with_no_room_route_is_blocked_and_counted", world)

    world = two_trips_in_two_queues(split)
    _obstacle_on_the_walk(world, "A001")
    _obstacle_on_the_walk(world, "A002")
    world.duration_minutes = 3
    add("a_replan_past_the_end_of_the_run_is_blocked_in_its_schedule", world)

    world = _own_queue_with_trips(scenario, ("A001", "A002"))
    state = world.model_state[KEY]
    for agent_id in ("A001", "A002"):
        state["journeys"][agent_id]["purpose"] = "toilet_queue_service"
    move_the_toilet(world, dict(state["positions"]["A001"]["point"]))
    add("a_replan_to_where_the_person_stands_takes_no_trip", world)

    world = _own_queue_with_trips(scenario, ("A001", "A002"))
    world.agents["A001"].activity = Activity.MOVING
    world.agents["A001"].target_id = "elsewhere"
    move_the_toilet(world)
    add("someone_on_their_way_on_a_queue_trip_is_queued_by_the_replan", world)

    world = _own_queue_with_trips(scenario, ("A001", "A002"))
    world.agents["A001"].activity = Activity.MOVING
    move_the_toilet(world, {"x": -1000.0, "y": -1000.0})
    add("someone_on_their_way_whose_replan_is_blocked_stands_still", world)

    world = _own_queue_with_trips(scenario, ("A001",))
    world.model_state[KEY]["journeys"]["A001"]["facility_id"] = "another_facility"
    # The move is of the name on the journey, which is no facility of the world.
    world.model_state[OVERLAY]["facility_points"]["another_facility"] = {
        "point_m": {"x": 0.0, "y": 0.0},
        "minute": 5,
        "source_ids": [],
        "event_id": "moved",
    }
    add("a_replan_when_the_facility_is_gone_only_ends_the_trip", world)

    world = _own_queue_with_trips(scenario, ("A001", "A002"))
    move_the_toilet(world)
    add("a_replan_of_the_facility_s_own_queue", world)

    for index, people in enumerate((("A001", "A002"), ("A002", "A001"))):
        world = _own_queue_with_trips(scenario, people)
        state = world.model_state[KEY]
        head, follower = people
        state["journeys"][head]["purpose"] = "toilet_queue_service"
        _obstacle_on_the_walk(world, follower)
        state["positions"][head]["progress"] = 0.9
        state["positions"][follower]["progress"] = 0.0
        add(f"a_slot_read_with_the_one_ahead_before_or_after_their_walk[people{index}]", world)

    world, slot = _head_and_follower_in_one_queue(split)
    world.model_state[OVERLAY]["facility_points"][TOILET] = {
        "point_m": dict(world.model_configs["facility_geometry"]["facility_points"][TOILET]),
        "minute": 5,
        "source_ids": [],
        "event_id": "moved",
    }
    add("the_one_ahead_moving_changes_the_follower_s_destination", world, slot=slot)

    world = _own_queue_with_trips(scenario, ("A001", "A002"))
    state = world.model_state[KEY]
    move_the_toilet(world)
    assert world.agents["A002"].activity is Activity.QUEUED
    state["journeys"]["A002"]["purpose"] = "return"
    state["journeys"]["A002"]["facility_id"] = None
    state["journeys"]["A002"]["started_minute"] = 0
    add("a_replan_seen_by_the_slot_of_someone_later_in_the_walk", world)

    for index, order in enumerate((("A001", "A002", "A003"), ("A003", "A002", "A001"))):
        world = _own_queue_with_trips(scenario, order)
        state = world.model_state[KEY]
        base = state["positions"][order[0]]["point"]
        for offset, agent_id in enumerate(order):
            state["positions"][agent_id]["point"] = {"x": base["x"], "y": base["y"] + offset * 0.1}
            state["journeys"][agent_id]["task_id"] = f"gone-{agent_id}"
        add(f"people_placed_in_slots_read_who_has_walked[queue_order{index}]", world)

    world = _own_queue_with_trips(scenario, ("A001", "A002"))
    world.event_queue.remove(world.model_state[KEY]["journeys"]["A001"]["event_id"])
    add("a_queued_person_past_their_trip_keeps_their_arrival", world)

    world = two_trips_in_two_queues(split)
    move_the_toilet(world)
    add("one_person_replans_in_two_ticks_running", world)

    world = two_trips_in_two_queues(split)
    state = world.model_state[KEY]
    world.zone_edges["route_sleeping_corridor"].closed = True
    add_task_and_event(world, "A003", "walk-3", "walk-event-3", 40, "travel_return")
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
    move_the_toilet(world)
    add("a_queue_replan_and_a_walker_who_must_replan_in_one_walk", world)

    world = _own_queue_with_trips(scenario, ("A001", "A002"))
    task_id = world.model_state[KEY]["journeys"]["A001"]["task_id"]
    world.active_tasks[task_id].resource_id = "another"
    move_the_toilet(world)
    add("a_trip_with_no_queue_task_is_python_s_error_before_any_change", world)


# ---- The worlds of the other movement rules ------------------------------------------------------


def walker(world: Any) -> str:
    """A person put on a two-edge walk that the queue and task of the world accept."""
    from sheltersim.domain.models import Activity

    state = world.model_state[KEY]
    assert world.zone_edges
    edge = next(iter(world.zone_edges.values()))
    follow = next(
        (item for item in world.zone_edges.values() if item.from_zone_id == edge.to_zone_id),
        edge,
    )
    agent_id = sorted(world.agents)[0]
    agent = world.agents[agent_id]
    agent.activity = Activity.MOVING
    agent.zone_id = edge.from_zone_id
    world.minute_index = max(world.minute_index, 10)
    task_id, event_id = "task-walk", "event-walk"
    add_task_and_event(world, agent_id, task_id, event_id, world.minute_index + 40)
    state["journeys"][agent_id] = {
        "event_id": event_id,
        "task_id": task_id,
        "purpose": "walk",
        "facility_id": None,
        "destination_zone_id": follow.to_zone_id,
        "route_zone_ids": [edge.from_zone_id, follow.from_zone_id, follow.to_zone_id],
        "route_edge_ids": [edge.id, follow.id],
        "route_points": [],
        "started_minute": world.minute_index - 3,
        "due_minute": world.minute_index + 7,
    }
    return agent_id


def _point(point: Any) -> dict[str, float] | None:
    return None if point is None else {"x": point.x, "y": point.y}


def _record(
    zone_id: str, point: Any, kind: str, edge_id: Any = None, progress: float = 0.0
) -> dict[str, Any]:
    return {
        "zone_id": zone_id,
        "point": _point(point),
        "edge_id": edge_id,
        "progress": progress,
        "position_kind": kind,
    }


def _route_point(
    geometry: Any, edge_id: str, progress: float, current_zone_id: str, world: Any
) -> Any:
    """The deleted rule's point along an edge's surveyed route, from the zone the walk left."""
    from sheltersim.domain import movement

    if geometry is None:
        return None
    route = next((item for item in geometry.routes if item.edge_id == edge_id), None)
    if route is None:
        return movement._zone_center(geometry, current_zone_id)
    edge = world.zone_edges[edge_id]
    points = list(route.points)
    if edge.from_zone_id != current_zone_id:
        points.reverse()
    lengths = [
        math.hypot(second.x - first.x, second.y - first.y)
        for first, second in zip(points, points[1:], strict=False)
    ]
    total = sum(lengths)
    if total <= 0:
        return points[0]
    remaining = min(1.0, max(0.0, progress)) * total
    for index, (first, second, length) in enumerate(zip(points, points[1:], lengths, strict=False)):
        if remaining <= length or index == len(lengths) - 1:
            ratio = 0.0 if length == 0 else min(1.0, max(0.0, remaining / length))
            return type(first)(
                x=first.x + (second.x - first.x) * ratio, y=first.y + (second.y - first.y) * ratio
            )
        remaining -= length
    return points[-1]


# The kinds of journey `start_movement` is given, and the minutes it starts from the last update.
START_KINDS = ("edges", "points", "nothing", "support", "queue")
START_OFFSETS = (0, 1, -1)
START_POINTS = ({"x": 1.0, "y": 2.0}, {"x": 4.0, "y": 6.0}, {"x": 4.0, "y": 9.5})


def _starts(world: Any) -> list[dict[str, Any]]:
    """What `start_movement` writes of each kind of journey, as the pytest computed it."""
    from sheltersim.contracts.extensions import Point2D
    from sheltersim.domain import movement

    state = world.model_state[KEY]
    agent_id = sorted(world.agents)[2]
    edge = next(iter(world.zone_edges.values()))
    geometry = movement._geometry(world)
    out = []
    for kind in START_KINDS:
        for offset in START_OFFSETS:
            started = state["last_updated_minute"] + offset
            edge_ids = [edge.id] if kind in {"edges", "support"} else []
            journey = {
                "event_id": "e",
                "task_id": "t",
                "purpose": "walk",
                "facility_id": None,
                "destination_zone_id": edge.to_zone_id,
                "route_zone_ids": [edge.from_zone_id, edge.to_zone_id],
                "route_edge_ids": edge_ids,
                "route_points": list(START_POINTS) if kind == "points" else [],
                "started_minute": started,
                "due_minute": started + 7,
                **({"queue_id": "queue"} if kind == "queue" else {}),
                **(
                    {"support_request_id": "request", "support_staff_id": "staff"}
                    if kind == "support"
                    else {}
                ),
            }
            # One minute along the route when movement was already advanced at its start.
            elapsed = 1 if offset <= 0 else 0
            progress = elapsed / 7
            origin = edge.from_zone_id
            if kind == "points":
                if elapsed:
                    # 1/7 of the 8.5 m polyline lies on its first 5 m segment, (1, 2) to (4, 6).
                    ratio = math.fsum([5.0, 3.5]) * progress / 5.0
                    point = Point2D(x=1.0 + 3.0 * ratio, y=2.0 + 4.0 * ratio)
                else:
                    point = Point2D(x=1.0, y=2.0)
                position = _record(origin, point, "physical", progress=progress)
            elif edge_ids:
                point = _route_point(geometry, edge.id, progress, origin, world)
                kind_of = "physical" if point is not None else "unavailable"
                position = _record(origin, point, kind_of, edge.id, progress)
            else:
                # No route to stand on: the centre of the zone, though not a place on a walk.
                point = movement._zone_center(geometry, origin)
                position = _record(origin, point, "unavailable", progress=progress)
            out.append(
                {
                    "kind": kind,
                    "offset": offset,
                    "agent": agent_id,
                    "origin": origin,
                    "journey": journey,
                    "position": position,
                }
            )
    return out


def routes_of(world: Any) -> list[Any]:
    from sheltersim.policies.space import ZoneRoute

    edges = list(world.zone_edges.values())
    routes = [
        ZoneRoute(
            edge.from_zone_id,
            edge.to_zone_id,
            (edge.from_zone_id, edge.to_zone_id),
            (edge.id,),
            edge.travel_minutes,
        )
        for edge in edges
    ]
    # A long one, since a duration is rounded up to whole minutes: 1% shows only at this length.
    routes.append(replace(routes[0], travel_minutes=997))
    routes.append(
        ZoneRoute(edges[0].from_zone_id, edges[0].from_zone_id, (edges[0].from_zone_id,), (), 0)
    )
    return routes


def deleted_minutes(world: Any, agent_id: str, route: Any) -> int:
    """The deleted rule: the longer of the declared and the surveyed walk, at the person's speed,
    slowed by the most crowded zone of the route."""
    from sheltersim.domain import movement

    if not route.edge_ids:
        return route.travel_minutes
    config = movement.canonical_movement_config(world.model_configs.get("movement_model"))
    speeds = config["speed_m_per_minute_by_mobility"]
    geometry = movement._geometry(world)
    surveyed = {item.edge_id: item.points for item in geometry.routes}
    distance = math.fsum(
        math.fsum(movement._distance(a, b) for a, b in zip(points, points[1:], strict=False))
        for points in (surveyed[edge_id] for edge_id in route.edge_ids if edge_id in surveyed)
    )
    physical = distance / speeds["independent"] if distance > 0 else float(route.travel_minutes)
    density = movement._max_route_density(world, route.zone_ids, geometry)
    crowding = min(
        config["maximum_density_slowdown"],
        1.0 + config["density_slowdown_per_person_m2"] * max(0.0, density - 0.5),
    )
    mobility = world.agents[agent_id].profile.mobility
    duration = (
        max(route.travel_minutes, physical) * (speeds["independent"] / speeds[mobility]) * crowding
    )
    return max(1, math.ceil(duration))


# Areas that leave a route uncrowded, crowded, and at the most crowding counts for.
ROUTE_AREAS = (None, 5.0, 0.2)
ROUTE_MODELS: tuple[dict[str, Any] | None, ...] = (
    None,
    {
        "speed_m_per_minute_by_mobility": {
            "independent": 60.5,
            "assisted": 21.25,
            "wheelchair": 33.0,
        },
        "density_slowdown_per_person_m2": 0.3,
        "maximum_density_slowdown": 1.75,
    },
    # A Scenario may give whole numbers: the rule computed with them as floats.
    {
        "speed_m_per_minute_by_mobility": {"independent": 60, "assisted": 21, "wheelchair": 33},
        "density_slowdown_per_person_m2": 1,
        "maximum_density_slowdown": 2,
    },
)


def _route_minutes(scenario: Any, model: Any, area: Any) -> tuple[Any, Any, list[Any]]:
    """The world of `test_a_route_s_minutes_follow_speed_and_crowding` and the minutes of the
    deleted rule for each person and route: `[agent, zones, edges, declared, minutes]`."""
    from sheltersim.domain.models import Mobility

    world, controller = run(scenario, 5)
    for zone in world.model_configs["facility_geometry"]["zones"] if area else ():
        zone["usable_area_m2"] = area
    for agent in world.agents.values() if area else ():
        agent.zone_id = next(iter(world.zone_edges.values())).from_zone_id
    if model is None:
        world.model_configs.pop("movement_model", None)
    else:
        world.model_configs["movement_model"] = copy.deepcopy(model)
    mobilities = [Mobility.INDEPENDENT, Mobility.ASSISTED, Mobility.WHEELCHAIR]
    for index, agent_id in enumerate(sorted(world.agents)[:3]):
        agent = world.agents[agent_id]
        agent.profile = replace(agent.profile, mobility=mobilities[index])
    expected = [
        [agent_id, list(route.zone_ids), list(route.edge_ids), route.travel_minutes]
        + [deleted_minutes(world, agent_id, route)]
        for agent_id in sorted(world.agents)[:3]
        for route in routes_of(world)
    ]
    return world, controller, expected


def _movement_rule_worlds(scenario: Any, export: Any) -> None:
    """Each test's world of `test_movement_rules.py` that moved, and what it read in Python."""
    from sheltersim.core.world import WorldState
    from sheltersim.domain import movement
    from sheltersim.domain.models import Activity

    def add(name: str, world: Any, controller: Any = None, **more: Any) -> dict[str, Any]:
        return export(name, world, controller or controller_of(world), **more)

    world, controller = run(scenario, 5)
    agent_id = walker(world)
    add("a_walk_along_two_edges_both_ways", world, controller, agent=agent_id)

    world, controller = run(scenario, 5)
    agent_id = sorted(world.agents)[3]
    del world.model_state[KEY]["positions"][agent_id]
    add("someone_with_no_position_gets_one_first", world, controller, agent=agent_id)

    world, controller = run(scenario, 5)
    ids = [
        agent for agent in sorted(world.agents) if agent not in world.model_state[KEY]["journeys"]
    ]
    facility_id = sorted(world.facilities)[0]
    world.agents[ids[0]].activity = Activity.IN_SERVICE
    world.agents[ids[0]].target_id = facility_id
    world.agents[ids[1]].activity = Activity.IN_SERVICE
    world.agents[ids[1]].target_id = "nowhere"
    for index in (2, 3, 4, 5):
        world.agents[ids[index]].activity = Activity.SLEEPING
    world.agents[ids[6]].activity = Activity.QUEUED
    world.agents[ids[6]].target_id = None
    overlay = world.model_state[OVERLAY]
    overlay["bed_reservations"] = {
        ids[2]: {"room_id": "room", "bed_index": 1},
        ids[3]: {"room_id": "room", "bed_index": 7},
        ids[4]: {"room_id": "no-room", "bed_index": 0},
        ids[5]: {"room_id": "room", "bed_index": True},
    }
    overlay["temporary_rooms"] = {
        "room": {"bed_points_m": [{"x": 1.5, "y": 2.5}, {"x": 3.25, "y": 4.125}]},
    }
    world.model_state[KEY]["last_updated_minute"] = world.minute_index - 1
    add("in_service_and_sleeping_stand_at_their_places", world, controller, sleeper=ids[2])

    world = WorldState.from_scenario(scenario)
    del world.model_configs["facility_geometry"]
    world.model_state.pop(OVERLAY, None)
    world.model_configs.pop("spatial_constraints", None)
    for index, zone_id in enumerate(sorted(world.zones)):
        world.zones[zone_id].display_x = None if index == 0 else float(index) + 0.5
        world.zones[zone_id].display_y = None if index == 0 else float(index) + 0.25
    world.model_state[KEY]["last_updated_minute"] = -1
    add("without_a_floor_plan_people_have_display_points", world, reader(scenario))

    world, controller = run(scenario, 5)
    agent_id = walker(world)
    state = world.model_state[KEY]
    journey = state["journeys"][agent_id]
    zone = world.agents[agent_id].zone_id
    geometry = movement._geometry(world)
    polygon = next(item for item in geometry.zones if item.zone_id == zone).polygon
    center = movement._zone_center(geometry, zone)
    corner = polygon[0]
    near = {"x": corner.x * 0.1 + center.x * 0.9, "y": corner.y * 0.1 + center.y * 0.9}
    journey["route_edge_ids"] = []
    journey["route_zone_ids"] = [zone, zone]
    journey["destination_zone_id"] = zone
    journey["route_points"] = [
        {"x": center.x, "y": center.y},
        near,
        {"x": center.x, "y": near["y"]},
    ]
    state["positions"][agent_id]["point"] = {"x": center.x, "y": center.y}
    world.model_configs["spatial_constraints"] = {}
    add(
        "a_room_walk_follows_its_points_and_stops_at_what_blocks_them",
        world,
        controller,
        agent=agent_id,
    )
    # The obstacle the test puts on the walk afterwards: the core's test takes its sections.
    world.model_configs["spatial_constraints"] = {
        "obstacles": [
            {
                "id": "block",
                "polygon": [
                    {"x": near["x"] - 0.01, "y": near["y"] - 0.01},
                    {"x": near["x"] + 0.01, "y": near["y"] - 0.01},
                    {"x": near["x"] + 0.01, "y": near["y"] + 0.01},
                    {"x": near["x"] - 0.01, "y": near["y"] + 0.01},
                ],
            }
        ]
    }
    add("a_room_walk_follows_its_points_and_stops_at_what_blocks_them/blocked", world, controller)

    world, controller = run(scenario, 5)
    facility = next(
        item
        for item in world.facilities.values()
        if item.id in movement._geometry(world).facility_points
    )
    ids = sorted(world.agents)[:4]
    state = world.model_state[KEY]
    for agent_id in ids:
        world.agents[agent_id].activity = Activity.QUEUED
        world.agents[agent_id].target_id = facility.id
        world.agents[agent_id].zone_id = facility.zone_id
        facility.queue_agent_ids.append(agent_id)
    finishing = add("finishing_one_trip_may_read_the_slots", world, controller)
    geometry = movement._geometry(world)
    placed = 0
    expected = []
    for agent_id in ids:
        journey = state["journeys"].get(agent_id)
        if journey is not None and journey["purpose"] in movement._QUEUE_TRIP_PURPOSES:
            # The end of a queue trip keeps the point the person walked to.
            position = {**state["positions"][agent_id], "edge_id": None, "progress": 0.0}
            position["position_kind"] = "physical"
        else:
            # Otherwise a queued person stands in the slot behind those already placed.
            slot = movement._queue_slot_position(world, geometry, agent_id, facility.id)
            point = slot or movement._zone_center(geometry, facility.zone_id)
            position = _record(facility.zone_id, point, "physical")
            placed += slot is not None
        movement.finish_movement(world, agent_id)
        assert agent_id not in state["journeys"]
        assert state["positions"][agent_id] == position
        expected.append([agent_id, json.loads(json.dumps(position))])
    assert placed
    finishing["expected"] = expected

    world, controller = run(scenario, 5)
    add(
        "start_movement_writes_the_journey_and_the_first_position",
        world,
        controller,
        starts=_starts(world),
    )

    world, controller = run(scenario, 5)
    agent_id = walker(world)
    state = world.model_state[KEY]
    state["journeys"][agent_id]["purpose"] = "toilet_queue_position"
    state["positions"][agent_id].update(edge_id=next(iter(world.zone_edges)), progress=0.5)
    center = movement._zone_center(movement._geometry(world), world.agents[agent_id].zone_id)
    add(
        "finish_movement_of_each_kind",
        world,
        controller,
        agent=agent_id,
        center=_point(center),
    )

    for model_index, model in enumerate(ROUTE_MODELS):
        for area_index, area in enumerate(ROUTE_AREAS):
            world, controller, expected = _route_minutes(scenario, model, area)
            add(
                f"a_route_s_minutes_follow_speed_and_crowding[model{model_index}-area{area_index}]",
                world,
                controller,
                minutes=expected,
            )

    world, controller = run(scenario, 5)
    _footprints(world, controller, add)


def _footprints(world: Any, controller: Any, add: Any) -> None:
    """The world of each facility's footprint in the walk test: the footprint around its point in
    the spatial constraints, and three people of each mobility standing at the zone's centre."""
    from sheltersim.domain import movement
    from sheltersim.domain.models import Mobility

    geometry = movement._geometry(world)
    configs = world.model_configs
    ids = sorted(world.agents)[:3]
    mobilities = (Mobility.INDEPENDENT, Mobility.ASSISTED, Mobility.WHEELCHAIR)
    for facility_id, point in sorted(geometry.facility_points.items()):
        zone_id = world.facilities[facility_id].zone_id
        center = movement._zone_center(geometry, zone_id)
        half = 0.2
        polygon = [
            {"x": point.x + dx, "y": point.y + dy}
            for dx, dy in ((-half, -half), (half, -half), (half, half), (-half, half))
        ]
        configs["spatial_constraints"] = {
            "equipment_footprints": [
                {"facility_id": facility_id, "polygon": polygon, "area_m2": 0.16}
            ]
        }
        for agent_id, mobility in zip(ids, mobilities, strict=True):
            agent = world.agents[agent_id]
            agent.profile = replace(agent.profile, mobility=mobility)
            agent.zone_id = zone_id
            world.model_state[KEY]["positions"][agent_id]["point"] = {"x": center.x, "y": center.y}
        name = f"the_walk_to_a_facility_with_a_footprint_is_the_deleted_rule_s/{facility_id}"
        add(name, world, controller, agents=ids, facility=facility_id)


# ---- The file -----------------------------------------------------------------------------------


def _file(build: Any) -> dict[str, Any]:
    """A fixture file of the worlds `build` exports from the generated Scenario."""
    fixture = Fixture()
    inputs: dict[str, Any] = {}

    def export(name: str, world: Any, controller: Any, **more: Any) -> dict[str, Any]:
        world_name = fixture.world(name, world, controller, full=not fixture.entries)
        inputs[name] = {"world": world_name, **more}
        return inputs[name]

    build(generated(), export)
    return json.loads(json.dumps({"worlds": fixture.worlds, "inputs": inputs}))


def replan_journeys_fixture() -> dict[str, Any]:
    """`native/core/tests/data/replan_journeys.json`."""
    return _file(_replan_worlds)


def movement_walk_order_fixture() -> dict[str, Any]:
    """`native/core/tests/data/movement_walk_order.json`."""
    return _file(_walk_order_worlds)


def movement_rules_fixture() -> dict[str, Any]:
    """`native/core/tests/data/movement_rules.json`."""
    return _file(_movement_rule_worlds)
