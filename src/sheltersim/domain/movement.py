"""Checkpoint-safe metre-scale positions and route progress for Agents."""

from __future__ import annotations

import math
from typing import Any

from sheltersim import native
from sheltersim.contracts.api import AgentPosition
from sheltersim.contracts.extensions import FacilityGeometry, Point2D, point_in_polygon
from sheltersim.contracts.spatial import SpatialConstraints
from sheltersim.core.kernels import kernel_first, kernel_only
from sheltersim.core.state_mirror import register_rust_validator
from sheltersim.core.world import WorldState
from sheltersim.domain.facility_queues import FACILITY_QUEUES_STATE_KEY
from sheltersim.domain.models import Activity
from sheltersim.events.models import (
    EventKind,
    EventLogRecord,
    EventStatus,
    ScheduledEvent,
)
from sheltersim.policies.space import ZoneRoute

MOVEMENT_STATE_KEY = "movement_v1"
MOVEMENT_POLICY_VERSION = "spatial-route-and-room-waypoint-v2"
MOVEMENT_SOURCE_ID = "ASM-SPATIAL-MOVEMENT-V2"
_QUEUE_TRIP_PURPOSES = frozenset(
    {
        "toilet_queue_position",
        "resource_queue_position",
        "toilet_queue_service",
        "resource_queue_service",
    }
)
_MOVEMENT_DEFAULTS = {
    "schema_version": 1,
    "policy_version": MOVEMENT_POLICY_VERSION,
    "source_id": MOVEMENT_SOURCE_ID,
    "speed_m_per_minute_by_mobility": {
        "independent": 72.0,
        "assisted": 36.0,
        "wheelchair": 48.0,
    },
    "density_slowdown_per_person_m2": 0.12,
    "maximum_density_slowdown": 2.5,
}


def canonical_movement_config(raw: Any) -> dict[str, Any]:
    """Validate versioned speed/crowding assumptions used by physical routes."""
    if raw is None:
        raw = {}
    if not isinstance(raw, dict) or set(raw) - set(_MOVEMENT_DEFAULTS):
        raise ValueError("movement_model config has unsupported fields")
    config = {**_MOVEMENT_DEFAULTS, **raw}
    if isinstance(config["speed_m_per_minute_by_mobility"], dict):
        # The speed table is nested mutable data; isolate each Scenario so a
        # coefficient override cannot mutate the module defaults or its siblings.
        config["speed_m_per_minute_by_mobility"] = dict(config["speed_m_per_minute_by_mobility"])
    if (
        config["schema_version"] != 1
        or config["policy_version"] != MOVEMENT_POLICY_VERSION
        or config["source_id"] != MOVEMENT_SOURCE_ID
    ):
        raise ValueError("unsupported movement_model version or source")
    speeds = config["speed_m_per_minute_by_mobility"]
    if not isinstance(speeds, dict) or set(speeds) != {"independent", "assisted", "wheelchair"}:
        raise ValueError("movement_model speed table must define every mobility class")
    for value in speeds.values():
        if (
            not isinstance(value, (int, float))
            or isinstance(value, bool)
            or not math.isfinite(value)
            or value <= 0
        ):
            raise ValueError("movement_model speeds must be finite positive metres per minute")
    for name in ("density_slowdown_per_person_m2", "maximum_density_slowdown"):
        value = config[name]
        if (
            not isinstance(value, (int, float))
            or isinstance(value, bool)
            or not math.isfinite(value)
            or value < 0
        ):
            raise ValueError(f"movement_model {name} must be finite and non-negative")
    if config["maximum_density_slowdown"] < 1.0:
        raise ValueError("movement_model maximum_density_slowdown must be at least one")
    return config


@kernel_only("movement", "route_with_movement_time")
def route_with_movement_time(world: WorldState, agent_id: str, route: ZoneRoute) -> ZoneRoute:
    """Apply accessibility speed and measured occupancy density to declared edge time.

    Without the `facility_geometry` and `movement_model` configs, and for a route of no edges, the
    route comes back as it is. Otherwise its minutes become ceil(max(declared minutes, surveyed
    length of its edges / independent speed) * independent speed / the person's speed * crowding),
    at least 1. Crowding is min(maximum_density_slowdown, 1 + density_slowdown_per_person_m2 *
    max(0, density - 0.5)), for the most crowded zone of the route in people per usable m2.
    """


def create_movement_runtime_state(world: WorldState) -> dict[str, Any]:
    """Create versioned movement state with one initial position per Agent."""
    geometry = _geometry(world)
    positions = {}
    staff_positions = {}
    for agent_id, agent in sorted(world.agents.items()):
        point = _zone_center(geometry, agent.zone_id) if geometry is not None else None
        positions[agent_id] = _position_record(
            agent.zone_id, point, "physical" if point is not None else "unavailable"
        )
    for staff_id, staff in sorted(world.staff.items()):
        point = _zone_center(geometry, staff.zone_id) if geometry is not None else None
        staff_positions[staff_id] = _position_record(
            staff.zone_id, point, "physical" if point is not None else "unavailable"
        )
    return {
        "schema_version": 2,
        "policy_version": MOVEMENT_POLICY_VERSION,
        "positions": positions,
        "staff_positions": staff_positions,
        "journeys": {},
        "edge_usage_minutes": {},
        "last_updated_minute": world.minute_index - 1,
    }


def validate_movement_runtime_state(world: WorldState) -> None:
    """Validate exact fields and references before restoring movement state."""
    state = world.model_state.get(MOVEMENT_STATE_KEY)
    if state is None:
        return
    expected = {
        "schema_version",
        "policy_version",
        "positions",
        "staff_positions",
        "journeys",
        "edge_usage_minutes",
        "last_updated_minute",
    }
    if (
        not isinstance(state, dict)
        or set(state) != expected
        or state.get("schema_version") != 2
        or state.get("policy_version") != MOVEMENT_POLICY_VERSION
        or not isinstance(state.get("positions"), dict)
        or not isinstance(state.get("staff_positions"), dict)
        or not isinstance(state.get("journeys"), dict)
        or not isinstance(state.get("edge_usage_minutes"), dict)
    ):
        raise ValueError("movement_v1 has an invalid exact schema")
    if set(state["positions"]) != set(world.agents):
        raise ValueError("movement_v1 positions must contain exactly the active Agents")
    if set(state["staff_positions"]) != set(world.staff):
        raise ValueError("movement_v1 staff positions must contain exactly the active staff")
    for staff_id, position in state["staff_positions"].items():
        if (
            not isinstance(position, dict)
            or set(position) != {"zone_id", "point", "edge_id", "progress", "position_kind"}
            or position["zone_id"] not in world.zones
            or (position["edge_id"] is not None and position["edge_id"] not in world.zone_edges)
            or position["position_kind"] not in {"physical", "display_only", "unavailable"}
            or not isinstance(position["progress"], (int, float))
            or isinstance(position["progress"], bool)
            or not 0.0 <= position["progress"] <= 1.0
        ):
            raise ValueError(f"movement_v1 staff position for {staff_id!r} is invalid")
    positions_valid = native.movement_positions_valid(
        state["positions"], world.zones, world.zone_edges
    )
    for agent_id, position in () if positions_valid else state["positions"].items():
        if (
            not isinstance(position, dict)
            or set(position) != {"zone_id", "point", "edge_id", "progress", "position_kind"}
            or position["zone_id"] not in world.zones
            or position["position_kind"] not in {"physical", "display_only", "unavailable"}
        ):
            raise ValueError(f"movement_v1 position for {agent_id!r} is invalid")
        point = position["point"]
        if point is not None and (
            not isinstance(point, dict)
            or set(point) != {"x", "y"}
            or any(
                not isinstance(point[key], (int, float))
                or isinstance(point[key], bool)
                or not math.isfinite(point[key])
                for key in ("x", "y")
            )
        ):
            raise ValueError(f"movement_v1 point for {agent_id!r} is invalid")
        progress = position["progress"]
        if (
            not isinstance(progress, (int, float))
            or isinstance(progress, bool)
            or not math.isfinite(progress)
            or not 0.0 <= progress <= 1.0
        ):
            raise ValueError(f"movement_v1 progress for {agent_id!r} is invalid")
        if position["edge_id"] is not None and position["edge_id"] not in world.zone_edges:
            raise ValueError(f"movement_v1 edge for {agent_id!r} is unknown")
    for agent_id, journey in state["journeys"].items():
        expected_journey_fields = {
            "event_id",
            "task_id",
            "purpose",
            "facility_id",
            "destination_zone_id",
            "route_zone_ids",
            "route_edge_ids",
            "route_points",
            "started_minute",
            "due_minute",
        }
        staff_support_fields = {"support_request_id", "support_staff_id"}
        if (
            agent_id not in world.agents
            or not isinstance(journey, dict)
            or set(journey)
            not in (
                expected_journey_fields,
                expected_journey_fields | {"queue_id"},
                expected_journey_fields | staff_support_fields,
                expected_journey_fields | staff_support_fields | {"queue_id"},
            )
            or (
                staff_support_fields <= set(journey)
                and journey["purpose"] != "staff_support_travel"
            )
            or (
                journey["purpose"] == "staff_support_travel"
                and (
                    not staff_support_fields <= set(journey)
                    or not all(
                        isinstance(journey[key], str) and journey[key]
                        for key in staff_support_fields
                    )
                )
            )
            or (
                "queue_id" in journey
                and (not isinstance(journey["queue_id"], str) or not journey["queue_id"])
            )
            or ("queue_id" in journey and journey["purpose"] not in _QUEUE_TRIP_PURPOSES)
            or (
                "queue_id" in journey
                and (
                    FACILITY_QUEUES_STATE_KEY not in world.model_state
                    or journey["queue_id"]
                    not in world.model_state[FACILITY_QUEUES_STATE_KEY]["queues"]
                    or world.model_state[FACILITY_QUEUES_STATE_KEY]["queues"][journey["queue_id"]][
                        "facility_id"
                    ]
                    != journey["facility_id"]
                )
            )
            or journey["destination_zone_id"] not in world.zones
            or not isinstance(journey["route_zone_ids"], list)
            or not isinstance(journey["route_edge_ids"], list)
            or not isinstance(journey["route_points"], list)
            or any(zone_id not in world.zones for zone_id in journey["route_zone_ids"])
            or any(edge_id not in world.zone_edges for edge_id in journey["route_edge_ids"])
            or not isinstance(journey["started_minute"], int)
            or not isinstance(journey["due_minute"], int)
            or journey["due_minute"] <= journey["started_minute"]
        ):
            raise ValueError(f"movement_v1 journey for {agent_id!r} is invalid")
        if journey["route_points"] and (
            len(journey["route_points"]) < 2
            or journey["route_edge_ids"]
            or journey["route_zone_ids"]
            != [world.agents[agent_id].zone_id, journey["destination_zone_id"]]
            or any(
                not isinstance(point, dict)
                or set(point) != {"x", "y"}
                or any(
                    not isinstance(point[key], (int, float))
                    or isinstance(point[key], bool)
                    or not math.isfinite(point[key])
                    for key in ("x", "y")
                )
                for point in journey["route_points"]
            )
        ):
            raise ValueError(f"movement_v2 in-room route for {agent_id!r} is invalid")
        if journey["route_points"]:
            geometry = _geometry(world)
            zone = (
                next(
                    (
                        item
                        for item in geometry.zones
                        if item.zone_id == world.agents[agent_id].zone_id
                    ),
                    None,
                )
                if geometry is not None
                else None
            )
            points = tuple(Point2D.model_validate(point) for point in journey["route_points"])
            if zone is None or any(not point_in_polygon(point, zone.polygon) for point in points):
                raise ValueError(f"movement_v2 in-room route for {agent_id!r} leaves its room")
            # Geometry may have changed since this route was checkpointed. The
            # movement phase checks the untraversed segment before advancing and
            # replans queue trips against the current wall/obstacle set.


@kernel_only("movement", "start_movement")
def start_movement(
    world: WorldState,
    *,
    agent_id: str,
    route: ZoneRoute,
    purpose: str,
    facility_id: str | None,
    started_minute: int,
    due_minute: int,
    event_id: str,
    task_id: str,
    route_points: tuple[Point2D, ...] = (),
    queue_id: str | None = None,
    support_request_id: str | None = None,
    support_staff_id: str | None = None,
) -> None:
    """Attach the route and its completion event to checkpointed movement state.

    Writes the person's journey: its event, task, purpose and facility, the route's zones, edges
    and room points (as JSON), the two minutes, `queue_id` when given, and the support fields only
    when both are given. Places the person at the first room point or the start of the first edge,
    or one minute along it when movement was already advanced to `started_minute`; with neither,
    at the centre of the origin zone. The world holds movement_v1, as its callers check.
    """


@kernel_first("movement", "advance_movement")
def advance_movement(world: WorldState, minute: int) -> tuple[str, ...]:
    """Interpolate active journeys for this minute; return any route to replan."""
    state = _state(world)
    geometry = _geometry(world)
    if state["last_updated_minute"] >= minute:
        return ()
    state["last_updated_minute"] = minute
    pending = {event.event_id for event in world.event_queue.pending()}
    replan: list[str] = []
    completed_queue_trips: set[str] = set()
    zone_centers: dict[str, Point2D | None] = {}
    for agent_id, journey in sorted(tuple(state["journeys"].items())):
        agent = world.agents.get(agent_id)
        if agent is None:
            state["journeys"].pop(agent_id, None)
            state["positions"].pop(agent_id, None)
            continue
        queue_trip_active = (
            agent.activity is Activity.QUEUED
            and journey["purpose"] in _QUEUE_TRIP_PURPOSES
            and journey["task_id"] in world.active_tasks
        )
        if queue_trip_active and journey["event_id"] not in pending:
            # The typed travel completion was applied. Keep the person at the
            # reserved queue waypoint so arbitration can move them to service.
            finish_movement(world, agent_id)
            continue
        if (
            agent.activity is not Activity.MOVING
            and not queue_trip_active
            or journey["event_id"] not in pending
            or journey["task_id"] not in world.active_tasks
        ):
            state["journeys"].pop(agent_id, None)
            _set_stationary_position(world, agent_id, geometry, zone_centers)
            continue
        facility = world.facilities.get(journey["facility_id"]) if journey["facility_id"] else None
        destination = facility.zone_id if facility is not None else journey["destination_zone_id"]
        if destination != journey["destination_zone_id"]:
            if journey["purpose"] in _QUEUE_TRIP_PURPOSES:
                _replan_physical_queue_trip(world, agent_id, journey, minute)
                continue
            replan.append(agent_id)
            continue
        if _facility_move_invalidated_journey(world, journey):
            if journey["purpose"] in _QUEUE_TRIP_PURPOSES:
                _replan_physical_queue_trip(world, agent_id, journey, minute)
            else:
                replan.append(agent_id)
            continue
        room_points = tuple(Point2D.model_validate(point) for point in journey["route_points"])
        if room_points:
            if not _room_path_is_clear(
                world, agent_id, journey["destination_zone_id"], room_points, journey
            ):
                if journey["purpose"] in _QUEUE_TRIP_PURPOSES:
                    _replan_physical_queue_trip(world, agent_id, journey, minute)
                else:
                    replan.append(agent_id)
                continue
            duration = journey["due_minute"] - journey["started_minute"]
            boundary_minute = min(minute + 1, journey["due_minute"])
            progress = min(1.0, max(0.0, (boundary_minute - journey["started_minute"]) / duration))
            point = _point_along_polyline(room_points, progress)
            state["positions"][agent_id] = _position_record(
                journey["destination_zone_id"], point, "physical", progress=progress
            )
            if (
                agent.activity is Activity.QUEUED
                and journey["purpose"] in _QUEUE_TRIP_PURPOSES
                and boundary_minute >= journey["due_minute"]
            ):
                completed_queue_trips.add(agent_id)
            continue
        durations = [world.zone_edges[item].travel_minutes for item in journey["route_edge_ids"]]
        # A frame at boundary M is the state before tick M. Movement is advanced
        # over [M, M+1), so the interpolated sample is the next boundary.
        boundary_minute = min(minute + 1, journey["due_minute"])
        elapsed = max(0, boundary_minute - journey["started_minute"])
        edge_index = 0
        progress = 0.0
        cumulative = 0
        if durations:
            duration_scale = (journey["due_minute"] - journey["started_minute"]) / sum(durations)
            for index, raw_duration in enumerate(durations):
                duration = raw_duration * duration_scale
                if elapsed < cumulative + duration or index == len(durations) - 1:
                    edge_index = index
                    progress = min(1.0, max(0.0, (elapsed - cumulative) / duration))
                    break
                cumulative += duration
                edge_index = min(index + 1, len(durations) - 1)
                progress = 0.0
        edge_id = journey["route_edge_ids"][edge_index] if durations else None
        if edge_id is not None and (
            world.zone_edges[edge_id].closed
            or not route_is_physically_clear(world, edge_id, mobility=agent.profile.mobility.value)
        ):
            # A closure invalidates this completion event. The controller records a
            # suppression receipt and computes a new route before any further movement.
            replan.append(agent_id)
            continue
        current_zone = (
            journey["route_zone_ids"][edge_index] if durations else journey["destination_zone_id"]
        )
        point = (
            _route_point(geometry, edge_id, progress, current_zone, world)
            if edge_id is not None
            else _zone_center(geometry, current_zone)
        )
        state["positions"][agent_id] = _position_record(
            current_zone,
            point,
            "physical" if point is not None else "unavailable",
            edge_id=edge_id,
            progress=progress,
        )
        if edge_id is not None:
            state["edge_usage_minutes"][edge_id] = state["edge_usage_minutes"].get(edge_id, 0) + 1
        if (
            agent.activity is Activity.QUEUED
            and journey["purpose"] in _QUEUE_TRIP_PURPOSES
            and boundary_minute >= journey["due_minute"]
        ):
            completed_queue_trips.add(agent_id)
    for agent_id in sorted(world.agents):
        if (
            agent_id not in state["journeys"]
            and agent_id not in completed_queue_trips
            and world.agents[agent_id].activity is not Activity.QUEUED
        ):
            _set_stationary_position(world, agent_id, geometry, zone_centers)
    staff_position_snapshots(world, boundary_minute=minute + 1)
    return tuple(replan)


def shared_passage_edge_loads(world: WorldState, minute: int | None = None) -> dict[str, int]:
    """Count active people and staff against the same configured passage capacities.

    Route display samples and historical edge usage are deliberately excluded; only
    current, scheduled trips reserve a passage. Callers planning another trip pass
    this map to ``SpaceRouter.route(..., edge_loads=...)``.
    """
    at_minute = world.minute_index if minute is None else minute
    loads: dict[str, int] = {}
    movement = world.model_state.get(MOVEMENT_STATE_KEY, {})
    represented_agent_ids: set[str] = set()
    journeys = movement.get("journeys", {}) if isinstance(movement, dict) else {}
    for agent_id, journey in journeys.items():
        if not journey["started_minute"] <= at_minute < journey["due_minute"]:
            continue
        edge_id = _current_journey_edge(world, journey, at_minute)
        if edge_id is not None:
            loads[edge_id] = loads.get(edge_id, 0) + 1
            represented_agent_ids.add(agent_id)
    for event in world.event_queue.pending():
        if (
            event.event_type != "agent_travel_complete"
            or event.payload.get("agent_id") in represented_agent_ids
        ):
            continue
        if not event.payload.get("origin_zone_id") or not event.payload.get("route_edge_ids"):
            continue
        task = world.active_tasks.get(event.payload.get("task_id"))
        if task is None or not task.start_minute <= at_minute < task.due_minute:
            continue
        edge_id = _current_journey_edge(
            world,
            {
                "started_minute": task.start_minute,
                "due_minute": task.due_minute,
                "route_edge_ids": event.payload["route_edge_ids"],
            },
            at_minute,
        )
        if edge_id is not None:
            loads[edge_id] = loads.get(edge_id, 0) + 1
    operations = world.model_state.get("staff_operations_v1", {})
    staff_records = operations.get("staff", {}) if isinstance(operations, dict) else {}
    for record in staff_records.values():
        if (
            record.get("phase") != "travel"
            or not record["started_minute"] <= at_minute < record["travel_due_minute"]
        ):
            continue
        edge_id = _current_journey_edge(
            world,
            {
                "started_minute": record["started_minute"],
                "due_minute": record["travel_due_minute"],
                "route_edge_ids": record["route_edge_ids"],
            },
            at_minute,
        )
        if edge_id is not None:
            loads[edge_id] = loads.get(edge_id, 0) + 1
    return dict(sorted(loads.items()))


def _current_journey_edge(world: WorldState, journey: dict[str, Any], minute: int) -> str | None:
    edge_ids = journey.get("route_edge_ids", ())
    if not edge_ids:
        return None
    start, due = journey["started_minute"], journey["due_minute"]
    duration = due - start
    if duration <= 0:
        return None
    elapsed = minute - start
    weights = [world.zone_edges[edge_id].travel_minutes for edge_id in edge_ids]
    total = sum(weights)
    if total <= 0:
        return edge_ids[0]
    scaled_elapsed = elapsed / duration * total
    cumulative = 0
    for edge_id, weight in zip(edge_ids, weights):
        if scaled_elapsed < cumulative + weight:
            return edge_id
        cumulative += weight
    return edge_ids[-1]


@kernel_only("movement", "finish_movement")
def finish_movement(world: WorldState, agent_id: str) -> None:
    """Remove a completed journey and set its final location.

    Nothing without movement_v1. After a queue trip the person stays reserved in the FIFO until
    service promotes them: a surveyed point is kept, off any edge. Anyone else stands where a person
    without a journey stands (`_set_stationary_position`).
    """


def agent_positions(world: WorldState) -> tuple[AgentPosition, ...]:
    """Expose shared timeline position records for application adapters."""
    state = world.model_state.get(MOVEMENT_STATE_KEY)
    if state is None:
        return tuple(
            AgentPosition(agent_id=item.id, zone_id=item.zone_id, position_kind="unavailable")
            for item in sorted(world.agents.values(), key=lambda value: value.id)
        )
    return tuple(
        AgentPosition(
            agent_id=agent_id,
            zone_id=record["zone_id"],
            point=record["point"],
            edge_id=record["edge_id"],
            progress=record["progress"],
            position_kind=record["position_kind"],
        )
        for agent_id, record in sorted(state["positions"].items())
    )


@kernel_first("movement", "staff_position_snapshots")
def staff_position_snapshots(
    world: WorldState, *, boundary_minute: int | None = None
) -> dict[str, dict[str, Any]]:
    """Return detached staff locations using the same physical coordinate contract."""
    state = world.model_state.get(MOVEMENT_STATE_KEY)
    if state is None:
        return {}
    geometry = _geometry(world)
    boundary = world.minute_index if boundary_minute is None else boundary_minute
    operations = world.model_state.get("staff_operations_v1", {})
    for staff_id, staff in sorted(world.staff.items()):
        operation = (
            operations.get("staff", {}).get(staff_id) if isinstance(operations, dict) else None
        )
        if isinstance(operation, dict) and operation.get("phase") == "travel":
            elapsed = max(
                0, min(operation["travel_minutes"], boundary - operation["started_minute"])
            )
            cumulative = 0
            edge_id = None
            progress = 0.0
            current_zone = staff.zone_id
            for index, route_edge_id in enumerate(operation["route_edge_ids"]):
                edge = world.zone_edges[route_edge_id]
                if elapsed < cumulative + edge.travel_minutes:
                    edge_id = route_edge_id
                    progress = (elapsed - cumulative) / edge.travel_minutes
                    current_zone = operation["route_zone_ids"][index]
                    break
                cumulative += edge.travel_minutes
                current_zone = edge.to_zone_id
                if index == len(operation["route_edge_ids"]) - 1:
                    edge_id = route_edge_id
                    progress = 1.0
            point = (
                _route_point(geometry, edge_id, progress, current_zone, world)
                if edge_id is not None
                else _zone_center(geometry, current_zone)
            )
            state["staff_positions"][staff_id] = _position_record(
                current_zone,
                point,
                "physical" if point is not None else "unavailable",
                edge_id=edge_id,
                progress=progress,
            )
            continue
        point = _zone_center(geometry, staff.zone_id) if geometry is not None else None
        state["staff_positions"][staff_id] = _position_record(
            staff.zone_id,
            point,
            "physical" if point is not None else "unavailable",
        )
    return {staff_id: dict(record) for staff_id, record in sorted(state["staff_positions"].items())}


def _reserved_bed_point(world: WorldState, agent_id: str) -> Point2D | None:
    """The temporary-room bed a sleeping person reserved, so they stay in it (#84)."""
    overlay = world.model_state.get("geometry_overlay_v1")
    if not isinstance(overlay, dict):
        return None
    reservation = overlay.get("bed_reservations", {}).get(agent_id)
    if not isinstance(reservation, dict):
        return None
    room = overlay.get("temporary_rooms", {}).get(reservation.get("room_id"), {})
    beds = room.get("bed_points_m", []) if isinstance(room, dict) else []
    index = reservation.get("bed_index")
    if type(index) is not int or not 0 <= index < len(beds):
        return None
    return Point2D.model_validate(beds[index])


def _set_stationary_position(
    world: WorldState,
    agent_id: str,
    geometry: FacilityGeometry | None,
    zone_centers: dict[str, Point2D | None] | None = None,
) -> None:
    """Record where a person not on a journey is; ``zone_centers`` keeps the centers of
    one geometry for a caller that places everyone in one tick."""
    state = world.model_state.get(MOVEMENT_STATE_KEY)
    agent = world.agents.get(agent_id)
    if state is None or agent is None:
        return
    point = None
    if geometry is not None:
        if agent.activity is Activity.QUEUED and agent.target_id:
            point = _queue_slot_position(world, geometry, agent.id, agent.target_id)
        elif agent.activity is Activity.IN_SERVICE and agent.target_id:
            point = geometry.facility_points.get(agent.target_id)
        elif agent.activity is Activity.SLEEPING:
            point = _reserved_bed_point(world, agent_id)
        if not point:
            if zone_centers is None:
                point = _zone_center(geometry, agent.zone_id)
            else:
                if agent.zone_id not in zone_centers:
                    zone_centers[agent.zone_id] = _zone_center(geometry, agent.zone_id)
                point = zone_centers[agent.zone_id]
    elif agent.zone_id in world.zones:
        zone = world.zones[agent.zone_id]
        if zone.display_x is not None and zone.display_y is not None:
            point = Point2D(x=zone.display_x, y=zone.display_y)
    kind = (
        "physical"
        if geometry is not None and point is not None
        else "display_only"
        if point is not None
        else "unavailable"
    )
    state["positions"][agent_id] = _position_record(agent.zone_id, point, kind)


def _state(world: WorldState) -> dict[str, Any]:
    if MOVEMENT_STATE_KEY not in world.model_state:
        world.model_state[MOVEMENT_STATE_KEY] = create_movement_runtime_state(world)
    state = world.model_state[MOVEMENT_STATE_KEY]
    for agent_id in world.agents:
        state["positions"].setdefault(
            agent_id,
            _position_record(world.agents[agent_id].zone_id, None, "unavailable"),
        )
    return state


def _geometry(world: WorldState) -> FacilityGeometry | None:
    from sheltersim.domain.geometry_overlay import effective_geometry

    return effective_geometry(world)


def _spatial_constraints(world: WorldState) -> SpatialConstraints | None:
    if (
        "spatial_constraints" not in world.model_configs
        and "geometry_overlay_v1" not in world.model_state
    ):
        return None
    from sheltersim.domain.geometry_overlay import effective_spatial_constraints

    # The effective constraints are built from validated parts, so they are used as
    # they are; a JSON dump parsed again gave the same model at each call (#45).
    return effective_spatial_constraints(world)


def _zone_center(geometry: FacilityGeometry | None, zone_id: str) -> Point2D | None:
    if geometry is None:
        return None
    zone = next((item for item in geometry.zones if item.zone_id == zone_id), None)
    if zone is None:
        return None
    points = [(point.x, point.y) for point in zone.polygon]
    twice_area = sum(
        x1 * y2 - x2 * y1 for (x1, y1), (x2, y2) in zip(points, points[1:] + points[:1])
    )
    if abs(twice_area) < 1e-12:
        candidate = Point2D(
            x=sum(x for x, _ in points) / len(points),
            y=sum(y for _, y in points) / len(points),
        )
    else:
        x = sum(
            (x1 + x2) * (x1 * y2 - x2 * y1)
            for (x1, y1), (x2, y2) in zip(points, points[1:] + points[:1])
        ) / (3.0 * twice_area)
        y = sum(
            (y1 + y2) * (x1 * y2 - x2 * y1)
            for (x1, y1), (x2, y2) in zip(points, points[1:] + points[:1])
        ) / (3.0 * twice_area)
        candidate = Point2D(x=x, y=y)
    if point_in_polygon(candidate, zone.polygon):
        return candidate

    # The area centroid of a concave room may lie outside its walls. Try an
    # inward point from each boundary segment, with deterministic offsets.
    orientation = 1.0 if twice_area > 0 else -1.0
    scale = max(
        max(p.x for p in zone.polygon) - min(p.x for p in zone.polygon),
        max(p.y for p in zone.polygon) - min(p.y for p in zone.polygon),
        1.0,
    )
    for first, second in zip(zone.polygon, (*zone.polygon[1:], zone.polygon[0])):
        dx, dy = second.x - first.x, second.y - first.y
        length = math.hypot(dx, dy)
        if length <= 1e-12:
            continue
        for fraction in (1e-7, 1e-5, 1e-3, 1e-2):
            offset = scale * fraction
            candidate = Point2D(
                x=(first.x + second.x) / 2.0 - orientation * dy / length * offset,
                y=(first.y + second.y) / 2.0 + orientation * dx / length * offset,
            )
            if point_in_polygon(candidate, zone.polygon):
                return candidate
    return None


@kernel_only("movement", "queue_slot_position")
def _queue_slot_position(
    world: WorldState,
    geometry: FacilityGeometry,
    agent_id: str,
    facility_id: str,
    queue_id: str | None = None,
) -> Point2D | None:
    """Return a distinct, accessible queue point only when a real room route exists.

    None for an unknown facility, or without its point or its room. The independent queue of
    `queue_id` gives its service point and members; otherwise the facility point and the resource
    or toilet queue that holds the person. Someone not in the queue gets the point itself. Slots
    lie from the point towards the room's centre, `spacing` apart by mobility (0.6, 0.9, 1.0 m),
    with sideways offsets 0, 1, -1, 2, -2 in each of 64 rings beyond the person's place. The first
    walkable one, `spacing` away from the people ahead and reached by plan_in_room_route from the
    person's point (else the last one ahead's, else the centre), is the slot; None when none is.
    `geometry` is `_geometry(world)`, as every caller passes.
    """


def facility_queue_slot_route(
    world: WorldState, agent_id: str, facility_id: str, queue_id: str | None = None
) -> tuple[ZoneRoute, tuple[Point2D, ...]] | None:
    """Plan an obstacle-safe in-room route and duration to this FIFO's slot.

    The returned path is a planning result. Integrators that schedule queue movement
    must preserve FIFO reservation order and complete it through the existing travel
    event before placing the person in the service queue.
    """
    geometry = _geometry(world)
    if geometry is None or agent_id not in world.agents:
        return None
    destination = _queue_slot_position(world, geometry, agent_id, facility_id, queue_id)
    facility = world.facilities.get(facility_id)
    if destination is None or facility is None:
        return None
    state = world.model_state.get(MOVEMENT_STATE_KEY, {})
    record = state.get("positions", {}).get(agent_id, {}) if isinstance(state, dict) else {}
    raw_start = record.get("point") if isinstance(record, dict) else None
    start = Point2D.model_validate(raw_start) if isinstance(raw_start, dict) else None
    path = plan_in_room_route(
        world,
        zone_id=facility.zone_id,
        start=start,
        destination=destination,
        mobility=world.agents[agent_id].profile.mobility.value,
    )
    if path is None:
        return None
    duration = in_room_travel_minutes(world, agent_id, path)
    if len(path) < 2 or all(
        _distance(first, second) <= 1e-8 for first, second in zip(path, path[1:])
    ):
        duration = 0
    route = ZoneRoute(
        origin_zone_id=facility.zone_id,
        destination_zone_id=facility.zone_id,
        zone_ids=(facility.zone_id, facility.zone_id),
        edge_ids=(),
        travel_minutes=duration,
    )
    return route, path


@kernel_first("queues", "progress_physical_queue_positions")
def progress_physical_queue_positions(
    world: WorldState,
    facility_id: str,
    *,
    resource: bool,
    minute: int,
    queue_id: str | None = None,
) -> bool:
    """Move one FIFO member to its reserved in-room slot; return whether work remains.

    Existing Facility queue lists remain the sole reservation/order source. Only one
    person per facility travels at a time, so a following person cannot reserve an
    occupied slot while the previous person is still in transit.
    """
    if (
        MOVEMENT_STATE_KEY not in world.model_state
        or "facility_geometry" not in world.model_configs
    ):
        return False
    facility = world.facilities.get(facility_id)
    geometry = _geometry(world)
    if facility is None or geometry is None:
        return True
    from sheltersim.domain.facility_queues import FACILITY_QUEUES_STATE_KEY, queue_agent_ids

    queue = (
        queue_agent_ids(world, queue_id)
        if FACILITY_QUEUES_STATE_KEY in world.model_state and queue_id is not None
        else facility.resource_queue_agent_ids
        if resource
        else facility.queue_agent_ids
    )
    movement = world.model_state[MOVEMENT_STATE_KEY]
    trips = _queue_trips(movement, facility_id, queue_id)
    head = queue[0] if queue else None
    if any(journey["purpose"].endswith("_position") for journey in trips.values()):
        # Followers walk to their slots one at a time. That walk holds back service
        # only when it is the head's own; otherwise every shift of a long queue
        # stopped service until the whole line had moved up.
        return head in trips
    for index, agent_id in enumerate(queue):
        if index == 0 and agent_id in trips:
            continue
        if index == 0 and agent_is_at_facility_service_point(
            world,
            agent_id,
            facility_id,
            service_point_id=(
                world.model_state[FACILITY_QUEUES_STATE_KEY]["queues"][queue_id]["service_point_id"]
                if queue_id is not None and FACILITY_QUEUES_STATE_KEY in world.model_state
                else None
            ),
        ):
            continue
        target = _queue_slot_position(world, geometry, agent_id, facility_id, queue_id)
        record = movement["positions"].get(agent_id, {})
        raw = record.get("point") if isinstance(record, dict) else None
        current = Point2D.model_validate(raw) if isinstance(raw, dict) else None
        if target is not None and current is not None and _distance(target, current) <= 0.05:
            continue
        route = facility_queue_slot_route(world, agent_id, facility_id, queue_id)
        if route is None or not route[1]:
            world.metric_state["physical_queue_route_blocked_count"] = (
                world.metric_state.get("physical_queue_route_blocked_count", 0.0) + 1.0
            )
            if index > 0:
                # A follower with no reachable slot (the room is full or its path is
                # blocked) waits where it stands, and so does everyone behind it. It
                # must not hold back the positioned members ahead, or the head is
                # never served once the queue outgrows the room.
                break
            return True
        _schedule_queue_trip(
            world,
            facility_id=facility_id,
            agent_id=agent_id,
            resource=resource,
            route=route[0],
            route_points=route[1],
            minute=minute,
            phase="position",
            queue_id=queue_id,
        )
        return index == 0
    return False


@kernel_only("queues", "progress_physical_queue_head_to_service")
def progress_physical_queue_head_to_service(
    world: WorldState,
    facility_id: str,
    *,
    resource: bool,
    minute: int,
    queue_id: str | None = None,
) -> bool:
    """Walk the positioned FIFO head to the facility point before service starts.

    False, writing nothing, without movement_v1 or a floor plan config, for an empty queue, and for
    a head already at the point (an approach of no minutes or of fewer than two points). True for an
    unknown facility, while the head's own queue trip lasts, and when no approach route is found,
    which counts `physical_queue_route_blocked_count`. Otherwise True after `_schedule_queue_trip`
    of the head's service trip.
    """


def _queue_trips(
    movement: dict[str, Any], facility_id: str, queue_id: str | None
) -> dict[str, dict[str, Any]]:
    return {
        agent_id: journey
        for agent_id, journey in movement["journeys"].items()
        if journey.get("facility_id") == facility_id
        and (queue_id is None or journey.get("queue_id") == queue_id)
        and journey.get("purpose") in _QUEUE_TRIP_PURPOSES
    }


@kernel_only("movement", "agent_is_at_queue_trip_destination")
def agent_is_at_queue_trip_destination(
    world: WorldState, agent_id: str, journey: dict[str, Any]
) -> bool:
    """Check that a queue walk ended where it was sent.

    True when the person's point is within 0.05 m of the journey's last room point; False for a
    journey without room points or a person without a point.

    People ahead may be served while a follower walks, so the slot it was sent to can
    differ from its slot on arrival; the next tick moves it on to the current slot.
    """


@kernel_only("movement", "agent_is_at_queue_slot")
def agent_is_at_queue_slot(
    world: WorldState, agent_id: str, facility_id: str, queue_id: str | None = None
) -> bool:
    """Check the physical reservation point against the active FIFO ordering.

    True when the person's point is within 0.05 m of `_queue_slot_position`. False without a floor
    plan, for an unknown facility, or without a slot or a point.
    """


@kernel_only("movement", "agent_is_at_facility_service_point")
def agent_is_at_facility_service_point(
    world: WorldState,
    agent_id: str,
    facility_id: str,
    service_point_id: str | None = None,
) -> bool:
    """The installed access planner defines the service point, including footprint clearance.

    True when facility_approach_route finds an approach of no minutes or of fewer than two points.
    """


def physical_service_capacity(
    world: WorldState, facility_id: str, configured: int, queue_id: str | None = None
) -> int:
    """A single surveyed point reserves one physical service place until per-unit
    geometry exists.
    """
    if queue_id is not None:
        from sheltersim.domain.facility_queues import point_for_queue

        point = point_for_queue(world, queue_id)
        if point is not None and point.facility_id == facility_id:
            return min(configured, point.service_capacity)
    if (
        MOVEMENT_STATE_KEY in world.model_state
        and "facility_geometry" in world.model_configs
        and facility_id
        in (_geometry(world).facility_points if _geometry(world) is not None else {})
    ):
        return min(configured, 1)
    return configured


def _schedule_queue_trip(
    world: WorldState,
    *,
    facility_id: str,
    agent_id: str,
    resource: bool,
    route: ZoneRoute,
    route_points: tuple[Point2D, ...],
    minute: int,
    phase: str,
    queue_id: str | None = None,
) -> None:
    duration = route.travel_minutes
    due_minute = minute + duration
    if duration <= 0 or due_minute >= world.duration_minutes:
        world.metric_state["physical_queue_route_blocked_count"] = (
            world.metric_state.get("physical_queue_route_blocked_count", 0.0) + 1.0
        )
        return
    from sheltersim.domain.facility_queues import (
        FACILITY_QUEUES_STATE_KEY,
        request_for_agent,
    )

    queue_request = (
        request_for_agent(world, agent_id, queue_id=queue_id)
        if queue_id is not None and FACILITY_QUEUES_STATE_KEY in world.model_state
        else None
    )
    queue_task = next(
        (
            task
            for task in world.active_tasks.values()
            if task.agent_id == agent_id
            and task.resource_id == facility_id
            and task.kind == ("resource_service_queue" if resource else "toilet_queue")
        ),
        None,
    )
    if queue_request is not None:
        queue_task = world.active_tasks.get(queue_request["queue_task_id"])
    if queue_task is None:
        raise ValueError("physical queue reservation has no matching active queue task")
    sequence = world.allocate_event_sequence()
    event_id = f"physical-queue-trip-{sequence}"
    purpose = (
        f"{'resource' if resource else 'toilet'}_queue_"
        f"{'position' if phase == 'position' else 'service'}"
    )
    event = ScheduledEvent(
        event_id=event_id,
        kind=EventKind.COMPLETION,
        due_minute=due_minute,
        priority=0,
        sequence=sequence,
        event_type="agent_travel_complete",
        payload={
            "agent_id": agent_id,
            "task_id": queue_task.id,
            "purpose": purpose,
            "destination_zone_id": facility_id and world.facilities[facility_id].zone_id,
            "facility_id": facility_id,
            **(
                {"queue_id": queue_id, "request_id": queue_request["request_id"]}
                if queue_request is not None
                else {}
            ),
            "origin_zone_id": route.origin_zone_id,
            "route_zone_ids": list(route.zone_ids),
            "route_edge_ids": list(route.edge_ids),
            "travel_minutes": duration,
            "start_minute": minute,
        },
        source_id=queue_task.id,
        causation_id=queue_task.id,
        provenance={
            "source_type": "assumption",
            "source_id": MOVEMENT_SOURCE_ID,
            "policy_version": MOVEMENT_POLICY_VERSION,
            "queue_phase": phase,
            "queue_requested_at": queue_task.start_minute,
            "fifo_position": (
                world.model_state[FACILITY_QUEUES_STATE_KEY]["queues"][queue_id][
                    "queued_request_ids"
                ].index(queue_request["request_id"])
                if queue_request is not None
                else (
                    world.facilities[facility_id].resource_queue_agent_ids
                    if resource
                    else world.facilities[facility_id].queue_agent_ids
                ).index(agent_id)
            ),
        },
    )
    agent = world.agents[agent_id]
    agent.activity = Activity.QUEUED
    agent.target_id = facility_id
    world.schedule_event(event)
    start_movement(
        world,
        agent_id=agent_id,
        route=route,
        purpose=purpose,
        facility_id=facility_id,
        started_minute=minute,
        due_minute=due_minute,
        event_id=event_id,
        task_id=queue_task.id,
        route_points=route_points,
        queue_id=queue_id,
    )


def _room_path_is_clear(
    world: WorldState,
    agent_id: str,
    zone_id: str,
    points: tuple[Point2D, ...],
    journey: dict[str, Any],
) -> bool:
    """Check only the not-yet-traversed portion against current walls and footprints."""
    constraints = _spatial_constraints(world)
    geometry = _geometry(world)
    if constraints is None or geometry is None:
        return True
    zone = next((item for item in geometry.zones if item.zone_id == zone_id), None)
    if zone is None:
        return False
    movement = world.model_state[MOVEMENT_STATE_KEY]
    position = movement["positions"].get(agent_id, {})
    raw_point = position.get("point") if isinstance(position, dict) else None
    current = Point2D.model_validate(raw_point) if isinstance(raw_point, dict) else points[0]
    lengths = [_distance(first, second) for first, second in zip(points, points[1:])]
    total = math.fsum(lengths)
    target_distance = min(1.0, max(0.0, position.get("progress", 0.0))) * total
    cumulative = 0.0
    remaining: list[Point2D] = [current]
    for index, (first, second, length) in enumerate(zip(points, points[1:], lengths)):
        if cumulative + length + 1e-9 >= target_distance:
            if length > 1e-12:
                fraction = min(1.0, max(0.0, (target_distance - cumulative) / length))
                if fraction < 1.0 - 1e-9:
                    remaining.append(second)
            remaining.extend(points[index + 2 :])
            break
        cumulative += length
    if len(remaining) == 1 and points:
        remaining.append(points[-1])
    return all(
        _room_segment_clear(
            first,
            second,
            zone.polygon,
            constraints,
            mobility=world.agents[agent_id].profile.mobility,
        )
        for first, second in zip(remaining, remaining[1:])
    )


def _facility_move_invalidated_journey(world: WorldState, journey: dict[str, Any]) -> bool:
    """Detect a same-zone equipment move after an active trip was planned."""
    facility_id = journey.get("facility_id")
    if not isinstance(facility_id, str):
        return False
    overlay = world.model_state.get("geometry_overlay_v1", {})
    override = (
        overlay.get("facility_points", {}).get(facility_id) if isinstance(overlay, dict) else None
    )
    return (
        isinstance(override, dict)
        and type(override.get("minute")) is int
        and override["minute"] > journey.get("started_minute", -1)
    )


def _replan_physical_queue_trip(
    world: WorldState, agent_id: str, journey: dict[str, Any], minute: int
) -> None:
    """Suppress a stale in-room trip and schedule a new typed arrival if reachable."""
    from sheltersim.domain.facility_queues import request_for_agent

    old_event = next(
        (event for event in world.event_queue.pending() if event.event_id == journey["event_id"]),
        None,
    )
    if old_event is not None:
        world.event_queue.remove(old_event.event_id)
        world.event_log.append(
            EventLogRecord(
                event=old_event,
                realized_minute=minute,
                status=EventStatus.SUPPRESSED,
                detail="In-room queue route changed after a wall, obstacle, or equipment update",
            )
        )
    state = world.model_state[MOVEMENT_STATE_KEY]
    state["journeys"].pop(agent_id, None)
    facility_id = journey.get("facility_id")
    facility = world.facilities.get(facility_id) if isinstance(facility_id, str) else None
    if facility is None:
        return
    resource = journey["purpose"].startswith("resource_")
    queue_id = journey.get("queue_id")
    if journey["purpose"].endswith("_position"):
        route = facility_queue_slot_route(world, agent_id, facility_id, queue_id)
    else:
        request = request_for_agent(world, agent_id, queue_id=queue_id) if queue_id else None
        route = facility_approach_route(
            world,
            agent_id,
            facility_id,
            request["service_point_id"] if request is not None else None,
        )
    if route is None or not route[1]:
        world.metric_state["physical_queue_replan_blocked_count"] = (
            world.metric_state.get("physical_queue_replan_blocked_count", 0.0) + 1.0
        )
        world.metric_state["movement_replan_blocked_count"] = (
            world.metric_state.get("movement_replan_blocked_count", 0.0) + 1.0
        )
        return
    if route[0].travel_minutes <= 0 or len(route[1]) < 2:
        target = route[1][-1]
        state["positions"][agent_id] = _position_record(facility.zone_id, target, "physical")
        world.metric_state["physical_queue_replan_count"] = (
            world.metric_state.get("physical_queue_replan_count", 0.0) + 1.0
        )
        world.metric_state["movement_replan_count"] = (
            world.metric_state.get("movement_replan_count", 0.0) + 1.0
        )
        return
    _schedule_queue_trip(
        world,
        facility_id=facility_id,
        agent_id=agent_id,
        resource=resource,
        route=route[0],
        route_points=route[1],
        minute=minute,
        phase="position" if journey["purpose"].endswith("_position") else "service",
        queue_id=queue_id,
    )
    world.metric_state["physical_queue_replan_count"] = (
        world.metric_state.get("physical_queue_replan_count", 0.0) + 1.0
    )
    world.metric_state["movement_replan_count"] = (
        world.metric_state.get("movement_replan_count", 0.0) + 1.0
    )


def _route_point(
    geometry: FacilityGeometry | None,
    edge_id: str,
    progress: float,
    current_zone_id: str,
    world: WorldState,
) -> Point2D | None:
    if geometry is None:
        return None
    route = next((item for item in geometry.routes if item.edge_id == edge_id), None)
    if route is None:
        return _zone_center(geometry, current_zone_id)
    edge = world.zone_edges[edge_id]
    points = list(route.points)
    if edge.from_zone_id != current_zone_id:
        points.reverse()
    lengths = [
        math.hypot(second.x - first.x, second.y - first.y)
        for first, second in zip(points, points[1:])
    ]
    total = sum(lengths)
    if total <= 0:
        return points[0]
    remaining = min(1.0, max(0.0, progress)) * total
    for index, (first, second, length) in enumerate(zip(points, points[1:], lengths)):
        if remaining <= length or index == len(lengths) - 1:
            ratio = 0.0 if length == 0 else min(1.0, max(0.0, remaining / length))
            return Point2D(
                x=first.x + (second.x - first.x) * ratio,
                y=first.y + (second.y - first.y) * ratio,
            )
        remaining -= length
    return points[-1]


@kernel_only("movement", "route_is_physically_clear")
def route_is_physically_clear(
    world: WorldState, edge_id: str, *, mobility: str = "independent"
) -> bool:
    """Reject a zone edge whose surveyed polyline crosses a closed wall or obstacle.

    True without a floor plan. False for an edge without a surveyed route, another mobility, an
    inaccessible route for an assisted or wheelchair user, or a route narrower than twice the
    clearance (0.25, 0.4, 0.45 m by mobility). With spatial constraints, each segment keeps the
    clearance from obstacles and equipment footprints, and crosses a wall only through an opening
    of this edge on it, wide enough, accessible unless the person walks independently, with the
    crossing within its half width less the clearance.
    """


@kernel_only("movement", "plan_in_room_route")
def plan_in_room_route(
    world: WorldState,
    *,
    zone_id: str,
    start: Point2D | None,
    destination: Point2D,
    mobility: str = "independent",
) -> tuple[Point2D, ...] | None:
    """Find a shortest visible waypoint path inside one measured room.

    None without a floor plan or the room. Without spatial constraints, the straight segment from
    `start` (else the room's centre) when it stays in the room. Otherwise both ends must be walkable
    (in the room, outside obstacles and footprints), and the path runs over them and the walkable
    points 0.05 m beyond the mobility's clearance off each convex obstacle corner and on both sides
    of each opening, along segments `_room_segment_clear` accepts: Dijkstra, ties broken by the
    nodes' coordinate order. None when no path connects them.
    """


def in_room_travel_minutes(world: WorldState, agent_id: str, path: tuple[Point2D, ...]) -> int:
    """Convert measured route length to minutes using explicit experimental speed assumptions."""
    if len(path) < 2:
        return 0
    mobility = world.agents[agent_id].profile.mobility
    config = canonical_movement_config(world.model_configs.get("movement_model"))
    speed = config["speed_m_per_minute_by_mobility"][mobility]
    distance = math.fsum(_distance(first, second) for first, second in zip(path, path[1:]))
    density = _max_route_density(world, (world.agents[agent_id].zone_id,), _geometry(world))
    crowding = min(
        config["maximum_density_slowdown"],
        1.0 + config["density_slowdown_per_person_m2"] * max(0.0, density - 0.5),
    )
    return max(1, math.ceil(distance / speed * crowding))


@kernel_only("movement", "facility_approach_route")
def facility_approach_route(
    world: WorldState,
    agent_id: str,
    facility_id: str,
    service_point_id: str | None = None,
) -> tuple[ZoneRoute, tuple[Point2D, ...]] | None:
    """Plan an in-room route to a walkable service point beside the facility footprint.

    Without a floor plan: a route of no edges and no minutes from the person's zone, and no points.
    None when the person is in another zone, when `service_point_id` names no surveyed point of
    this facility, or when the facility has no point. Without a service point, a footprint adds
    candidates outward of each side's midpoint and each corner by a clearance (0.30, 0.45, 0.50 m
    by mobility). The shortest planned path wins, then the smaller points; its minutes are
    in_room_travel_minutes, or 0 for a path that doesn't move. Returns the route within the zone
    and the path.
    """


def _max_route_density(
    world: WorldState, zone_ids: tuple[str, ...], geometry: FacilityGeometry | None
) -> float:
    if geometry is None:
        return 0.0
    areas = {item.zone_id: item.usable_area_m2 for item in geometry.zones}
    loads = {zone_id: 0 for zone_id in zone_ids}
    for agent in world.agents.values():
        if agent.zone_id in loads:
            loads[agent.zone_id] += 1
    return max(
        (loads[zone_id] / areas[zone_id] for zone_id in loads if areas.get(zone_id, 0) > 0),
        default=0.0,
    )


def _room_segment_clear(
    start: Point2D,
    end: Point2D,
    zone_polygon: tuple[Point2D, ...],
    constraints: Any,
    *,
    mobility: str = "independent",
) -> bool:
    clearance = {"independent": 0.25, "assisted": 0.4, "wheelchair": 0.45}.get(mobility)
    if clearance is None:
        return False
    if not _segment_inside_polygon(start, end, zone_polygon):
        return False
    blocked_polygons = tuple(item.polygon for item in constraints.obstacles) + tuple(
        item.polygon for item in constraints.equipment_footprints
    )
    if any(
        not _segment_clear_of_polygon(start, end, polygon, clearance)
        for polygon in blocked_polygons
    ):
        return False
    for wall in constraints.walls:
        crossing = _segment_intersection(start, end, wall.start, wall.end)
        if crossing is None:
            continue
        if not any(
            item.wall_id == wall.id
            and item.width_m + 1e-9 >= 2.0 * clearance
            and (item.accessible or mobility == "independent")
            and _distance(crossing, item.center) <= item.width_m / 2.0 - clearance + 1e-8
            for item in constraints.openings
        ):
            return False
    return True


def _point_walkable(
    point: Point2D, zone_polygon: tuple[Point2D, ...], obstacles: tuple[tuple[Point2D, ...], ...]
) -> bool:
    return point_in_polygon(point, zone_polygon) and not any(
        point_in_polygon(point, polygon) for polygon in obstacles
    )


def _segment_inside_polygon(start: Point2D, end: Point2D, polygon: tuple[Point2D, ...]) -> bool:
    cuts = [0.0, 1.0]
    for first, second in zip(polygon, (*polygon[1:], polygon[0])):
        t = _segment_intersection_parameter(start, end, first, second)
        if t is not None:
            cuts.append(t)
    cuts = sorted(set(cuts))
    for left, right in zip(cuts, cuts[1:]):
        middle = (left + right) / 2.0
        sample = Point2D(
            x=start.x + (end.x - start.x) * middle, y=start.y + (end.y - start.y) * middle
        )
        if not point_in_polygon(sample, polygon):
            return False
    return True


def _segment_hits_polygon(start: Point2D, end: Point2D, polygon: tuple[Point2D, ...]) -> bool:
    if point_in_polygon(start, polygon) or point_in_polygon(end, polygon):
        return True
    for first, second in zip(polygon, (*polygon[1:], polygon[0])):
        if _segment_intersection(start, end, first, second) is not None:
            return True
    return point_in_polygon(Point2D(x=(start.x + end.x) / 2.0, y=(start.y + end.y) / 2.0), polygon)


def _segment_clear_of_polygon(
    start: Point2D, end: Point2D, polygon: tuple[Point2D, ...], clearance: float
) -> bool:
    """Require a person's swept centerline to keep a mobility-sized obstacle margin."""
    if _segment_hits_polygon(start, end, polygon):
        return False
    for first, second in zip(polygon, (*polygon[1:], polygon[0])):
        if (
            min(
                _point_segment_distance(start, first, second),
                _point_segment_distance(end, first, second),
                _point_segment_distance(first, start, end),
                _point_segment_distance(second, start, end),
            )
            + 1e-9
            < clearance
        ):
            return False
    return True


def _point_segment_distance(point: Point2D, start: Point2D, end: Point2D) -> float:
    dx, dy = end.x - start.x, end.y - start.y
    denominator = dx * dx + dy * dy
    if denominator <= 1e-18:
        return _distance(point, start)
    fraction = min(
        1.0,
        max(0.0, ((point.x - start.x) * dx + (point.y - start.y) * dy) / denominator),
    )
    nearest = Point2D(x=start.x + fraction * dx, y=start.y + fraction * dy)
    return _distance(point, nearest)


def _segment_intersection(
    start: Point2D, end: Point2D, first: Point2D, second: Point2D
) -> Point2D | None:
    t = _segment_intersection_parameter(start, end, first, second)
    if t is not None:
        return Point2D(x=start.x + (end.x - start.x) * t, y=start.y + (end.y - start.y) * t)
    dx, dy = end.x - start.x, end.y - start.y
    denominator = dx * dx + dy * dy
    if denominator <= 1e-18 or abs(dx * (first.y - start.y) - dy * (first.x - start.x)) > 1e-9:
        return None
    first_t = ((first.x - start.x) * dx + (first.y - start.y) * dy) / denominator
    second_t = ((second.x - start.x) * dx + (second.y - start.y) * dy) / denominator
    overlap_start = max(0.0, min(first_t, second_t))
    overlap_end = min(1.0, max(first_t, second_t))
    if overlap_start > overlap_end + 1e-9:
        return None
    midpoint = (overlap_start + overlap_end) / 2.0
    return Point2D(x=start.x + dx * midpoint, y=start.y + dy * midpoint)


def _segment_intersection_parameter(
    start: Point2D, end: Point2D, first: Point2D, second: Point2D
) -> float | None:
    dx, dy = end.x - start.x, end.y - start.y
    ex, ey = second.x - first.x, second.y - first.y
    denominator = dx * ey - dy * ex
    if abs(denominator) <= 1e-10:
        return None
    ax, ay = first.x - start.x, first.y - start.y
    t = (ax * ey - ay * ex) / denominator
    u = (ax * dy - ay * dx) / denominator
    return t if -1e-9 <= t <= 1.0 + 1e-9 and -1e-9 <= u <= 1.0 + 1e-9 else None


def _distance(first: Point2D, second: Point2D) -> float:
    return math.hypot(second.x - first.x, second.y - first.y)


def _point_along_polyline(points: tuple[Point2D, ...], progress: float) -> Point2D:
    lengths = [_distance(first, second) for first, second in zip(points, points[1:])]
    total = math.fsum(lengths)
    if total <= 1e-12:
        return points[-1]
    remaining = total * min(1.0, max(0.0, progress))
    for first, second, length in zip(points, points[1:], lengths):
        if remaining <= length:
            ratio = 0.0 if length <= 1e-12 else remaining / length
            return Point2D(
                x=first.x + (second.x - first.x) * ratio, y=first.y + (second.y - first.y) * ratio
            )
        remaining -= length
    return points[-1]


def _position_record(
    zone_id: str,
    point: Point2D | None,
    position_kind: str,
    *,
    edge_id: str | None = None,
    progress: float = 0.0,
) -> dict[str, Any]:
    return {
        "zone_id": zone_id,
        "point": None if point is None else {"x": point.x, "y": point.y},
        "edge_id": edge_id,
        "progress": progress,
        "position_kind": position_kind,
    }


register_rust_validator(MOVEMENT_STATE_KEY, native.rules("movement").validate)
