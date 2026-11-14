"""The answers of the input contracts (policy E15) the core's `contracts::validate` must give
(Issue #346, part of #250).

The native host answers `Host::contract` with pydantic (`native/src/host.rs`, `contract`): the
model's `model_dump(mode="json")`, or the error `model_validate` raised. The core answers the same
without Python (`native/core/src/contracts/`), and `native/core/tests/contracts.rs` compares its
answer with the one recorded here for each case: the dump, or the error's class and `str(exc)`.

Each case is asked of the native host itself (`sheltersim_native.host_call(..., "contract", ...)`),
so that its input reaches pydantic as the kernels' calls give it: an event's values frozen, the
other inputs as dicts and lists. A case is a valid input of the contract (`bases`) changed by a
patch, or an input of its own: the generic changes put a panel of values at each field (types,
coercions, bounds, IDs, long and non-ASCII texts), remove each field and add an unknown one; the
cases written by hand reach each validator's refusals, several errors at once and long inputs.
The inputs go through JSON with sorted keys first, as the Rust test reads them.
"""

from __future__ import annotations

import json
from typing import Any

import pydantic

# The contracts of each fixture file, by the name `host_call` takes (`call_on` of
# native/src/host.rs): an operator's name is the model of its parameters (`_PARAMETER_MODELS`).
EFFECT_CONTRACTS = (
    "guard",
    "presence_command",
    "add_temporary_space",
    "facility_repair",
    "modify_demand_rate",
    "set_ambient_noise",
    "set_lighting",
    "set_privacy",
    "set_staff_role",
    "set_temperature",
    "set_ventilation",
    "staff_handoff",
    "staff_task_order",
)
OVERLAY_CONTRACTS = (
    "route_access",
    "equipment_transfer",
    "move_facility",
    "partition",
    "temporary_room",
)
GEOMETRY_CONTRACTS = ("facility_geometry", "spatial_constraints")
POPULATION_CONTRACTS = (
    "population_spec",
    "person_traits",
    "arrival_context",
    "zone_environment",
    "infrastructure",
)

JA = "日本語の長い識別子をここに書いて切り詰めを確かめます"

# The values put at each field of a valid input.
PANEL: tuple[Any, ...] = (
    None,
    True,
    0,
    -1,
    1.5,
    -0.0,
    10**30,
    "x",
    "",
    " a ",
    "1",
    "yes",
    "bad id",
    "a\n",
    "a" * 70,
    JA,
    [],
    ["x"],
    {},
)
# The values put at the nested fields.
NESTED: tuple[Any, ...] = (None, 1.5, "bad id", [], 10**30)
EXTRA = "zz_extra"


def _point(x: Any, y: Any) -> dict[str, Any]:
    return {"x": x, "y": y}


def _square(left: float, top: float, right: float, bottom: float) -> list[dict[str, Any]]:
    return [_point(left, top), _point(right, top), _point(right, bottom), _point(left, bottom)]


def _guard_chain(levels: int) -> dict[str, Any]:
    leaf = {"operator": "eq", "field": "zone.occupancy", "value": 1}
    node: dict[str, Any] = dict(leaf)
    for _ in range(levels):
        node = {"operator": "and", "operands": [node, dict(leaf)]}
    return node


LEAF = {"operator": "eq", "field": "zone.occupancy", "value": 3}
ORDERED = {"operator": "gt", "field": "simulation.minute", "value": 1.5}

BASES: dict[str, dict[str, Any]] = {
    # contracts_effects
    "guard": {"operator": "and", "operands": [LEAF, ORDERED]},
    "guard_leaf": LEAF,
    "presence_command": {
        "action": "temporary_exit",
        "agent_id": "agent-1",
        "expected_return_minute": 120,
        "source_ids": ["SRC-1"],
    },
    "add_temporary_space": {"people_capacity": 10, "sleeping_capacity": 4},
    "facility_repair": {
        "facility_id": "toilet-1",
        "component_id": "pump",
        "expected_working_units": 2,
        "restored_working_units": 1,
    },
    "modify_demand_rate": {
        "action": "rest",
        "probability_multiplier": 1.5,
        "duration_multiplier": 0.75,
    },
    "set_ambient_noise": {"level": 0.4},
    "set_lighting": {"level": 0.2},
    "set_privacy": {"level": 1},
    "set_staff_role": {"skills": ["medic", "intake"]},
    "set_temperature": {"temperature_c": 28.5},
    "set_ventilation": {"effectiveness": 0.9},
    "staff_handoff": {
        "staff_id": "staff-1",
        "replacement_staff_id": "staff-2",
        "task_id": "task-1",
        "reason_code": "shift_end",
    },
    "staff_task_order": {
        "task_kind": "maintenance",
        "target_zone_id": "hall",
        "target_id": "toilet-1",
        "reason": "the pump is broken",
    },
    # contracts_overlay
    "route_access": {"accessible": False},
    "equipment_transfer": {
        "equipment_id": "bed-1",
        "from_zone_id": "hall",
        "to_zone_id": "annex",
        "point_m": _point(12.5, 3),
        "source_ids": ["SRC-1"],
    },
    "move_facility": {"point_m": _point(1.5, 2)},
    "partition": {"polygon_m": [_point(0, 0), _point(4, 0), _point(4, 3)]},
    "temporary_room": {
        "room_id": "room-1",
        "zone_id": "hall",
        "polygon_m": _square(0, 0, 4, 4),
        "people_capacity": 4,
        "sleeping_capacity": 2,
        "bed_points_m": [_point(1, 1), _point(3, 3)],
        "route_edge_ids": ["e1"],
    },
    "facility_geometry": {
        "width_m": 20,
        "height_m": 10.0,
        "source_kind": "synthetic",
        "source_ids": ["SRC-1"],
        "zones": [
            {"zone_id": "hall", "polygon": _square(0, 0, 10, 10), "usable_area_m2": 80},
            {"zone_id": "annex", "polygon": _square(10, 0, 16, 10), "usable_area_m2": 60.0},
        ],
        "facility_points": {"toilet-1": _point(1, 1)},
        "service_points": [
            {
                "id": "sp-1",
                "facility_id": "toilet-1",
                "point_m": _point(1, 1),
                "service_capacity": 1,
                "queue_id": "q-1",
            }
        ],
        "routes": [{"edge_id": "e1", "points": [_point(1, 1), _point(9, 9)], "width_m": 1.2}],
        "arrival_approach_routes": [
            {
                "route_id": "door",
                "destination_zone_id": "annex",
                "points_m": [_point(19, 5), _point(12, 5)],
                "walking_speed_m_per_min": 60,
                "source_kind": "operator",
                "source_ids": ["SRC-1"],
            }
        ],
    },
    "spatial_constraints": {
        "walls": [{"id": "w1", "start": _point(0, 0), "end": _point(10, 0)}],
        "openings": [
            {
                "id": "o1",
                "wall_id": "w1",
                "center": _point(5, 0),
                "width_m": 0.9,
                "edge_ids": ["e1"],
            }
        ],
        "obstacles": [{"id": "ob1", "polygon": [_point(1, 1), _point(2, 1), _point(2, 2)]}],
        "equipment_footprints": [
            {"facility_id": "toilet-1", "polygon": _square(3, 3, 4, 4), "area_m2": 1}
        ],
        "planning_capacity_by_zone": {"hall": 30},
        "accessible_facility_ids": ["toilet-1"],
    },
    # contracts_population
    "person_traits": {
        "age_years": 34,
        "household_id": "hh-1",
        "household_role": "adult",
        "languages": ["ja", "en"],
        "medical_support": False,
    },
    "population_spec": {
        "people": {
            "p1": {"age_years": 34, "household_id": "hh-1"},
            "p2": {"age_years": 7, "household_role": "child", "medical_support": True},
        },
        "source_ids": ["SRC-1"],
    },
    "arrival_context": {"member_index": 0, "care_support_needs": ["medical_assessment"]},
    "zone_environment": {
        "policy_version": "zone-environment-v2",
        "noise_rest_efficiency_loss": 0.4,
        "max_active_modifiers_per_zone": 8,
        "baseline_by_zone": {"hall": {"temperature_c": 22, "lighting_level": 0.3}},
    },
    "zone_environment_legacy": {"noise_rest_efficiency_loss": 0.4, "min_rest_decision_minutes": 5},
    "infrastructure": {
        "dependency_edges": [
            {"dependent_id": "pump", "dependency_id": "power"},
            {"dependent_id": "light", "dependency_id": "power"},
        ],
        "heat_stress_per_minute": 0.002,
    },
}


def _case(
    contract: str, base: str | None = None, patch: list[list[Any]] | None = None, value: Any = None
) -> dict[str, Any]:
    case: dict[str, Any] = {"contract": contract}
    if base is None:
        case["input"] = value
    else:
        case["base"] = base
        case["patch"] = patch or []
    return case


def _set(path: list[Any], value: Any) -> list[Any]:
    return [path, value]


def _paths(value: Any, prefix: list[Any], depth: int) -> list[tuple[list[Any], bool]]:
    """The paths under `value` to change, each with whether its parent is a dict (whose key a patch
    may remove); the first item only of a list."""
    found: list[tuple[list[Any], bool]] = []
    if depth > 2:
        return found
    if isinstance(value, dict):
        for key, item in value.items():
            found.append(([*prefix, key], True))
            found.extend(_paths(item, [*prefix, key], depth + 1))
    elif isinstance(value, list) and value:
        found.append(([*prefix, 0], False))
        found.extend(_paths(value[0], [*prefix, 0], depth + 1))
    return found


def _generic(contract: str, base: str) -> list[dict[str, Any]]:
    """The base, each field at each value of the panel, each field removed, an unknown field at
    each dict, and every top field at once at a wrong type."""
    value = BASES[base]
    cases = [_case(contract, base)]
    for path, in_dict in _paths(value, [], 0):
        panel = PANEL if len(path) == 1 else NESTED
        cases.extend(_case(contract, base, [_set(path, item)]) for item in panel)
        if in_dict:
            cases.append(_case(contract, base, [[path]]))
    maps = [[]] + [path for path, _ in _paths(value, [], 0) if isinstance(_at(value, path), dict)]
    cases.extend(_case(contract, base, [_set([*path, EXTRA], 1)]) for path in maps)
    cases.append(
        _case(contract, base, [_set([key], "bad id") for key in value] + [_set([EXTRA], [1])])
    )
    cases.append(_case(contract, base, [[[key]] for key in value]))
    return cases


def _at(value: Any, path: list[Any]) -> Any:
    for step in path:
        value = value[step]
    return value


def _whole(contract: str) -> list[dict[str, Any]]:
    """Inputs that are not a dict, and the empty dict."""
    return [_case(contract, value=item) for item in (None, 5, "x" * 80, [], [{"a": 1}], {})]


def _effect_cases() -> list[dict[str, Any]]:
    cases: list[dict[str, Any]] = []
    for contract in EFFECT_CONTRACTS:
        cases.extend(_whole(contract))
        cases.extend(_generic(contract, contract))
    cases.extend(_generic("guard", "guard_leaf"))
    cases.append(_case("no_such_operator", value={"x": 1}))
    leaf, ordered = LEAF, ORDERED
    guards: list[Any] = [
        {"operator": "eq"},
        {"operator": "eq", "field": "zone.occupancy"},
        {"operator": "eq", "value": 1},
        {"operator": "eq", "field": "zone.occupancy", "value": 1, "operands": [leaf, leaf]},
        {"operator": "lt", "field": "zone.occupancy", "value": "x"},
        {"operator": "lte", "field": "zone.occupancy", "value": True},
        {"operator": "gte", "field": "zone.occupancy", "value": 10**30},
        {"operator": "ne", "field": "staff.available", "value": False},
        {"operator": "ne", "field": "target.capacity", "value": "y" * 256},
        {"operator": "ne", "field": "target.capacity", "value": "y" * 257},
        {"operator": "ne", "field": "target.capacity", "value": "あ" * 257},
        {"operator": "ne", "field": "target.capacity", "value": [1]},
        {"operator": "ne", "field": "target.capacity", "value": {"a": 1}},
        {"operator": "and", "operands": [leaf]},
        {"operator": "or", "field": "zone.occupancy", "operands": [leaf, leaf]},
        {"operator": "or", "value": 1, "operands": [leaf, leaf]},
        {"operator": "or", "operands": [leaf, ordered, leaf, ordered]},
        {"operator": "or", "operands": [leaf, ordered, leaf, ordered, leaf]},
        {"operator": "or", "operands": [leaf, "x", None, {"operator": "x"}, 3]},
        {"operator": "or", "operands": [leaf, "x", None, {"operator": "x"}, 3, 4]},
        {"operator": "or", "operands": "x"},
        {"operator": "or", "operands": {"operator": "eq"}},
        _guard_chain(3),
        _guard_chain(4),
        _guard_chain(5),
        {"operator": "and", "operands": [_guard_chain(1)] * 4},
        {
            "operator": "and",
            "operands": [{"operator": "or", "operands": [_guard_chain(1)] * 3}] * 2,
        },
        {
            "operator": "and",
            "operands": [{"operator": "or", "operands": [_guard_chain(1)] * 4}] * 2,
        },
        {"field": "x", "value": [1], "operands": 5, "zz": 1, "aa": None},
    ]
    cases.extend(_case("guard", value=guard) for guard in guards)
    params: list[tuple[str, Any]] = [
        ("add_temporary_space", {"people_capacity": 2, "sleeping_capacity": 3}),
        ("add_temporary_space", {"people_capacity": 3, "sleeping_capacity": 3}),
        ("add_temporary_space", {"people_capacity": 1001, "sleeping_capacity": 1001}),
        ("add_temporary_space", {"people_capacity": 0}),
        (
            "facility_repair",
            {
                "facility_id": "f",
                "component_id": "c",
                "expected_working_units": 10**30,
                "restored_working_units": -(10**30),
            },
        ),
        (
            "facility_repair",
            {
                "schema_version": 1.0,
                "facility_id": "f",
                "component_id": "c",
                "expected_working_units": 0,
                "restored_working_units": 0,
            },
        ),
        (
            "facility_repair",
            {
                "schema_version": True,
                "facility_id": "f",
                "component_id": "c",
                "expected_working_units": 0,
                "restored_working_units": 0,
                "source_ids": ["a", "a"],
            },
        ),
        (
            "facility_repair",
            {
                "schema_version": 2,
                "facility_id": "f",
                "component_id": "c",
                "expected_working_units": 0,
                "restored_working_units": 0,
            },
        ),
        (
            "facility_repair",
            {
                "schema_version": 10**30,
                "facility_id": "f",
                "component_id": "c",
                "expected_working_units": 0,
                "restored_working_units": 0,
            },
        ),
        ("modify_demand_rate", {"action": "sleep", "duration_multiplier": 2}),
        ("modify_demand_rate", {"action": "seek_help", "duration_multiplier": 1.5}),
        ("modify_demand_rate", {"action": "seek_help", "probability_multiplier": 1.0}),
        ("modify_demand_rate", {"action": "seek_help"}),
        ("modify_demand_rate", {"action": "fetch_water", "probability_multiplier": 0.5}),
        (
            "modify_demand_rate",
            {"action": "rest", "probability_multiplier": 3, "duration_multiplier": 0.1},
        ),
        ("set_staff_role", {"skills": ["medic", "medic"]}),
        ("set_staff_role", {"skills": []}),
        ("set_staff_role", {"skills": ["s" + str(index) for index in range(20)]}),
        ("set_staff_role", {"skills": ["s" + str(index) for index in range(21)]}),
        ("set_staff_role", {"skills": [1, 2] + ["s"] * 19}),
        ("set_staff_role", {"skills": [1] * 20}),
        ("set_staff_role", {"skills": [1] * 30}),
        ("set_staff_role", {"skills": ["a" * 64, "b" * 65, "", "c d"]}),
        ("set_staff_role", [1, 2]),
        ("set_temperature", {"temperature_c": -40}),
        ("set_temperature", {"temperature_c": 60.0000001}),
        ("set_temperature", {"temperature_c": 10**400}),
        ("set_temperature", {"temperature_c": 10**30}),
        ("set_ventilation", {"effectiveness": 1e-320}),
        (
            "staff_handoff",
            {"staff_id": "a", "replacement_staff_id": "a", "task_id": "t", "reason_code": "r"},
        ),
        (
            "staff_handoff",
            {"staff_id": "a", "replacement_staff_id": None, "task_id": "t", "reason_code": "r"},
        ),
        (
            "staff_task_order",
            {"task_kind": "cleaning", "target_zone_id": "z", "reason": "r", "target_id": "t"},
        ),
        ("staff_task_order", {"task_kind": "maintenance", "target_zone_id": "z", "reason": "r"}),
        ("staff_task_order", {"task_kind": "patrol", "target_zone_id": "z", "reason": "r" * 200}),
        ("staff_task_order", {"task_kind": "patrol", "target_zone_id": "z", "reason": "r" * 201}),
        (
            "staff_task_order",
            {"task_kind": "patrol", "target_zone_id": "z" * 65, "reason": "あ" * 201},
        ),
        ("presence_command", {"action": "relocate", "agent_id": "a"}),
        (
            "presence_command",
            {"action": "relocate", "agent_id": "a", "zone_id": "z", "expected_return_minute": 5},
        ),
        ("presence_command", {"action": "reunite", "agent_ids": ["a", "b"]}),
        ("presence_command", {"action": "return", "expected_return_minute": 1.0}),
        ("presence_command", {"action": "return", "agent_ids": ["a"] * 1000}),
        ("presence_command", {"action": "return", "agent_ids": ["a"] * 1001}),
        ("presence_command", {"action": "return", "source_ids": [1, 1] + ["a"] * 17}),
        ("presence_command", {"action": "return", "source_ids": ["a"] * 17 + [1]}),
        ("presence_command", {"action": "return", "source_ids": ["a"] * 16 + [1] * 3}),
        ("presence_command", {"action": "return", "agent_ids": [1, 2, 3], "source_ids": [1] * 17}),
    ]
    cases.extend(_case(contract, value=value) for contract, value in params)
    # The reprs of texts and numbers in a frozen input.
    texts = [
        "it's",
        'say "hi"',
        'it\'s "both"',
        "back\\slash",
        "tab\tand\nnew",
        "\x01\x7f\x85",
        "\u200b\u3000",
        "😀" * 13,
        "é" * 30,
        "a😀" * 12,
        "あ" * 16,
        "あ" * 17,
    ]
    cases.extend(
        _case("staff_task_order", "staff_task_order", [_set(["target_zone_id"], text)])
        for text in texts
    )
    numbers = [1e16, 1e-5, 123456789012345678.0, 0.1 + 0.2, 5e-324, -1.5e300, 2**63, -(2**63) - 1]
    cases.extend(
        _case("set_lighting", value={"level": number, "x": [number, {"y": (number,)}]})
        for number in numbers
    )
    cases.append(
        _case("set_lighting", value={"level": 0.5, "x": [[], {}, [[1]], {"k": []}, ("a",)]})
    )
    return cases


def _overlay_cases() -> list[dict[str, Any]]:
    cases: list[dict[str, Any]] = []
    for contract in OVERLAY_CONTRACTS:
        cases.extend(_whole(contract))
        cases.extend(_generic(contract, contract))
    room = BASES["temporary_room"]
    rooms: list[list[list[Any]]] = [
        [_set(["polygon_m"], [_point(0, 0), _point(4, 4), _point(4, 0), _point(0, 4)])],
        [_set(["polygon_m"], [_point(0, 0), _point(0, 0), _point(4, 0)])],
        [_set(["polygon_m"], [_point(0, 0), _point(2, 2), _point(4, 4)])],
        [_set(["sleeping_capacity"], 5), _set(["bed_points_m"], [])],
        [_set(["sleeping_capacity"], 3)],
        [_set(["sleeping_capacity"], 0)],
        [_set(["sleeping_capacity"], 0), _set(["bed_points_m"], [])],
        [_set(["bed_points_m"], [_point(1, 1), _point(4, 2)])],
        [_set(["bed_points_m"], [_point(1, 1), _point(5, 5)])],
        [_set(["route_edge_ids"], ["e1", "e1"])],
        [_set(["route_edge_ids"], [])],
        [_set(["source_ids"], ["s", "s"])],
        [_set(["people_capacity"], 0), _set(["sleeping_capacity"], -1), _set(["polygon_m"], [])],
        [_set(["polygon_m"], [_point(index % 8, index // 8) for index in range(65)])],
        [_set(["polygon_m"], [_point("x", None), 5, _point(1, "1e400")])],
        [
            _set(["bed_points_m"], [_point(1, 1)] * 2),
            _set(["polygon_m"], _square(0, 0, 1e-5, 1e-5)),
        ],
    ]
    cases.extend(_case("temporary_room", "temporary_room", patch) for patch in rooms)
    del room
    points: list[Any] = [
        _point(True, False),
        _point("1_0", " 2.5 "),
        _point("inf", "-inf"),
        _point("nan", "1e400"),
        _point(10**400, "_1"),
        _point(10**30, 1e308),
        _point("1__0", "1_"),
        _point(".5", "5."),
        _point("+.5e-3", "Infinity"),
        _point([1], {"x": 1}),
        _point(None, "x"),
    ]
    cases.extend(_case("move_facility", value={"point_m": point}) for point in points)
    others: list[tuple[str, Any]] = [
        ("route_access", {"accessible": 1}),
        ("route_access", {"accessible": "true"}),
        ("equipment_transfer", {**BASES["equipment_transfer"], "to_zone_id": "hall"}),
        ("partition", {"polygon_m": [_point(0, 0), 5] * 40}),
        ("partition", {"polygon_m": [5] * 65}),
        ("partition", {"polygon_m": [_point(0, 0), _point(1, 0)]}),
        ("partition", {"polygon_m": [_point(0, 0)] * 65}),
    ]
    cases.extend(_case(contract, value=value) for contract, value in others)
    return cases


def _geometry_cases() -> list[dict[str, Any]]:
    cases: list[dict[str, Any]] = []
    for contract in GEOMETRY_CONTRACTS:
        cases.extend(_whole(contract))
        cases.extend(_generic(contract, contract))
    geometry: list[list[list[Any]]] = [
        [_set(["zones", 1, "zone_id"], "hall")],
        [_set(["routes"], [BASES["facility_geometry"]["routes"][0]] * 2)],
        [_set(["width_m"], 15)],
        [_set(["facility_points", "far"], _point(-1, 0))],
        [_set(["service_points", 0, "point_m"], _point(1, 10.5))],
        [_set(["arrival_approach_routes", 0, "destination_zone_id"], "nowhere")],
        [_set(["arrival_approach_routes", 0, "points_m"], [_point(19, 5), _point(17, 5)])],
        [_set(["arrival_approach_routes", 0, "points_m"], [_point(5, 5), _point(12, 5)])],
        [_set(["arrival_approach_routes", 0, "points_m"], [_point(19, 5), _point(19, 5)])],
        [_set(["arrival_approach_routes", 0, "source_ids"], ["a", "a"])],
        [_set(["arrival_approach_routes", 0, "source_ids"], [])],
        [_set(["arrival_approach_routes", 0, "walking_speed_m_per_min"], 120.5)],
        [_set(["service_points"], [BASES["facility_geometry"]["service_points"][0]] * 2)],
        [
            _set(
                ["service_points", 1],
                {**BASES["facility_geometry"]["service_points"][0], "id": "sp-2"},
            )
        ],
        [
            _set(
                ["service_points", 0, "access"],
                {"accessibility": "restricted", "required_access_tags": ["wheelchair"]},
            )
        ],
        [
            _set(
                ["service_points", 0, "access"],
                {"accessibility": "maybe", "required_staff_skills": ["x"] * 65},
            )
        ],
        [_set(["service_points", 0, "service_capacity"], 1_000_001)],
        [_set(["zones", 0, "usable_area_m2"], 100.00000001)],
        [_set(["zones", 0, "usable_area_m2"], 100.0000001)],
        [_set(["zones", 0, "polygon"], [_point(0, 0), _point(1, 1), _point(2, 2)])],
        [_set(["zones", 0, "polygon"], [_point(0, 0)] * 129)],
        [_set(["zones"], [])],
        [_set(["units"], "ft"), _set(["origin"], "center"), _set(["schema_version"], 2)],
        [_set(["facility_points"], {"bad key": _point(1, 1), "ok": 5, JA: None})],
        [_set(["routes", 0, "accessible"], "off"), _set(["routes", 0, "width_m"], "1_000.5")],
        [_set(["routes", 0, "points"], [_point(" 1 ", "2.5e0"), _point(True, "nan")])],
    ]
    cases.extend(_case("facility_geometry", "facility_geometry", patch) for patch in geometry)
    spatial: list[list[list[Any]]] = [
        [
            _set(
                ["planning_capacity_by_zone"],
                {
                    "a": "12",
                    "b": " 7 ",
                    "c": "1_0",
                    "d": "12.00",
                    "e": "-0",
                    "f": "+5",
                    "g": True,
                    "h": 1e18,
                    "i": 2**63,
                    "j": "99999999999999999999999",
                    "k": "1." + "0" * 5000,
                    "l": "1_000.000",
                    "m": "　12",
                    "n": -0.0,
                    "o": 9.2e18,
                    "p": "0000000000000000000012",
                },
            )
        ],
        [
            _set(
                ["planning_capacity_by_zone"],
                {
                    "a": "0x10",
                    "b": "1e3",
                    "c": float(2**63),
                    "d": -9.2233720368547758e18,
                    "e": "1__0",
                    "f": "12.",
                    "g": ".0",
                    "h": "-",
                    "i": "",
                    "j": "+-1",
                    "k": "9" * 4301,
                    "l": "12.0_0",
                    "m": 1.5,
                    "n": "١٢",
                    "bad key": "x",
                    "o": [],
                    "p": None,
                    "q": "_1",
                    "r": "1_",
                },
            )
        ],
        [_set(["equipment_footprints", 0, "polygon"], _square(0, 0, 1, 1)[:3])],
        [_set(["equipment_footprints", 0, "polygon"], _square(0, 0, 1, 1) + [_point(0, 0)])],
        [_set(["openings", 0, "accessible"], "No"), _set(["openings", 0, "edge_ids"], "e1")],
        [
            _set(["openings", 0, "accessible"], 0.0),
            _set(["walls", 0, "start"], _point("_1", "1_0")),
        ],
        [_set(["obstacles", 0, "polygon"], [_point(0, 0)] * 129)],
        [_set(["obstacles", 0, "polygon"], [5] * 3 + [_point(0, 0)] * 129)],
        [_set(["walls"], [{"id": "w" + str(index)} for index in range(12)])],
    ]
    cases.extend(_case("spatial_constraints", "spatial_constraints", patch) for patch in spatial)
    # pydantic-core's parse of a text as an int or a float.
    ints = [
        "+0000000000000000000012",
        "-1_000_000_000_000_000_000",
        "1__000000000000000000000",
        "1000000000000000000_",
        "_1000000000000000000000",
        "9" * 4300,
        "-" + "9" * 4300,
        "0" * 4301,
        "1_000_000_000_000_000_000.000",
        " -9223372036854775808 ",
        "9223372036854775808",
        "-+1",
        "+_1",
        "1 000",
        "1\n",
        "\t5\t",
        "12.0\n",
        "1.00e0",
        "0b1",
        "  ",
        "-_1",
        "+1_0",
        "0_1",
        "00_1",
        "0_0",
        "_0",
        "1_",
        "+1__0",
        "0__1",
        "0_",
        "-0_0_1",
        "+0_1",
        "-01",
        "0" * 4300 + "1",
        "-" + "0" * 4301,
        "0_" * 2200 + "1",
        "00.00",
        "0_0.0",
        "+00",
        "0x10",
        "0_.0",
        "-0",
        "+",
        "00000000000000000001_0",
        "1111111111111111111_0",
        "-" + "1" * 4299 + ".0",
        "1" * 4300,
        "+" + "1" * 4300,
        "0" * 4300 + "1" * 4300,
    ]
    cases.append(
        _case(
            "spatial_constraints",
            value={
                "planning_capacity_by_zone": {f"k{index}": text for index, text in enumerate(ints)}
            },
        )
    )
    floats = [
        "1._5",
        "1_.5",
        " 1_0 ",
        "1_0 ",
        "١",
        "1e5_0",
        "+inf",
        "-Infinity",
        "iNf",
        "NaN",
        "-nan",
        "0x1p3",
        "1e",
        "e1",
        ".",
        "+.5",
        "-5.",
        "1,5",
        "１２",
        "1__0.5",
    ]
    cases.extend(
        _case("facility_geometry", "facility_geometry", [_set(["routes", 0, "width_m"], text)])
        for text in floats
    )
    return cases


def _population_cases() -> list[dict[str, Any]]:
    cases: list[dict[str, Any]] = []
    for contract in POPULATION_CONTRACTS:
        cases.extend(_whole(contract))
        cases.extend(_generic(contract, contract))
    cases.extend(_generic("zone_environment", "zone_environment_legacy"))
    traits: list[Any] = [
        {"age_years": True},
        {"age_years": "12.000"},
        {"age_years": 12.0, "medical_support": "YES"},
        {"age_years": 12.5, "medical_support": " yes"},
        {"age_years": "1_2", "medical_support": 2},
        {"age_years": 121, "medical_support": 1.0, "languages": "ja"},
        {"age_years": -1, "medical_support": "off", "languages": []},
        {"age_years": 1e20, "medical_support": "T"},
        {"age_years": "x", "medical_support": "N", "household_role": "parent"},
        {"age_years": 3, "languages": ["ja", "ja", "en"], "accessibility_needs": ["wheelchair"]},
        {"age_years": 3, "languages": ["not a language", "", None, 5, "a" * 65, "a" * 64]},
        {"age_years": 3, "household_id": "日本"},
    ]
    cases.extend(_case("person_traits", value=value) for value in traits)
    cases.extend(
        _case("population_spec", value=value)
        for value in (
            {"people": {"p": {"age_years": 1}, "bad id": None}, "source_ids": None},
            {"people": []},
            {"people": {}, "schema_version": 1.0},
            {"people": {"p" + str(index): {"age_years": "x"} for index in range(12)}},
        )
    )
    arrivals: list[Any] = [
        {"member_index": 0},
        {"member_index": 0, "care_support_needs": ["medical_assessment", "medical_assessment"]},
        {"member_index": 999, "care_support_needs": ["language_access"] * 9},
        {"member_index": 1000, "care_support_needs": ["unknown"]},
        {
            "member_index": 1.0,
            "care_support_needs": list(BASES["arrival_context"]["care_support_needs"]),
        },
    ]
    cases.extend(_case("arrival_context", value=value) for value in arrivals)
    environments: list[Any] = [
        {},
        {"policy_version": "zone-environment-v1"},
        {"policy_version": "zone-environment-v1", "lighting_sleep_efficiency_loss": 0.1},
        {"policy_version": "zone-environment-v1", "baseline_by_zone": {"hall": {}}},
        {"policy_version": "zone-environment-v1", "privacy_discontent_per_minute": -0.0},
        {"lighting_sleep_efficiency_loss": 0.0},
        {"baseline_by_zone": {}},
        {"min_probability_multiplier": 1.0, "max_probability_multiplier": 1.0},
        {
            "min_probability_multiplier": 0.9,
            "max_probability_multiplier": 1.0,
            "comfortable_min_temperature_c": 30,
        },
        {"min_rest_decision_minutes": 61, "max_rest_decision_minutes": 60},
        {"min_rest_decision_minutes": 30, "max_rest_decision_minutes": 20},
        {
            "min_probability_multiplier": 1.0,
            "max_probability_multiplier": 1.0,
            "policy_version": "zone-environment-v2",
            "comfortable_max_temperature_c": 18,
        },
        {
            "noise_rest_efficiency_loss": 2,
            "min_rest_decision_minutes": 0,
            "max_rest_decision_minutes": 300,
        },
        {"policy_version": "zone-environment-v3"},
        {"policy_version": 1, "source_id": "  ab  "},
        {"source_id": " " * 3, "baseline_by_zone": {" hall ": {"temperature_c": 61}, "": {}}},
        {"baseline_by_zone": {"a": {"x": 1}, "b": None, "c": {"privacy_level": True}}},
        {"min_probability_multiplier": 0.5, "max_probability_multiplier": 0.4, "zz": 1},
    ]
    cases.extend(_case("zone_environment", value=value) for value in environments)
    keys = [
        {" hall": {"lighting_level": 0.1}, "hall": {"lighting_level": 0.2}, "annex ": {}},
        {"\u3000ab\u3000": {}, "a b": {}},
    ]
    cases.extend(_case("zone_environment", value={"baseline_by_zone": value}) for value in keys)
    bools = ["TRUE", "On", "f", "n", "No", "y", "Yes ", "oN", "0", "1", "2", "true\n", "İ", ""]
    cases.extend(
        _case("person_traits", value={"age_years": 1, "medical_support": text}) for text in bools
    )
    cases.extend(
        _case("person_traits", value={"age_years": 1, "medical_support": number})
        for number in (0.0, -0.0, 1.0, 0.5, -1, 10**30)
    )
    edges = [
        [{"dependent_id": "b", "dependency_id": "a"}, {"dependent_id": "a", "dependency_id": "0"}],
        [{"dependent_id": "b", "dependency_id": "a"}, {"dependent_id": "b", "dependency_id": "a"}],
        [{"dependent_id": "a", "dependency_id": "a"}],
        [{"dependent_id": "b", "dependency_id": "a"}, {"dependent_id": "a", "dependency_id": "b"}],
        [
            {"dependent_id": "c", "dependency_id": "b"},
            {"dependent_id": "b", "dependency_id": "a"},
            {"dependent_id": "a", "dependency_id": "c"},
        ],
        [
            {"dependent_id": " b ", "dependency_id": "a"},
            {"dependent_id": "b", "dependency_id": " a"},
        ],
        [
            {"dependent_id": "z" + str(index), "dependency_id": "y" + str(index)}
            for index in range(30)
        ],
        [{"dependent_id": "", "dependency_id": "x" * 65}, {"dependent_id": "a"}, 5],
    ]
    cases.extend(_case("infrastructure", value={"dependency_edges": value}) for value in edges)
    cases.extend(
        _case("infrastructure", value=value)
        for value in (
            {"heat_stress_per_minute": 1e-9, "cold_stress_per_minute": 0},
            {"heat_stress_per_minute": "0.005", "cold_stress_per_minute": True},
            {"heat_stress_per_minute": 10**400, "cold_stress_per_minute": "nan"},
            {"source_id": "  ab  ", "policy_version": "x", "source_type": "measured"},
        )
    )
    return cases


def _normal(value: Any) -> Any:
    """The value as the Rust test reads it from the fixture: through JSON, its keys sorted."""
    return json.loads(json.dumps(value, sort_keys=True))


def _apply(value: Any, patch: list[list[Any]]) -> Any:
    for change in patch:
        path = change[0]
        parent = _at(value, path[:-1])
        if len(change) == 1:
            del parent[path[-1]]
        elif isinstance(parent, list) and path[-1] == len(parent):
            parent.append(_normal(change[1]))
        else:
            parent[path[-1]] = _normal(change[1])
    return value


def _answer(controller: Any, contract: str, value: Any) -> dict[str, Any]:
    import sheltersim_native

    from sheltersim import native

    try:
        dump = sheltersim_native.host_call(controller, "contract", (contract, value))
    except pydantic.ValidationError as exc:
        return {"raises": ["Value", str(exc)]}
    except KeyError as exc:
        return {"raises": ["Key", str(exc)]}
    except TypeError as exc:
        return {"raises": ["Type", str(exc)]}
    except native.Unsupported as exc:
        return {"raises": ["Unsupported", str(exc)]}
    return {"dump": dump}


def _controller() -> Any:
    from sheltersim.core.world import WorldState
    from sheltersim.experiments.metrics import MetricsRecorder
    from sheltersim.experiments.runner import OfflineSimulationController
    from sheltersim.experiments.scenario import ScenarioSpec
    from sheltersim.experiments.timeline import _MetricSink
    from support.paths import SCENARIO_100_AGENTS

    scenario = ScenarioSpec.model_validate(json.loads(SCENARIO_100_AGENTS.read_text()))
    return OfflineSimulationController(
        WorldState.from_scenario(scenario), _MetricSink(MetricsRecorder())
    )


def _document(contracts: tuple[str, ...], cases: list[dict[str, Any]]) -> dict[str, Any]:
    controller = _controller()
    used: set[str] = set()
    seen: set[str] = set()
    out: list[dict[str, Any]] = []
    for case in cases:
        if "base" in case:
            used.add(case["base"])
            value = _apply(_normal(BASES[case["base"]]), case["patch"])
        else:
            value = _normal(case["input"])
        key = json.dumps([case["contract"], value], sort_keys=True)
        if key in seen:
            continue
        seen.add(key)
        out.append({**case, **_answer(controller, case["contract"], value)})
    return {
        "pydantic": pydantic.VERSION,
        "contracts": list(contracts),
        "bases": {name: BASES[name] for name in sorted(used)},
        "cases": out,
    }


def contracts_effects_fixture() -> dict[str, Any]:
    """The guard, the effects' parameters and the presence command: an event's values."""
    return _document(EFFECT_CONTRACTS, _effect_cases())


def contracts_overlay_fixture() -> dict[str, Any]:
    """The overlay's parameters and the temporary room."""
    return _document(OVERLAY_CONTRACTS, _overlay_cases())


def contracts_geometry_fixture() -> dict[str, Any]:
    """The facility geometry and its spatial constraints."""
    return _document(GEOMETRY_CONTRACTS, _geometry_cases())


def contracts_population_fixture() -> dict[str, Any]:
    """The population's contracts and the configs of the zone environment and the infrastructure."""
    return _document(POPULATION_CONTRACTS, _population_cases())
