"""The world of the overlay rules' tests, for pytest and for the core's tests (#225, lane M3).

`integration/test_overlay_rules.py` and `native/core/tests/overlay_rules.rs` start from the
generated Scenario's world at minute 0 with the simulator's extensions (`overlay_world`). A test
adds rooms, spaces and facilities to it (`room_record`, `space`), and calls the rules of
`domain/temporary_spaces.py` and `domain/geometry_overlay.py`, whose kernels are
`native/core/src/rules/overlay.rs`.

`overlay_rules_fixture` writes `native/core/tests/data/overlay_rules.json`: that world
(`export_world`), and what the pytest read in Python that the core's tests need as inputs: the
order of the world's dicts, the rules' limits, the Python points and cases of the tests next to a
limit, and the contracts' answers (`Model.model_validate(input).model_dump(mode="json")`) for the
parameters the tests give, which the core's `TestHost` gives back as the PyO3 crate's host does. A
dict whose order the core's test reads is written as `{"pairs": [[key, value], ...]}`. The core's
tests change the facility geometry and the spatial constraints of the configs the way the pytest
did, on the configs and on their sections both: `_config_is_its_section` checks that each of those
configs is what its contract makes of it (the sections `support/core_fixtures.py` exports).
"""

from __future__ import annotations

import copy
import json
from typing import Any

ROOM_ZONE = "sleeping"
# Where the Python rule placed four toilets in the generated toilet zone, in turn (lane L10).
PLACED = (
    (13.62417562100671, 9.737087810503356),
    (14.524175621006712, 9.737087810503356),
    (13.62417562100671, 11.21867753513065),
    (14.524175621006712, 11.21867753513065),
)
SEPARATIONS = (0.5999, 0.6, 0.6001, 0.603)


def overlay_world(scenario: Any) -> Any:
    """The world of a Scenario at minute 0, with the simulator's extensions."""
    from sheltersim.core.world import WorldState
    from sheltersim.domain.simulator_state import initialize_simulator_extensions

    world = WorldState.from_scenario(scenario)
    initialize_simulator_extensions(world)
    return world


def ordered(value: Any) -> Any:
    """A JSON value whose dicts keep their order: `{"pairs": [[key, value], ...]}`."""
    if isinstance(value, dict):
        return {"pairs": [[key, ordered(item)] for key, item in value.items()]}
    if isinstance(value, (list, tuple)):
        return [ordered(item) for item in value]
    return value


# ---- The records a test puts in the state --------------------------------------------------------


def space(space_id: str, zone_id: str, *, active: bool = True, people: int = 5) -> dict[str, Any]:
    return {
        "space_id": space_id,
        "zone_id": zone_id,
        "people_capacity": people,
        "sleeping_capacity": 2,
        "start_minute": 0,
        "end_minute": 100,
        "active": active,
        "start_event_id": "start",
        "end_event_id": None if active else "end",
        "program_id": "program",
        "effect_id": "effect",
        "source_ids": ["ASM-A"],
        "geometry_status": "zone_only_capacity",
    }


def room_record(world: Any, room_id: str, *, people: int = 2, beds: int = 1) -> dict[str, Any]:
    """A temporary room in the sleeping zone's corner, its beds down its middle."""
    geometry = world.model_configs["facility_geometry"]
    sleeping = next(zone for zone in geometry["zones"] if zone["zone_id"] == ROOM_ZONE)
    left = min(point["x"] for point in sleeping["polygon"])
    bottom = max(point["y"] for point in sleeping["polygon"])
    return {
        "schema_version": 1,
        "room_id": room_id,
        "zone_id": ROOM_ZONE,
        "polygon_m": [
            {"x": left + 0.5, "y": bottom - 4.4},
            {"x": left + 2.5, "y": bottom - 4.4},
            {"x": left + 2.5, "y": bottom - 0.4},
            {"x": left + 0.5, "y": bottom - 0.4},
        ],
        "people_capacity": people,
        "sleeping_capacity": beds,
        "bed_points_m": [
            {"x": left + 1.5, "y": bottom - 2.4 + 0.8 * index} for index in range(beds)
        ],
        "route_edge_ids": ["route_corridor_sleeping", "route_sleeping_corridor"],
        "source_ids": ["ASM-TEMP-ROOM-TEST-V1"],
        "start_minute": 0,
        "end_minute": world.duration_minutes,
        "active": True,
        "start_event_id": "start",
        "end_event_id": None,
        "program_id": "program",
        "effect_id": "effect",
    }


ROOM_FIELDS = (
    "schema_version",
    "room_id",
    "zone_id",
    "polygon_m",
    "people_capacity",
    "sleeping_capacity",
    "bed_points_m",
    "route_edge_ids",
    "source_ids",
)


def corner(world: Any, zone_id: str, *, x: float = 1.0, y: float = 1.0) -> list[dict]:
    geometry = world.model_configs["facility_geometry"]
    zone = next(item for item in geometry["zones"] if item["zone_id"] == zone_id)
    left = min(point["x"] for point in zone["polygon"]) + x
    top = min(point["y"] for point in zone["polygon"]) + y
    return [
        {"x": left, "y": top},
        {"x": left + 0.4, "y": top},
        {"x": left + 0.4, "y": top + 0.4},
        {"x": left, "y": top + 0.4},
    ]


def change_cases(world: Any) -> list[tuple[str, str, Any, int | None]]:
    """Changes of the floor plan, each check's way through: what the rule said of each is in the
    core's test (`CHANGE_TEXTS`)."""
    toilet = next(key for key, item in world.facilities.items() if item.kind == "toilet")
    geometry = world.model_configs["facility_geometry"]
    point = geometry["facility_points"][toilet]
    room = {
        key: value for key, value in room_record(world, "new-room").items() if key in ROOM_FIELDS
    }
    return [
        ("set_route_access", "route_corridor_sleeping", {"accessible": False}, 50),
        ("set_route_access", "route_corridor_sleeping", {"accessible": True}, 50),
        ("set_route_access", "route_corridor_sleeping", {"accessible": False}, None),
        ("set_route_access", "no_route", {"accessible": False}, 50),
        ("move_facility", toilet, {"point_m": {"x": point["x"] + 0.1, "y": point["y"]}}, None),
        ("move_facility", toilet, {"point_m": {"x": point["x"] + 0.1, "y": point["y"]}}, 50),
        ("move_facility", toilet, {"point_m": {"x": -1.0, "y": point["y"]}}, None),
        ("move_facility", "no_facility", {"point_m": {"x": 1.0, "y": 1.0}}, None),
        ("set_partition", ROOM_ZONE, {"polygon_m": corner(world, ROOM_ZONE)}, 50),
        ("set_partition", ROOM_ZONE, {"polygon_m": corner(world, ROOM_ZONE)}, None),
        ("set_partition", "no_zone", {"polygon_m": corner(world, ROOM_ZONE)}, 50),
        ("temporary_room_geometry", ROOM_ZONE, room, 50),
        ("temporary_room_geometry", ROOM_ZONE, room, None),
        ("temporary_room_geometry", "corridor", room, 50),
        (
            "transfer_equipment",
            toilet,
            {
                "schema_version": 1,
                "equipment_id": toilet,
                "from_zone_id": world.facilities[toilet].zone_id,
                "to_zone_id": "sleeping",
                "point_m": {"x": 5.0, "y": 9.0},
            },
            None,
        ),
        ("no_such_operator", ROOM_ZONE, {}, 50),
    ]


# ---- What the tests next to a limit computed in Python -------------------------------------------


def _xy(point: Any) -> list[float]:
    return [point.x, point.y]


def _toilet_zone(world: Any) -> tuple[Any, ...]:
    from sheltersim.contracts.extensions import Point2D

    geometry = world.model_configs["facility_geometry"]
    zone = next(item for item in geometry["zones"] if item["zone_id"] == "toilet")
    return tuple(Point2D.model_validate(point) for point in zone["polygon"])


def _others_in_the_toilet_zone(world: Any) -> list[str]:
    """The facilities of the toilet zone with a point, but the one a test adds (`extra`)."""
    geometry = world.model_configs["facility_geometry"]
    return [
        key
        for key, item in world.facilities.items()
        if item.zone_id == "toilet" and key in geometry["facility_points"]
    ]


def _separation(world: Any) -> dict[str, Any]:
    """For the core's `a_placed_facility_keeps_its_separation_from_the_others`: the first slot
    of the toilet zone's grid for one more facility, and the others there."""
    from sheltersim.region.facility import facility_grid_point

    others = _others_in_the_toilet_zone(world)
    slot = facility_grid_point(_toilet_zone(world), 0, len(others) + 1)
    return {"slot": _xy(slot), "others": others}


def _sixteen_grids(world: Any) -> list[dict[str, Any]]:
    """For the core's `the_search_of_a_grid_ends_after_sixteen_grids`: the last slot of the
    grids of 16 and 17 more facilities than the zone has, and whether the search reaches it."""
    from sheltersim.region.facility import facility_grid_point

    others = len(_others_in_the_toilet_zone(world))
    polygon = _toilet_zone(world)
    return [
        {
            "count": count,
            "slot": _xy(facility_grid_point(polygon, count - 1, count)),
            "found": found,
        }
        for count, found in ((others + 16, True), (others + 17, False))
    ]


def _walls(world: Any) -> list[dict[str, Any]]:
    """For the core's `a_moved_facility_keeps_half_a_metre_from_walls`: points either side of
    each wall of the plan, just inside and just past half a metre, and what the Python rule said
    of each."""
    from sheltersim.contracts.extensions import Point2D, point_in_polygon
    from sheltersim.contracts.spatial import SpatialConstraints

    toilet = next(key for key, item in world.facilities.items() if item.kind == "toilet")
    zone_id = world.facilities[toilet].zone_id
    geometry = world.model_configs["facility_geometry"]
    zone = next(item for item in geometry["zones"] if item["zone_id"] == zone_id)
    polygon = tuple(Point2D.model_validate(point) for point in zone["polygon"])
    walls = SpatialConstraints.model_validate(world.model_configs["spatial_constraints"]).walls
    cases = []
    for wall in walls:
        dx, dy = wall.end.x - wall.start.x, wall.end.y - wall.start.y
        length = (dx * dx + dy * dy) ** 0.5
        if length == 0:
            continue
        nx, ny = -dy / length, dx / length
        for fraction in (0.25, 0.5, 0.75):
            for side in (1, -1):
                for clearance in (0.499, 0.503):
                    point = {
                        "x": wall.start.x + dx * fraction + nx * side * clearance,
                        "y": wall.start.y + dy * fraction + ny * side * clearance,
                    }
                    if not point_in_polygon(Point2D.model_validate(point), polygon):
                        expected: str | None = (
                            "area_invalid: cross-zone facility moves are unsupported"
                        )
                    elif clearance < 0.5:
                        expected = (
                            "clear_width_insufficient: moved service point requires half a "
                            "metre from walls"
                        )
                    else:
                        expected = None
                    cases.append({"point": point, "clearance": clearance, "expected": expected})
    return cases


# ---- The contracts ----------------------------------------------------------------------------


def _contract(model: Any, parameters: Any) -> list[Any]:
    """The model's name, its input and its answer (the dump, in its order)."""
    dumped = model.model_validate(parameters).model_dump(mode="json")
    return [model.__name__, parameters, ordered(dumped)]


def _contracts(world: Any) -> list[list[Any]]:
    """What each contract answers for the parameters the core's tests give."""
    from sheltersim.contracts.operations import EquipmentTransferParameters, TemporaryRoomGeometry
    from sheltersim.domain.geometry_overlay import (
        MoveFacilityParameters,
        PartitionParameters,
        RouteAccessParameters,
    )

    models = {
        "set_route_access": RouteAccessParameters,
        "move_facility": MoveFacilityParameters,
        "set_partition": PartitionParameters,
        "temporary_room_geometry": TemporaryRoomGeometry,
        "transfer_equipment": EquipmentTransferParameters,
    }
    answers: list[list[Any]] = []

    def add(op: str, parameters: Any) -> None:
        answer = _contract(models[op], parameters)
        if answer not in answers:
            answers.append(answer)

    for op, _, parameters, _ in change_cases(world):
        if op in models:
            add(op, parameters)
    for case in _walls(world):
        add("move_facility", {"point_m": case["point"]})
    return answers


# ---- The configs a test changes ---------------------------------------------------------------


def _config_is_its_section(configs: dict[str, Any]) -> None:
    """The facility geometry and the spatial constraints of these configs are what their contracts
    make of them (the sections `support/core_fixtures.py` exports), but each approach route's
    `travel_minutes`: so a test may change both the same way."""
    from support.core_fixtures import _facility_geometry, _spatial_constraints

    raw = json.loads(json.dumps(configs["facility_geometry"]))
    section = copy.deepcopy(_facility_geometry(configs)["value"])
    for route in section["arrival_approach_routes"]:
        route.pop("travel_minutes")
    assert section == raw
    assert _spatial_constraints(configs)["value"] == configs["spatial_constraints"]


def _square(x: float, y: float, size: float) -> list[dict[str, float]]:
    return [
        {"x": x, "y": y},
        {"x": x + size, "y": y},
        {"x": x + size, "y": y + size},
        {"x": x, "y": y + size},
    ]


def _changed_configs(world: Any) -> None:
    """The configs of the tests next to a limit are their sections (`_config_is_its_section`)."""
    _config_is_its_section(world.model_configs)
    geometry = world.model_configs["facility_geometry"]
    slot = _separation(world)["slot"]
    for distance in SEPARATIONS:
        configs = copy.deepcopy(world.model_configs)
        for key in _separation(world)["others"]:
            configs["facility_geometry"]["facility_points"][key] = {
                "x": slot[0],
                "y": slot[1] + distance,
            }
        _config_is_its_section(configs)
    # The core's `a_bed_s_routes_end_where_the_python_rule_s_do`: an obstacle around a corner.
    configs = copy.deepcopy(world.model_configs)
    configs["spatial_constraints"].setdefault("obstacles", []).append(
        {"id": "around-the-corner", "polygon": _square(1.0, 1.0, 0.1), "source_ids": []}
    )
    _config_is_its_section(configs)
    # The core's `a_room_inspects_256_points_of_its_grid`: a wide hall and an L of an obstacle.
    configs = copy.deepcopy(world.model_configs)
    hall = configs["facility_geometry"]
    zone = next(item for item in hall["zones"] if item["zone_id"] == ROOM_ZONE)
    x0, side = geometry["width_m"], 20.0
    hall["width_m"] = x0 + side
    hall["height_m"] = max(hall["height_m"], side)
    zone["polygon"] = _square(x0, 0.0, side)
    corners = [(x0 - 1, -1.0), (x0 + 5.0, -1.0), (x0 + 5.0, 3.0), (x0 + 4.25, 3.0)]
    corners += [(x0 + 4.25, side + 1), (x0 - 1, side + 1)]
    configs["spatial_constraints"].setdefault("obstacles", []).append(
        {
            "id": "all-but-the-last",
            "polygon": [{"x": x, "y": y} for x, y in corners],
            "source_ids": [],
        }
    )
    _config_is_its_section(configs)


def overlay_rules_fixture() -> dict[str, Any]:
    """`overlay_rules.json`: the world of the overlay tests, with what they read in Python."""
    from sheltersim.domain import geometry_overlay, temporary_spaces
    from support.core_fixtures import export_world
    from support.core_fixtures_queues import generated

    scenario = generated()
    world = overlay_world(scenario)
    _changed_configs(world)
    entry = export_world(world)
    entry["order"] = {
        "zones": list(world.zones),
        "agents": list(world.agents),
        "facilities": list(world.facilities),
    }
    entry["limits"] = {
        "spaces": temporary_spaces.MAX_TEMPORARY_SPACES,
        "space_history": temporary_spaces.MAX_TEMPORARY_SPACE_HISTORY,
        "room_points": geometry_overlay.MAX_ROOM_ADMISSION_POINTS,
    }
    entry["spaces_state"] = temporary_spaces.create_temporary_spaces_state()
    entry["separation"] = _separation(world)
    entry["sixteen_grids"] = _sixteen_grids(world)
    entry["walls"] = _walls(world)
    entry["contracts"] = _contracts(world)
    return {"worlds": {"generated": entry}}
