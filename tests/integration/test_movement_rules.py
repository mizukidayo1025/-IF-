"""The Python entries of native/src/rules/movement.rs, the rules of domain/movement.py.

The rules have no Python rule left (`kernel_only`). Their branches are checked without Python in
`native/core/tests/movement.rs` (hand-built worlds), `movement_rules.rs` and
`movement_walk_order.rs` (the generated Scenario's worlds, `support/core_fixtures_movement.py`,
#225). Here are the entries: each still reaches the kernel and gives its answer, the mirror of the
movement state, what taints it, what the entries read before the kernel (a route's class, the
values `start_movement` is given) and what the configs' scope keeps, and the comparisons with the
Python checks that are left (`canonical_movement_config`, `validate_movement_runtime_state`).
`test_each_tick_of_a_run_is_python_s_bit_for_bit` chains, over a Run, what `advance_movement`
returned and the whole world after each call, in canonical JSON (which keeps a float's bits), into
one digest, compared with the deleted Python rule's (ANSWERS, at the end); the random floor plans,
the answers of the geometry's kernels with the deleted rules' (RANDOM_PLAN_ANSWERS).
"""

from __future__ import annotations

import copy
import decimal
import hashlib
import math
import random
from collections.abc import Iterator
from dataclasses import replace
from typing import Any

import pytest
from support.core_fixtures_movement import ROUTE_MODELS, deleted_minutes, routes_of, walker

from sheltersim import native
from sheltersim.contracts.extensions import FacilityGeometry, Point2D
from sheltersim.core.engine import TickEngine
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.world import WorldState
from sheltersim.domain import movement
from sheltersim.domain.geometry_overlay import geometry_config_scope
from sheltersim.domain.models import Activity
from sheltersim.domain.movement import MOVEMENT_STATE_KEY as KEY
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.timeline import _MetricSink
from sheltersim.policies.space import ZoneRoute


def _rules() -> Any:
    return native.rules("movement")


def _run(scenario: Any, minutes: int) -> WorldState:
    world = WorldState.from_scenario(scenario)
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    engine = TickEngine(world, phase_handler=controller)
    for _ in range(minutes):
        engine.step()
    return world


def _bytes(world: WorldState) -> str:
    """The whole world, floats by their bits. A hand-made state need not pass validation."""
    return world._canonical_text(world._canonical_data())


def _mirror(world: WorldState) -> Any:
    return world._core.mirror(KEY)


def _answer(world: WorldState, result: Any) -> bytes:
    """What the rule returned and left, as the deleted rule's answers were recorded."""
    return repr((repr(result), _bytes(world))).encode()


# Each test's answers, chained in the order of its calls of `_same_after`.
_CHAINS: dict[str, Any] = {}
_NODE = [""]


@pytest.fixture(autouse=True)
def _answers_are_the_deleted_rule_s(request: pytest.FixtureRequest) -> Iterator[None]:
    """After each test, its chained answers against the deleted Python rule's (ANSWERS).

    The worlds hold floats (the floor plan's lengths and points), recorded on Linux; they are the
    same bits on every OS, as the gate's hashes are (lane L13).
    """
    _NODE[0] = request.node.name
    yield
    chain = _CHAINS.pop(request.node.name, None)
    if chain is not None:
        assert chain.hexdigest() == ANSWERS[request.node.name]


def _same_after(world: WorldState, name: str, *arguments: Any, **options: Any) -> Any:
    """The rule on the world, its answer chained for the test's check, the row still the tree's."""
    result = getattr(movement, name)(world, *arguments, **options)
    _CHAINS.setdefault(_NODE[0], hashlib.sha256()).update(_answer(world, result) + b"\n")
    assert _mirror(world).tainted is None
    assert _mirror(world).verify()
    assert _mirror(world).row_json() == native.canonical_json(world.model_state[KEY])
    return result


def _kernel_after(world: WorldState, name: str, *arguments: Any, **options: Any) -> Any:
    """A rule of no Python rule, with the mirror of the namespace still the row's."""
    result = getattr(movement, name)(world, *arguments, **options)
    assert _mirror(world).tainted is None
    assert _mirror(world).verify()
    assert _mirror(world).row_json() == native.canonical_json(world.model_state[KEY])
    return result


def _digest(texts: list[str]) -> str:
    """Answers by their repr, whose floats are exact: recorded once from the deleted rules."""
    digest = hashlib.sha256()
    for text in texts:
        digest.update(text.encode() + b"\n")
    return digest.hexdigest()


def _record(
    zone_id: str, point: Any, kind: str, edge_id: Any = None, progress: float = 0.0
) -> dict:
    return {
        "zone_id": zone_id,
        "point": None if point is None else {"x": point.x, "y": point.y},
        "edge_id": edge_id,
        "progress": progress,
        "position_kind": kind,
    }


def _refuses(world: WorldState, name: str, *arguments: Any, **options: Any) -> str:
    """Unsupported, with nothing changed."""
    before = _bytes(world)
    forwards = _mirror(world).stats()
    with pytest.raises(native.Unsupported) as refused:
        getattr(_rules(), name)(world, *arguments, **options)
    assert _bytes(world) == before
    assert _mirror(world).stats() == forwards
    return str(refused.value)


# ---- Every tick ---------------------------------------------------------------------------------


def test_each_tick_of_a_run_is_python_s_bit_for_bit(generated_scenario: Any) -> None:
    world = WorldState.from_scenario(generated_scenario)
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    engine = TickEngine(world, phase_handler=controller)
    ran = 0
    for _ in range(90):
        engine.step()
        state = world.model_state[KEY]
        ran += bool(state["journeys"])
        state["last_updated_minute"] = world.minute_index - 1
        _same_after(world, "advance_movement", world.minute_index)
    assert ran > 10


def test_a_kernel_forwards_nothing_and_writes_no_namespace(generated_scenario: Any) -> None:
    world = _run(generated_scenario, 30)
    stats = _mirror(world).stats()
    assert _rules().advance_movement(world, world.minute_index) == ()
    assert _mirror(world).stats()["forwards"] == stats["forwards"]
    assert _mirror(world).stats()["whole_writes"] == stats["whole_writes"]


def test_a_person_who_stands_still_is_not_written_again(generated_scenario: Any) -> None:
    world = _run(generated_scenario, 30)
    state = world.model_state[KEY]
    _rules().advance_movement(world, world.minute_index)
    still = next(
        agent_id
        for agent_id, agent in world.agents.items()
        if agent_id not in state["journeys"] and agent.activity is not Activity.QUEUED
    )
    record = state["positions"][still]
    _rules().advance_movement(world, world.minute_index + 1)
    assert state["positions"][still] is record


def _plain(value: Any) -> Any:
    if isinstance(value, dict):
        return {key: _plain(item) for key, item in value.items()}
    if isinstance(value, list):
        return [_plain(item) for item in value]
    return value


# ---- What the kernels take ----------------------------------------------------------------------


def test_int_coordinates_taint_and_are_a_kernel_refusal(generated_scenario: Any) -> None:
    """An int where the row has a float: only a hand-made state has one, and nothing changes."""
    world = _run(generated_scenario, 5)
    state = world.model_state[KEY]
    state["positions"][sorted(world.agents)[0]]["progress"] = 0
    assert _mirror(world).tainted
    before = _bytes(world)
    with pytest.raises(KernelRefusal, match="the namespace has no row"):
        movement.advance_movement(world, world.minute_index + 1)
    assert _bytes(world) == before


def test_a_state_past_the_table_is_not_the_row_s(generated_scenario: Any) -> None:
    world = _run(generated_scenario, 5)
    dict.__setitem__(world.model_state, KEY, _plain(world.model_state[KEY]))
    with pytest.raises(native.Unsupported):
        _rules().advance_movement(world, world.minute_index + 1)


def test_an_edge_the_state_does_not_know_is_a_kernel_refusal(generated_scenario: Any) -> None:
    """The deleted rule's KeyError: a journey on an edge the world lacks, which no Run makes."""
    world = _run(generated_scenario, 5)
    agent_id = walker(world)
    state = world.model_state[KEY]
    state["journeys"][agent_id]["route_edge_ids"][0] = "no-such-edge"
    state["last_updated_minute"] = world.minute_index - 1
    before = _bytes(world)
    with pytest.raises(KernelRefusal, match="a journey's edge is unknown"):
        movement.advance_movement(world, world.minute_index)
    assert _bytes(world) == before


def test_a_config_edited_outside_a_scope_is_seen_at_the_next_call(generated_scenario: Any) -> None:
    world = _run(generated_scenario, 2)
    geometry = world.model_configs["facility_geometry"]
    geometry["arrival_approach_routes"] = []
    zone_id = sorted(world.zones)[0]
    first = _rules().zone_center(world, zone_id)
    assert first == _python_center(world, zone_id)
    zone = next(item for item in geometry["zones"] if item["zone_id"] == zone_id)
    zone["polygon"] = [{"x": p["x"] * 0.5, "y": p["y"] * 0.5} for p in zone["polygon"]]
    zone["usable_area_m2"] = 1e-6
    assert _rules().zone_center(world, zone_id) == _python_center(world, zone_id) != first


def test_a_moved_facility_is_seen_by_value(generated_scenario: Any) -> None:
    world = _run(generated_scenario, 2)
    geometry = movement._geometry(world)
    agent_id = sorted(world.agents)[0]
    with geometry_config_scope():
        facility_id = next(iter(world.model_configs["facility_geometry"]["facility_points"]))
        zone_id = world.facilities[facility_id].zone_id
        center = movement._zone_center(geometry, zone_id)
        world.agents[agent_id].zone_id = zone_id
        world.model_state[KEY]["positions"][agent_id]["point"] = {"x": center.x, "y": center.y}
        before = _kernel_after(world, "facility_approach_route", agent_id, facility_id)
        original = world.model_configs["facility_geometry"]["facility_points"][facility_id]
        moved = {"x": (original["x"] * 3 + center.x) / 4, "y": (original["y"] * 3 + center.y) / 4}
        world.model_state["geometry_overlay_v1"]["facility_points"][facility_id] = {
            "point_m": moved,
            "minute": 1,
            "source_ids": [],
            "event_id": "moved",
        }
        after = _kernel_after(world, "facility_approach_route", agent_id, facility_id)
        assert before is not None and after is not None
        assert repr(before) != repr(after)
        # Without a footprint the walk ends at the facility's point, where it is now.
        assert (before[1][-1].x, before[1][-1].y) == (original["x"], original["y"])
        assert (after[1][-1].x, after[1][-1].y) == (moved["x"], moved["y"])


def _python_center(world: WorldState, zone_id: str) -> tuple[float, float] | None:
    point = movement._zone_center(movement._geometry(world), zone_id)
    return None if point is None else (point.x, point.y)


# ---- start_movement, finish_movement, routes ----------------------------------------------------


def test_start_movement_with_only_half_of_the_support_writes_neither(
    generated_scenario: Any,
) -> None:
    world = _run(generated_scenario, 5)
    edge = next(iter(world.zone_edges.values()))
    route = ZoneRoute(
        edge.from_zone_id, edge.to_zone_id, (edge.from_zone_id, edge.to_zone_id), (edge.id,), 3
    )
    _kernel_after(
        world,
        "start_movement",
        agent_id=sorted(world.agents)[2],
        route=route,
        purpose="walk",
        facility_id="f",
        started_minute=9,
        due_minute=11,
        event_id="e",
        task_id="t",
        support_request_id="request",
    )
    assert "support_request_id" not in world.model_state[KEY]["journeys"][sorted(world.agents)[2]]


@pytest.mark.parametrize(
    "change",
    [
        {"started_minute": 9, "due_minute": 9},
        {"purpose": 1},
        {"route": ZoneRoute("a", "b", ["a", "b"], (), 3)},  # type: ignore[arg-type]
    ],
)
def test_start_movement_refuses_what_it_does_not_decide(
    generated_scenario: Any, change: dict[str, Any]
) -> None:
    world = _run(generated_scenario, 5)
    arguments = {
        "agent_id": sorted(world.agents)[2],
        "route": ZoneRoute("a", "b", ("a", "b"), (), 3),
        "purpose": "walk",
        "facility_id": None,
        "started_minute": 9,
        "due_minute": 11,
        "event_id": "e",
        "task_id": "t",
        **change,
    }
    _refuses(world, "start_movement", **arguments)


def test_finish_movement_of_each_kind(generated_scenario: Any) -> None:
    world = _run(generated_scenario, 5)
    agent_id = walker(world)
    state = world.model_state[KEY]
    state["journeys"][agent_id]["purpose"] = "toilet_queue_position"
    state["positions"][agent_id].update(edge_id=next(iter(world.zone_edges)), progress=0.5)
    point = dict(state["positions"][agent_id]["point"])
    zone_id = state["positions"][agent_id]["zone_id"]
    _kernel_after(world, "finish_movement", agent_id)
    # Still reserved in the queue: the person keeps their point, off the edge.
    assert agent_id not in state["journeys"]
    assert state["positions"][agent_id] == {
        "zone_id": zone_id,
        "point": point,
        "edge_id": None,
        "progress": 0.0,
        "position_kind": "physical",
    }
    # No journey now: the person stands where a person stands, the centre of their zone.
    world.agents[agent_id].activity = Activity.IDLE
    _kernel_after(world, "finish_movement", agent_id)
    zone_id = world.agents[agent_id].zone_id
    center = movement._zone_center(movement._geometry(world), zone_id)
    assert state["positions"][agent_id] == _record(zone_id, center, "physical")
    # Nobody by that name: nothing to place.
    before = _bytes(world)
    _kernel_after(world, "finish_movement", "nobody")
    assert _bytes(world) == before
    # Without the namespace, nothing at all.
    del world.model_state[KEY]
    movement.finish_movement(world, agent_id)
    assert KEY not in world.model_state


def test_a_route_s_minutes_follow_speed_and_crowding(generated_scenario: Any) -> None:
    """The entry on a crowded room with a model of floats: the minutes of the deleted rule's
    arithmetic. The core's test checks every model and area (`movement_rules.rs`)."""
    from sheltersim.domain.models import Mobility

    world = _run(generated_scenario, 5)
    for zone in world.model_configs["facility_geometry"]["zones"]:
        zone["usable_area_m2"] = 5.0
    for agent in world.agents.values():
        agent.zone_id = next(iter(world.zone_edges.values())).from_zone_id
    world.model_configs["movement_model"] = copy.deepcopy(ROUTE_MODELS[1])
    mobilities = [Mobility.INDEPENDENT, Mobility.ASSISTED, Mobility.WHEELCHAIR]
    for index, agent_id in enumerate(sorted(world.agents)[:3]):
        agent = world.agents[agent_id]
        agent.profile = replace(agent.profile, mobility=mobilities[index])
        for route in routes_of(world):
            minutes = deleted_minutes(world, agent_id, route)
            assert repr(movement.route_with_movement_time(world, agent_id, route)) == repr(
                replace(route, travel_minutes=minutes)
            )


def test_a_route_s_minutes_without_a_model_leave_the_route(generated_scenario: Any) -> None:
    world = _run(generated_scenario, 5)
    world.model_configs.pop("movement_model", None)
    del world.model_configs["facility_geometry"]
    route = routes_of(world)[0]
    assert _rules().route_with_movement_time(world, sorted(world.agents)[0], route) is route


@pytest.mark.parametrize(
    "model",
    [
        {"extra": 1},
        {"speed_m_per_minute_by_mobility": {"independent": 60}},
        {"maximum_density_slowdown": 0.5},
        {"density_slowdown_per_person_m2": True},
        {"schema_version": 2},
    ],
)
def test_a_model_a_scenario_turns_down_is_a_kernel_refusal(
    generated_scenario: Any, model: dict[str, Any]
) -> None:
    """A Scenario's movement_model passes canonical_movement_config, so no Run holds these."""
    with pytest.raises(ValueError, match="movement_model"):
        movement.canonical_movement_config(model)
    world = _run(generated_scenario, 5)
    world.model_configs["movement_model"] = model
    route = routes_of(world)[0]
    with pytest.raises(KernelRefusal, match="route_with_movement_time refused"):
        movement.route_with_movement_time(world, sorted(world.agents)[0], route)


# ---- Random floor plans -------------------------------------------------------------------------


def _cos_sin(angle: float) -> tuple[float, float]:
    """cos and sin of `angle` by their series in decimal, rounded once to a float: the same bits on
    every OS. math.cos and math.sin are the platform's libm, whose last bits differ between glibc
    and macOS (and glibc's are not always rounded correctly)."""
    with decimal.localcontext() as context:
        context.prec = 60
        x = decimal.Decimal(angle)
        # The terms x**n / n! with their signs: cos takes n = 0, 2, 4, ..., sin n = 1, 3, 5, ...
        sums = [decimal.Decimal(0)] * 4
        term = decimal.Decimal(1)
        n = 0
        while n < 8 or abs(term) > decimal.Decimal("1e-58"):
            sums[n % 4] += term
            n += 1
            term = term * x / n
        return float(sums[0] - sums[2]), float(sums[1] - sums[3])


def _random_polygon(
    rng: random.Random, cx: float, cy: float, radius: float
) -> list[dict[str, float]]:
    count = rng.randint(3, 9)
    angles = sorted(rng.uniform(0.0, math.tau) for _ in range(count))
    points = []
    for angle in angles:
        cos, sin = _cos_sin(angle)
        points.append(
            {
                "x": cx + rng.uniform(0.2, 1.0) * radius * cos,
                "y": cy + rng.uniform(0.2, 1.0) * radius * sin,
            }
        )
    return points


def _random_plan(rng: random.Random) -> tuple[dict[str, Any], dict[str, Any]]:
    size = 100.0
    zones = []
    for index in range(4):
        cx, cy = rng.uniform(25, 75), rng.uniform(25, 75)
        polygon = _random_polygon(rng, cx, cy, rng.choice([0.5, 5.0, 20.0]))
        if rng.random() < 0.1:
            # No area at all: the mean of its corners.
            polygon = [{"x": cx, "y": cy}, {"x": cx + 1.0, "y": cy}, {"x": cx + 2.0, "y": cy}]
        zones.append(
            {
                "zone_id": f"z{index}",
                "polygon": polygon,
                "usable_area_m2": rng.uniform(1e-6, 1e-5),
            }
        )
    routes = [
        {
            "edge_id": f"e{index}",
            "points": [
                {"x": rng.uniform(0, size), "y": rng.uniform(0, size)}
                for _ in range(rng.randint(2, 4))
            ],
            "width_m": rng.choice([0.4, 0.85, 1.2, 3.0]),
            "accessible": rng.random() < 0.7,
        }
        for index in range(5)
    ]
    geometry = {
        "width_m": size,
        "height_m": size,
        "source_kind": "synthetic",
        "zones": zones,
        "routes": routes,
    }
    walls = [
        {
            "id": f"w{index}",
            "start": {"x": rng.uniform(0, size), "y": rng.uniform(0, size)},
            "end": {"x": rng.uniform(0, size), "y": rng.uniform(0, size)},
        }
        for index in range(rng.randint(0, 3))
    ]
    openings = []
    for index, wall in enumerate(walls):
        share = rng.random()
        openings.append(
            {
                "id": f"o{index}",
                "wall_id": wall["id"],
                "center": {
                    "x": wall["start"]["x"] + (wall["end"]["x"] - wall["start"]["x"]) * share,
                    "y": wall["start"]["y"] + (wall["end"]["y"] - wall["start"]["y"]) * share,
                },
                "width_m": rng.uniform(0.5, 30.0),
                "edge_ids": [f"e{rng.randrange(5)}"],
                "accessible": rng.random() < 0.7,
            }
        )
    constraints = {
        "walls": walls,
        "openings": openings,
        "obstacles": [
            {
                "id": f"b{index}",
                "polygon": _random_polygon(
                    rng, rng.uniform(10, 90), rng.uniform(10, 90), rng.uniform(0.5, 8.0)
                ),
            }
            for index in range(rng.randint(0, 4))
        ],
    }
    return geometry, constraints


def _inside(rng: random.Random, polygon: list[dict[str, float]]) -> Point2D:
    a, b, c = rng.sample(polygon, 3)
    x, y = sorted((rng.random(), rng.random()))
    return Point2D(
        x=a["x"] * x + b["x"] * (y - x) + c["x"] * (1 - y),
        y=a["y"] * x + b["y"] * (y - x) + c["y"] * (1 - y),
    )


# The deleted Python rules' answers on the plans below, recorded from them before they went.
RANDOM_PLAN_ANSWERS = "8ac3860105498b78fbf55ebe38d7ef2f34a92a17138af65026c7d05a06d662a2"


def test_random_floor_plans_are_the_deleted_rules_bit_for_bit() -> None:
    rng = random.Random(20261101)
    seen = {"centers": 0, "clear": 0, "blocked": 0, "routes": 0, "no_route": 0, "turns": 0}
    seen["turned_down"] = 1
    answers: list[str] = []
    for _ in range(250):
        geometry, constraints = _random_plan(rng)
        try:
            FacilityGeometry.model_validate(geometry)
        except ValueError:
            seen["turned_down"] += 1
            continue
        world = WorldState(
            "s", 1, 100, model_state={}, model_configs={"facility_geometry": geometry}
        )
        if rng.random() < 0.8:
            world.model_configs["spatial_constraints"] = constraints
        for zone in geometry["zones"]:
            assert _rules().zone_center(world, zone["zone_id"]) == _python_center(
                world, zone["zone_id"]
            )
            seen["centers"] += 1
        for route in geometry["routes"]:
            for mobility in ("independent", "assisted", "wheelchair", "other"):
                clear = movement.route_is_physically_clear(
                    world, route["edge_id"], mobility=mobility
                )
                answers.append(repr(clear))
                seen["clear" if clear else "blocked"] += 1
        for zone in geometry["zones"]:
            for mobility in ("independent", "assisted", "wheelchair"):
                start = _inside(rng, zone["polygon"]) if rng.random() < 0.9 else None
                destination = _inside(rng, zone["polygon"])
                options = {
                    "zone_id": zone["zone_id"],
                    "start": start,
                    "destination": destination,
                    "mobility": mobility,
                }
                path = movement.plan_in_room_route(world, **options)
                answers.append(repr(path))
                seen["routes" if path else "no_route"] += 1
                seen["turns"] += bool(path) and len(path) > 2
    assert seen == {
        "centers": 660,
        "clear": 791,
        "blocked": 2509,
        "routes": 1409,
        "no_route": 571,
        "turns": 7,
        "turned_down": 86,
    }
    # The same bits on every OS: the plans are drawn without the platform's libm (_cos_sin).
    assert _digest(answers) == RANDOM_PLAN_ANSWERS


def test_the_route_of_a_plan_is_found_again_the_same(generated_scenario: Any) -> None:
    world = WorldState.from_scenario(generated_scenario)
    geometry = movement._geometry(world)
    zone = geometry.zones[0]
    center = movement._zone_center(geometry, zone.zone_id)
    options = {
        "zone_id": zone.zone_id,
        "start": center,
        "destination": center,
        "mobility": "independent",
    }
    with geometry_config_scope():
        first = _rules().plan_in_room_route(world, **options)
        assert repr(first) == repr(_rules().plan_in_room_route(world, **options))


# ---- The validator ------------------------------------------------------------------------------


def _set(*path: Any) -> Any:
    def change(world: WorldState, agent_id: str) -> None:
        record: Any = world.model_state[KEY]
        for part in path[:-2]:
            record = record[agent_id if part is None else part]
        record[path[-2]] = path[-1]

    return change


BROKEN_JOURNEYS = {
    "unknown destination": _set("journeys", None, "destination_zone_id", "nowhere"),
    "unknown zone on the route": _set("journeys", None, "route_zone_ids", ["nowhere"]),
    "unknown edge": _set("journeys", None, "route_edge_ids", ["nowhere"]),
    "due before it starts": _set("journeys", None, "due_minute", -5),
    "support fields on another trip": _set("journeys", None, "support_request_id", "r"),
    "queue on another trip": _set("journeys", None, "queue_id", "q"),
    "one room point": _set("journeys", None, "route_points", [{"x": 1.0, "y": 1.0}]),
    "room points beyond the room": _set(
        "journeys", None, "route_points", [{"x": 1e6, "y": 1e6}, {"x": 2e6, "y": 2e6}]
    ),
    "position out of range": _set("positions", None, "progress", 1.5),
    "position on an unknown edge": _set("positions", None, "edge_id", "nowhere"),
    "another kind of position": _set("positions", None, "position_kind", "nowhere"),
    "a person nobody knows": _set(
        "positions",
        "nobody",
        {
            "zone_id": "hall",
            "point": None,
            "edge_id": None,
            "progress": 0.0,
            "position_kind": "unavailable",
        },
    ),
}


@pytest.mark.parametrize("broken", BROKEN_JOURNEYS)
def test_the_validator_takes_nothing_the_python_rule_refuses(
    generated_scenario: Any, broken: str
) -> None:
    world = _run(generated_scenario, 5)
    agent_id = walker(world)
    state = world.model_state[KEY]
    state["journeys"][agent_id]["purpose"] = "walk"
    assert _rules().validate(world) is True
    movement.validate_movement_runtime_state(world)
    BROKEN_JOURNEYS[broken](world, agent_id)
    try:
        accepted = _rules().validate(world)
    except native.Unsupported:
        accepted = False
    assert accepted is False
    with pytest.raises(ValueError, match="movement_v"):
        movement.validate_movement_runtime_state(world)


# The deleted Python rule's answers of each test, chained in the order of its calls: recorded
# once from the Python rule at 2d8fb46, on Linux.
ANSWERS = {
    "test_each_tick_of_a_run_is_python_s_bit_for_bit": (
        "2901640ba74176555a31b4c92a91465f617ded299f2e6ab28327397d43486510"
    ),
}
