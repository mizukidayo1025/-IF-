"""Broken worlds for the validators of lane E of #250 (#350): `movement_v1`,
`geometry_overlay_v1`, `facility_queues_v1` and `temporary_spaces_v1` (`validate_places.json`),
and `shared_operations_v1` with its history (`validate_shared_operations.json`).

Each case is the generated world of `core_validate.py` with one change, so that the first rule
Python breaks is one of these validators': `validate_simulator_extensions` raises its error as it
is, and `WorldState.validate` raises "invalid simulator extension state". A case that adds a room,
a space or a receipt builds it from the world it changes. `native/core/tests/validate_places.rs`
compares the core's answers.
"""

from __future__ import annotations

import copy
import hashlib
from collections.abc import Callable
from typing import Any

from support.core_validate import Case, cases_fixture

Doc = dict[str, Any]
Edit = Callable[[Doc], None]

BASE = "generated"


def _state(document: Doc, name: str) -> Any:
    return document["model_state"][name]


def _edits(*edits: Edit) -> Edit:
    """One edit that makes several, in order."""

    def edit(document: Doc) -> None:
        for each in edits:
            each(document)

    return edit


def _at(*path: Any, value: Any) -> Edit:
    """Set the value at a path of the document (a function of the document makes the value)."""

    def edit(document: Doc) -> None:
        node = document
        for key in path[:-1]:
            node = node[key]
        node[path[-1]] = value(document) if callable(value) else copy.deepcopy(value)

    return edit


def _drop(*path: Any) -> Edit:
    def edit(document: Doc) -> None:
        node = document
        for key in path[:-1]:
            node = node[key]
        del node[path[-1]]

    return edit


def _case(name: str, *edits: Edit) -> Case:
    return Case(name, BASE, edit=_edits(*edits))


def _entity(document: Doc, table: str, entity_id: str) -> Doc:
    rows = document[table]
    if isinstance(rows, dict):
        return rows[entity_id]
    return next(
        row
        for row in rows
        if (row["profile"]["id"] if "profile" in row else row["id"]) == entity_id
    )


# ---- temporary_spaces_v1 ------------------------------------------------------------------------

SPACES = ("model_state", "temporary_spaces_v1")


def _space(**changes: Any) -> Doc:
    space = {
        "space_id": "space-1",
        "zone_id": "sleeping",
        "people_capacity": 4,
        "sleeping_capacity": 2,
        "start_minute": 1,
        "end_minute": 30,
        "active": True,
        "start_event_id": "event-space-1",
        "end_event_id": None,
        "program_id": "program-1",
        "effect_id": "effect-1",
        "source_ids": ["ASM-TEST"],
        "geometry_status": "zone_only_capacity",
    }
    space.update(changes)
    return space


def _space_history(**changes: Any) -> Doc:
    record = {
        "event_id": "event-space-1",
        "space_id": "space-1",
        "zone_id": "sleeping",
        "minute": 1,
        "transition": "activated",
        "causation_id": None,
    }
    record.update(changes)
    return record


def _with_space(**changes: Any) -> Edit:
    space = _space(**changes)
    return _at(*SPACES, "spaces", space.get("space_id", "space-1"), value=space)


def _with_space_record(**changes: Any) -> Edit:
    return _at(*SPACES, "history", value=[_space_history(**changes)])


SPACE_CASES = [
    _case("spaces_valid_space", _with_space(), _with_space_record()),
    _case("spaces_schema_version_true", _at(*SPACES, "schema_version", value=True)),
    _case("spaces_not_dict", _at(*SPACES, value=["spaces"])),
    _case("spaces_extra_field", _at(*SPACES, "extra", value=1)),
    _case("spaces_schema_version_2", _at(*SPACES, "schema_version", value=2)),
    _case("spaces_policy", _at(*SPACES, "policy_version", value="other")),
    _case("spaces_spaces_list", _at(*SPACES, "spaces", value=[])),
    _case("spaces_history_dict", _at(*SPACES, "history", value={})),
    _case("spaces_row_not_dict", _at(*SPACES, "spaces", "space-1", value=[])),
    _case(
        "spaces_row_missing_field",
        _at(
            *SPACES,
            "spaces",
            "space-1",
            value=lambda d: {k: v for k, v in _space().items() if k != "effect_id"},
        ),
    ),
    _case(
        "spaces_id_mismatch",
        _with_space(space_id="space-2"),
        _drop(*SPACES, "spaces", "space-2"),
        _at(*SPACES, "spaces", "space-1", value=_space(space_id="space-2")),
    ),
    _case("spaces_zone_unknown", _with_space(zone_id="nowhere")),
    _case("spaces_zone_list", _with_space(zone_id=["sleeping"])),
    _case("spaces_people_zero", _with_space(people_capacity=0, sleeping_capacity=0)),
    _case("spaces_people_too_many", _with_space(people_capacity=1001)),
    _case("spaces_people_bool", _with_space(people_capacity=True, sleeping_capacity=0)),
    _case("spaces_people_float", _with_space(people_capacity=4.0)),
    _case("spaces_sleeping_over_people", _with_space(sleeping_capacity=5)),
    _case("spaces_sleeping_negative", _with_space(sleeping_capacity=-1)),
    _case("spaces_start_negative", _with_space(start_minute=-1)),
    _case("spaces_end_before_start", _with_space(end_minute=1)),
    _case("spaces_end_big", _with_space(end_minute=10**30)),
    _case("spaces_active_text", _with_space(active="yes")),
    _case("spaces_start_event_empty", _with_space(start_event_id="")),
    _case("spaces_active_with_end", _with_space(end_event_id="event-end")),
    _case("spaces_inactive_without_end", _with_space(active=False)),
    _case("spaces_inactive_with_end", _with_space(active=False, end_event_id="event-end")),
    _case("spaces_program_id_number", _with_space(program_id=5)),
    _case("spaces_source_empty", _with_space(source_ids=[""])),
    _case("spaces_sources_text", _with_space(source_ids="ASM-TEST")),
    _case("spaces_geometry_status", _with_space(geometry_status="room")),
    _case("spaces_history_not_dict", _at(*SPACES, "history", value=["record"])),
    _case(
        "spaces_history_extra", _at(*SPACES, "history", value=[{**_space_history(), "extra": 1}])
    ),
    _case("spaces_history_event_number", _with_space_record(event_id=1)),
    _case("spaces_history_zone_unknown", _with_space_record(zone_id="nowhere")),
    _case("spaces_history_zone_dict", _with_space_record(zone_id={"a": 1})),
    _case("spaces_history_minute_float", _with_space_record(minute=1.0)),
    _case("spaces_history_transition", _with_space_record(transition="moved")),
    _case("spaces_history_transition_list", _with_space_record(transition=["activated"])),
    _case("spaces_history_causation", _with_space_record(causation_id=3)),
]


# ---- movement_v1 --------------------------------------------------------------------------------

MOVEMENT = ("model_state", "movement_v1")
POSITIONS = (*MOVEMENT, "positions")
STAFF_POSITIONS = (*MOVEMENT, "staff_positions")
JOURNEYS = (*MOVEMENT, "journeys")
QUEUES = ("model_state", "facility_queues_v1")


def _support_fields(document: Doc) -> None:
    journey = _state(document, "movement_v1")["journeys"]["A004"]
    journey["support_request_id"] = "support-464131df94b1012ff8261017"
    journey["support_staff_id"] = "staff_001"


def _queue_trip(**changes: Any) -> Edit:
    """A004's journey made a trip to the queue of toilet_01."""

    def edit(document: Doc) -> None:
        journey = _state(document, "movement_v1")["journeys"]["A004"]
        journey.update(
            {
                "queue_id": "toilet_01",
                "purpose": "toilet_queue_position",
                "facility_id": "toilet_01",
            }
        )
        journey.update(copy.deepcopy(changes))

    return edit


MOVEMENT_CASES = [
    _case("movement_schema_version_float", _at(*MOVEMENT, "schema_version", value=2.0)),
    _case("movement_not_dict", _at(*MOVEMENT, value=[1])),
    _case("movement_missing_field", _drop(*MOVEMENT, "last_updated_minute")),
    _case("movement_schema_version_1", _at(*MOVEMENT, "schema_version", value=1)),
    _case("movement_policy", _at(*MOVEMENT, "policy_version", value="spatial-route-v1")),
    _case("movement_positions_list", _at(*POSITIONS, value=[])),
    _case("movement_edge_usage_list", _at(*MOVEMENT, "edge_usage_minutes", value=[])),
    _case("movement_positions_missing", _drop(*POSITIONS, "A003")),
    _case(
        "movement_positions_extra",
        _at(
            *POSITIONS,
            "A999",
            value=lambda d: copy.deepcopy(_state(d, "movement_v1")["positions"]["A003"]),
        ),
    ),
    _case("movement_staff_missing", _drop(*STAFF_POSITIONS, "staff_002")),
    _case("movement_staff_not_dict", _at(*STAFF_POSITIONS, "staff_002", value=None)),
    _case("movement_staff_extra_field", _at(*STAFF_POSITIONS, "staff_002", "extra", value=1)),
    _case(
        "movement_staff_zone_unknown",
        _at(*STAFF_POSITIONS, "staff_002", "zone_id", value="nowhere"),
    ),
    _case(
        "movement_staff_zone_list", _at(*STAFF_POSITIONS, "staff_002", "zone_id", value=["food"])
    ),
    _case(
        "movement_staff_edge_unknown",
        _at(*STAFF_POSITIONS, "staff_002", "edge_id", value="route_nowhere"),
    ),
    _case("movement_staff_edge_dict", _at(*STAFF_POSITIONS, "staff_002", "edge_id", value={})),
    _case(
        "movement_staff_kind", _at(*STAFF_POSITIONS, "staff_002", "position_kind", value="flying")
    ),
    _case(
        "movement_staff_kind_list", _at(*STAFF_POSITIONS, "staff_002", "position_kind", value=[])
    ),
    _case(
        "movement_staff_progress_bool", _at(*STAFF_POSITIONS, "staff_002", "progress", value=True)
    ),
    _case(
        "movement_staff_progress_high", _at(*STAFF_POSITIONS, "staff_002", "progress", value=1.5)
    ),
    _case("movement_staff_progress_int", _at(*STAFF_POSITIONS, "staff_002", "progress", value=1)),
    _case("movement_staff_point_text", _at(*STAFF_POSITIONS, "staff_002", "point", value="here")),
    _case("movement_position_not_dict", _at(*POSITIONS, "A003", value=[])),
    _case("movement_position_zone_unknown", _at(*POSITIONS, "A003", "zone_id", value="nowhere")),
    _case("movement_position_zone_list", _at(*POSITIONS, "A003", "zone_id", value=[])),
    _case("movement_position_kind_dict", _at(*POSITIONS, "A003", "position_kind", value={})),
    _case("movement_point_text", _at(*POSITIONS, "A003", "point", value="here")),
    _case("movement_point_extra", _at(*POSITIONS, "A003", "point", "z", value=0.0)),
    _case("movement_point_bool", _at(*POSITIONS, "A003", "point", "x", value=False)),
    _case("movement_point_none", _at(*POSITIONS, "A003", "point", value=None)),
    _case("movement_point_int", _at(*POSITIONS, "A003", "point", "x", value=2)),
    _case("movement_progress_text", _at(*POSITIONS, "A003", "progress", value="0")),
    _case("movement_progress_negative", _at(*POSITIONS, "A003", "progress", value=-0.5)),
    _case("movement_edge_unknown", _at(*POSITIONS, "A003", "edge_id", value="route_nowhere")),
    _case("movement_edge_list", _at(*POSITIONS, "A003", "edge_id", value=["route_corridor_food"])),
    _case(
        "movement_journey_unknown_agent",
        _at(
            *JOURNEYS,
            "A999",
            value=lambda d: copy.deepcopy(_state(d, "movement_v1")["journeys"]["A004"]),
        ),
    ),
    _case("movement_journey_not_dict", _at(*JOURNEYS, "A004", value="travel")),
    _case("movement_journey_extra_field", _at(*JOURNEYS, "A004", "extra", value=1)),
    _case("movement_journey_missing_field", _drop(*JOURNEYS, "A004", "task_id")),
    _case(
        "movement_journey_half_support",
        _at(*JOURNEYS, "A004", "support_staff_id", value="staff_001"),
    ),
    _case("movement_journey_support_not_support", _support_fields),
    _case(
        "movement_journey_support_without_fields",
        _at(*JOURNEYS, "A004", "purpose", value="staff_support_travel"),
    ),
    _case("movement_journey_support_empty", _at(*JOURNEYS, "A001", "support_staff_id", value="")),
    _case(
        "movement_journey_support_with_queue", _at(*JOURNEYS, "A001", "queue_id", value="toilet_01")
    ),
    _case("movement_journey_queue_not_text", _queue_trip(queue_id=7)),
    _case("movement_journey_queue_purpose", _queue_trip(purpose="return")),
    _case("movement_journey_queue_purpose_list", _queue_trip(purpose=["toilet_queue_position"])),
    _case("movement_journey_queue_unknown", _queue_trip(queue_id="nowhere")),
    _case("movement_journey_queue_facility", _queue_trip(facility_id="water_station_main")),
    _case("movement_journey_queue_state_none", _queue_trip(), _at(*QUEUES, value=None)),
    _case("movement_journey_queues_missing", _queue_trip(), _drop(*QUEUES, "queues")),
    _case(
        "movement_journey_queues_list", _queue_trip(), _at(*QUEUES, "queues", value=["toilet_01"])
    ),
    _case("movement_journey_queues_text", _queue_trip(), _at(*QUEUES, "queues", value="toilet_01")),
    _case("movement_journey_queues_number", _queue_trip(), _at(*QUEUES, "queues", value=5)),
    _case(
        "movement_journey_queue_row_list",
        _queue_trip(),
        _at(*QUEUES, "queues", "toilet_01", value=[]),
    ),
    _case(
        "movement_journey_destination_unknown",
        _at(*JOURNEYS, "A004", "destination_zone_id", value="nowhere"),
    ),
    _case(
        "movement_journey_destination_list",
        _at(*JOURNEYS, "A004", "destination_zone_id", value=["sleeping"]),
    ),
    _case(
        "movement_journey_route_zones_text",
        _at(*JOURNEYS, "A004", "route_zone_ids", value="toilet"),
    ),
    _case(
        "movement_journey_route_zone_unknown",
        _at(*JOURNEYS, "A004", "route_zone_ids", 1, value="nowhere"),
    ),
    _case(
        "movement_journey_route_zone_list", _at(*JOURNEYS, "A004", "route_zone_ids", 1, value=[])
    ),
    _case(
        "movement_journey_route_edge_unknown",
        _at(*JOURNEYS, "A004", "route_edge_ids", 0, value="route_nowhere"),
    ),
    _case("movement_journey_route_points_dict", _at(*JOURNEYS, "A004", "route_points", value={})),
    _case("movement_journey_started_text", _at(*JOURNEYS, "A004", "started_minute", value="12")),
    _case("movement_journey_started_bool", _at(*JOURNEYS, "A004", "started_minute", value=True)),
    _case("movement_journey_due_not_after", _at(*JOURNEYS, "A004", "due_minute", value=12)),
    _case(
        "movement_route_one_point",
        _at(
            *JOURNEYS,
            "A005",
            "route_points",
            value=lambda d: _state(d, "movement_v1")["journeys"]["A005"]["route_points"][:1],
        ),
    ),
    _case(
        "movement_route_with_edges",
        _at(*JOURNEYS, "A005", "route_edge_ids", value=["route_toilet_corridor"]),
    ),
    _case("movement_route_zones", _at(*JOURNEYS, "A005", "route_zone_ids", value=["toilet"])),
    _case("movement_route_point_bool", _at(*JOURNEYS, "A005", "route_points", 0, "x", value=True)),
    _case("movement_route_point_extra", _at(*JOURNEYS, "A005", "route_points", 1, "z", value=1.0)),
    _case(
        "movement_route_leaves_room",
        _at(*JOURNEYS, "A005", "route_points", 1, value={"x": 2.0, "y": 5.0}),
    ),
    _case(
        "movement_route_int_points",
        _at(*JOURNEYS, "A005", "route_points", 1, value={"x": 11, "y": 8}),
    ),
    _case(
        "movement_route_overlay_point_bad",
        _at(
            "model_state",
            "geometry_overlay_v1",
            "facility_points",
            "toilet_01",
            "point_m",
            value={"x": "far", "y": 1},
        ),
    ),
    _case(
        "movement_route_overlay_point_missing",
        _drop("model_state", "geometry_overlay_v1", "facility_points", "toilet_01", "point_m"),
    ),
    _case(
        "movement_route_overlay_row_none",
        _at("model_state", "geometry_overlay_v1", "facility_points", "toilet_01", value=None),
    ),
]


# ---- facility_queues_v1 -------------------------------------------------------------------------

REQUEST = "toilet-queue-eeb07fbe43b9f4b9aa4ff72f"
TOILET_QUEUE = (*QUEUES, "queues", "accessible_toilet_01")


def _no_queue_trip(document: Doc) -> None:
    """A005's journey without its queue, so that movement passes without the queues."""
    journey = _state(document, "movement_v1")["journeys"]["A005"]
    del journey["queue_id"]


def _second_request(document: Doc) -> None:
    """A second request of A005, queued at toilet_01."""
    state = _state(document, "facility_queues_v1")
    request = copy.deepcopy(state["requests"][REQUEST])
    request.update(
        {
            "request_id": "toilet-queue-second",
            "queue_id": "toilet_01",
            "facility_id": "toilet_01",
            "service_point_id": state["queues"]["toilet_01"]["service_point_id"],
        }
    )
    state["requests"]["toilet-queue-second"] = request
    state["queues"]["toilet_01"]["queued_request_ids"] = ["toilet-queue-second"]


def _active_request(document: Doc) -> None:
    """A005's request moved to the active requests."""
    state = _state(document, "facility_queues_v1")
    queue = state["queues"]["accessible_toilet_01"]
    queue["queued_request_ids"] = []
    queue["active_request_ids"] = [REQUEST]
    state["requests"][REQUEST]["status"] = "active"


def _one_point_per_queue(document: Doc) -> None:
    """The second service point of toilet_01 gone: no point needs an independent queue."""
    geometry = document["model_configs"]["facility_geometry"]
    geometry["service_points"] = [
        point for point in geometry["service_points"] if point["id"] != "toilet_01-second"
    ]


def _legacy_marker(document: Doc) -> None:
    document["model_state"]["facility_queues_legacy_mode_v1"] = {
        "schema_version": 1,
        "mode": "pre_independent_queue_checkpoint",
        "policy_version": "facility_fifo_legacy-v1",
    }


def _in_service(staff_id: str | None) -> Edit:
    """A005's request made an active toilet service, its task too, served by staff_id."""

    def edit(document: Doc) -> None:
        _active_request(document)
        task = _entity(document, "active_tasks", REQUEST)
        task["kind"] = "toilet_service"
        task["staff_id"] = staff_id
        _entity(document, "agents", "A005")["activity"] = "in_service"

    return edit


QUEUE_CASES = [
    _case("queues_none", _no_queue_trip, _at(*QUEUES, value=None)),
    _case("queues_none_legacy", _no_queue_trip, _at(*QUEUES, value=None), _legacy_marker),
    _case("queues_not_required", _one_point_per_queue),
    _case("queues_not_dict", _at(*QUEUES, value=[]), _no_queue_trip),
    _case("queues_extra_field", _at(*QUEUES, "extra", value=1)),
    _case("queues_schema_version_true", _at(*QUEUES, "schema_version", value=True)),
    _case("queues_schema_version_2", _at(*QUEUES, "schema_version", value=2)),
    _case("queues_requests_list", _at(*QUEUES, "requests", value=[])),
    _case("queues_ids_mismatch", _drop(*QUEUES, "queues", "intake_desk")),
    _case("queues_row_not_dict", _at(*QUEUES, "queues", "intake_desk", value="row")),
    _case(
        "queues_row_facility",
        _at(*QUEUES, "queues", "intake_desk", "facility_id", value="medical_desk"),
    ),
    _case(
        "queues_row_point", _at(*QUEUES, "queues", "intake_desk", "service_point_id", value="other")
    ),
    _case(
        "queues_row_queued_text",
        _at(*QUEUES, "queues", "intake_desk", "queued_request_ids", value="a"),
    ),
    _case("queues_row_repeat", _at(*TOILET_QUEUE, "queued_request_ids", value=[REQUEST, REQUEST])),
    _case("queues_row_unhashable", _at(*TOILET_QUEUE, "queued_request_ids", value=[REQUEST, [1]])),
    _case("queues_row_active_unhashable", _at(*TOILET_QUEUE, "active_request_ids", value=[{}])),
    _case("queues_row_overlap", _at(*TOILET_QUEUE, "active_request_ids", value=[REQUEST])),
    _case(
        "queues_row_over_capacity",
        _at(*QUEUES, "queues", "intake_desk", "active_request_ids", value=["a", "b"]),
    ),
    _case(
        "queues_stall_valid",
        _at(
            *QUEUES,
            "queues",
            "intake_desk",
            "last_stall",
            value={"minute": 3, "request_id": "r", "reason": "staff"},
        ),
    ),
    _case(
        "queues_stall_negative",
        _at(
            *QUEUES,
            "queues",
            "intake_desk",
            "last_stall",
            value={"minute": -1, "request_id": "r", "reason": "staff"},
        ),
    ),
    _case(
        "queues_stall_reason_list",
        _at(
            *QUEUES,
            "queues",
            "intake_desk",
            "last_stall",
            value={"minute": 3, "request_id": "r", "reason": ["staff"]},
        ),
    ),
    _case(
        "queues_stall_text", _at(*QUEUES, "queues", "intake_desk", "last_stall", value="stalled")
    ),
    _case(
        "queues_request_missing",
        _at(*QUEUES, "queues", "intake_desk", "queued_request_ids", value=["ghost"]),
    ),
    _case(
        "queues_request_number",
        _at(*QUEUES, "queues", "intake_desk", "queued_request_ids", value=[5]),
    ),
    _case("queues_request_queue", _at(*QUEUES, "requests", REQUEST, "queue_id", value="toilet_01")),
    _case(
        "queues_request_agent_list", _at(*QUEUES, "requests", REQUEST, "agent_id", value=["A005"])
    ),
    _case(
        "queues_request_agent_unknown", _at(*QUEUES, "requests", REQUEST, "agent_id", value="A999")
    ),
    _case("queues_request_kind_list", _at(*QUEUES, "requests", REQUEST, "service_kind", value=[])),
    _case("queues_request_future", _at(*QUEUES, "requests", REQUEST, "requested_minute", value=99)),
    _case(
        "queues_request_minute_bool",
        _at(*QUEUES, "requests", REQUEST, "requested_minute", value=True),
    ),
    _case("queues_request_status", _at(*QUEUES, "requests", REQUEST, "status", value="active")),
    _case(
        "queues_request_task_number", _at(*QUEUES, "requests", REQUEST, "active_task_id", value=3)
    ),
    _case("queues_two_queues_one_agent", _second_request),
    _case("queues_kind_mismatch", _at(*QUEUES, "requests", REQUEST, "service_kind", value="water")),
    _case(
        "queues_queued_no_task", _at(*QUEUES, "requests", REQUEST, "queue_task_id", value="nope")
    ),
    _case("queues_queued_not_projected", _at("facilities", 0, "queue_agent_ids", value=[])),
    _case("queues_active_no_task", _active_request),
    _case("queues_staff_without_skills", _in_service("staff_001")),
    _case("queues_service_not_projected", _in_service(None)),
    _case(
        "queues_unassigned_request",
        _at(
            *QUEUES,
            "requests",
            "orphan",
            value=lambda d: copy.deepcopy(_state(d, "facility_queues_v1")["requests"][REQUEST]),
        ),
    ),
    _case("queues_journey_without_request", _queue_trip()),
]


# ---- geometry_overlay_v1 ------------------------------------------------------------------------

OVERLAY = ("model_state", "geometry_overlay_v1")
ROOM_SQUARE = [
    {"x": 0.5, "y": 3.0},
    {"x": 2.5, "y": 3.0},
    {"x": 2.5, "y": 5.0},
    {"x": 0.5, "y": 5.0},
]


def _room(**changes: Any) -> Doc:
    room = {
        "schema_version": 1,
        "room_id": "room-1",
        "zone_id": "sleeping",
        "polygon_m": ROOM_SQUARE,
        "people_capacity": 2,
        "sleeping_capacity": 1,
        "bed_points_m": [{"x": 1.5, "y": 4.0}],
        "route_edge_ids": ["route_corridor_sleeping", "route_sleeping_corridor"],
        "start_minute": 2,
        "end_minute": 60,
        "active": True,
        "start_event_id": "event-room-1",
        "end_event_id": None,
        "program_id": "program-1",
        "effect_id": "effect-1",
        "source_ids": ["ASM-TEST"],
    }
    room.update(copy.deepcopy(changes))
    return room


def _with_room(key: str = "room-1", **changes: Any) -> Edit:
    return _at(*OVERLAY, "temporary_rooms", key, value=_room(**changes))


def _bed_target(room_id: str, index: int) -> str:
    return "bed-" + hashlib.sha256(f"{room_id}:{index}".encode()).hexdigest()[:24]


def _reservation(agent_id: str = "A003", **changes: Any) -> Doc:
    row = {
        "agent_id": agent_id,
        "room_id": "room-1",
        "bed_index": 0,
        "target_id": _bed_target("room-1", 0),
        "reserved_minute": 3,
    }
    row.update(changes)
    return row


def _with_reservation(agent_id: str = "A003", **changes: Any) -> Edit:
    return _at(*OVERLAY, "bed_reservations", agent_id, value=_reservation(agent_id, **changes))


def _admission(agent_id: str = "A003", **changes: Any) -> Doc:
    row = {
        "agent_id": agent_id,
        "room_id": "room-1",
        "admitted_minute": 3,
        "event_id": "event-admit",
        "source_ids": [],
    }
    row.update(changes)
    return row


def _with_admission(agent_id: str = "A003", **changes: Any) -> Edit:
    return _at(*OVERLAY, "room_admissions", agent_id, value=_admission(agent_id, **changes))


def _partition(**changes: Any) -> Doc:
    row = {
        "zone_id": "sleeping",
        "polygon_m": [
            {"x": 3.0, "y": 6.0},
            {"x": 4.0, "y": 6.0},
            {"x": 4.0, "y": 7.0},
            {"x": 3.0, "y": 7.0},
        ],
        "event_id": "event-partition",
        "source_ids": ["ASM-TEST"],
        "minute": 2,
        "end_minute": 40,
    }
    row.update(copy.deepcopy(changes))
    return row


def _modifier(**changes: Any) -> Doc:
    record = {
        "accessible": False,
        "event_id": "event-close",
        "source_ids": ["ASM-TEST"],
        "minute": 2,
        "end_minute": 40,
    }
    record.update(changes)
    return {"base_closed": False, "modifiers": {"event-close": record}}


def _close_edge(edge_id: str) -> Edit:
    def edit(document: Doc) -> None:
        _entity(document, "zone_edges", edge_id)["closed"] = True

    return edit


def _footprint(facility_id: str, polygon: list[Doc]) -> Edit:
    def edit(document: Doc) -> None:
        document["model_configs"]["spatial_constraints"]["equipment_footprints"] = [
            {
                "facility_id": facility_id,
                "polygon": polygon,
                "area_m2": 0.25,
                "source_ids": ["ASM-TEST"],
            }
        ]

    return edit


def _no_room_route(document: Doc) -> None:
    """A005's journey without its room points, which need the floor plan."""
    journey = _state(document, "movement_v1")["journeys"]["A005"]
    journey["route_points"] = []


def _many_points(document: Doc) -> None:
    row = _state(document, "geometry_overlay_v1")["facility_points"]["toilet_01"]
    for index in range(256):
        _state(document, "geometry_overlay_v1")["facility_points"][f"f{index}"] = row


def _history_copy(**changes: Any) -> Edit:
    def edit(document: Doc) -> None:
        history = _state(document, "geometry_overlay_v1")["history"]
        row = copy.deepcopy(history[0])
        row.update(changes)
        history.append(row)

    return edit


def _concave_sleeping(document: Doc) -> None:
    """The sleeping zone made an L, so that a polygon with its corners inside can cross it."""
    geometry = document["model_configs"]["facility_geometry"]
    zone = next(zone for zone in geometry["zones"] if zone["zone_id"] == "sleeping")
    zone["polygon"] = [
        {"x": 0.0, "y": 1.8},
        {"x": 4.6, "y": 1.8},
        {"x": 4.6, "y": 6.0},
        {"x": 2.0, "y": 6.0},
        {"x": 2.0, "y": 10.0},
        {"x": 0.0, "y": 10.0},
    ]
    zone["usable_area_m2"] = 20.0


ACROSS_THE_CORNER = [{"x": 1.0, "y": 5.5}, {"x": 4.0, "y": 5.5}, {"x": 1.5, "y": 9.0}]

FOOTPRINT_IN_TOILET = [
    {"x": 10.3, "y": 3.0},
    {"x": 10.8, "y": 3.0},
    {"x": 10.8, "y": 3.5},
    {"x": 10.3, "y": 3.5},
]

OVERLAY_CASES = [
    _case("overlay_valid_room", _with_room()),
    _case(
        "overlay_without_optional_parts",
        _drop(*OVERLAY, "temporary_rooms"),
        _drop(*OVERLAY, "bed_reservations"),
        _drop(*OVERLAY, "room_admissions"),
    ),
    _case("overlay_not_dict", _at(*OVERLAY, value=[])),
    _case("overlay_schema_version_2", _at(*OVERLAY, "schema_version", value=2)),
    _case("overlay_schema_version_true", _at(*OVERLAY, "schema_version", value=True)),
    _case("overlay_extra_field", _at(*OVERLAY, "extra", value={})),
    _case(
        "overlay_point_minute_float",
        _at(*OVERLAY, "facility_points", "toilet_01", "minute", value=2.0),
    ),
    _case(
        "overlay_point_coordinate_text",
        _at(*OVERLAY, "facility_points", "toilet_01", "point_m", "x", value="11.29"),
    ),
    _case("overlay_room_end_before_start", _with_room(end_minute=2)),
    _case("overlay_room_beds_mismatch", _with_room(sleeping_capacity=2)),
    _case("overlay_room_active_with_end", _with_room(end_event_id="event-end")),
    _case(
        "overlay_room_repeated_route",
        _with_room(route_edge_ids=["route_corridor_sleeping", "route_corridor_sleeping"]),
    ),
    _case("overlay_too_many_points", _many_points),
    _case("overlay_history_repeat", _history_copy(minute=3)),
    _case("overlay_history_future", _history_copy(transition_id="t-2", minute=99)),
    _case("overlay_history_order", _history_copy(transition_id="t-2", minute=1)),
    _case("overlay_room_id_mismatch", _with_room(room_id="room-2")),
    _case("overlay_room_zone_unknown", _with_room(zone_id="nowhere")),
    _case("overlay_room_future", _with_room(start_minute=50)),
    _case("overlay_room_expired_active", _with_room(start_minute=1, end_minute=10)),
    _case(
        "overlay_room_expired_inactive",
        _with_room(start_minute=1, end_minute=10, active=False, end_event_id="event-end"),
    ),
    _case(
        "overlay_room_degenerate",
        _with_room(polygon_m=ROOM_SQUARE[:2], sleeping_capacity=0, bed_points_m=[]),
    ),
    _case(
        "overlay_room_outside_zone",
        _with_room(
            polygon_m=[
                {"x": 3.0, "y": 3.0},
                {"x": 5.5, "y": 3.0},
                {"x": 5.5, "y": 5.0},
                {"x": 3.0, "y": 5.0},
            ],
            bed_points_m=[{"x": 3.5, "y": 4.0}],
        ),
    ),
    _case(
        "overlay_room_crosses_zone",
        _concave_sleeping,
        _with_room(polygon_m=ACROSS_THE_CORNER, sleeping_capacity=0, bed_points_m=[]),
    ),
    _case(
        "overlay_partition_crosses_zone",
        _concave_sleeping,
        _at(*OVERLAY, "partitions", "partition-1", value=_partition(polygon_m=ACROSS_THE_CORNER)),
    ),
    _case("overlay_room_bed_outside", _with_room(bed_points_m=[{"x": 3.5, "y": 4.0}])),
    _case("overlay_room_route_unknown", _with_room(route_edge_ids=["route_nowhere"])),
    _case("overlay_room_no_egress", _with_room(route_edge_ids=["route_corridor_sleeping"])),
    _case(
        "overlay_room_closed_route",
        _with_room(),
        _close_edge("route_sleeping_corridor"),
        _at(*OVERLAY, "route_modifiers", "route_sleeping_corridor", value=_modifier()),
    ),
    _case(
        "overlay_rooms_overlap",
        _with_room(),
        _with_room(
            "room-2",
            room_id="room-2",
            polygon_m=[
                {"x": 2.0, "y": 4.0},
                {"x": 4.0, "y": 4.0},
                {"x": 4.0, "y": 6.0},
                {"x": 2.0, "y": 6.0},
            ],
            bed_points_m=[{"x": 3.0, "y": 5.0}],
        ),
    ),
    _case(
        "overlay_rooms_apart",
        _with_room(),
        _with_room(
            "room-2",
            room_id="room-2",
            polygon_m=[
                {"x": 0.5, "y": 6.0},
                {"x": 2.5, "y": 6.0},
                {"x": 2.5, "y": 8.0},
                {"x": 0.5, "y": 8.0},
            ],
            bed_points_m=[{"x": 1.5, "y": 7.0}],
        ),
    ),
    _case(
        "overlay_room_covers_facility",
        _with_room(
            zone_id="toilet",
            polygon_m=[
                {"x": 10.2, "y": 7.0},
                {"x": 11.8, "y": 7.0},
                {"x": 11.8, "y": 9.5},
                {"x": 10.2, "y": 9.5},
            ],
            bed_points_m=[{"x": 11.0, "y": 7.5}],
            route_edge_ids=["route_corridor_toilet", "route_toilet_corridor"],
        ),
    ),
    _case(
        "overlay_point_unknown_facility",
        _at(
            *OVERLAY,
            "facility_points",
            "ghost",
            value=lambda d: copy.deepcopy(
                _state(d, "geometry_overlay_v1")["facility_points"]["toilet_01"]
            ),
        ),
    ),
    _case(
        "overlay_point_future", _at(*OVERLAY, "facility_points", "toilet_01", "minute", value=99)
    ),
    _case(
        "overlay_point_leaves_zone",
        _at(*OVERLAY, "facility_points", "toilet_01", "point_m", value={"x": 2.0, "y": 5.0}),
    ),
    _case(
        "overlay_service_point_leaves_zone",
        _at(
            *OVERLAY,
            "facility_points",
            "toilet_01",
            "point_m",
            value={"x": 11.29193338482967, "y": 2.0},
        ),
        _no_room_route,
    ),
    _case(
        "overlay_valid_partition", _at(*OVERLAY, "partitions", "partition-1", value=_partition())
    ),
    _case(
        "overlay_partition_zone_unknown",
        _at(*OVERLAY, "partitions", "partition-1", value=_partition(zone_id="nowhere")),
    ),
    _case(
        "overlay_partition_future",
        _at(*OVERLAY, "partitions", "partition-1", value=_partition(minute=30)),
    ),
    _case(
        "overlay_partition_outside",
        _at(
            *OVERLAY,
            "partitions",
            "partition-1",
            value=_partition(
                polygon_m=[
                    {"x": 4.0, "y": 6.0},
                    {"x": 5.0, "y": 6.0},
                    {"x": 5.0, "y": 7.0},
                    {"x": 4.0, "y": 7.0},
                ]
            ),
        ),
    ),
    _case(
        "overlay_partition_end_not_after",
        _at(*OVERLAY, "partitions", "partition-1", value=_partition(end_minute=2)),
    ),
    _case(
        "overlay_valid_route_modifier",
        _close_edge("route_quiet_corridor"),
        _at(*OVERLAY, "route_modifiers", "route_quiet_corridor", value=_modifier()),
    ),
    _case(
        "overlay_route_modifier_unknown",
        _at(*OVERLAY, "route_modifiers", "route_nowhere", value=_modifier()),
    ),
    _case(
        "overlay_route_modifier_empty",
        _at(
            *OVERLAY,
            "route_modifiers",
            "route_quiet_corridor",
            value={"base_closed": True, "modifiers": {}},
        ),
        _close_edge("route_quiet_corridor"),
    ),
    _case(
        "overlay_route_modifier_future",
        _close_edge("route_quiet_corridor"),
        _at(*OVERLAY, "route_modifiers", "route_quiet_corridor", value=_modifier(minute=30)),
    ),
    _case(
        "overlay_route_closure_disagrees",
        _at(*OVERLAY, "route_modifiers", "route_quiet_corridor", value=_modifier()),
    ),
    _case(
        "overlay_route_modifier_bool_text",
        _at(
            *OVERLAY, "route_modifiers", "route_quiet_corridor", value=_modifier(accessible="false")
        ),
    ),
    _case("overlay_valid_footprint", _footprint("accessible_toilet_01", FOOTPRINT_IN_TOILET)),
    _case(
        "overlay_footprint_leaves_zone",
        _footprint(
            "accessible_toilet_01",
            [
                {"x": 9.5, "y": 3.0},
                {"x": 10.8, "y": 3.0},
                {"x": 10.8, "y": 3.5},
                {"x": 9.5, "y": 3.5},
            ],
        ),
    ),
    _case("overlay_footprint_unknown_facility", _footprint("ghost", FOOTPRINT_IN_TOILET)),
    _case(
        "overlay_footprint_moved",
        _footprint(
            "toilet_01",
            [
                {"x": 11.2, "y": 8.7},
                {"x": 11.4, "y": 8.7},
                {"x": 11.4, "y": 8.9},
                {"x": 11.2, "y": 8.9},
            ],
        ),
    ),
    _case(
        "overlay_footprint_moved_out",
        _footprint(
            "toilet_01",
            [
                {"x": 11.2, "y": 3.7},
                {"x": 11.4, "y": 3.7},
                {"x": 11.4, "y": 3.9},
                {"x": 11.2, "y": 3.9},
            ],
        ),
    ),
    _case(
        "overlay_room_over_footprint",
        _with_room(
            zone_id="toilet",
            polygon_m=[
                {"x": 10.2, "y": 2.5},
                {"x": 11.8, "y": 2.5},
                {"x": 11.8, "y": 4.5},
                {"x": 10.2, "y": 4.5},
            ],
            bed_points_m=[{"x": 11.0, "y": 4.2}],
            route_edge_ids=["route_corridor_toilet", "route_toilet_corridor"],
        ),
        _footprint("accessible_toilet_01", FOOTPRINT_IN_TOILET),
    ),
    _case(
        "overlay_spatial_constraints_invalid",
        _at("model_configs", "spatial_constraints", "obstacles", value=5),
    ),
    _case("overlay_valid_reservation", _with_room(), _with_reservation()),
    _case(
        "overlay_reservation_agent_mismatch",
        _with_room(),
        _at(*OVERLAY, "bed_reservations", "A003", value=_reservation("A002")),
    ),
    _case("overlay_reservation_agent_unknown", _with_room(), _with_reservation("A999")),
    _case("overlay_reservation_room_unknown", _with_reservation()),
    _case(
        "overlay_reservation_bed_past_capacity",
        _with_room(),
        _with_reservation(bed_index=1, target_id=_bed_target("room-1", 1)),
    ),
    _case("overlay_reservation_target", _with_room(), _with_reservation(target_id="bed-x")),
    _case("overlay_reservation_future", _with_room(), _with_reservation(reserved_minute=99)),
    _case(
        "overlay_reservation_duplicate",
        _with_room(),
        _with_reservation(),
        _with_reservation("A002"),
    ),
    _case("overlay_valid_admission", _with_room(), _with_admission()),
    _case(
        "overlay_admission_agent_mismatch",
        _with_room(),
        _at(*OVERLAY, "room_admissions", "A003", value=_admission("A002")),
    ),
    _case("overlay_admission_agent_unknown", _with_room(), _with_admission("A999")),
    _case("overlay_admission_room_unknown", _with_admission()),
    _case("overlay_admission_future", _with_room(), _with_admission(admitted_minute=99)),
    _case(
        "overlay_admissions_over_capacity",
        _with_room(),
        _with_admission(),
        _with_admission("A002"),
        _with_admission("A006"),
    ),
]


def validate_places_fixture() -> dict[str, Any]:
    """`validate_places.json`: the cases of `temporary_spaces_v1`, `movement_v1`,
    `facility_queues_v1` and `geometry_overlay_v1`."""
    return cases_fixture(SPACE_CASES + MOVEMENT_CASES + QUEUE_CASES + OVERLAY_CASES)


# ---- shared_operations_v1 -----------------------------------------------------------------------

SHARED = ("model_state", "shared_operations_v1")
HISTORY = (*SHARED, "history")
HEADS = (*SHARED, "support_requests")
A001_REQUEST = "support-464131df94b1012ff8261017"
A007_REQUEST = "support-54390414266e615c91fbc528"


def _log_record(document: Doc, event_id: str) -> Doc:
    return next(
        record for record in document["event_log"] if record["event"]["event_id"] == event_id
    )


def _receipt(document: Doc, event_id: str) -> Doc:
    return next(
        item
        for item in _state(document, "shared_operations_v1")["history"]
        if item["event_id"] == event_id
    )


def _change_receipt(event_id: str, **changes: Any) -> Edit:
    """Change a receipt and its event's payload alike, so that they still link."""

    def edit(document: Doc) -> None:
        for row in (
            _receipt(document, event_id),
            _log_record(document, event_id)["event"]["payload"],
        ):
            row.update(copy.deepcopy(changes))

    return edit


def _change_receipt_in_log(event_id: str, **changes: Any) -> Edit:
    """Change only the payload of a receipt's event."""

    def edit(document: Doc) -> None:
        _log_record(document, event_id)["event"]["payload"].update(copy.deepcopy(changes))

    return edit


def _receipt_in_log(document: Doc, event_id: str) -> Doc:
    return copy.deepcopy(_log_record(document, event_id)["event"]["payload"])


def _append_event(event_type: str, payload: Doc, *, status: str = "applied") -> Edit:
    """An event realized at the world's minute, appended to the log."""

    def edit(document: Doc) -> None:
        log = document["event_log"]
        sequences = [record["event"]["sequence"] for record in log]
        sequences += [event["sequence"] for event in document["event_queue"]]
        sequence = max(sequences) + 1
        counters = document["id_counters"]
        counters["event"] = max(counters["event"], sequence + 1)
        minute = document["minute_index"]
        log.append(
            {
                "detail": None,
                "event": {
                    "causation_id": None,
                    "due_minute": minute,
                    "event_id": f"{event_type}-{sequence}",
                    "event_type": event_type,
                    "kind": "endogenous",
                    "payload": copy.deepcopy(payload),
                    "priority": 0,
                    "provenance": {},
                    "sequence": sequence,
                    "source_id": None,
                },
                "realized_minute": minute,
                "status": status,
            }
        )

    return edit


def _drop_receipt(event_id: str) -> Edit:
    def edit(document: Doc) -> None:
        history = _state(document, "shared_operations_v1")["history"]
        history[:] = [item for item in history if item["event_id"] != event_id]

    return edit


def _truncated(document: Doc) -> None:
    """The history emptied and marked truncated: the heads stand on their events alone."""
    state = _state(document, "shared_operations_v1")
    state["history"] = []
    state["history_truncated"] = True


def _request_entity(request_id: str, **changes: Any) -> Edit:
    def edit(document: Doc) -> None:
        _entity(document, "support_requests", request_id).update(changes)

    return edit


def _agent(agent_id: str, **changes: Any) -> Edit:
    def edit(document: Doc) -> None:
        _entity(document, "agents", agent_id).update(changes)

    return edit


def _receipt_of(event_type: str, document: Doc, **fields: Any) -> Doc:
    """A receipt of this type as its model dumps it, its defaults filled."""
    from sheltersim.contracts.operations import (
        StaffOperationReceipt,
        StaffSupportContinuityReceipt,
    )

    model = {
        "staff_support_continuity": StaffSupportContinuityReceipt,
        "staff_operation_transition": StaffOperationReceipt,
    }[event_type]
    return model.model_validate(fields).model_dump(mode="json")


def _append_receipt(
    receipt: Callable[[Doc], Doc], *, in_log: bool = True, status: str = "applied"
) -> Edit:
    """Append a receipt to the history, and its applied event to the log (next sequence)."""

    def edit(document: Doc) -> None:
        item = receipt(document)
        _state(document, "shared_operations_v1")["history"].append(item)
        if not in_log:
            return
        log = document["event_log"]
        sequences = [record["event"]["sequence"] for record in log]
        sequences += [event["sequence"] for event in document["event_queue"]]
        sequence = max(sequences) + 1
        counters = document["id_counters"]
        counters["event"] = max(counters["event"], sequence + 1)
        log.append(
            {
                "detail": None,
                "event": {
                    "causation_id": item.get("causation_id"),
                    "due_minute": item["minute"],
                    "event_id": item["event_id"],
                    "event_type": item["event_type"],
                    "kind": "endogenous",
                    "payload": copy.deepcopy(item),
                    "priority": 0,
                    "provenance": {},
                    "sequence": sequence,
                    "source_id": None,
                },
                "realized_minute": item["minute"],
                "status": status,
            }
        )

    return edit


def _continuity(document: Doc, **changes: Any) -> Doc:
    fields = {
        "event_id": "continuity-1",
        "request_id": A001_REQUEST,
        "agent_id": "A001",
        "task_id": "staff-support-75",
        "previous_staff_id": "staff_001",
        "status": "requeued",
        "minute": 13,
        "causation_id": "care-support-transition-77",
    }
    fields.update(changes)
    return _receipt_of("staff_support_continuity", document, **fields)


def _staff_operation(document: Doc, **changes: Any) -> Doc:
    fields = {
        "event_id": "staff-operation-1",
        "task_id": "staff-task-1",
        "staff_id": "staff_002",
        "task_kind": "cleaning",
        "target_zone_id": "toilet",
        "reason": "scheduled",
        "status": "completed",
        "minute": 14,
        "start_minute": 10,
        "arrival_minute": 12,
        "travel_minutes": 2,
        "outcome_observation": "not_measured",
        "total_state_change_count": 0,
        "causation_id": "cause-1",
    }
    fields.update(changes)
    return _receipt_of("staff_operation_transition", document, **fields)


def _completion(**changes: Any) -> Edit:
    """A retained staff completion of staff_operations_v1 for `_staff_operation`."""

    def edit(document: Doc) -> None:
        row = {
            "task_id": "staff-task-1",
            "complete_minute": 14,
            "staff_id": "staff_002",
            "task_kind": "cleaning",
            "target_id": None,
            "target_zone_id": "toilet",
            "reason": "scheduled",
            "status": "completed",
            "start_minute": 10,
            "arrival_minute": 12,
            "travel_minutes": 2,
            "causation_event_ids": ["cause-1"],
        }
        row.update(changes)
        _state(document, "staff_operations_v1")["history"].append(row)

    return edit


def _shared(name: str, *edits: Edit) -> Case:
    return Case(name, BASE, edit=_edits(*edits))


SHARED_CASES = [
    _shared("shared_not_dict", _at(*SHARED, value=[])),
    _shared("shared_policy", _at(*SHARED, "policy_version", value="other")),
    _shared("shared_extra_field", _at(*SHARED, "extra", value=1)),
    _shared("shared_truncated_text", _at(*SHARED, "history_truncated", value="no")),
    _shared("shared_without_history", _drop(*HISTORY)),
    _shared("shared_without_heads", _drop(*HEADS)),
    _shared("shared_receipt_unknown_type", _at(*HISTORY, 0, "event_type", value="other")),
    _shared("shared_receipt_bad_minute", _at(*HISTORY, 0, "minute", value=-1)),
    _shared("shared_receipt_old_bad", _at(*HISTORY, 0, "agent_id", value="")),
    _shared("shared_receipt_new_bad", _at(*HISTORY, -1, "agent_id", value="")),
    _shared("shared_head_key", _at(*HEADS, A007_REQUEST, "request_id", value="other")),
    _shared("shared_head_terminal", _at(*HEADS, A007_REQUEST, "status", value="completed")),
    _shared("shared_head_time", _at(*HEADS, A007_REQUEST, "created_minute", value=5)),
    _shared("shared_out_of_order", _at(*HISTORY, 3, "minute", value=3)),
    _shared("shared_future", _change_receipt("care-support-transition-77", minute=20)),
    _shared(
        "shared_duplicate",
        _at(
            *HISTORY,
            value=lambda d: [
                *_state(d, "shared_operations_v1")["history"],
                _state(d, "shared_operations_v1")["history"][-1],
            ],
        ),
    ),
    _shared("shared_unlinked_payload", _at(*HISTORY, 0, "source_ids", value=["ASM-OTHER"])),
    _shared("shared_unlinked_last_payload", _at(*HISTORY, -1, "source_ids", value=["ASM-OTHER"])),
    _shared("shared_unlinked_minute", _change_receipt("care-support-transition-77", minute=13)),
    _shared("shared_unlinked_missing", _append_receipt(lambda d: _continuity(d), in_log=False)),
    _shared("shared_unlinked_failed", _append_receipt(lambda d: _continuity(d), status="failed")),
    _shared(
        "shared_unlinked_causation",
        _append_receipt(lambda d: _continuity(d)),
        _at("event_log", -1, "event", "causation_id", value="other"),
    ),
    _shared(
        "shared_unlinked_source",
        _append_receipt(lambda d: _continuity(d)),
        _at("event_log", -1, "event", "source_id", value="ASM-OTHER"),
    ),
    _shared(
        "shared_old_cause_type",
        _append_receipt(lambda d: _continuity(d)),
        _drop("event_log", -1, "event", "payload", "cause_type"),
    ),
    _shared("shared_begins_assigned", _drop_receipt("care-support-transition-22")),
    _shared(
        "shared_identity_changed",
        _change_receipt("care-support-transition-33", household_id="H000009"),
    ),
    _shared(
        "shared_transition_out_of_order",
        _change_receipt("care-support-transition-33", status="requested"),
    ),
    _shared("shared_head_mismatch", _at(*HEADS, A001_REQUEST, "updated_minute", value=11)),
    _shared("shared_head_missing", _drop(*HEADS, A007_REQUEST)),
    _shared(
        "shared_head_without_request",
        _at(
            "support_requests",
            value=lambda d: [row for row in d["support_requests"] if row["id"] != A007_REQUEST],
        ),
    ),
    _shared("shared_head_status_disagrees", _request_entity(A007_REQUEST, status="assigned")),
    _shared("shared_head_unresolved", _request_entity(A007_REQUEST, status="unresolved")),
    _shared(
        "shared_head_unresolved_timeout",
        _request_entity(A007_REQUEST, status="unresolved"),
        _agent("A007", support_request_id=None),
        _append_event("support_request_timeout", {"request_id": A007_REQUEST}),
    ),
    _shared(
        "shared_head_unresolved_failed_timeout",
        _request_entity(A007_REQUEST, status="unresolved"),
        _append_event("support_request_timeout", {"request_id": A007_REQUEST}, status="failed"),
    ),
    _shared(
        "shared_assigned_staff_disagrees",
        _request_entity(A001_REQUEST, assigned_staff_id="staff_002"),
    ),
    _shared("shared_truncated_heads", _truncated),
    _shared(
        "shared_truncated_head_minute",
        _truncated,
        _at(*HEADS, A007_REQUEST, "updated_minute", value=1),
    ),
    _shared(
        "shared_truncated_head_event",
        _truncated,
        _at(*HEADS, A007_REQUEST, "latest_event_id", value="care-support-transition-16"),
    ),
    _shared(
        "shared_truncated_head_failed",
        _truncated,
        _at(
            "event_log",
            value=lambda d: [
                {**r, "status": "failed"}
                if r["event"]["event_id"] == "care-support-transition-19"
                else r
                for r in d["event_log"]
            ],
        ),
    ),
    _shared(
        "shared_truncated_head_receipt",
        _truncated,
        _change_receipt_in_log("care-support-transition-19", support_kind="other"),
    ),
    _shared(
        "shared_truncated_head_disagrees",
        _truncated,
        _at(*HEADS, A007_REQUEST, "household_id", value="H000009"),
    ),
    _shared("shared_continuity_requeued", _append_receipt(lambda d: _continuity(d))),
    _shared(
        "shared_continuity_requeued_head",
        _append_receipt(lambda d: _continuity(d)),
        _at(
            *HEADS,
            A001_REQUEST,
            value=lambda d: {
                **_state(d, "shared_operations_v1")["support_requests"][A001_REQUEST],
                "status": "requested",
                "updated_minute": 13,
                "latest_event_id": "continuity-1",
            },
        ),
        _request_entity(A001_REQUEST, status="pending", assigned_staff_id=None, task_id=None),
    ),
    _shared(
        "shared_continuity_unresolved",
        _append_receipt(lambda d: _continuity(d, status="unresolved")),
    ),
    _shared(
        "shared_continuity_without_care",
        _append_receipt(
            lambda d: _continuity(d, request_id="support-87cc42433937ba8716311ea1", agent_id="A006")
        ),
    ),
    _shared(
        "shared_continuity_other_agent", _append_receipt(lambda d: _continuity(d, agent_id="A002"))
    ),
    _shared(
        "shared_continuity_self_cause",
        _append_receipt(lambda d: {**_continuity(d), "causation_id": "continuity-1"}),
    ),
    _shared(
        "shared_truncated_created_late",
        _truncated,
        _at(*HISTORY, value=lambda d: [_receipt_in_log(d, "care-support-transition-77")]),
        _at(*HEADS, A001_REQUEST, "created_minute", value=13),
        _at(*HEADS, A001_REQUEST, "updated_minute", value=13),
    ),
    _shared(
        "shared_truncated_history_kept",
        _truncated,
        _at(*HISTORY, value=lambda d: [_receipt_in_log(d, "care-support-transition-77")]),
    ),
    _shared(
        "shared_truncated_continuity",
        _truncated,
        _append_receipt(
            lambda d: _continuity(
                d,
                request_id=A007_REQUEST,
                agent_id="A007",
                causation_id="care-support-transition-19",
                event_id="care-support-transition-19x",
            )
        ),
    ),
    _shared(
        "shared_truncated_continuity_unknown",
        _truncated,
        _append_receipt(lambda d: _continuity(d, request_id="support-new", agent_id="A002")),
    ),
    _shared(
        "shared_truncated_continuity_unresolved",
        _truncated,
        _append_receipt(
            lambda d: _continuity(d, request_id="support-new", agent_id="A002", status="unresolved")
        ),
    ),
    _shared(
        "shared_truncated_continuity_early",
        _truncated,
        _at(
            *HEADS,
            A007_REQUEST,
            value=lambda d: {
                **_state(d, "shared_operations_v1")["support_requests"][A007_REQUEST],
                "created_minute": 14,
                "updated_minute": 14,
                "latest_event_id": "continuity-1",
            },
        ),
        _append_receipt(
            lambda d: _continuity(
                d,
                request_id=A007_REQUEST,
                agent_id="A007",
                causation_id="care-support-transition-19",
            )
        ),
    ),
    _shared(
        "shared_staff_operation", _completion(), _append_receipt(lambda d: _staff_operation(d))
    ),
    _shared("shared_staff_operation_unretained", _append_receipt(lambda d: _staff_operation(d))),
    _shared(
        "shared_staff_operation_truncated_later",
        _completion(complete_minute=15),
        _at(*SHARED, "history_truncated", value=True),
        _append_receipt(lambda d: _staff_operation(d)),
    ),
    _shared(
        "shared_staff_operation_twice",
        _completion(),
        _completion(),
        _append_receipt(lambda d: _staff_operation(d)),
    ),
    _shared(
        "shared_staff_operation_disagrees",
        _completion(reason="other"),
        _append_receipt(lambda d: _staff_operation(d)),
    ),
    _shared(
        "shared_staff_operation_cause",
        _completion(causation_event_ids=[]),
        _append_receipt(lambda d: _staff_operation(d)),
    ),
    _shared(
        "shared_staff_operation_bad_count",
        _completion(),
        _append_receipt(lambda d: {**_staff_operation(d), "total_state_change_count": 2}),
    ),
]


def validate_shared_operations_fixture() -> dict[str, Any]:
    """`validate_shared_operations.json`: the cases of `shared_operations_v1` and its history."""
    return cases_fixture(SHARED_CASES)
