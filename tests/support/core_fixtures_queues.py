"""The world of the queue rules' tests, for pytest and for the core's tests (Issue #225, lane M3).

`integration/test_queue_rules.py` and `native/core/tests/queue_rules.rs` start from the same world:
the generated Scenario (`support/generated.py`, the Project screen's) after `QUEUE_TICKS` ticks
through `TickEngine`. A test then lines a few people up at a facility (`line_up`) and calls the
queue rules (`domain/queueing.py`, `domain/movement.py`, whose kernels are
`native/core/src/rules/queues.rs`).

`queue_rules_fixture` writes `native/core/tests/data/queue_rules.json`: that world (`export_world`),
and what the pytest read of it in Python that the core's tests need as inputs: the facility of each
kind a test uses (`facility_of`), the people with nothing under way (`free_people`), the middle of
each zone (`line_up` stands people there), the configs and selection of the strict FIFO Scenario,
and the points and speeds the walking tests computed in Python on a world they had lined up.
"""

from __future__ import annotations

import math
import pathlib
import tempfile
from typing import Any

# The ticks the world has run before a test lines people up.
QUEUE_TICKS = 2
# The kinds of facility the tests line people up at.
KINDS = ("toilet", "water_station", "meal_distribution")
# The configs the strict FIFO Scenario selects its queue policy with.
STRICT_CONFIGS = ("policy_registry", "facility_queues")
KEY = "movement_v1"


def generated(**fields: Any) -> Any:
    """The `generated_scenario` (or `generated_scenario_with(**fields)`) of `support/generated.py`,
    made through the Project screen's preview as the fixture makes it."""
    from support.generated import preview_scenario

    return preview_scenario(pathlib.Path(tempfile.mkdtemp(prefix="core-fixture-queues-")), **fields)


def run_minutes(scenario: Any, minutes: int) -> tuple[Any, Any]:
    """The world and its controller after `minutes` ticks of `TickEngine`."""
    from sheltersim.core.engine import TickEngine
    from sheltersim.core.world import WorldState
    from sheltersim.experiments.metrics import MetricsRecorder
    from sheltersim.experiments.runner import OfflineSimulationController
    from sheltersim.experiments.timeline import _MetricSink

    world = WorldState.from_scenario(scenario)
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    engine = TickEngine(world, phase_handler=controller)
    for _ in range(minutes):
        engine.step()
    return world, controller


def queue_world(scenario: Any) -> Any:
    """The world a queue test starts from."""
    return run_minutes(scenario, QUEUE_TICKS)[0]


def facility_of(world: Any, kind: str) -> str:
    """The first facility, by ID, of this kind with a point in the floor plan."""
    from sheltersim.domain import movement

    return next(
        item.id
        for item in sorted(world.facilities.values(), key=lambda item: item.id)
        if kind in item.kind and item.id in movement._geometry(world).facility_points
    )


def free_people(world: Any, count: int) -> list[str]:
    """Agents with nothing under way, so what a test gives them is all they have."""
    out = []
    for agent_id in sorted(world.agents):
        if agent_id in world.model_state[KEY]["journeys"]:
            continue
        if any(task.agent_id == agent_id for task in world.active_tasks.values()):
            continue
        if any(
            agent_id in item
            for facility in world.facilities.values()
            for item in (
                facility.queue_agent_ids,
                facility.active_agent_ids,
                facility.resource_queue_agent_ids,
                facility.resource_active_agent_ids,
            )
        ):
            continue
        out.append(agent_id)
    return out[:count]


def line_up(
    world: Any,
    facility_id: str,
    people: list[str],
    *,
    resource: bool = False,
    starts: list[int] | None = None,
) -> None:
    """These people in this facility's queue, in the zone's middle, as `request` leaves them."""
    from sheltersim.domain import movement
    from sheltersim.domain.models import ActiveTask, Activity

    facility = world.facilities[facility_id]
    queue = facility.resource_queue_agent_ids if resource else facility.queue_agent_ids
    center = movement._zone_center(movement._geometry(world), facility.zone_id)
    for place, agent_id in enumerate(people):
        agent = world.agents[agent_id]
        agent.zone_id = facility.zone_id
        agent.activity = Activity.QUEUED
        agent.target_id = facility_id
        queue.append(agent_id)
        task_id = f"queue-task-{agent_id}"
        world.active_tasks[task_id] = ActiveTask(
            id=task_id,
            kind="resource_service_queue" if resource else "toilet_queue",
            agent_id=agent_id,
            resource_id=facility_id,
            start_minute=starts[place] if starts else 0,
            due_minute=starts[place] if starts else 0,
        )
        world.model_state[KEY]["positions"][agent_id].update(
            zone_id=facility.zone_id,
            point={"x": center.x, "y": center.y},
            edge_id=None,
            progress=0.0,
            position_kind="physical",
        )


# ---- What the walking tests computed in Python --------------------------------------------------


def _xy(point: Any) -> list[float]:
    return [point.x, point.y]


def _follower(scenario: Any) -> dict[str, Any]:
    """For the core's `a_follower_within_the_tolerance_stands_in_their_slot`: the head stands at
    the end of its approach route, and the follower's slot."""
    from sheltersim.domain import movement

    world = queue_world(scenario)
    toilet = facility_of(world, "toilet")
    head, follower = free_people(world, 2)
    line_up(world, toilet, [head, follower])
    route = movement.facility_approach_route(world, head, toilet)
    last = route[1][-1]
    world.model_state[KEY]["positions"][head]["point"] = {"x": last.x, "y": last.y}
    slot = movement._queue_slot_position(world, movement._geometry(world), follower, toilet)
    return {"head_point": _xy(last), "slot": _xy(slot)}


def _walk_speed(scenario: Any, phase: str) -> float:
    """For the core's `a_walk_s_minutes_turn_over_where_python_s_do`: the speed that puts the walk
    of one person lined up at the toilet just under 5 minutes."""
    from sheltersim.contracts.extensions import Point2D
    from sheltersim.domain import movement

    world = queue_world(scenario)
    toilet = facility_of(world, "toilet")
    person = free_people(world, 1)[0]
    line_up(world, toilet, [person])
    agent = world.agents[person]
    geometry = movement._geometry(world)
    if phase == "position":
        slot = movement._queue_slot_position(world, geometry, person, toilet)
        raw = world.model_state[KEY]["positions"][person]["point"]
        path = movement.plan_in_room_route(
            world,
            zone_id=world.facilities[toilet].zone_id,
            start=Point2D.model_validate(raw),
            destination=slot,
            mobility=agent.profile.mobility.value,
        )
    else:
        path = movement.facility_approach_route(world, person, toilet)[1]
    length = math.fsum(movement._distance(a, b) for a, b in zip(path, path[1:]))
    config = world.model_configs["movement_model"]
    density = movement._max_route_density(world, (agent.zone_id,), geometry)
    crowding = min(
        config["maximum_density_slowdown"],
        1.0 + config["density_slowdown_per_person_m2"] * max(0.0, density - 0.5),
    )
    return length * crowding / 4.99


def _head_route(scenario: Any) -> dict[str, Any]:
    """For the core's `the_head_walks_from_its_slot_to_the_service_point`: the approach route of
    the one person lined up at the toilet."""
    from sheltersim.domain import movement

    world = queue_world(scenario)
    toilet = facility_of(world, "toilet")
    person = free_people(world, 1)[0]
    line_up(world, toilet, [person])
    route, path = movement.facility_approach_route(world, person, toilet)
    return {"points": [_xy(point) for point in path], "travel_minutes": route.travel_minutes}


def queue_rules_fixture() -> dict[str, Any]:
    """`queue_rules.json`: the world the queue tests start from, with what they read in Python."""
    from sheltersim.core.world_export import export_world, sections
    from sheltersim.domain import movement

    scenario = generated()
    world, controller = run_minutes(scenario, QUEUE_TICKS)
    geometry = movement._geometry(world)
    entry = export_world(world, controller)
    entry["facilities"] = {kind: facility_of(world, kind) for kind in KINDS}
    entry["people"] = free_people(world, len(world.agents))
    entry["zone_centers"] = {
        zone.zone_id: _xy(movement._zone_center(geometry, zone.zone_id)) for zone in geometry.zones
    }
    strict = queue_world(generated(queue_policy_id="strict-fifo-queue-v1"))
    entry["strict_queue"] = {
        "configs": {key: strict.model_configs[key] for key in STRICT_CONFIGS},
        "selection": sections(strict)["selection"],
    }
    entry["walks"] = {
        "follower": _follower(scenario),
        "speeds": {phase: _walk_speed(scenario, phase) for phase in ("position", "service")},
        "head_route": _head_route(scenario),
    }
    return {"worlds": {"generated_minute_2": entry}}
