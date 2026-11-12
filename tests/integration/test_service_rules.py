"""The Python entries of rules/proposals.rs, rules/service.rs and rules/travel.rs, and the
service's state.

The rules' cases are tested in the core without Python (native/core/tests/service_rules.rs,
Issue #225), on the worlds this file built (`tests/support/core_fixtures_service.py`). This file
keeps one test of each PyO3 entry that reaches the kernel and gives its answer (`_apply_effect`,
`_route`, `_schedule_travel`, `serve_head`, `facilities_to_serve`, `complete`, `request`,
`record_request_outcome`), and the boundary: a refusal as Python's error, the world's whole
validation, and the row of resource_service_v1 that the Python tree follows (state_mirror, taint).

These rules have no Python rule left (lane L10, `kernel_only`): the digests below are of the world
the Python rule made, written down from it before it went (`_is_python_s`), pinned on every OS as
the gate's hashes are.
"""

from __future__ import annotations

import copy
import hashlib
import json
from types import SimpleNamespace
from typing import Any

import pytest

from sheltersim import native
from sheltersim.core.engine import TickEngine
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.state_mirror import detach_unread, read_only_part, rust_validated
from sheltersim.core.world import StateInvariantError, WorldState
from sheltersim.domain import movement
from sheltersim.domain.models import ActiveTask, Activity
from sheltersim.domain.movement import MOVEMENT_STATE_KEY
from sheltersim.domain.resource_service import SERVICE_STATE_KEY as KEY
from sheltersim.domain.resource_service import ResourceServiceManager
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


# ---- _apply_effect ------------------------------------------------------------------------------


def test_a_held_bed_is_given_up_in_the_row_of_a_detached_overlay(generated_scenario: Any) -> None:
    # #134: the overlay's row alone holds it; the kernel deletes from the row and builds no tree.
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
    detach_unread(world)
    mirror = world._core.mirror("geometry_overlay_v1")
    rebuilds = mirror.stats()["rebuilds"]
    kernel = native.rules("proposals").apply_effect
    kernel(controller, _proposal(ActionKind.WAIT, first, world))
    kernel(controller, _proposal(ActionKind.SLEEP, second, world))
    assert mirror.is_detached and mirror.stats()["rebuilds"] == rebuilds
    assert list(read_only_part(world, "geometry_overlay_v1", "bed_reservations")) == [second]


def test_a_position_point2d_refuses_is_python_s_error(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    world.model_configs.setdefault("geometry_overlay", {})["schema_version"] = 4
    agent_id = _idle_people(world, 1)[0]
    world.model_state[MOVEMENT_STATE_KEY]["positions"][agent_id]["point"]["x"] = float("nan")
    with pytest.raises(native.Unsupported):
        native.rules("proposals").apply_effect(
            controller, _proposal(ActionKind.SLEEP, agent_id, world)
        )


# ---- _route and _schedule_travel ----------------------------------------------------------------


def test_an_edge_capacity_is_counted(generated_scenario: Any) -> None:
    world, controller = _run(generated_scenario, 5)
    kernel = native.rules("travel").route
    agent_id = _idle_people(world, 1)[0]
    zone = world.agents[agent_id].zone_id
    # The Run reads its capacities when its controller starts (native/src/core/run.rs), and builds
    # its router from the world's tables when first asked: a key no edge has is ZoneGraph's error.
    controller._run_core = native.run_core({"any": 1})
    with pytest.raises(ValueError, match="route capacity references an unknown zone edge"):
        kernel(controller, zone, zone, agent_id)
    controller._run_core = native.run_core({next(iter(world.zone_edges)): 1})
    assert kernel(controller, zone, zone, agent_id).edge_ids == ()


def test_a_trip_refused_writes_nothing(generated_scenario: Any) -> None:
    """A person the world does not have: the kernel's refusal, as the step's KernelRefusal, before
    anything is written. The Python rule's ValueErrors are the core's cases."""
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
    with pytest.raises(KernelRefusal, match="^travel.schedule_travel refused: "):
        controller._schedule_travel(
            agent_id="nobody",
            route=route,
            purpose="return",
            facility_id=None,
            minute=world.minute_index,
            validate_state=False,
        )
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
