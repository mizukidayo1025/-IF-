"""The kernels of rules/proposals.rs, rules/service.rs and rules/travel.rs against the Python rules.

Each case runs the kernel on a world and the Python rule on a copy made before it, and compares the
whole world in canonical JSON: the branches the gate's runs may not reach, the configured values at
what changes the result, and the state of resource_service_v1 (docs/rust-port/lanes/m4-service.md).

The water and meal service, `_route`, `_schedule_travel` and `_apply_effect` have no Python rule
left (lane L10, `kernel_only` and KERNEL_ONLY_SEGMENTS): their cases compare the world with what the
Python rule made, written down from it before it went (`_is_python_s` and `world_digest`, digests
of the world, pinned on every OS as the gate's hashes are), and with what the rule says it writes.
"""

from __future__ import annotations

import copy
import hashlib
import json
from types import SimpleNamespace
from typing import Any

import pytest
from support.world import world_digest

from sheltersim import native
from sheltersim.core.engine import TickEngine
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.state_mirror import rust_validated
from sheltersim.core.world import StateInvariantError, WorldState
from sheltersim.domain import movement
from sheltersim.domain.agent_dynamics import learn_facility
from sheltersim.domain.models import ActiveTask, Activity
from sheltersim.domain.movement import MOVEMENT_STATE_KEY
from sheltersim.domain.resource_service import SERVICE_STATE_KEY as KEY
from sheltersim.domain.resource_service import ResourceServiceManager
from sheltersim.events.models import EventApplicationError
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.timeline import _MetricSink
from sheltersim.policies.behavior import ActionKind


def _controller(world: WorldState) -> OfflineSimulationController:
    return OfflineSimulationController(world, _MetricSink(MetricsRecorder()))


def _run(scenario: Any, minutes: int) -> tuple[WorldState, OfflineSimulationController]:
    world = WorldState.from_scenario(scenario)
    controller = _controller(world)
    engine = TickEngine(world, phase_handler=controller)
    for _ in range(minutes):
        engine.step()
    return world, controller


def _bytes(world: WorldState) -> str:
    return world._canonical_text(world._canonical_data())


def _row_follows(world: WorldState) -> None:
    mirror = world._core.mirror(KEY)
    assert mirror.tainted is None
    assert mirror.verify()
    assert mirror.row_json() == native.canonical_json(world.model_state[KEY])
    assert world._core.entities().verify(world) is None


def _is_python_s(world: WorldState, digest: str) -> None:
    """The world, its counters and its pending events are what the Python rule made: their digest,
    taken from that rule. Pinned on every OS, as the gate's hashes are."""
    _row_follows(world)
    text = json.dumps(
        [
            _bytes(world),
            world.id_counters,
            [repr(event) for event in world.event_queue.pending()],
        ],
        sort_keys=True,
    )
    assert hashlib.sha256(text.encode("utf-8")).hexdigest()[:16] == digest


def _same_world(world: WorldState, twin: WorldState) -> None:
    assert _bytes(world) == _bytes(twin)
    assert world.id_counters == twin.id_counters
    assert [repr(event) for event in world.event_queue.pending()] == [
        repr(event) for event in twin.event_queue.pending()
    ]
    mirror = world._core.mirror(KEY)
    assert mirror.tainted is None
    assert mirror.verify()
    assert mirror.row_json() == native.canonical_json(world.model_state[KEY])
    assert world._core.entities().verify(world) is None


def _idle_people(world: WorldState, count: int) -> list[str]:
    out = []
    for agent_id in sorted(world.agents):
        if agent_id in world.model_state[MOVEMENT_STATE_KEY]["journeys"]:
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


def _proposal(
    action: ActionKind, agent_id: str, world: WorldState, target: str | None = None
) -> Any:
    return SimpleNamespace(
        action=action,
        agent_id=agent_id,
        next_decision_at=world.minute_index + 7,
        target_id=target,
    )


def _effect(
    world: WorldState, controller: Any, proposal: Any, expected: tuple[Any, ...], digest: str
) -> Any:
    """The kernel on the world: the result and the world (`world_digest`) of the Python step,
    recorded from it on 45a9bc9 before it went."""
    result = native.rules("proposals").apply_effect(controller, proposal)
    assert tuple(result) == expected
    assert type(result[2]) is bool
    _row_follows(world)
    assert world_digest(world) == digest
    return result


def _same_zone_pair(world: WorldState) -> tuple[str, str]:
    people = _idle_people(world, 100)
    for first in people:
        for second in people:
            if first != second and world.agents[first].zone_id == world.agents[second].zone_id:
                return first, second
    pytest.fail("no two people in one zone")


# ---- _apply_effect ------------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("action", "digest"),
    [
        (ActionKind.WAIT, "1fecc84b22ca7b75"),
        (ActionKind.REST, "8dba68cbcb659dc2"),
        (ActionKind.SLEEP, "9dd05aad2d72d58b"),
    ],
)
def test_a_still_action_is_python_s(
    generated_scenario: Any, action: ActionKind, digest: str
) -> None:
    world, controller = _run(generated_scenario, 5)
    agent_id = _idle_people(world, 1)[0]
    world.agents[agent_id].activity = Activity.RESTING
    world.agents[agent_id].target_id = "kitchen"
    _effect(world, controller, _proposal(action, agent_id, world), (None, 12, True), digest)


@pytest.mark.parametrize(
    ("action", "digest"),
    [
        (ActionKind.SOCIALIZE, "4a0541d31b600176"),
        (ActionKind.CARE_FAMILY, "2861f4df8b51b5a8"),
        (ActionKind.HELP_OTHERS, "7ba195e90b4e41d3"),
    ],
)
def test_a_social_action_schedules_python_s_event(
    generated_scenario: Any, action: ActionKind, digest: str
) -> None:
    world, controller = _run(generated_scenario, 5)
    first, second = _same_zone_pair(world)
    pending = len(world.event_queue.pending())
    target, next_decision, committed = _effect(
        world, controller, _proposal(action, first, world, second), ("A006", 10, True), digest
    )
    assert (target, committed) == (second, True)
    assert next_decision == world.minute_index + 5
    assert len(world.event_queue.pending()) == pending + 1


def test_the_config_s_minutes_and_their_end_of_the_run(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    first, second = _same_zone_pair(world)
    world.model_configs["body_dynamics"]["social_interaction_minutes"] = 11
    _, next_decision, _ = _effect(
        world,
        controller,
        _proposal(ActionKind.SOCIALIZE, first, world, second),
        ("A006", 16, True),
        "de59a5ea0e95b1e8",
    )
    assert next_decision == world.minute_index + 11
    first, second = _same_zone_pair(world)
    # Past the last minute there is no event, and the planned decision stands.
    world.model_configs["body_dynamics"]["social_interaction_minutes"] = world.duration_minutes
    _effect(
        world,
        controller,
        _proposal(ActionKind.SOCIALIZE, first, world, second),
        (None, world.minute_index + 7, False),
        "72fe8db9763f1642",
    )


def test_without_a_config_the_default_minutes_are_python_s(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    first, second = _same_zone_pair(world)
    del world.model_configs["body_dynamics"]["social_interaction_minutes"]
    _effect(
        world,
        controller,
        _proposal(ActionKind.SOCIALIZE, first, world, second),
        ("A006", world.minute_index + 5, True),
        "50325313983b94ac",
    )


def test_a_social_action_needs_someone_else_in_the_zone(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    first = _idle_people(world, 1)[0]
    other_zone = next(
        item for item in world.agents.values() if item.zone_id != world.agents[first].zone_id
    )
    for target in (None, first, "nobody", other_zone.id):
        _effect(
            world,
            controller,
            _proposal(ActionKind.HELP_OTHERS, first, world, target),
            (None, world.minute_index + 7, False),
            "a8a42ffe67df1a3a",
        )


def test_a_held_bed_is_given_up_and_sleep_keeps_it(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    first, second = _idle_people(world, 2)
    reservations = world.model_state["geometry_overlay_v1"]["bed_reservations"]
    for agent_id in (first, second):
        reservations[agent_id] = {
            "agent_id": agent_id,
            "room_id": "room",
            "bed_index": 0,
            "target_id": "bed-x",
            "reserved_minute": 0,
        }
    # Reservations of no real room: the world would not validate, so no comparison of worlds.
    kernel = native.rules("proposals").apply_effect
    kernel(controller, _proposal(ActionKind.WAIT, first, world))
    kernel(controller, _proposal(ActionKind.SLEEP, second, world))
    assert list(reservations) == [second]


def test_a_position_point2d_refuses_is_python_s_error(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    world.model_configs.setdefault("geometry_overlay", {})["schema_version"] = 4
    agent_id = _idle_people(world, 1)[0]
    world.model_state[MOVEMENT_STATE_KEY]["positions"][agent_id]["point"]["x"] = float("nan")
    with pytest.raises(native.Unsupported):
        native.rules("proposals").apply_effect(
            controller, _proposal(ActionKind.SLEEP, agent_id, world)
        )


# The target and the world of each trip the Python step made, in the order of the case below.
PYTHON_TRIPS = {
    ActionKind.REQUEST_TOILET: [
        ("toilet_01", "ef35babb922449d7"),
        ("accessible_toilet_01", "3f4544a1805474c0"),
        ("toilet_01", "a53264f9443095ee"),
        ("accessible_toilet_01", "ca1d6a34ff6a5042"),
        ("toilet_01", "c6caf11c2796f45e"),
        ("accessible_toilet_01", "a1a54fbf6f3273c2"),
        ("toilet_01", "f9aa5d9f595842a4"),
        ("accessible_toilet_01", "1b18a106bdf97c9b"),
    ],
    ActionKind.FETCH_WATER: [
        ("water_station_main", "380f95b1a0afe0d6"),
        ("water_station_main", "5c9ee603c1513077"),
        ("water_station_main", "c3df9235a07ce17f"),
        ("water_station_main", "39f5a9ebc440cda6"),
        ("water_station_main", "4d9328ad35356386"),
        ("water_station_main", "f3e18579c9b1d399"),
        ("water_station_main", "0def5e0b9c6b62b9"),
        ("water_station_main", "b14fe3a6f2f63df7"),
    ],
    ActionKind.COLLECT_MEAL: [
        ("meal_distribution_main", "a127fd3634a648c6"),
        ("meal_distribution_main", "ad282e6c278f6615"),
        ("meal_distribution_main", "0783ea8604e643ac"),
        ("meal_distribution_main", "ac22643c67fbb4b6"),
        ("meal_distribution_main", "12ccc9cb0954bb01"),
        ("meal_distribution_main", "b5a6c9e54f256e49"),
        ("meal_distribution_main", "5304e0060d6c4432"),
        ("meal_distribution_main", "5e54d657a219eea9"),
    ],
}


@pytest.mark.parametrize(
    "action",
    [ActionKind.REQUEST_TOILET, ActionKind.FETCH_WATER, ActionKind.COLLECT_MEAL],
)
def test_each_trip_action_from_each_zone(generated_scenario: Any, action: ActionKind) -> None:
    world, controller = _run(generated_scenario, 5)
    expected = iter(PYTHON_TRIPS[action])
    ran = 0
    for agent_id in _idle_people(world, 100):
        for zone_id in sorted(world.zones):
            agent = world.agents[agent_id]
            agent.zone_id = zone_id
            world.model_state[MOVEMENT_STATE_KEY]["positions"][agent_id]["zone_id"] = zone_id
            target, digest = next(expected)
            _effect(
                world, controller, _proposal(action, agent_id, world), (target, 12, True), digest
            )
            ran += 1
            if agent.activity is Activity.MOVING:
                break
        if ran >= 12:
            break
    assert next(expected, None) is None


def test_a_trip_past_the_end_of_the_run_is_refused_as_python_refuses(
    generated_scenario: Any,
) -> None:
    world, controller = _run(generated_scenario, 5)
    world.duration_minutes = world.minute_index + 1
    for action, digest in (
        (ActionKind.FETCH_WATER, "7e223ff83025985b"),
        (ActionKind.REQUEST_TOILET, "7e223ff83025985b"),
        (ActionKind.COLLECT_MEAL, "49966c9a404bdcb4"),
    ):
        agent_id = _idle_people(world, 100)[0]
        _effect(world, controller, _proposal(action, agent_id, world), (None, 12, False), digest)


def test_no_known_facility_is_recorded_as_python_records(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    agent_id = _idle_people(world, 1)[0]
    known = world.model_state["agent_dynamics_v1"]["agents"][agent_id]["known_facility_ids"]
    known[:] = []
    for action, digest in (
        (ActionKind.FETCH_WATER, "de420a4402712c0d"),
        (ActionKind.COLLECT_MEAL, "67380efd43798097"),
        (ActionKind.REQUEST_TOILET, "67380efd43798097"),
    ):
        _effect(world, controller, _proposal(action, agent_id, world), (None, 12, False), digest)
    history = world.model_state[KEY]["request_history"]
    assert [record["reason"] for record in history[-2:]] == ["no_facility", "no_facility"]
    assert world.metric_state["resource_service_request_rejected_count"] >= 2.0


def test_the_lower_load_goes_before_the_shorter_walk(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    toilets = [item for item in world.facilities.values() if "toilet" in item.kind]
    walker, waiter = _idle_people(world, 2)
    known = world.model_state["agent_dynamics_v1"]["agents"][walker]["known_facility_ids"]
    for toilet in toilets:
        if toilet.id not in known:
            known.append(toilet.id)
    world.model_configs.pop("facility_queues", None)
    # The generated toilets share a zone: one moves to where the walk from here is longer.
    near, far = toilets[0], toilets[1]
    zones = sorted(world.zones)
    walks = None
    for start in zones:
        for near_zone in zones:
            for far_zone in zones:
                first = controller._route(start, near_zone, walker)
                second = controller._route(start, far_zone, walker)
                if first and second and 0 < first.travel_minutes < second.travel_minutes:
                    walks = (start, near_zone, far_zone)
                    break
            if walks:
                break
        if walks:
            break
    assert walks is not None
    start, near.zone_id, far.zone_id = walks
    for other in toilets[2:]:
        other.queue_agent_ids.append(waiter)
    world.agents[walker].zone_id = start
    world.model_state[MOVEMENT_STATE_KEY]["positions"][walker]["zone_id"] = start
    near.queue_agent_ids.append(waiter)
    # The least load, then the shortest walk, then the least ID (`_select_toilet` of 45a9bc9).
    target, *_ = native.rules("proposals").apply_effect(
        controller, _proposal(ActionKind.REQUEST_TOILET, walker, world)
    )
    assert target == far.id


def test_a_toilet_kind_beyond_ascii_is_folded_as_python_folds_it(generated_scenario: Any) -> None:
    """`"toilet" in kind.casefold()`: a Scenario's kind is any text. The kernel refused one that
    isn't ASCII, and the Python step chose by `str.casefold`; now the kernel asks it too."""
    world, controller = _run(generated_scenario, 5)
    world.facilities["toilet_01"].kind = "多目的toilet"
    # Full-width letters fold to full-width ones: no toilet.
    world.facilities["accessible_toilet_01"].kind = "ＴＯＩＬＥＴ"
    agent_id = next(
        key
        for key in sorted(world.agents)
        if world.agents[key].zone_id != "toilet"
        and key not in world.model_state[MOVEMENT_STATE_KEY]["journeys"]
        and not any(task.agent_id == key for task in world.active_tasks.values())
    )
    for facility_id in ("toilet_01", "accessible_toilet_01"):
        learn_facility(world, agent_id, facility_id)
    _effect(
        world,
        controller,
        _proposal(ActionKind.REQUEST_TOILET, agent_id, world),
        ("toilet_01", 12, True),
        "068b0babfdc6141d",
    )


@pytest.mark.parametrize(
    ("action", "digest"),
    [
        (ActionKind.SEEK_HELP, "66c8a5870bf1b6f4"),
        (ActionKind.SEEK_INFORMATION, "32f0d7926c7ef2c5"),
    ],
)
def test_a_request_of_the_run_s_last_minute_is_closed_at_once(
    generated_scenario: Any, action: ActionKind, digest: str
) -> None:
    world, controller = _run(generated_scenario, 5)
    agent_id = _idle_people(world, 1)[0]
    world.agents[agent_id].activity = Activity.IDLE
    world.agents[agent_id].support_cooldown_until = 0
    world.agents[agent_id].support_request_id = None
    world.duration_minutes = world.minute_index + 1
    target, _, committed = _effect(
        world,
        controller,
        _proposal(action, agent_id, world),
        ("support-5d7322b2c209034dac8ce21e", 12, False),
        digest,
    )
    assert committed is False
    assert world.support_requests[target].resolved_minute == world.minute_index


# ---- _route and _schedule_travel ----------------------------------------------------------------


def test_every_route_is_python_s_for_every_mobility(generated_scenario: Any) -> None:
    """From each zone to each, for each mobility: the routes the Python rule found, their digest
    taken from it (45a9bc9), pinned on every OS."""
    world, controller = _run(generated_scenario, 5)
    seen = set()
    found = []
    for agent_id in sorted(world.agents):
        mobility = world.agents[agent_id].profile.mobility
        if mobility in seen:
            continue
        seen.add(mobility)
        for origin in sorted(world.zones):
            for destination in sorted(world.zones):
                found.append(repr(controller._route(origin, destination, agent_id)))
    assert seen
    assert len(found) == 192 and "None" not in found
    digest = hashlib.sha256(json.dumps(found).encode("utf-8")).hexdigest()[:16]
    assert digest == "c5d1362fa0212f15"


def test_an_equal_time_goes_to_the_lower_edge_id(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    kernel = native.rules("travel").route
    agent_id = _idle_people(world, 1)[0]
    # The graph's own travel times equal: only the IDs can decide.
    from sheltersim.domain.zones import ZoneConnection, ZoneGraph
    from sheltersim.policies.space import SpaceRouter

    zones = list(controller.space.graph.zones)[:4]
    controller.space = SpaceRouter(
        ZoneGraph(
            zones={zone: controller.space.graph.zones[zone] for zone in zones},
            edges=(
                ZoneConnection("z-b", zones[0], zones[1], 1),
                ZoneConnection("a-c", zones[0], zones[2], 1),
                ZoneConnection("m-d", zones[1], zones[3], 1),
                ZoneConnection("n-d", zones[2], zones[3], 1),
            ),
        )
    )
    route = kernel(controller, zones[0], zones[3], agent_id)
    assert route.edge_ids == ("a-c", "n-d")
    # As the Python rule found it.
    assert (route.zone_ids, route.travel_minutes) == ((zones[0], zones[2], zones[3]), 4)


def test_an_edge_capacity_is_counted_and_another_policy_goes_to_python(
    generated_scenario: Any,
) -> None:
    world, controller = _run(generated_scenario, 5)
    kernel = native.rules("travel").route
    agent_id = _idle_people(world, 1)[0]
    zone = world.agents[agent_id].zone_id
    controller.route_capacities = {"any": 1}
    assert kernel(controller, zone, zone, agent_id).edge_ids == ()
    controller.route_capacities = {}
    from sheltersim.policies.space import SpaceRouter

    class Other:
        def route(self, *arguments: Any, **options: Any) -> None:
            return None

    controller.space = SpaceRouter(controller.space.graph, Other())
    with pytest.raises(native.Unsupported, match="policy"):
        kernel(controller, zone, zone, agent_id)


def _travel(world: WorldState, controller: Any, digest: str, **options: Any) -> Any:
    """`_schedule_travel`, which leaves the world the Python rule left: `digest`."""
    result = controller._schedule_travel(**options)
    _is_python_s(world, digest)
    return result


def test_a_trip_and_each_limit_are_python_s(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    first, second = _idle_people(world, 2)
    origin = world.agents[first].zone_id
    destination = next(
        zone
        for zone in sorted(world.zones)
        if zone != origin and controller._route(origin, zone, first)
    )
    route = controller._route(origin, destination, first)
    common = {
        "agent_id": first,
        "route": route,
        "purpose": "return",
        "facility_id": None,
        "minute": world.minute_index,
        "validate_state": False,
    }
    # The zone is full, counting who walks there.
    capacity = world.zones[destination].capacity
    world.zones[destination].capacity = 0
    assert _travel(world, controller, "167bbd68b5a94255", **common) is False
    world.zones[destination].capacity = capacity
    # It ends past the run.
    saved = world.duration_minutes
    world.duration_minutes = world.minute_index + route.travel_minutes
    assert _travel(world, controller, "623e47f172d00aff", **common) is False
    world.duration_minutes = saved
    assert _travel(world, controller, "80054702e0e729ce", **common) is True
    assert world.agents[first].activity is Activity.MOVING


def test_a_trip_refused_writes_nothing(generated_scenario: Any) -> None:
    """The Python rule's errors, raised before anything is written (that rule allocated a sequence
    before the second; the kernel does not)."""
    world, controller = _run(generated_scenario, 5)
    first = _idle_people(world, 1)[0]
    origin = world.agents[first].zone_id
    destination = next(
        zone
        for zone in sorted(world.zones)
        if zone != origin and controller._route(origin, zone, first)
    )
    route = controller._route(origin, destination, first)
    before = (_bytes(world), dict(world.id_counters))
    options = {
        "agent_id": first,
        "route": route,
        "purpose": "return",
        "facility_id": None,
        "minute": world.minute_index,
        "validate_state": False,
    }
    with pytest.raises(
        ValueError, match="^staff support travel requires request and staff IDs$"
    ) as made:
        controller._schedule_travel(**{**options, "purpose": "staff_support_travel"})
    assert type(made.value) is ValueError
    other = next(item for item in sorted(world.agents) if world.agents[item].zone_id != origin)
    with pytest.raises(ValueError, match="^travel route origin does not match Agent zone$") as made:
        controller._schedule_travel(**{**options, "agent_id": other})
    assert type(made.value) is ValueError
    with pytest.raises(KernelRefusal, match="^travel.schedule_travel refused: "):
        controller._schedule_travel(**{**options, "agent_id": "nobody"})
    assert (_bytes(world), dict(world.id_counters)) == before


def test_a_trip_validates_the_whole_world_when_asked_to(generated_scenario: Any) -> None:
    """With `validate_state`, as by default, `world.validate()` after the trip is written."""
    world, controller = _run(generated_scenario, 5)
    first, moved, second = _idle_people(world, 3)
    origin = world.agents[first].zone_id
    destination = next(
        zone
        for zone in sorted(world.zones)
        if zone != origin and controller._route(origin, zone, first)
    )
    options = {
        "route": controller._route(origin, destination, first),
        "purpose": "return",
        "facility_id": None,
        "minute": world.minute_index,
    }
    assert controller._schedule_travel(agent_id=first, **options) is True
    assert world.agents[first].activity is Activity.MOVING
    # Someone moved by hand, which the whole validation turns down.
    assert _at_facility(world)[1] == moved
    with pytest.raises(StateInvariantError) as validated:
        world.validate()
    origin = world.agents[second].zone_id
    destination = next(
        zone
        for zone in sorted(world.zones)
        if zone != origin and controller._route(origin, zone, second)
    )
    options["route"] = controller._route(origin, destination, second)
    with pytest.raises(StateInvariantError) as ours:
        controller._schedule_travel(agent_id=second, **options, validate_state=True)
    assert str(ours.value) == str(validated.value)
    assert world.agents[second].activity is Activity.MOVING


# ---- arbitrate ----------------------------------------------------------------------------------


def _service_facility(world: WorldState, kind: str) -> Any:
    return next(
        item
        for item in sorted(world.facilities.values(), key=lambda item: item.id)
        if item.kind == kind
    )


def _at_service(world: WorldState, kind: str = "water_station") -> tuple[Any, str]:
    """Someone first in line and standing at the service point, with a free person on staff."""
    facility = _service_facility(world, kind)
    facility.resource_queue_agent_ids.clear()
    agent_id = _idle_people(world, 1)[0]
    agent = world.agents[agent_id]
    agent.zone_id = facility.zone_id
    agent.activity = Activity.QUEUED
    agent.target_id = facility.id
    facility.resource_queue_agent_ids.append(agent_id)
    task_id = f"queue-task-{agent_id}"
    world.active_tasks[task_id] = ActiveTask(
        id=task_id,
        kind="resource_service_queue",
        agent_id=agent_id,
        resource_id=facility.id,
        start_minute=max(world.minute_index - 3, 0),
        due_minute=max(world.minute_index - 3, 0),
    )
    point = movement._geometry(world).facility_points[facility.id]
    world.model_state[MOVEMENT_STATE_KEY]["positions"][agent_id].update(
        zone_id=facility.zone_id,
        point={"x": point.x, "y": point.y},
        edge_id=None,
        progress=0.0,
        position_kind="physical",
    )
    return facility, agent_id


def _serve(
    world: WorldState, facility: Any, resource: str = "water_ml", digest: str | None = None
) -> Any:
    manager = ResourceServiceManager(world)
    result = native.rules("service").serve_head(manager, facility.id, resource, world.minute_index)
    if digest is not None:
        _is_python_s(world, digest)
    else:
        _row_follows(world)
    return result


def _free_all_staff(world: WorldState, facility: Any) -> None:
    for person in world.staff.values():
        person.zone_id = facility.zone_id
        person.active_task_id = None
        person.work_queue[:] = []


def test_a_head_at_the_point_starts_its_service(generated_scenario: Any) -> None:
    world, _ = _run(generated_scenario, 5)
    facility, agent_id = _at_service(world)
    _free_all_staff(world, facility)
    event_id = _serve(world, facility, digest="2a3c0767c9e08409")
    assert event_id is not None
    assert world.agents[agent_id].activity is Activity.IN_SERVICE
    assert world.event_queue.find(event_id).event_type == "resource_service_complete"
    assert world.metric_state["water_service_wait_count"] >= 1.0


WAITS = {
    "capacity": "bb6a050583414052",
    "staff": "aea91f1779cd9538",
    "stock": "83e5c9064839b2b3",
    "run_end": "62eee99e27894a3c",
    "water_outage": "1b96482b22525348",
}


@pytest.mark.parametrize("reason", sorted(WAITS))
def test_each_wait_is_python_s_record(generated_scenario: Any, reason: str) -> None:
    world, _ = _run(generated_scenario, 5)
    facility, agent_id = _at_service(world)
    _free_all_staff(world, facility)
    if reason == "capacity":
        facility.working_units = 0
    elif reason == "staff":
        for person in world.staff.values():
            person.zone_id = "elsewhere"
    elif reason == "stock":
        ledger = world.model_state["resource_ledger_v1"]["accounts"]["water_ml"]
        world.model_configs["resource_service"]["water_ml_per_service"] = ledger["remaining"] + 1
    elif reason == "run_end":
        world.duration_minutes = world.minute_index + 1
    else:
        world.model_state["operational_events_v1"]["water_outages"]["cut"] = {
            "active": True,
            "target_kind": "facility",
            "target_id": facility.id,
        }
    count = world.metric_state.get(f"resource_service_wait_{reason}_count", 0.0)
    assert _serve(world, facility, digest=WAITS[reason]) is None
    record = world.model_state[KEY]["stall_state"][facility.id]
    assert record == {"agent_id": agent_id, "reason": reason}
    history = list(world.model_state[KEY]["queue_stall_history"])
    assert history[-1] == {
        "minute": world.minute_index,
        "facility_id": facility.id,
        "agent_id": agent_id,
        "reason": reason,
    }
    assert world.metric_state[f"resource_service_wait_{reason}_count"] == count + 1.0
    # The same wait the next minute adds nothing.
    before = _bytes(world)
    assert _serve(world, facility) is None
    assert _bytes(world) == before


def test_a_taken_reservation_id_is_python_s_error(generated_scenario: Any) -> None:
    world, _ = _run(generated_scenario, 5)
    facility, agent_id = _at_service(world)
    world.model_state["resource_ledger_v1"]["used_reservation_ids"].append(f"queue-task-{agent_id}")
    before = _bytes(world)
    with pytest.raises(StateInvariantError) as raised:
        ResourceServiceManager(world)._serve_head(facility.id, "water_ml", world.minute_index)
    assert str(raised.value) == (
        f"resource service task ID 'queue-task-{agent_id}' conflicts with an H01 reservation"
    )
    assert _bytes(world) == before


def test_only_the_queues_with_people_are_served(generated_scenario: Any) -> None:
    world, _ = _run(generated_scenario, 5)
    for facility in world.facilities.values():
        facility.resource_queue_agent_ids.clear()
    manager = ResourceServiceManager(world)
    kernel = native.rules("service").facilities_to_serve
    assert kernel(manager) == []
    facility, _ = _at_service(world, "meal_distribution")
    assert kernel(manager) == [(facility, "food_meal_units")]


# ---- complete -----------------------------------------------------------------------------------


def _started(
    world: WorldState, kind: str, resource: str, digest: str | None = None
) -> tuple[Any, str, Any]:
    facility, agent_id = _at_service(world, kind)
    _free_all_staff(world, facility)
    event_id = _serve(world, facility, resource, digest)
    return world.event_queue.find(event_id), agent_id, facility


@pytest.mark.parametrize(
    ("kind", "resource", "need", "started", "completed"),
    [
        ("water_station", "water_ml", "hydration_need", "2a3c0767c9e08409", "4f39327b201771c8"),
        ("meal_distribution", "food_meal_units", "hunger", "0d30579820e3bf95", "46c48784b7891a9e"),
    ],
)
def test_a_completion_relieves_python_s_need(
    generated_scenario: Any, kind: str, resource: str, need: str, started: str, completed: str
) -> None:
    world, _ = _run(generated_scenario, 5)
    event, agent_id, facility = _started(world, kind, resource, started)
    setattr(world.agents[agent_id].needs, need, 0.812345)
    manager = ResourceServiceManager(world)
    relief = manager.config["hydration_relief" if need == "hydration_need" else "hunger_relief"]
    native.rules("service").complete(manager, event)
    _is_python_s(world, completed)
    assert getattr(world.agents[agent_id].needs, need) == max(0.0, 0.812345 - relief)
    assert world.agents[agent_id].activity is Activity.IDLE
    assert agent_id not in facility.resource_active_agent_ids
    assert event.payload["task_id"] not in world.active_tasks


def test_the_relief_is_read_at_its_value_and_stops_at_zero(generated_scenario: Any) -> None:
    world, _ = _run(generated_scenario, 5)
    event, agent_id, _ = _started(world, "water_station", "water_ml")
    world.agents[agent_id].needs.hydration_need = 0.5
    manager = ResourceServiceManager(world)
    manager.config["hydration_relief"] *= 1.01
    relief = manager.config["hydration_relief"]
    native.rules("service").complete(manager, event)
    assert world.agents[agent_id].needs.hydration_need == max(0.0, 0.5 - relief)
    assert relief != ResourceServiceManager(world).config["hydration_relief"]


@pytest.mark.parametrize(
    "change",
    [
        lambda world, event, agent_id: world.staff[event.payload["staff_id"]].__setattr__(
            "busy_until", 0
        ),
        lambda world, event, agent_id: world.model_state[KEY]["active_reservation_ids"].clear(),
        lambda world, event, agent_id: world.facilities[
            event.payload["facility_id"]
        ].resource_active_agent_ids.clear(),
    ],
)
def test_a_completion_that_does_not_match_is_python_s_error(
    generated_scenario: Any, change: Any
) -> None:
    world, _ = _run(generated_scenario, 5)
    event, agent_id, _ = _started(world, "water_station", "water_ml")
    change(world, event, agent_id)
    before = _bytes(world)
    with pytest.raises(EventApplicationError) as ours:
        native.rules("service").complete(ResourceServiceManager(world), event)
    assert _bytes(world) == before
    assert str(ours.value) == (
        "resource service completion does not match its full reservation, slot, or staff"
    )


# ---- request and its record ---------------------------------------------------------------------


def _at_facility(world: WorldState, kind: str = "water_station") -> tuple[Any, str]:
    facility = _service_facility(world, kind)
    agent_id = _idle_people(world, 1)[0]
    agent = world.agents[agent_id]
    agent.zone_id = facility.zone_id
    agent.activity = Activity.IDLE
    agent.target_id = None
    return facility, agent_id


def test_a_request_is_python_s(generated_scenario: Any) -> None:
    world, _ = _run(generated_scenario, 5)
    facility, agent_id = _at_facility(world)
    counter = world.id_counters.get("resource_task", 0)
    result = native.rules("service").request(
        ResourceServiceManager(world), agent_id, facility.id, validate_state=False
    )
    _is_python_s(world, "22667c113d8b86dd")
    task_id = f"resource-service-task-{counter}"
    assert (result.agent_id, result.facility_id, result.resource_id) == (
        agent_id,
        facility.id,
        "water_ml",
    )
    assert (result.requested_at, result.queue_task_id) == (world.minute_index, task_id)
    assert result.queue_position == len(facility.resource_queue_agent_ids)
    assert world.agents[agent_id].activity is Activity.QUEUED
    assert world.active_tasks[task_id].kind == "resource_service_queue"


@pytest.mark.parametrize(
    ("change", "text"),
    [
        (
            lambda world, agent_id: world.agents[agent_id].__setattr__("zone_id", "kitchen"),
            "Agent must arrive at the resource service facility before queueing",
        ),
        (
            lambda world, agent_id: world.agents[agent_id].__setattr__("activity", Activity.MOVING),
            "Agent {agent} cannot request resource service while active",
        ),
    ],
)
def test_a_request_refused_has_python_s_text(
    generated_scenario: Any, change: Any, text: str
) -> None:
    world, _ = _run(generated_scenario, 5)
    facility, agent_id = _at_facility(world)
    change(world, agent_id)
    if world.agents[agent_id].zone_id == facility.zone_id and "arrive" in text:
        pytest.skip("the scenario's facility is in that zone")
    manager = ResourceServiceManager(world)
    before = _bytes(world)
    # The kernel raises the Python rule's ValueError itself, before it writes anything.
    with pytest.raises(ValueError) as ours:
        native.rules("service").request(manager, agent_id, facility.id, validate_state=False)
    assert _bytes(world) == before
    assert str(ours.value) == text.format(agent=repr(agent_id))


def test_a_request_validates_the_whole_world_when_asked_to(generated_scenario: Any) -> None:
    world, _ = _run(generated_scenario, 5)
    facility, agent_id = _at_facility(world)
    # This world has someone moved by hand, which the whole validation turns down first.
    with pytest.raises(StateInvariantError) as validated:
        world.validate()
    with pytest.raises(StateInvariantError) as ours:
        native.rules("service").request(ResourceServiceManager(world), agent_id, facility.id)
    assert str(ours.value) == str(validated.value)


def test_a_record_of_a_request_is_python_s(generated_scenario: Any) -> None:
    world, _ = _run(generated_scenario, 5)
    facility, agent_id = _at_facility(world)
    twin = copy.deepcopy(world)
    options = {
        "agent_id": agent_id,
        "action": "fetch_water",
        "outcome": "rejected",
        "reason": "travel_rejected",
        "facility_id": facility.id,
        "origin_zone_id": facility.zone_id,
        "detail": "text",
    }
    native.rules("service").record_request_outcome(ResourceServiceManager(world), **options)
    # The Python rule appended the record, with the world's minute, and wrote nothing else.
    twin.model_state[KEY]["request_history"].append({"minute": twin.minute_index, **options})
    _same_world(world, twin)


# ---- The state ----------------------------------------------------------------------------------


def test_the_row_is_the_tree_and_a_tick_writes_no_whole_namespace(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 1)
    mirror = world._core.mirror(KEY)
    before = mirror.stats()["whole_writes"]
    engine = TickEngine(world, phase_handler=controller)
    for _ in range(120):
        engine.step()
    assert mirror.stats()["whole_writes"] == before
    assert mirror.tainted is None
    assert mirror.verify()
    assert mirror.row_json() == native.canonical_json(world.model_state[KEY])
    assert rust_validated(world, KEY)


def test_a_person_leaving_rewrites_the_histories_through_the_row(generated_scenario: Any) -> None:
    world, _ = _run(generated_scenario, 5)
    state = world.model_state[KEY]
    state["request_history"].append(
        {
            "minute": world.minute_index,
            "agent_id": "A001",
            "action": "fetch_water",
            "outcome": "rejected",
            "reason": "no_facility",
            "facility_id": None,
            "origin_zone_id": sorted(world.zones)[0],
            "detail": None,
        }
    )
    state["request_history"] = [
        item for item in state["request_history"] if item["agent_id"] != "A001"
    ]
    mirror = world._core.mirror(KEY)
    assert mirror.tainted is None
    assert mirror.verify()
    assert mirror.row_json() == native.canonical_json(state)


def test_a_bad_value_taints_and_goes_to_python(generated_scenario: Any) -> None:
    world, _ = _run(generated_scenario, 5)
    state = world.model_state[KEY]
    state["request_history"].append({"minute": "soon"})
    assert world._core.mirror(KEY).tainted
    assert not rust_validated(world, KEY)
    with pytest.raises(StateInvariantError, match="request history is invalid"):
        world.validate(validate_event_history=False)
    facility, agent_id = _at_facility(world)
    with pytest.raises(native.Unsupported):
        native.rules("service").request(
            ResourceServiceManager(world), agent_id, facility.id, validate_state=False
        )


@pytest.mark.parametrize(
    ("write", "text"),
    [
        (
            lambda state, world: state["stall_state"].__setitem__(
                _service_facility(world, "water_station").id,
                {"agent_id": _idle_people(world, 1)[0], "reason": "capacity"},
            ),
            "must describe the current facility queue head",
        ),
        (
            lambda state, world: state["stall_state"].__setitem__(
                _service_facility(world, "water_station").id,
                {"agent_id": sorted(world.agents)[0], "reason": "weather"},
            ),
            "stall state is invalid",
        ),
        (
            lambda state, world: state["cancelled_task_ids"].extend(["same", "same"]),
            "resource_service_v1 state is invalid",
        ),
        (
            lambda state, world: state["queue_stall_history"].append(
                {
                    "minute": world.minute_index + 1,
                    "facility_id": sorted(world.facilities)[0],
                    "agent_id": sorted(world.agents)[0],
                    "reason": "staff",
                }
            ),
            "stall history is invalid",
        ),
    ],
)
def test_the_tick_s_check_takes_nothing_python_refuses(
    generated_scenario: Any, write: Any, text: str
) -> None:
    world, _ = _run(generated_scenario, 5)
    world.validate(validate_event_history=False)
    write(world.model_state[KEY], world)
    assert not rust_validated(world, KEY)
    with pytest.raises(StateInvariantError, match=text):
        world.validate(validate_event_history=False)


def test_the_save_check_still_reads_the_whole_history(generated_scenario: Any) -> None:
    world, _ = _run(generated_scenario, 5)
    state = world.model_state[KEY]
    state["request_history"].insert(
        0,
        {
            "minute": 0,
            "agent_id": "nobody",
            "action": "fetch_water",
            "outcome": "rejected",
            "reason": "no_facility",
            "facility_id": None,
            "origin_zone_id": sorted(world.zones)[0],
            "detail": None,
        },
    )
    world.validate(validate_event_history=False)
    with pytest.raises(StateInvariantError, match="request history is invalid"):
        world.validate()


def test_a_deep_copy_has_its_own_row(generated_scenario: Any) -> None:
    world, _ = _run(generated_scenario, 5)
    twin = copy.deepcopy(world)
    twin.model_state[KEY]["cancelled_task_ids"].append("gone")
    assert world.model_state[KEY]["cancelled_task_ids"] == []
    assert world._core.mirror(KEY).verify()
    assert twin._core.mirror(KEY).verify()
