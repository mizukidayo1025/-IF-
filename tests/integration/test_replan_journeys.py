"""`_replan_journeys`, native/src/rules/replan.rs, which has no Python step left (lane L10).

A replan is rare: one call in the gate and none in the 7-day run, so these tests are what checks
most of its branches. Each runs the step and compares the whole world, its counters, metrics,
pending events, log and journeys with what the Python step made on the same world: their digest,
taken once from that step (45a9bc9) before it went, and pinned on Linux, as the gate's hashes are.
"""

from __future__ import annotations

import hashlib
import sys
from typing import Any

import pytest

from sheltersim import native
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.world import WorldState
from sheltersim.domain.care_receipts import (
    append_applied_care_transition,
    care_source_ids_for_agent,
)
from sheltersim.domain.geometry_overlay import GEOMETRY_OVERLAY_KEY, reserve_sleep_bed
from sheltersim.domain.models import Activity, SupportRequestStatus, ZoneEdgeState
from sheltersim.domain.movement import MOVEMENT_STATE_KEY as KEY
from sheltersim.domain.population_lifecycle import current_household_id
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
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.timeline import _MetricSink

ALTERNATE = "route_sleeping_toilet_alt"
FIRST_LEG = "route_sleeping_corridor"
SLOW = 0.2


def _controller(world: WorldState) -> OfflineSimulationController:
    return OfflineSimulationController(world, _MetricSink(MetricsRecorder()))


def _bytes(world: WorldState) -> str:
    return world._canonical_text(world._canonical_data())


def _records(world: WorldState) -> list[Any]:
    return [
        (
            record.event.event_id,
            record.event.sequence,
            record.event.causation_id,
            record.realized_minute,
            record.status.value,
            record.detail,
            sorted(record.event.payload.items(), key=lambda item: item[0]),
        )
        for record in world.event_log.records
    ]


def _pending(world: WorldState) -> list[Any]:
    return [
        (
            event.event_id,
            event.sequence,
            event.due_minute,
            event.source_id,
            event.causation_id,
            sorted(event.payload.items(), key=lambda item: item[0]),
            sorted(event.provenance.items(), key=lambda item: item[0]),
        )
        for event in world.event_queue.pending()
    ]


def _is_python_s(world: WorldState, digest: str) -> None:
    """What the Python step made of this world: its digest, taken from that step."""
    data = [
        _bytes(world),
        sorted(world.id_counters.items()),
        sorted(world.metric_state.items()),
        _pending(world),
        _records(world),
        list(world.model_state[KEY]["journeys"]),
    ]
    if sys.platform == "linux":
        assert hashlib.sha256(repr(data).encode("utf-8")).hexdigest()[:16] == digest
    mirror = world._core.mirror(KEY)
    assert mirror.tainted is None
    assert mirror.verify()
    assert mirror.row_json() == native.canonical_json(world.model_state[KEY])


def _replanned(world: WorldState, agent_ids: tuple[str, ...], minute: int, digest: str) -> None:
    """The step on the world, which must then be what the Python step made."""
    controller = _controller(world)
    controller._replan_journeys(world.model_state[KEY], agent_ids, minute)
    _is_python_s(world, digest)


def _world(scenario: Any, *, slow: bool = True) -> WorldState:
    world = WorldState.from_scenario(scenario)
    if slow:
        speeds = world.model_configs["movement_model"]["speed_m_per_minute_by_mobility"]
        for mobility in speeds:
            speeds[mobility] = SLOW
    world.minute_index = 1
    return world


def _with_alternate_route(world: WorldState) -> None:
    """A second sleeping-to-toilet edge that closing the first leg does not block."""
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


def _walk_to_the_toilet(world: WorldState, agent_id: str = "A001") -> dict[str, Any]:
    controller = _controller(world)
    route = controller._route(world.agents[agent_id].zone_id, "toilet", agent_id)
    assert route is not None
    assert controller._schedule_travel(
        agent_id=agent_id,
        route=route,
        purpose="toilet_request",
        facility_id="toilet_01",
        minute=world.minute_index,
        validate_state=False,
    )
    return world.model_state[KEY]["journeys"][agent_id]


def test_a_walk_takes_the_other_route(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    _with_alternate_route(world)
    old = dict(_walk_to_the_toilet(world))
    world.zone_edges[FIRST_LEG].closed = True
    _replanned(world, ("A001",), 1, "1cdf310433881a39")
    journey = world.model_state[KEY]["journeys"]["A001"]
    assert journey["route_edge_ids"] == [ALTERNATE]
    assert journey["event_id"] != old["event_id"]
    assert world.metric_state["movement_replan_count"] == 1.0
    assert (
        next(
            record for record in world.event_log.records if record.event.event_id == old["event_id"]
        ).status
        is EventStatus.SUPPRESSED
    )


def test_a_walk_with_no_route_stops_and_is_counted(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    _walk_to_the_toilet(world)
    world.zone_edges[FIRST_LEG].closed = True
    _replanned(world, ("A001",), 1, "6689c48e638a2b72")
    assert "A001" not in world.model_state[KEY]["journeys"]
    assert world.metric_state["movement_replan_blocked_count"] == 1.0
    assert world.agents["A001"].activity is Activity.IDLE


def test_people_replan_in_the_order_given_and_take_their_event_numbers(
    generated_scenario: Any,
) -> None:
    world = _world(generated_scenario)
    _with_alternate_route(world)
    for agent_id in ("A001", "A002", "A003"):
        world.agents[agent_id].zone_id = "sleeping"
        _walk_to_the_toilet(world, agent_id)
    world.zone_edges[FIRST_LEG].closed = True
    _replanned(world, ("A003", "A001", "A002"), 1, "62922aa33b07b046")
    journeys = world.model_state[KEY]["journeys"]
    numbers = [int(journeys[key]["event_id"].rsplit("-", 1)[1]) for key in ("A003", "A001", "A002")]
    assert numbers == [numbers[0], numbers[0] + 1, numbers[0] + 2]


def test_someone_with_no_journey_or_gone_is_passed_over(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    _with_alternate_route(world)
    _walk_to_the_toilet(world)
    world.zone_edges[FIRST_LEG].closed = True
    _replanned(world, ("A002", "nobody", "A001"), 1, "1cdf310433881a39")
    assert world.metric_state["movement_replan_count"] == 1.0


def test_a_walk_stopped_in_another_zone_is_where_its_position_says(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    _with_alternate_route(world)
    _walk_to_the_toilet(world)
    world.model_state[KEY]["positions"]["A001"]["zone_id"] = "corridor"
    world.zone_edges[FIRST_LEG].closed = True
    _replanned(world, ("A001",), 1, "8204ab4a3512b531")
    assert world.agents["A001"].zone_id == "corridor"


def test_the_trip_past_the_end_of_the_run_is_not_made(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    _with_alternate_route(world)
    _walk_to_the_toilet(world)
    world.zone_edges[FIRST_LEG].closed = True
    world.duration_minutes = 4
    _replanned(world, ("A001",), 1, "9e655475230ed9b8")
    assert "A001" not in world.model_state[KEY]["journeys"]


def _in_the_toilet_area(world: WorldState, agent_id: str = "A001") -> dict[str, Any]:
    """Walking within the room of the toilet, as a trip to the facility is."""
    world.agents[agent_id].zone_id = "toilet"
    routes = world.model_configs["facility_geometry"]["routes"]
    entrance = next(route for route in routes if route["edge_id"] == "route_corridor_toilet")
    position = world.model_state[KEY]["positions"][agent_id]
    position.update(zone_id="toilet", point=dict(entrance["points"][-1]), position_kind="physical")
    controller = _controller(world)
    room = controller._facility_approach(agent_id, "toilet_01")
    assert room is not None
    assert controller._schedule_travel(
        agent_id=agent_id,
        route=room[0],
        route_points=room[1],
        purpose="toilet_request",
        facility_id="toilet_01",
        minute=world.minute_index,
        validate_state=False,
    )
    return world.model_state[KEY]["journeys"][agent_id]


def test_a_room_walk_has_a_new_path(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    old = dict(_in_the_toilet_area(world))
    world.model_state[KEY]["positions"]["A001"]["point"] = dict(old["route_points"][0])
    world.model_state[KEY]["positions"]["A001"]["point"]["x"] += 0.1
    _replanned(world, ("A001",), 1, "5f4fe364be4806b3")
    assert world.model_state[KEY]["journeys"]["A001"]["route_points"][0] != old["route_points"][0]


def test_a_room_walk_that_is_already_there_is_placed(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    old = _in_the_toilet_area(world)
    world.model_state[KEY]["positions"]["A001"]["point"] = dict(old["route_points"][-1])
    _replanned(world, ("A001",), 1, "5eb2ecd919c1a9ed")
    assert "A001" not in world.model_state[KEY]["journeys"]
    assert world.model_state[KEY]["positions"]["A001"]["point"] == old["route_points"][-1]


def test_a_room_walk_with_no_path_is_counted(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    _in_the_toilet_area(world)
    point = world.model_configs["facility_geometry"]["facility_points"]["toilet_01"]
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
    _replanned(world, ("A001",), 1, "16f3ec3b3f8a1874")
    assert world.metric_state["movement_replan_blocked_count"] == 1.0


def test_without_a_floor_plan_a_room_walk_is_counted(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    _in_the_toilet_area(world)
    world.model_state[KEY]["journeys"]["A001"]["route_points"] = []
    del world.model_configs["facility_geometry"]
    world.model_configs.pop("spatial_constraints", None)
    world.model_configs.pop("geometry_overlay", None)
    world.model_state.pop(GEOMETRY_OVERLAY_KEY, None)
    _replanned(world, ("A001",), 1, "91b6e7d954882f29")
    assert world.metric_state["movement_replan_blocked_count"] == 1.0


# ---- sleep_bed ----------------------------------------------------------------------------------


def _room_with_a_bed(world: WorldState) -> dict[str, float]:
    """A temporary room in the sleeping area, by the effect a Run applies."""
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


def _walk_to_a_bed(world: WorldState) -> dict[str, Any]:
    """A001 on the walk that `sleep` starts."""
    controller = _controller(world)
    reservation = reserve_sleep_bed(world, "A001")
    assert reservation is not None and reservation["travel_minutes"] > 0
    from sheltersim.policies.space import ZoneRoute

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


def test_a_walk_to_a_bed_has_a_new_path(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    _room_with_a_bed(world)
    old = dict(_walk_to_a_bed(world))
    world.model_state[KEY]["positions"]["A001"]["point"]["x"] += 0.2
    _replanned(world, ("A001",), 1, "2bf07a168417f468")
    journey = world.model_state[KEY]["journeys"]["A001"]
    assert journey["purpose"] == "sleep_bed"
    assert journey["event_id"] != old["event_id"]


def test_a_walk_to_a_bed_that_arrived_sleeps_there(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    bed = _room_with_a_bed(world)
    _walk_to_a_bed(world)
    world.model_state[KEY]["positions"]["A001"]["point"] = dict(bed)
    _replanned(world, ("A001",), 1, "d64b769f6a133e6b")
    assert world.agents["A001"].activity is Activity.SLEEPING
    assert world.model_state[KEY]["positions"]["A001"]["progress"] == 1.0


def test_a_walk_to_a_bed_past_the_end_of_the_run_gives_it_up(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    _room_with_a_bed(world)
    _walk_to_a_bed(world)
    world.duration_minutes = 3
    _replanned(world, ("A001",), 1, "4ebefdf01c3c0bd7")
    assert world.agents["A001"].activity is Activity.SLEEPING
    assert "A001" not in world.model_state[GEOMETRY_OVERLAY_KEY]["bed_reservations"]
    assert world.metric_state["movement_replan_blocked_count"] == 1.0


def test_a_walk_to_a_room_that_went_gives_it_up(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    _room_with_a_bed(world)
    _walk_to_a_bed(world)
    overlay = world.model_state[GEOMETRY_OVERLAY_KEY]
    overlay["bed_reservations"].clear()
    overlay["temporary_rooms"].clear()
    _replanned(world, ("A001",), 1, "c0f2b22e1af2cd51")
    assert world.metric_state["movement_replan_blocked_count"] == 1.0
    assert world.agents["A001"].activity is Activity.SLEEPING


# ---- staff_support_travel -----------------------------------------------------------------------


def _assigned_support(scenario: Any) -> tuple[WorldState, str]:
    world = _world(scenario, slow=False)
    world.minute_index = 0
    controller = _controller(world)
    receipt = controller.staff_service.request("A001", validate_state=False)
    append_applied_care_transition(
        world,
        request_id=receipt.request_id,
        agent_id="A001",
        household_id=current_household_id(world, "A001"),
        support_kind="emotional_support",
        status="requested",
        source_ids=care_source_ids_for_agent(world, "A001"),
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
        source_ids=care_source_ids_for_agent(world, "A001"),
    )
    assert world.model_state[KEY]["journeys"]["A001"]["purpose"] == "staff_support_travel"
    world.minute_index = 1
    return world, receipt.request_id


def _alternate_to_the_staff(world: WorldState) -> None:
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


def test_support_with_another_route_is_rescheduled(generated_scenario: Any) -> None:
    world, request_id = _assigned_support(generated_scenario)
    _alternate_to_the_staff(world)
    world.zone_edges[FIRST_LEG].closed = True
    _replanned(world, ("A001",), 1, "9a3489ab32e34ae5")
    journey = world.model_state[KEY]["journeys"]["A001"]
    assert journey["route_edge_ids"] == ["route_sleeping_staff_alt"]
    assert world.support_requests[request_id].status is SupportRequestStatus.ASSIGNED
    assert world.metric_state["movement_replan_count"] == 1.0
    assert any(
        event.provenance.get("rescheduled_after_route_change")
        for event in world.event_queue.pending()
    )


def test_support_where_the_staff_already_are_starts_there(generated_scenario: Any) -> None:
    world, request_id = _assigned_support(generated_scenario)
    world.model_state[KEY]["positions"]["A001"]["zone_id"] = "staff"
    _replanned(world, ("A001",), 1, "b4b0409a6739a93b")
    assert world.agents["A001"].activity is Activity.SEEKING_HELP
    assert world.support_requests[request_id].status is SupportRequestStatus.ASSIGNED


def test_support_with_no_route_goes_back_to_the_queue(generated_scenario: Any) -> None:
    world, request_id = _assigned_support(generated_scenario)
    world.zone_edges[FIRST_LEG].closed = True
    _replanned(world, ("A001",), 1, "1d90584fe7362f7d")
    assert world.support_requests[request_id].status is SupportRequestStatus.PENDING
    assert world.metric_state["movement_replan_blocked_count"] == 1.0
    assert world.agents["A001"].support_request_id == request_id


def test_support_past_its_timeout_is_left_unresolved(generated_scenario: Any) -> None:
    world, request_id = _assigned_support(generated_scenario)
    world.zone_edges[FIRST_LEG].closed = True
    world.support_requests[request_id].timeout_minute = 1
    _replanned(world, ("A001",), 1, "ab4c53aa38c8236f")
    assert world.support_requests[request_id].status is SupportRequestStatus.UNRESOLVED
    assert world.agents["A001"].activity is Activity.IDLE
    assert world.metric_state["support_unresolved_count"] == 1.0


def test_support_past_the_end_of_the_run_goes_back_to_the_queue(generated_scenario: Any) -> None:
    world, request_id = _assigned_support(generated_scenario)
    _alternate_to_the_staff(world)
    world.zone_edges[FIRST_LEG].closed = True
    world.duration_minutes = 3
    _replanned(world, ("A001",), 1, "f09cd2fb0aaafad2")
    assert world.support_requests[request_id].status is SupportRequestStatus.PENDING


def _says_inconsistent(world: WorldState, spoil: Any, match: str, digest: str) -> None:
    """The Python step's error, after the stop it has written, as that step left the world."""
    spoil(world)
    controller = _controller(world)
    with pytest.raises(ValueError, match=match) as raised:
        controller._replan_journeys(world.model_state[KEY], ("A001",), 1)
    assert type(raised.value) is ValueError
    _is_python_s(world, digest)


def test_support_that_lost_its_task_is_python_s_error(generated_scenario: Any) -> None:
    world, request_id = _assigned_support(generated_scenario)

    def spoil(world: WorldState) -> None:
        del world.active_tasks[world.support_requests[request_id].task_id]

    _says_inconsistent(
        world,
        spoil,
        "^replanned resident trip lost its assigned support task$",
        "11ea2f11622faeae",
    )


def test_support_with_a_moved_completion_is_python_s_error(generated_scenario: Any) -> None:
    world, request_id = _assigned_support(generated_scenario)

    def spoil(world: WorldState) -> None:
        world.active_tasks[world.support_requests[request_id].task_id].due_minute += 1

    _says_inconsistent(
        world,
        spoil,
        "^replanned resident trip has inconsistent support completion$",
        "34d27c8b92f9f8ce",
    )


# ---- More branches -------------------------------------------------------------------------------


def test_a_walk_whose_event_is_gone_stops_without_a_record(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    _with_alternate_route(world)
    old = dict(_walk_to_the_toilet(world))
    world.event_queue.remove(old["event_id"])
    world.zone_edges[FIRST_LEG].closed = True
    _replanned(world, ("A001",), 1, "2541b8d93f8fbef8")
    assert not any(record.event.event_id == old["event_id"] for record in world.event_log.records)
    assert old["task_id"] not in world.active_tasks
    assert world.model_state[KEY]["journeys"]["A001"]["route_edge_ids"] == [ALTERNATE]


def test_a_walk_to_its_own_zone_has_no_route_and_is_counted(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    controller = _controller(world)
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
    _replanned(world, ("A001",), 1, "eb8eb130a300260a")
    assert "A001" not in world.model_state[KEY]["journeys"]
    assert world.metric_state["movement_replan_blocked_count"] == 1.0
    assert world.metric_state["movement_replan_count"] == 1.0


def test_a_room_walk_past_the_end_of_the_run_is_not_made_nor_blocked(
    generated_scenario: Any,
) -> None:
    world = _world(generated_scenario)
    old = dict(_in_the_toilet_area(world))
    world.model_state[KEY]["positions"]["A001"]["point"] = dict(old["route_points"][0])
    world.model_state[KEY]["positions"]["A001"]["point"]["x"] += 0.1
    world.duration_minutes = 2
    _replanned(world, ("A001",), 1, "321b3db4a63a4f25")
    assert "A001" not in world.model_state[KEY]["journeys"]
    assert world.metric_state.get("movement_replan_blocked_count", 0.0) == 0.0
    assert world.metric_state["movement_replan_count"] == 1.0


def test_support_whose_walk_a_full_zone_refuses_goes_back_to_the_queue(
    generated_scenario: Any,
) -> None:
    world, request_id = _assigned_support(generated_scenario)
    _alternate_to_the_staff(world)
    world.zone_edges[FIRST_LEG].closed = True
    world.zones["staff"].capacity = 0
    _replanned(world, ("A001",), 1, "2ddf59bf7eb7550d")
    assert world.support_requests[request_id].status is SupportRequestStatus.PENDING
    assert world.metric_state["movement_replan_blocked_count"] == 1.0
    assert "A001" not in world.model_state[KEY]["journeys"]


# ---- What the kernel turns down ------------------------------------------------------------------


def test_a_refusal_before_the_first_stop_changes_nothing(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    _with_alternate_route(world)
    _walk_to_the_toilet(world)
    world.zone_edges[FIRST_LEG].closed = True
    before = (_bytes(world), _pending(world), _records(world))
    controller = _controller(world)
    with pytest.raises(KernelRefusal, match="^replan.replan_journeys refused: "):
        controller._replan_journeys(world.model_state[KEY], ("A001",), "1")
    assert (_bytes(world), _pending(world), _records(world)) == before


def test_a_refusal_after_a_stop_is_an_error(generated_scenario: Any) -> None:
    """A stop is written, so what the kernel turns down then cannot leave the world as it was."""
    from sheltersim.policies.space import SpaceRouter

    world = _world(generated_scenario)
    _with_alternate_route(world)
    old = dict(_walk_to_the_toilet(world))
    world.zone_edges[FIRST_LEG].closed = True
    controller = _controller(world)

    class Other:
        def route(self, *arguments: Any, **options: Any) -> None:
            return None

    controller.space = SpaceRouter(controller.space.graph, Other())
    with pytest.raises(RuntimeError, match="after a replan was written: another route policy"):
        controller._replan_journeys(world.model_state[KEY], ("A001",), 1)
    assert "A001" not in world.model_state[KEY]["journeys"]
    assert world.event_log.find(old["event_id"]).status is EventStatus.SUPPRESSED


# ---- Beside the kernel ---------------------------------------------------------------------------


def test_edge_capacities_are_counted_on_the_new_route(generated_scenario: Any) -> None:
    world = _world(generated_scenario)
    _with_alternate_route(world)
    _walk_to_the_toilet(world)
    world.zone_edges[FIRST_LEG].closed = True
    world.zone_edges[ALTERNATE].max_agents_in_transit = 5
    _replanned(world, ("A001",), 1, "1cdf310433881a39")
    assert "A001" in world.model_state[KEY]["journeys"]


def test_a_tick_with_a_closed_edge_replans_through_the_driver(generated_scenario: Any) -> None:
    """The driver's path against the phases' path, over ticks that really replan."""
    from sheltersim.core.engine import TickEngine

    def run(driver: bool) -> tuple[str, float]:
        world = _world(generated_scenario)
        world.minute_index = 0
        _with_alternate_route(world)
        controller = _controller(world)
        if not driver:
            controller.run_tick = lambda *_arguments: False  # type: ignore[method-assign]
        engine = TickEngine(world, phase_handler=controller)
        engine.step()
        _walk_to_the_toilet(world, sorted(world.agents)[-1])
        world.zone_edges[FIRST_LEG].closed = True
        controller._refresh_space()
        for _ in range(10):
            engine.step()
        return _bytes(world), world.metric_state.get("movement_replan_count", 0.0)

    with_driver, without = run(True), run(False)
    assert with_driver[1] >= 1.0
    assert with_driver == without
