"""The worlds of the core's tests of the service, travel and staff steps (Issue #225, lane M6).

`native/core/tests/service_rules.rs`, `requests_at_once.rs` and `staff_assignment.rs` call the
core's steps of `_apply_effect` (`propose::apply_alone`), `_route` and `_schedule_travel`, the
water and meal service (`serve`, `complete`, `request`, the record of a request), the staff's
assignment (`assign_operations`, `relief`, `assign_pending`) and the wait snapshot
(`open_waits`) on the worlds the pytest files of the same names built. This module builds those
worlds and exports them (`core_fixtures.export_world`), each world as the variant of the one
added before whose changes are the shortest (`core_texts.Fixture`; `cases::entry` makes it
again):

- `service_rules.json`: the generated Scenario of the Project screen at minute 5 (`minute_5`, with
  the Run's policies), and the worlds the tests edited of it.
- `staff_assignment.json`: worlds of `scenario_100_agents.json` as the staff tests made them (the
  configs of the staff, `initialize_simulator_extensions`), the worlds of a controller whose Run
  has route capacities, and the Scenario without a floor plan of the requests at once.
- `generated_texts.json` (`generated_texts_fixture`): the requests at once on the generated
  Scenario at minute 0, its worlds as the variants of `generated` (`at_once_worlds`).

Where a pytest picked IDs in Python's order or after a step it called (`_idle_people`, the zones of
each trip), the builder replays the test through the PyO3 entries and writes the IDs down in the
file's `inputs`, by the name of the test without `test_`; so does a router a Python caller gave
(`router_of`) and the draws of Python's `random.Random`. The expected values are the core's,
recorded in the Rust tests.
"""

from __future__ import annotations

import copy
import json
import random
from types import SimpleNamespace
from typing import Any

from support.core_fixtures import export_world
from support.core_texts import Fixture

# ---- Shared pieces ------------------------------------------------------------------------------


def _controller(world: Any) -> Any:
    from sheltersim.experiments.metrics import MetricsRecorder
    from sheltersim.experiments.runner import OfflineSimulationController
    from sheltersim.experiments.timeline import _MetricSink

    return OfflineSimulationController(world, _MetricSink(MetricsRecorder()))


def _proposal(action: Any, agent_id: str, world: Any, target: str | None = None) -> Any:
    return SimpleNamespace(
        action=action,
        agent_id=agent_id,
        next_decision_at=world.minute_index + 7,
        target_id=target,
    )


def _apply(controller: Any, proposal: Any) -> Any:
    from sheltersim import native

    return native.rules("proposals").apply_effect(controller, proposal)


def _manager_config(entry: dict[str, Any], world: Any) -> dict[str, Any]:
    """The entry with the Run's resource manager's config as `ResourceServiceManager(world)` reads
    it now: the pytest made a new manager after it changed the configs."""
    from sheltersim.domain.resource_service import ResourceServiceManager

    attrs = entry["run"]["policies"]["controller"]["attrs"]["resource_service"]["object"]["attrs"]
    attrs["config"] = {"value": json.loads(json.dumps(ResourceServiceManager(world).config))}
    return entry


def _service_attr(entry: dict[str, Any], service: str, name: str, value: Any) -> dict[str, Any]:
    """The entry with an attribute of a service of the Run's controller set, or the service None."""
    attrs = entry["run"]["policies"]["controller"]["attrs"]
    if name == "":
        attrs[service] = {"value": value}
    else:
        attrs[service]["object"]["attrs"][name] = {"value": value}
    return entry


# ---- service_rules.json -------------------------------------------------------------------------


def _idle_people(world: Any, count: int) -> list[str]:
    from sheltersim.domain.movement import MOVEMENT_STATE_KEY

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


def _same_zone_pair(world: Any) -> list[str]:
    people = _idle_people(world, 100)
    for first in people:
        for second in people:
            if first != second and world.agents[first].zone_id == world.agents[second].zone_id:
                return [first, second]
    raise AssertionError("no two people in one zone")


def _service_facility(world: Any, kind: str) -> Any:
    return next(
        item
        for item in sorted(world.facilities.values(), key=lambda item: item.id)
        if item.kind == kind
    )


def _at_service(world: Any, kind: str = "water_station") -> tuple[Any, str]:
    """`_at_service` of test_service_rules.py: someone first in line at the service point."""
    from sheltersim.domain import movement
    from sheltersim.domain.models import ActiveTask, Activity
    from sheltersim.domain.movement import MOVEMENT_STATE_KEY

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


def _free_all_staff(world: Any, facility: Any) -> None:
    for person in world.staff.values():
        person.zone_id = facility.zone_id
        person.active_task_id = None
        person.work_queue[:] = []


def _at_facility(world: Any, kind: str = "water_station") -> tuple[Any, str]:
    from sheltersim.domain.models import Activity

    facility = _service_facility(world, kind)
    agent_id = _idle_people(world, 1)[0]
    agent = world.agents[agent_id]
    agent.zone_id = facility.zone_id
    agent.activity = Activity.IDLE
    agent.target_id = None
    return facility, agent_id


def _minute_5() -> tuple[Any, Any]:
    from support.core_fixtures_texts import _generated_scenario
    from support.core_texts import _run

    return _run(_generated_scenario(), 5)


class _Worlds:
    """The worlds of one file and the inputs of its tests: `make` makes the first world and its
    controller again for each test, as each pytest made its own."""

    def __init__(self, make: Any, name: str) -> None:
        self.fixture = Fixture()
        self.make = make
        self.world0, self.controller = make()
        self.fixture.world(name, self.world0, self.controller, full=True)
        self.inputs: dict[str, Any] = {}

    def copy(self) -> Any:
        """A new first world, whose controller is the first's in the file."""
        return self.make()[0]

    def fresh(self) -> tuple[Any, Any]:
        """A new first world and its own controller, for a replay of a test's steps."""
        return self.make()

    def add(self, name: str, world: Any, *, edit: Any = None, controller: Any = None) -> str:
        entry = export_world(world, controller or self.controller, validate=False)
        if edit is not None:
            entry = edit(entry)
        return self.fixture.add(name, entry)

    def document(self) -> dict[str, Any]:
        return {"worlds": self.fixture.worlds, "inputs": json.loads(json.dumps(self.inputs))}


def _effect_worlds(made: _Worlds) -> None:
    """The worlds and inputs of the `_apply_effect` cases."""
    from sheltersim.domain.agent_dynamics import learn_facility
    from sheltersim.domain.models import Activity
    from sheltersim.domain.movement import MOVEMENT_STATE_KEY
    from sheltersim.policies.behavior import ActionKind

    world = made.copy()
    agent_id = _idle_people(world, 1)[0]
    world.agents[agent_id].activity = Activity.RESTING
    world.agents[agent_id].target_id = "kitchen"
    made.inputs["still_action"] = {"world": made.add("resting", world), "agent": agent_id}

    made.inputs["social_action"] = {"pair": _same_zone_pair(made.world0)}

    # The configured minutes, and past the run's end: the second pair is the test's after its
    # first step.
    world = made.copy()
    first, second = _same_zone_pair(world)
    world.model_configs["body_dynamics"]["social_interaction_minutes"] = 11
    name = made.add("social_11", world)
    replay, controller = made.fresh()
    replay.model_configs["body_dynamics"]["social_interaction_minutes"] = 11
    _apply(controller, _proposal(ActionKind.SOCIALIZE, first, replay, second))
    made.inputs["config_s_minutes"] = {
        "world": name,
        "pairs": [[first, second], _same_zone_pair(replay)],
    }

    world = made.copy()
    del world.model_configs["body_dynamics"]["social_interaction_minutes"]
    made.inputs["default_minutes"] = {"world": made.add("social_default", world)}

    world = made.world0
    first = _idle_people(world, 1)[0]
    other_zone = next(
        item for item in world.agents.values() if item.zone_id != world.agents[first].zone_id
    )
    made.inputs["needs_someone_else"] = {
        "agent": first,
        "targets": [None, first, "nobody", other_zone.id],
    }

    world = made.copy()
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
    made.inputs["held_bed"] = {"world": made.add("beds_held", world), "pair": [first, second]}

    # Each trip action from each zone: the person and the zone of each step, as the test went.
    trips: dict[str, Any] = {}
    for action in (ActionKind.REQUEST_TOILET, ActionKind.FETCH_WATER, ActionKind.COLLECT_MEAL):
        world, controller = made.fresh()
        steps = []
        for agent_id in _idle_people(world, 100):
            for zone_id in sorted(world.zones):
                agent = world.agents[agent_id]
                agent.zone_id = zone_id
                world.model_state[MOVEMENT_STATE_KEY]["positions"][agent_id]["zone_id"] = zone_id
                target, *_ = _apply(controller, _proposal(action, agent_id, world))
                steps.append([agent_id, zone_id, target])
                if agent.activity is Activity.MOVING:
                    break
            if len(steps) >= 12:
                break
        trips[action.value] = steps
    made.inputs["trip_action_from_each_zone"] = trips

    world, controller = made.fresh()
    world.duration_minutes = world.minute_index + 1
    name = made.add("last_minute_next", world)
    people = []
    for action in (ActionKind.FETCH_WATER, ActionKind.REQUEST_TOILET, ActionKind.COLLECT_MEAL):
        agent_id = _idle_people(world, 100)[0]
        people.append([action.value, agent_id])
        _apply(controller, _proposal(action, agent_id, world))
    made.inputs["trip_past_the_end"] = {"world": name, "steps": people}

    world = made.copy()
    agent_id = _idle_people(world, 1)[0]
    world.model_state["agent_dynamics_v1"]["agents"][agent_id]["known_facility_ids"][:] = []
    made.inputs["no_known_facility"] = {"world": made.add("knows_none", world), "agent": agent_id}

    made.inputs["lower_load"] = _lower_load(made)

    world = made.copy()
    world.facilities["toilet_01"].kind = "多目的toilet"
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
    made.inputs["toilet_kind_beyond_ascii"] = {
        "world": made.add("toilet_kinds", world),
        "agent": agent_id,
    }

    world = made.copy()
    agent_id = _idle_people(world, 1)[0]
    world.agents[agent_id].activity = Activity.IDLE
    world.agents[agent_id].support_cooldown_until = 0
    world.agents[agent_id].support_request_id = None
    world.duration_minutes = world.minute_index + 1
    made.inputs["request_of_the_last_minute"] = {
        "world": made.add("asks_at_the_end", world),
        "agent": agent_id,
    }


def _lower_load(made: _Worlds) -> dict[str, Any]:
    """`test_the_lower_load_goes_before_the_shorter_walk`'s world: the walks found by the
    controller's `_route` (no capacities: it writes nothing)."""
    from sheltersim.domain.movement import MOVEMENT_STATE_KEY

    world, controller = made.fresh()
    toilets = [item for item in world.facilities.values() if "toilet" in item.kind]
    walker, waiter = _idle_people(world, 2)
    known = world.model_state["agent_dynamics_v1"]["agents"][walker]["known_facility_ids"]
    for toilet in toilets:
        if toilet.id not in known:
            known.append(toilet.id)
    world.model_configs.pop("facility_queues", None)
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
    return {"world": made.add("lower_load", world), "agent": walker, "far": far.id}


def _travel_worlds(made: _Worlds) -> None:
    """The worlds and inputs of the `_route` and `_schedule_travel` cases."""
    from sheltersim.domain.models import ZoneEdgeState

    world = made.copy()
    agent_id = _idle_people(world, 1)[0]
    zones = sorted(world.zones)[:4]
    for edge in world.zone_edges.values():
        edge.closed = True
    for edge_id, origin, destination in (
        ("z-b", zones[0], zones[1]),
        ("a-c", zones[0], zones[2]),
        ("m-d", zones[1], zones[3]),
        ("n-d", zones[2], zones[3]),
    ):
        world.zone_edges[edge_id] = ZoneEdgeState(edge_id, origin, destination, 1)
    made.inputs["equal_time"] = {
        "world": made.add("equal_times", world),
        "agent": agent_id,
        "zones": zones,
    }

    world = made.world0
    controller = made.controller
    first, other = _idle_people(world, 2)
    origin = world.agents[first].zone_id
    destination = next(
        zone
        for zone in sorted(world.zones)
        if zone != origin and controller._route(origin, zone, first)
    )
    elsewhere = next(item for item in sorted(world.agents) if world.agents[item].zone_id != origin)
    made.inputs["trip"] = {"agent": first, "destination": destination, "elsewhere": elsewhere}


def _service_worlds(made: _Worlds) -> None:
    """The worlds and inputs of the service's cases: `arbitrate`, `complete` and `request`."""
    from sheltersim.domain.models import Activity
    from sheltersim.domain.resource_service import SERVICE_STATE_KEY

    for kind in ("water_station", "meal_distribution"):
        world = made.copy()
        facility, agent_id = _at_service(world, kind)
        _free_all_staff(world, facility)
        made.inputs[f"at_{kind}"] = {
            "world": made.add(f"at_{kind}", world, edit=lambda entry, w=world: _manager_config(entry, w)),
            "agent": agent_id,
            "facility": facility.id,
        }

    waits = {}
    for reason in ("capacity", "staff", "stock", "run_end", "water_outage"):
        world = made.copy()
        facility, agent_id = _at_service(world)
        _free_all_staff(world, facility)
        if reason == "capacity":
            facility.working_units = 0
        elif reason == "staff":
            for person in world.staff.values():
                person.zone_id = "elsewhere"
        elif reason == "stock":
            ledger = world.model_state["resource_ledger_v1"]["accounts"]["water_ml"]
            world.model_configs["resource_service"]["water_ml_per_service"] = (
                ledger["remaining"] + 1
            )
        elif reason == "run_end":
            world.duration_minutes = world.minute_index + 1
        else:
            world.model_state["operational_events_v1"]["water_outages"]["cut"] = {
                "active": True,
                "target_kind": "facility",
                "target_id": facility.id,
            }
        name = made.add(f"waits_{reason}", world, edit=lambda entry, w=world: _manager_config(entry, w))
        waits[reason] = {"world": name, "agent": agent_id, "facility": facility.id}
    made.inputs["each_wait"] = waits

    world = made.copy()
    facility, agent_id = _at_service(world)
    world.model_state["resource_ledger_v1"]["used_reservation_ids"].append(f"queue-task-{agent_id}")
    made.inputs["taken_reservation_id"] = {
        "world": made.add("reservation_taken", world, edit=lambda entry: _manager_config(entry, world)),
        "agent": agent_id,
        "facility": facility.id,
    }

    world = made.copy()
    for facility in world.facilities.values():
        facility.resource_queue_agent_ids.clear()
    empty = made.add("no_queues", world)
    facility, _ = _at_service(world, "meal_distribution")
    made.inputs["queues_with_people"] = {
        "worlds": [empty, made.add("meal_queue", world)],
        "facility": facility.id,
    }

    # The manager's relief, made 1% larger on the manager the test made.
    world = made.copy()
    facility, agent_id = _at_service(world)
    _free_all_staff(world, facility)

    def relief(entry: dict[str, Any]) -> dict[str, Any]:
        entry = _manager_config(entry, world)
        attrs = entry["run"]["policies"]["controller"]["attrs"]["resource_service"]["object"]
        attrs["attrs"]["config"]["value"]["hydration_relief"] *= 1.01
        return entry

    made.inputs["relief_at_its_value"] = {
        "world": made.add("relief_larger", world, edit=relief),
        "agent": agent_id,
        "facility": facility.id,
    }

    world = made.copy()
    facility, agent_id = _at_facility(world)
    name = made.add("at_the_water", world, edit=lambda entry: _manager_config(entry, world))
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
    twin.model_state[SERVICE_STATE_KEY]["request_history"].append(
        {"minute": twin.minute_index, **options}
    )
    made.inputs["request"] = {"world": name, "agent": agent_id, "facility": facility.id}
    made.inputs["record_of_a_request"] = {
        "world": name,
        "options": options,
        "after": made.add("request_recorded", twin),
    }

    refused = []
    for change in ("kitchen", "moving"):
        world = made.copy()
        facility, agent_id = _at_facility(world)
        if change == "kitchen":
            world.agents[agent_id].zone_id = "kitchen"
            if world.agents[agent_id].zone_id == facility.zone_id:
                continue
        else:
            world.agents[agent_id].activity = Activity.MOVING
        name = made.add(f"request_{change}", world, edit=lambda entry, w=world: _manager_config(entry, w))
        refused.append({"world": name, "agent": agent_id, "facility": facility.id, "change": change})
    made.inputs["request_refused"] = refused


def service_rules_fixture() -> dict[str, Any]:
    """`service_rules.json`."""
    made = _Worlds(_minute_5, "minute_5")
    _effect_worlds(made)
    _travel_worlds(made)
    _service_worlds(made)
    return made.document()
