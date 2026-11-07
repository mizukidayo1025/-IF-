"""Versioned runtime geometry changes; immutable Scenario geometry stays unchanged.

Idle facility moves and transfers, bounded obstacle partitions, and timed route
closures are supported. Physical temporary rooms require a separate explicit
geometry operator; zone-only capacity never pretends to be a surveyed room.
"""

from __future__ import annotations

from contextlib import AbstractContextManager, nullcontext
from contextvars import ContextVar
from typing import Any, Literal

from pydantic import Field, model_validator

from sheltersim import native
from sheltersim.contracts.extensions import (
    ExtensionModel,
    FacilityGeometry,
    Identifier,
    Point2D,
    point_in_polygon,
)
from sheltersim.contracts.spatial import SpatialConstraints, SpatialObstacle
from sheltersim.core.context import ContextScope
from sheltersim.core.kernels import kernel_only
from sheltersim.core.state_mirror import register_rust_validator

GEOMETRY_OVERLAY_KEY = "geometry_overlay_v1"

# Movement asks for the facility geometry hundreds of times per tick. Inside a
# config scope (one tick phase), each raw model_configs object is parsed once and
# the frozen model is shared. Outside a scope every call parses, so a config edited
# between phases (tests, scenario setup) is always seen. Entries hold the raw object,
# so its id cannot be reused within the scope.
_CONFIG_SCOPE: ContextVar[dict[tuple[str, int], tuple[Any, Any]] | None] = ContextVar(
    "geometry_config_scope", default=None
)
_EMPTY_SPATIAL_CONSTRAINTS: dict[str, Any] = {}


def geometry_config_scope(
    parsed: dict[tuple[str, int], tuple[Any, Any]] | None = None,
) -> AbstractContextManager[None]:
    """Reuse parsed facility geometry configs while model_configs cannot change.

    ``parsed`` carries the parses from an earlier scope; the caller clears it when the
    configs may have changed.
    """
    if _CONFIG_SCOPE.get() is not None:
        return nullcontext()
    return ContextScope(_CONFIG_SCOPE, {} if parsed is None else parsed)


def _parsed_config(name: str, raw: Any, model: Any) -> Any:
    scope = _CONFIG_SCOPE.get()
    if scope is None:
        return model.model_validate(raw)
    key = (name, id(raw))
    entry = scope.get(key)
    if entry is not None and entry[0] is raw:
        return entry[1]
    parsed = model.model_validate(raw)
    scope[key] = (raw, parsed)
    return parsed


GEOMETRY_OVERLAY_POLICY = "bounded-runtime-geometry-v1"
TEMPORARY_ROOM_BED_POLICY_VERSION = "exclusive-physical-sleep-beds-v1"
TEMPORARY_ROOM_BED_SOURCE_ID = "ASM-TEMPORARY-ROOM-BEDS-V1"
TEMPORARY_ROOM_INTAKE_POLICY_VERSION = "physical-temporary-room-intake-v1"
TEMPORARY_ROOM_INTAKE_SOURCE_ID = "ASM-TEMPORARY-ROOM-INTAKE-V1"
# geometry_overlay schema_version=4 places facilities installed during a Run on
# the floor plan with the Scenario generator's own grid rule.
INSTALLED_FACILITY_PLACEMENT_POLICY_VERSION = "generator-grid-installed-facility-v1"
INSTALLED_FACILITY_PLACEMENT_SOURCE_ID = "ASM-INSTALLED-FACILITY-PLACEMENT-V1"
# The narrowest physical queue spacing (independent walkers, movement.py); the
# generator itself can place facilities this close in a narrow zone.
INSTALLED_FACILITY_MIN_SEPARATION_M = 0.6
MAX_INSTALLED_FACILITY_GRID_GROWTH = 16
MAX_BED_ROUTE_CANDIDATES = 256
MAX_ROOM_ADMISSION_POINTS = 256
GEOMETRY_OPERATORS = frozenset(
    {
        "move_facility",
        "transfer_equipment",
        "set_partition",
        "set_route_access",
        "temporary_room_geometry",
    }
)


class MoveFacilityParameters(ExtensionModel):
    point_m: Point2D


class PartitionParameters(ExtensionModel):
    polygon_m: tuple[Point2D, ...] = Field(min_length=3, max_length=64)


class RouteAccessParameters(ExtensionModel):
    accessible: bool = Field(strict=True)


class _FacilityPointOverride(ExtensionModel):
    point_m: Point2D
    event_id: Identifier
    source_ids: tuple[Identifier, ...]
    minute: int = Field(strict=True, ge=0)


class _PartitionOverride(ExtensionModel):
    zone_id: Identifier
    polygon_m: tuple[Point2D, ...] = Field(min_length=3, max_length=64)
    event_id: Identifier
    source_ids: tuple[Identifier, ...]
    minute: int = Field(strict=True, ge=0)
    end_minute: int = Field(strict=True, gt=0)

    @model_validator(mode="after")
    def interval_is_ordered(self) -> _PartitionOverride:
        if self.end_minute <= self.minute:
            raise ValueError("partition end must follow its start")
        return self


class _RouteModifier(ExtensionModel):
    accessible: bool = Field(strict=True)
    event_id: Identifier
    source_ids: tuple[Identifier, ...]
    minute: int = Field(strict=True, ge=0)
    end_minute: int = Field(strict=True, gt=0)

    @model_validator(mode="after")
    def interval_is_ordered(self) -> _RouteModifier:
        if self.end_minute <= self.minute:
            raise ValueError("route modifier end must follow its start")
        return self


class _RouteOverlay(ExtensionModel):
    base_closed: bool = Field(strict=True)
    modifiers: dict[Identifier, _RouteModifier]


class _TemporaryRoomOverlay(ExtensionModel):
    schema_version: Literal[1]
    room_id: Identifier
    zone_id: Identifier
    polygon_m: tuple[Point2D, ...]
    people_capacity: int = Field(strict=True, gt=0, le=1_000_000)
    sleeping_capacity: int = Field(strict=True, ge=0, le=1_000_000)
    bed_points_m: tuple[Point2D, ...]
    route_edge_ids: tuple[Identifier, ...]
    start_minute: int = Field(strict=True, ge=0)
    end_minute: int = Field(strict=True, gt=0)
    active: bool = Field(strict=True)
    start_event_id: Identifier
    end_event_id: Identifier | None = None
    program_id: Identifier
    effect_id: Identifier
    source_ids: tuple[Identifier, ...]

    @model_validator(mode="after")
    def room_shape_is_consistent(self) -> _TemporaryRoomOverlay:
        if self.end_minute <= self.start_minute:
            raise ValueError("temporary room end must follow its start")
        if self.sleeping_capacity > self.people_capacity:
            raise ValueError("temporary room sleeping capacity exceeds people capacity")
        if self.sleeping_capacity != len(self.bed_points_m):
            raise ValueError("temporary room bed points must match sleeping capacity")
        if self.active == (self.end_event_id is not None):
            raise ValueError("temporary room active status and end event disagree")
        if len(self.route_edge_ids) != len(set(self.route_edge_ids)):
            raise ValueError("temporary room route edges must be unique")
        if len(self.source_ids) != len(set(self.source_ids)):
            raise ValueError("temporary room sources must be unique")
        return self


class _TemporaryBedReservation(ExtensionModel):
    agent_id: Identifier
    room_id: Identifier
    bed_index: int = Field(strict=True, ge=0)
    target_id: Identifier
    reserved_minute: int = Field(strict=True, ge=0)


class _TemporaryRoomAdmission(ExtensionModel):
    agent_id: Identifier
    room_id: Identifier
    admitted_minute: int = Field(strict=True, ge=0)
    event_id: Identifier
    source_ids: tuple[Identifier, ...] = ()


class _GeometryHistory(ExtensionModel):
    transition_id: Identifier
    op: Literal[
        "move_facility",
        "transfer_equipment",
        "set_partition",
        "set_route_access",
        "temporary_room_geometry",
        "install_facility",
    ]
    target_id: Identifier
    status: Literal["applied", "expired"]
    causation_id: Identifier | None
    event_id: Identifier
    source_ids: tuple[Identifier, ...]
    minute: int = Field(strict=True, ge=0)


class _GeometryOverlayState(ExtensionModel):
    schema_version: int = Field(strict=True, ge=1, le=1)
    policy_version: Literal[GEOMETRY_OVERLAY_POLICY]
    facility_points: dict[Identifier, _FacilityPointOverride]
    partitions: dict[Identifier, _PartitionOverride]
    route_modifiers: dict[Identifier, _RouteOverlay]
    history: tuple[_GeometryHistory, ...] = Field(max_length=20000)
    temporary_rooms: dict[Identifier, _TemporaryRoomOverlay] = Field(default_factory=dict)
    # Optional when reading older Checkpoints; newly created overlays include it.
    bed_reservations: dict[Identifier, _TemporaryBedReservation] = Field(default_factory=dict)
    # Room assignments are separate from bed use and were added in schema v3.
    room_admissions: dict[Identifier, _TemporaryRoomAdmission] = Field(default_factory=dict)


def create_geometry_overlay_state(world: Any) -> dict[str, Any]:
    if "facility_geometry" not in world.model_configs:
        raise ValueError("geometry_overlay requires facility_geometry")
    return {
        "schema_version": 1,
        "policy_version": GEOMETRY_OVERLAY_POLICY,
        "facility_points": {},
        "partitions": {},
        "route_modifiers": {},
        "history": [],
        "temporary_rooms": {},
        "bed_reservations": {},
        "room_admissions": {},
    }


@kernel_only("overlay", "reserve_sleep_bed")
def reserve_sleep_bed(world: Any, agent_id: str) -> dict[str, Any] | None:
    """Reserve a free bed in the Agent's current zone and return a measured route.

    None without the overlay state, for an unknown Agent, and before geometry_overlay
    schema_version 2. A state without bed_reservations gets an empty one first (a state saved
    before beds were reserved), even when no bed follows. None without a floor plan. The person's
    own reservation is the only candidate while they hold one; else each bed of an active room of
    the zone, in room ID and bed order, that nobody has reserved. A bed within 0.75 m of another
    person of the zone is in use and passed by. At most MAX_BED_ROUTE_CANDIDATES beds get a route
    (`plan_in_room_route` from the person's position, for their mobility); the bed of the fewest
    in-room minutes wins, then room ID, then bed index (0 minutes for a path shorter than 1e-8 m).
    A new reservation records the Agent, room, bed index, target ID and the minute. Returns them
    with the bed's point, the route points and the travel minutes; None when no bed has a route.
    """


@kernel_only("overlay", "release_sleep_bed")
def release_sleep_bed(world: Any, agent_id: str) -> bool:
    """Drop the Agent's bed reservation; True if it held one."""


@kernel_only("overlay", "available_temporary_room")
def available_temporary_room(world: Any, zone_id: str) -> str | None:
    """Return the first active physical room with an available admission slot.

    Rooms of zone_id in room ID order: the first active one with fewer admissions than its
    people_capacity and a room_admission_point. None before geometry_overlay schema_version 3 or
    without the overlay state.
    """


@kernel_only("overlay", "admit_to_temporary_room")
def admit_to_temporary_room(
    world: Any,
    *,
    agent_id: str,
    room_id: str,
    minute: int,
    event_id: str,
) -> Point2D:
    """Persist a physical room assignment and return a clear in-room arrival point.

    The point is room_admission_point's. The admission record keeps the minute, the event and the
    room's source_ids with TEMPORARY_ROOM_INTAKE_SOURCE_ID, sorted. ValueError("temporary room
    admission is no longer available") when the room is unknown or not active, the Agent is
    unknown, the room has no free point, or minute is negative; nothing is written then.
    """


@kernel_only("overlay", "release_temporary_room_admission")
def release_temporary_room_admission(world: Any, agent_id: str) -> bool:
    """Drop the Agent's room admission; True if it had one."""


@kernel_only("overlay", "room_admission_point")
def room_admission_point(world: Any, room_id: str) -> Point2D | None:
    """Find a deterministic walkable point in a room, avoiding assigned residents.

    The candidates are the room's bed points, then the centres of a 0.75 m grid over the room's
    bounding box that lie in its polygon (at most MAX_ROOM_ADMISSION_POINTS inspected), one per
    point rounded to 8 places. The first MAX_ROOM_ADMISSION_POINTS of them, nearest the mean of
    the polygon's vertices first (then by x, y), are tried: a candidate within 0.5 m of an admitted
    resident's position is passed by, and the first with an in-room route to itself is returned.
    None for an unknown room, a polygon of fewer than 3 points, or no such point.
    """


def _bed_target_id(room_id: str, bed_index: int) -> str:
    import hashlib

    digest = hashlib.sha256(f"{room_id}:{bed_index}".encode()).hexdigest()[:24]
    return f"bed-{digest}"


def parsed_facility_geometry(raw: Any) -> FacilityGeometry:
    """The Scenario's facility geometry without runtime overrides; see effective_geometry.

    Inside a config scope this shares the one parse of the same raw object (#45).
    """
    return _parsed_config("facility_geometry", raw, FacilityGeometry)


def effective_geometry(world: Any) -> FacilityGeometry | None:
    raw = world.model_configs.get("facility_geometry")
    if raw is None:
        return None
    geometry = _parsed_config("facility_geometry", raw, FacilityGeometry)
    overrides = world.model_state.get(GEOMETRY_OVERLAY_KEY, {}).get("facility_points", {})
    if not overrides:
        return geometry
    points = {
        **geometry.facility_points,
        **{key: Point2D.model_validate(row["point_m"]) for key, row in overrides.items()},
    }
    shifted_service_points = []
    for service_point in geometry.service_points:
        override = overrides.get(service_point.facility_id)
        if override is None:
            shifted_service_points.append(service_point)
            continue
        old = geometry.facility_points[service_point.facility_id]
        new = Point2D.model_validate(override["point_m"])
        shifted_service_points.append(
            service_point.model_copy(
                update={
                    "point_m": Point2D(
                        x=service_point.point_m.x + new.x - old.x,
                        y=service_point.point_m.y + new.y - old.y,
                    ),
                }
            )
        )
    return geometry.model_copy(
        update={
            "facility_points": points,
            "service_points": tuple(shifted_service_points),
        }
    )


@kernel_only("overlay", "installed_facility_placement_enabled")
def installed_facility_placement_enabled(world: Any) -> bool:
    """Whether this Scenario places installed facilities on its floor plan.

    geometry_overlay schema_version 4, with the overlay state.
    """


@kernel_only("overlay", "installed_facility_point")
def installed_facility_point(world: Any, facility_id: str) -> Point2D | None:
    """Choose a free generator-grid slot in the facility's zone, or None if none fits.

    Existing facilities keep their points. The grid of the zone's facilities plus
    this one is searched in slot order, then grown, for a walkable, reachable
    point at least the narrowest queue spacing away from every other facility.

    The grids are facility_grid_point's for counts from the zone's other positioned facilities
    plus one, MAX_INSTALLED_FACILITY_GRID_GROWTH of them. A slot must be walkable past the
    obstacles, equipment footprints and active temporary rooms, reachable by an in-room route, and
    at least INSTALLED_FACILITY_MIN_SEPARATION_M from the others. None for an unknown facility, no
    facility geometry, or a zone without a polygon.
    """


@kernel_only("overlay", "place_installed_facility")
def place_installed_facility(
    world: Any, facility_id: str, *, event_id: str, source_ids: tuple[str, ...]
) -> Point2D:
    """Record the floor-plan point of a facility installed during the Run.

    The point is installed_facility_point's, written to facility_points with event_id, the minute
    and source_ids plus INSTALLED_FACILITY_PLACEMENT_SOURCE_ID, sorted, and an "install_facility"
    history row whose transition_id is "install-" and the first 24 hex digits of
    sha256(f"{event_id}:{facility_id}"); the history keeps its last 20000 rows. ValueError(f"no
    free floor-plan point for installed facility {facility_id!r}") when there is none, with nothing
    written.
    """


def effective_spatial_constraints(world: Any) -> SpatialConstraints:
    raw = world.model_configs.get("spatial_constraints", _EMPTY_SPATIAL_CONSTRAINTS)
    constraints = _parsed_config("spatial_constraints", raw, SpatialConstraints)
    state = world.model_state.get(GEOMETRY_OVERLAY_KEY, {})
    obstacles = (
        *constraints.obstacles,
        *(
            SpatialObstacle(
                id=key,
                polygon=tuple(Point2D.model_validate(p) for p in row["polygon_m"]),
                source_ids=tuple(row["source_ids"]),
            )
            for key, row in sorted(state.get("partitions", {}).items())
        ),
    )
    # Runtime same-zone moves translate the footprint with its service point.
    original = world.model_configs.get("facility_geometry", {}).get("facility_points", {})
    footprints = []
    for item in constraints.equipment_footprints:
        override = state.get("facility_points", {}).get(item.facility_id)
        if override is None:
            footprints.append(item)
            continue
        old = Point2D.model_validate(original[item.facility_id])
        new = Point2D.model_validate(override["point_m"])
        footprints.append(
            item.model_copy(
                update={
                    "polygon": tuple(
                        Point2D(x=p.x + new.x - old.x, y=p.y + new.y - old.y) for p in item.polygon
                    )
                }
            )
        )
    return constraints.model_copy(
        update={"obstacles": obstacles, "equipment_footprints": tuple(footprints)}
    )


def validate_geometry_overlay_state(world: Any) -> None:
    state = world.model_state.get(GEOMETRY_OVERLAY_KEY)
    if state is None:
        return
    try:
        contract = _GeometryOverlayState.model_validate(state)
    except (TypeError, ValueError) as exc:
        raise ValueError("geometry_overlay_v1 has an invalid schema") from exc
    nested_route_modifiers = sum(
        len(route.modifiers) for route in contract.route_modifiers.values()
    )
    if (
        any(
            len(getattr(contract, name)) > 256
            for name in ("facility_points", "partitions", "route_modifiers", "temporary_rooms")
        )
        or nested_route_modifiers > 256
    ):
        raise ValueError("geometry_overlay_v1 exceeds its bounded override limit")
    if len(contract.bed_reservations) > 10_000:
        raise ValueError("geometry_overlay_v1 exceeds its bounded bed reservation limit")
    if len(contract.room_admissions) > 10_000:
        raise ValueError("geometry_overlay_v1 exceeds its bounded room admission limit")
    if (
        len({row.transition_id for row in contract.history}) != len(contract.history)
        or any(row.minute > world.minute_index for row in contract.history)
        or any(a.minute > b.minute for a, b in zip(contract.history, contract.history[1:]))
    ):
        raise ValueError("geometry_overlay_v1 history identity/order/time mismatch")
    geometry = effective_geometry(world)
    if geometry is None:
        raise ValueError("geometry_overlay requires surveyed geometry")
    zones = {row.zone_id: row for row in geometry.zones}
    room_polygons: dict[str, tuple[Point2D, ...]] = {}
    for room_id, room in contract.temporary_rooms.items():
        if room.room_id != room_id or room.zone_id not in zones:
            raise ValueError("temporary room references an unknown room or zone")
        if room.start_minute > world.minute_index:
            raise ValueError("temporary room starts in the future")
        if room.active and world.minute_index > room.end_minute:
            raise ValueError("temporary room remained active past its expiry")
        _validate_room_polygon(zones[room.zone_id].polygon, room.polygon_m)
        room_polygons[room_id] = room.polygon_m
        if any(not point_in_polygon(point, room.polygon_m) for point in room.bed_points_m):
            raise ValueError("temporary room bed point lies outside its polygon")
        if any(edge_id not in world.zone_edges for edge_id in room.route_edge_ids):
            raise ValueError("temporary room references an unknown access route")
        if room.active:
            routes = {route.edge_id: route for route in geometry.routes}
            incoming = any(
                world.zone_edges[edge_id].to_zone_id == room.zone_id
                and not world.zone_edges[edge_id].closed
                and routes.get(edge_id) is not None
                and routes[edge_id].accessible
                for edge_id in room.route_edge_ids
            )
            outgoing = any(
                world.zone_edges[edge_id].from_zone_id == room.zone_id
                and not world.zone_edges[edge_id].closed
                and routes.get(edge_id) is not None
                and routes[edge_id].accessible
                for edge_id in room.route_edge_ids
            )
            if not incoming or not outgoing:
                raise ValueError("active temporary room lacks a directed ingress or egress route")
    active_rooms = [
        (room_id, room) for room_id, room in contract.temporary_rooms.items() if room.active
    ]
    for index, (left_id, left_room) in enumerate(active_rooms):
        for right_id, right_room in active_rooms[index + 1 :]:
            if left_room.zone_id == right_room.zone_id and _polygons_overlap(
                room_polygons[left_id], room_polygons[right_id]
            ):
                raise ValueError("active temporary room polygons overlap")
        if any(
            point_in_polygon(point, left_room.polygon_m)
            for point in geometry.facility_points.values()
        ):
            raise ValueError("active temporary room covers a facility")
    for key, row in contract.facility_points.items():
        if key not in world.facilities or row.minute > world.minute_index:
            raise ValueError("geometry overlay references an unknown facility or invalid record")
        point = row.point_m
        if not point_in_polygon(point, zones[world.facilities[key].zone_id].polygon):
            raise ValueError("runtime facility point leaves its original zone")
    for service_point in geometry.service_points:
        facility = world.facilities.get(service_point.facility_id)
        if facility is None or not point_in_polygon(
            service_point.point_m, zones[facility.zone_id].polygon
        ):
            raise ValueError("runtime service point leaves its facility's physical zone")
    for row in contract.partitions.values():
        if row.zone_id not in zones or row.minute > world.minute_index:
            raise ValueError("invalid runtime partition record")
        _validate_partition_polygon(zones[row.zone_id].polygon, row.polygon_m)
    for key, row in contract.route_modifiers.items():
        if key not in world.zone_edges or not row.modifiers:
            raise ValueError("invalid runtime route-access record")
        for record in row.modifiers.values():
            if record.minute > world.minute_index:
                raise ValueError("invalid runtime route modifier")
        if world.zone_edges[key].closed != (
            row.base_closed or any(not r.accessible for r in row.modifiers.values())
        ):
            raise ValueError("route geometry overlay and graph closure disagree")
    constraints = effective_spatial_constraints(world)
    for footprint in constraints.equipment_footprints:
        zone = zones[world.facilities[footprint.facility_id].zone_id]
        if any(not point_in_polygon(p, zone.polygon) for p in footprint.polygon):
            raise ValueError("runtime equipment footprint leaves its original zone")
        if any(
            room.active
            and room.zone_id == world.facilities[footprint.facility_id].zone_id
            and _polygons_overlap(room.polygon_m, footprint.polygon)
            for room in contract.temporary_rooms.values()
        ):
            raise ValueError("active temporary room overlaps an equipment footprint")
    reserved_beds: set[tuple[str, int]] = set()
    for agent_id, reservation in contract.bed_reservations.items():
        room = contract.temporary_rooms.get(reservation.room_id)
        bed_key = (reservation.room_id, reservation.bed_index)
        if (
            reservation.agent_id != agent_id
            or agent_id not in world.agents
            or room is None
            or reservation.bed_index >= room.sleeping_capacity
            or reservation.target_id != _bed_target_id(reservation.room_id, reservation.bed_index)
            or reservation.reserved_minute > world.minute_index
            or bed_key in reserved_beds
        ):
            raise ValueError(
                "temporary room bed reservation references an invalid or duplicate bed"
            )
        reserved_beds.add(bed_key)
    room_admission_counts: dict[str, int] = {}
    for agent_id, admission in contract.room_admissions.items():
        room = contract.temporary_rooms.get(admission.room_id)
        if (
            admission.agent_id != agent_id
            or agent_id not in world.agents
            or room is None
            or admission.admitted_minute > world.minute_index
        ):
            raise ValueError("temporary room admission references an invalid resident or room")
        room_admission_counts[admission.room_id] = (
            room_admission_counts.get(admission.room_id, 0) + 1
        )
    if any(
        count > contract.temporary_rooms[room_id].people_capacity
        for room_id, count in room_admission_counts.items()
    ):
        raise ValueError("temporary room admissions exceed physical room capacity")


def _validate_partition_polygon(zone: tuple[Point2D, ...], polygon: tuple[Point2D, ...]) -> None:
    from sheltersim.domain.movement import _segment_inside_polygon
    from sheltersim.region.facility import _valid_simple_polygon

    if not _valid_simple_polygon(polygon) or any(not point_in_polygon(p, zone) for p in polygon):
        raise ValueError(
            "area_invalid: partition requires a simple positive-area polygon inside its zone"
        )
    if any(
        not _segment_inside_polygon(a, b, zone) for a, b in zip(polygon, (*polygon[1:], polygon[0]))
    ):
        raise ValueError("area_invalid: partition leaves a concave zone")


def _validate_room_polygon(zone: tuple[Point2D, ...], polygon: tuple[Point2D, ...]) -> None:
    from sheltersim.region.facility import _valid_simple_polygon

    if not _valid_simple_polygon(polygon):
        raise ValueError("area_invalid: temporary room polygon must be simple and nondegenerate")
    if any(not point_in_polygon(point, zone) for point in polygon):
        raise ValueError("area_invalid: temporary room polygon must lie inside its zone")
    from sheltersim.domain.movement import _segment_inside_polygon

    if any(
        not _segment_inside_polygon(first, second, zone)
        for first, second in zip(polygon, (*polygon[1:], polygon[0]))
    ):
        raise ValueError("area_invalid: temporary room polygon crosses the zone boundary")


def _polygons_overlap(left: tuple[Point2D, ...], right: tuple[Point2D, ...]) -> bool:
    from sheltersim.region.facility import _segment_intersection_points

    if any(point_in_polygon(point, right) for point in left) or any(
        point_in_polygon(point, left) for point in right
    ):
        return True
    return any(
        _segment_intersection_points(a, b, c, d)
        for a, b in zip(left, (*left[1:], left[0]))
        for c, d in zip(right, (*right[1:], right[0]))
    )


@kernel_only("overlay", "validate_geometry_change")
def validate_geometry_change(
    world: Any, op: str, target: str, parameters: dict[str, Any], end_minute: int | None
) -> None:
    """Check an approved geometry change against the world now; None when it may be applied.

    A refusal is a ValueError whose text starts with its code ("target_missing: ...",
    "area_invalid: ...", "facility_in_use: ..."). The texts are shown to the operator (the approval
    preview of validate_effect_program, branch commands) and saved as a failed event's detail, so
    each check raises its own text in a fixed order, and the first that fails decides. A
    parameter's contract (RouteAccessParameters, MoveFacilityParameters, PartitionParameters,
    EquipmentTransferParameters, TemporaryRoomGeometry) raises its own validation error.

    - Without the overlay state: "geometry_overlay_required: ...". Without facility geometry:
      "geometry_unavailable". Another op: "unsupported_geometry_operator".
    - set_route_access: a known route, a bounded end, no reopening of a closed route, and a closure
      that leaves every active temporary room on it a directed ingress and egress.
    - move_facility: a positioned facility, no end, no queue, service or staff task on it; the
      point, its service points and its footprint stay in its zone, clear of active temporary
      rooms, half a metre from walls, walkable, and clear of obstacles and other equipment.
    - transfer_equipment: the same for the destination zone, with matching IDs and zones, no end,
      an open directed route between the zones, and staff with the station's skills there.
    - set_partition: a known zone, a bounded end, a simple polygon inside it, covering no facility,
      equipment, obstacle, person or staff standing point, declared passage or current room travel.
    - temporary_room_geometry: the selected zone, an end before the Run's, a simple polygon inside
      it, open access routes into and out of it, a new room ID, and no overlap with active rooms,
      facilities, reserved beds, obstacles, equipment, walls, people or staff; every bed reachable
      from an access point.
    """


@kernel_only("overlay", "apply_geometry_change")
def apply_geometry_change(world: Any, event: Any, *, expire: bool = False) -> None:
    """Apply an approved geometry event to the overlay, or end it with expire=True.

    Before applying, validate_geometry_change's checks run and raise. Without the overlay state:
    ValueError("geometry_overlay_required"). A record keeps the event's ID, the minute and its
    sources: the event's source_id, the parameters' source_ids and the effect program's
    assumption_ids, non-empty, sorted. Moves and transfers write facility_points (a transfer also
    the facility's zone); a partition is written or popped; a route-access modifier is written or
    popped and the route's closure recomputed from its base and modifiers; a temporary room is
    written active or ended. Each change appends a history row, "applied" or "expired", and the
    history keeps its last 20000 rows.
    """


register_rust_validator(GEOMETRY_OVERLAY_KEY, native.rules("overlay").validate_overlay)
