"""The worlds of the core's tests of the service, travel and staff steps (Issue #225, lane M6).

`native/core/tests/service_rules.rs`, `requests_at_once.rs` and `staff_assignment.rs` call the
core's steps of `_apply_effect` (`propose::apply_alone`), `_route` and `_schedule_travel`, the
water and meal service (`serve`, `complete`, `request`, the record of a request), the staff's
assignment (`assign_operations`, `relief`, `assign_pending`) and the wait snapshot
(`open_waits`) on the worlds the pytest files of the same names built. This module builds those
worlds and exports them (`world_export.export_world`), each world as the variant of the one
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
import tempfile
from pathlib import Path
from types import SimpleNamespace
from typing import Any

from sheltersim.core.world_export import export_world
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
            "world": made.add(
                f"at_{kind}", world, edit=lambda entry, w=world: _manager_config(entry, w)
            ),
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
        name = made.add(
            f"waits_{reason}", world, edit=lambda entry, w=world: _manager_config(entry, w)
        )
        waits[reason] = {"world": name, "agent": agent_id, "facility": facility.id}
    made.inputs["each_wait"] = waits

    world = made.copy()
    facility, agent_id = _at_service(world)
    world.model_state["resource_ledger_v1"]["used_reservation_ids"].append(f"queue-task-{agent_id}")
    made.inputs["taken_reservation_id"] = {
        "world": made.add(
            "reservation_taken", world, edit=lambda entry: _manager_config(entry, world)
        ),
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
        name = made.add(
            f"request_{change}", world, edit=lambda entry, w=world: _manager_config(entry, w)
        )
        refused.append(
            {"world": name, "agent": agent_id, "facility": facility.id, "change": change}
        )
    made.inputs["request_refused"] = refused


def service_rules_fixture() -> dict[str, Any]:
    """`service_rules.json`."""
    made = _Worlds(_minute_5, "minute_5")
    _effect_worlds(made)
    _travel_worlds(made)
    _service_worlds(made)
    return made.document()


# ---- The requests at once (generated_texts.json and staff_assignment.json) ----------------------


def place_at(world: Any, agent_id: str, facility_id: str) -> None:
    """In the facility's zone, and knowing it, as the pytest files of the requests at once place
    people."""
    from sheltersim.domain.agent_dynamics import learn_facility
    from sheltersim.domain.movement import MOVEMENT_STATE_KEY

    facility = world.facilities[facility_id]
    world.agents[agent_id].zone_id = facility.zone_id
    state = world.model_state.get(MOVEMENT_STATE_KEY)
    if state is not None:
        state["positions"][agent_id]["zone_id"] = facility.zone_id
    if "agent_dynamics_v1" in world.model_state:
        learn_facility(world, agent_id, facility_id)


def facility_of(world: Any, word: str) -> str:
    return next(key for key, item in world.facilities.items() if word in item.kind.casefold())


def _scenario_world(change: Any = None) -> Any:
    """The generated Scenario at minute 0, its document changed by `change`."""
    from sheltersim.core.world import WorldState
    from sheltersim.experiments.scenario import ScenarioSpec
    from support.core_fixtures_texts import _generated_scenario

    scenario = _generated_scenario()
    if change is None:
        return WorldState.from_scenario(scenario)
    document = scenario.model_dump(mode="json")
    change(document)
    return WorldState.from_scenario(ScenarioSpec.model_validate(document))


def _split(document: dict[str, Any]) -> None:
    from support.core_typed_rows import _split_queues

    _split_queues(document)


def _own_queues(document: dict[str, Any]) -> None:
    document["model_configs"].pop("facility_queues", None)
    document["model_configs"]["facility_geometry"]["service_points"] = []


def restrict_water(document: dict[str, Any]) -> None:
    """The split water station's two points need a tag nobody has (`_restricted` of
    test_resource_service_texts.py)."""
    for point in document["model_configs"]["facility_geometry"]["service_points"]:
        if point["facility_id"] == "water_station_main":
            point["access"] = {
                **point["access"],
                "accessibility": "restricted",
                "required_access_tags": ["staff_key"],
            }


def at_once_worlds(generated: dict[str, Any]) -> tuple[dict[str, Any], dict[str, Any]]:
    """The worlds of the requests at once on the generated Scenario at minute 0, as variants of
    `generated` (the entry of `generated_texts.json`), and their inputs. Each world is exported
    with a controller made after the test's edits, as the pytest made it."""
    from sheltersim.domain.models import Activity

    fixture = Fixture()
    fixture.add("generated", generated, full=True)
    inputs: dict[str, Any] = {}

    def add(name: str, world: Any, edit: Any = None) -> str:
        entry = export_world(world, _controller(world), validate=False)
        return fixture.add(name, edit(entry) if edit else entry)

    world = _scenario_world(_split)
    place_at(world, "A001", "toilet_01")
    inputs["toilet_at_an_independent_queue"] = {"world": add("split_at_toilet", world)}

    water = []
    for why in ("plain", "moving"):
        world = _scenario_world(_split)
        facility_id = facility_of(world, "water")
        place_at(world, "A001", facility_id)
        if why == "moving":
            world.agents["A001"].activity = Activity.MOVING
        water.append(add(f"split_at_water_{why}", world))
    inputs["service_at_an_independent_queue"] = {"world": water[0], "facility": facility_id}
    inputs["service_turns_down"] = {"world": water[1]}

    at_point = {}
    for word in ("toilet", "water", "meal"):
        world = _scenario_world(_own_queues)
        facility_id = facility_of(world, word)
        place_at(world, "A001", facility_id)
        point = world.model_configs["facility_geometry"]["facility_points"][facility_id]
        world.model_state["movement_v1"]["positions"]["A001"]["point"] = dict(point)
        at_point[word] = {"world": add(f"own_queues_at_{word}", world), "facility": facility_id}
    inputs["at_the_point_asks_at_once"] = at_point

    world = _scenario_world()

    def no_service(entry: dict[str, Any]) -> dict[str, Any]:
        return _service_attr(entry, "resource_service", "", None)

    inputs["no_service"] = {"world": add("no_resource_service", world, no_service)}

    world = _scenario_world()
    world.minute_index = world.duration_minutes - 1
    last = add("last_minute", world)
    world.agents["A001"].support_cooldown_until = world.duration_minutes + 5
    inputs["last_minute"] = {"world": last, "cooldown": add("last_minute_cooldown", world)}

    # The request at once the queue turns down (test_resource_service_texts.py).
    turned = {}
    world = _scenario_world(lambda document: (_split(document), restrict_water(document)))
    place_at(world, "A001", "water_station_main")
    turned["queue"] = add("restricted_at_water", world)
    world = _scenario_world(_split)
    place_at(world, "A001", "water_station_main")
    world.agents["A001"].activity = Activity.SEEKING_HELP
    turned["active"] = add("split_at_water_seeking", world)
    world = _scenario_world(_split)
    place_at(world, "A001", "water_station_main")
    world.facilities["toilet_01"].queue_agent_ids.append("A001")
    turned["exclusive"] = add("split_at_water_queued", world)
    inputs["queue_turns_down"] = turned

    # Having arrived: three people asked the toilet's queue, and someone at an independent queue.
    world = _scenario_world(_own_queues)
    controller = _controller(world)
    people = ["A001", "A002", "A003"]
    for agent_id in people:
        place_at(world, agent_id, "toilet_01")
        controller.toilet_service.request(agent_id, "toilet_01", validate_state=False)
    point = world.model_configs["facility_geometry"]["facility_points"]["toilet_01"]
    inputs["arrived_at_a_slot"] = {
        "world": fixture.add("own_queues_asked", export_world(world, controller, validate=False)),
        "people": people,
        "point": dict(point),
    }
    world = _scenario_world(_split)
    controller = _controller(world)
    place_at(world, "A001", "toilet_01")
    queues = world.model_state["facility_queues_v1"]["queues"]
    queue_id = min(key for key, row in queues.items() if row["facility_id"] == "toilet_01")
    controller.toilet_service.request("A001", "toilet_01", queue_id=queue_id, validate_state=False)
    inputs["arrived_with_an_independent_queue"] = {
        "world": fixture.add("split_asked", export_world(world, controller, validate=False)),
        "queue": queue_id,
        "point": queues[queue_id]["service_point_id"],
    }
    worlds = {name: item for name, item in fixture.worlds.items() if name != "generated"}
    return worlds, json.loads(json.dumps(inputs))


# ---- staff_assignment.json ----------------------------------------------------------------------
#
# The worlds of test_staff_assignment.py, which imports these builders.


def staff_document(
    *,
    staff: list[dict[str, Any]] | None = None,
    duration: int = 200,
    configs: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """`scenario_100_agents.json` with no scheduled events, the staff and the configs given."""
    from sheltersim.experiments.scenario import ScenarioSpec
    from support.paths import SCENARIO_100_AGENTS

    document = ScenarioSpec.load(SCENARIO_100_AGENTS).model_dump(mode="json")
    document["duration_minutes"] = duration
    document["scheduled_events"] = []
    if staff is not None:
        document["staff"] = staff
    document["model_configs"]["staff_operations"] = {}
    document["model_configs"]["staff_dynamics"] = {}
    document["model_configs"]["shared_operations"] = {
        "schema_version": 1,
        "policy_version": "shared-operations-v1",
    }
    document["model_configs"].update(configs or {})
    return document


def staff_world(**options: Any) -> Any:
    """The world of `staff_document`, with the simulator's extensions."""
    from sheltersim.core.world import WorldState
    from sheltersim.domain.simulator_state import initialize_simulator_extensions
    from sheltersim.experiments.scenario import ScenarioSpec

    world = WorldState.from_scenario(ScenarioSpec.model_validate(staff_document(**options)))
    initialize_simulator_extensions(world)
    return world


def routing(capacities: dict[str, int]) -> dict[str, Any]:
    """A `layout_routing` config of these edge capacities."""
    return {
        "schema_version": 1,
        "source_type": "assumption",
        "source_id": "ASM-A05-ROUTE-EDIT-V1",
        "max_agents_in_transit_by_edge": capacities,
    }


def member(name: str, zone: str, *skills: str) -> dict[str, Any]:
    return {"id": name, "zone_id": zone, "skills": list(skills)}


def router_of(world: Any, capacities: dict[str, int] | None = None) -> Any:
    """The `SpaceRouter` of the world's tables a test gives `assign_staff_operations`."""
    from sheltersim.domain.zones import ZoneGraph
    from sheltersim.policies.space import SpaceRouter

    if capacities is None:
        return SpaceRouter(ZoneGraph.from_states(world.zones, world.zone_edges))
    graph = ZoneGraph.from_states(world.zones, world.zone_edges, edge_capacities=capacities)
    return SpaceRouter(graph)


def spread(world: Any, zone: str, stress: float) -> None:
    for agent in world.agents.values():
        agent.zone_id = zone
        agent.stress = stress


def pending_request(world: Any, number: int, agent_id: str, minute: int = 0) -> Any:
    from sheltersim.domain.models import SupportRequestState

    request = SupportRequestState(
        id=f"support-{number}",
        agent_id=agent_id,
        requested_minute=minute,
        timeout_minute=minute + 90,
    )
    world.support_requests[request.id] = request
    return request


def trip_event(world: Any, edge_id: str) -> Any:
    """A trip of nobody under way on the edge until the run's last minute."""
    from sheltersim.events.models import EventKind, ScheduledEvent

    sequence = world.allocate_event_sequence()
    return ScheduledEvent(
        event_id=f"agent-travel-complete-{sequence}",
        kind=EventKind.COMPLETION,
        due_minute=world.duration_minutes - 1,
        priority=0,
        sequence=sequence,
        event_type="agent_travel_complete",
        payload={"agent_id": "nobody", "task_id": "none", "route_edge_ids": [edge_id]},
        source_id="test",
        provenance={"source_type": "assumption", "source_id": "test"},
    )


def capacity_controller(root: Any, name: str, capacity: int = 1, *, started: bool = True) -> Any:
    """A controller writing a Run's artifacts under `root / name`, whose Run has this capacity on
    every edge. Without `started`, the writer has not written its initial artifacts, nor opened
    its trace spool: for a builder that only exports the world, which the writer does not change."""
    from sheltersim.core.engine import TraceLevel
    from sheltersim.core.world import WorldState
    from sheltersim.experiments.runner import OfflineSimulationController
    from sheltersim.experiments.scenario import ScenarioSpec
    from sheltersim.inspection.traces import RunArtifactWriter

    document = staff_document(staff=[member("S01", "toilet", "support")])
    edges = [edge["id"] for edge in document["zone_edges"]]
    document["model_configs"]["layout_routing"] = routing({edge: capacity for edge in edges})
    scenario = ScenarioSpec.model_validate(document)
    world = WorldState.from_scenario(scenario)
    artifacts = RunArtifactWriter(scenario, Path(root) / name, trace_level=TraceLevel.OFF)
    if started:
        artifacts.write_initial(world, execution_mode="offline")
    return OfflineSimulationController(world, artifacts)


def on_a_task() -> Any:
    """Four on staff, S01 at a cleaning task its shift ends during."""
    from sheltersim.domain.staff_operations import assign_staff_operations

    world = staff_world(
        duration=40,
        staff=[
            member("S01", "toilet", "cleaning"),
            member("S02", "toilet", "sanitation"),
            member("S03", "toilet", "sanitation"),
            member("S04", "sleeping", "cleaning"),
        ],
        configs={
            "staff_dynamics": {
                "shift_windows_by_staff": {
                    "S01": [{"start_minute": 0, "end_minute": 2}],
                    "S02": [{"start_minute": 2, "end_minute": 40}],
                    "S03": [{"start_minute": 2, "end_minute": 40}],
                    "S04": [{"start_minute": 2, "end_minute": 40}],
                }
            },
            "staff_operations": {
                "task_duration_minutes": {
                    "cleaning": 8,
                    "maintenance": 8,
                    "information_outreach": 8,
                    "intake": 8,
                    "patrol": 8,
                }
            },
        },
    )
    world.model_state["staff_operations_v1"]["zones"]["toilet"]["sanitation"] = 0.5
    assert assign_staff_operations(world, 0, router_of(world))
    return world


def _graph_of(router: Any) -> dict[str, Any]:
    """The graph of a `SpaceRouter` as the PyO3 entry reads it (`read_graph`)."""
    graph = router.graph
    return {
        "zones": list(graph.zones),
        "edges": [
            {
                "id": edge.id,
                "from": edge.from_zone_id,
                "to": edge.to_zone_id,
                "minutes": edge.travel_minutes,
                "closed": edge.closed,
                "capacity": edge.max_agents_in_transit,
            }
            for edge in graph.edges
        ],
    }


class _StaffWorlds:
    """The worlds of `staff_assignment.json`. A world the pytest made without a controller has the
    Run of the first world's controller (`load::loaded` reads a Run; the steps called on such a
    world, `assign_operations`, `relief` and the wait snapshot, read none of it)."""

    def __init__(self) -> None:
        self.fixture = Fixture()
        self.inputs: dict[str, Any] = {}
        world = staff_world()
        self.run = export_world(world, _controller(world))["run"]

    def add(self, name: str, world: Any, controller: Any = None, edit: Any = None) -> str:
        entry = export_world(world, controller, validate=False)
        if controller is None:
            entry["run"] = copy.deepcopy(self.run)
        if edit is not None:
            entry = edit(entry)
        return self.fixture.add(name, entry, full=not self.fixture.entries)

    def assigned(self, name: str, world: Any, minute: int, router: Any = None) -> dict[str, Any]:
        """A world `assign_staff_operations` is given, with its minute and router."""
        return {
            "world": self.add(name, world),
            "minute": minute,
            "router": _graph_of(router or router_of(world)),
        }

    def document(self) -> dict[str, Any]:
        return {"worlds": self.fixture.worlds, "inputs": json.loads(json.dumps(self.inputs))}


def _assign_worlds(made: _StaffWorlds) -> None:
    """`assign_staff_operations`'s worlds, by the label of the pytest's call."""
    key = "staff_operations_v1"
    calls: dict[str, Any] = {}
    for policy in (None, "staff-urgency-first-v1", "staff-travel-first-v1"):
        configs: dict[str, Any] = {}
        if policy is not None:
            configs["policy_registry"] = {"schema_version": 2, "staff_operations": policy}
        world = staff_world(
            staff=[
                member("S01", "toilet", "cleaning", "patrol", "maintenance"),
                member("S02", "sleeping", "cleaning", "support"),
                member("S03", "toilet", "repair"),
            ],
            configs=configs,
        )
        for zone, record in world.model_state[key]["zones"].items():
            record["sanitation"] = 0.5 if zone == "toilet" else 0.7
        spread(world, "sleeping", 0.8)
        facility = sorted(world.facilities)[0]
        world.facilities[facility].working_units = world.facilities[facility].capacity - 1
        calls[f"ranks.{policy}"] = made.assigned(f"ranks.{policy}", world, 0)

    world = staff_world(
        staff=[member("S01", "toilet", "cleaning")],
        configs={
            "staff_operations": {"thresholds_by_zone": {"toilet": {"cleaning_sanitation_below": 1}}}
        },
    )
    world.model_state[key]["zones"]["toilet"]["sanitation"] = 0.99
    calls["zone_threshold"] = made.assigned("zone_threshold", world, 0)

    for fatigue in (0.0005, 0.0015, 0.0025, 0.001):
        world = staff_world(staff=[member(f"S{n}", "toilet", "patrol") for n in range(3)])
        spread(world, "toilet", 0.9)
        state = world.model_state["staff_dynamics_v1"]["staff"]
        state["S0"]["fatigue"] = fatigue
        state["S1"]["fatigue"] = 0.0005
        calls[f"rotation.{fatigue}"] = made.assigned(f"rotation.{fatigue}", world, 0)

    for sanitation in (0.598, 0.602):
        world = staff_world(staff=[member("S01", "toilet", "cleaning", "patrol")])
        spread(world, "toilet", 0.8)
        for zone, record in world.model_state[key]["zones"].items():
            record["sanitation"] = sanitation if zone == "toilet" else 1.0
        calls[f"patrol_urgency.{sanitation}"] = made.assigned(
            f"patrol_urgency.{sanitation}", world, 0
        )

    world = staff_world(staff=[member(f"S{n}", "toilet", "patrol") for n in range(2)], duration=400)
    spread(world, "toilet", 0.9)
    world.minute_index = 300
    entry = {
        "task_id": "old",
        "task_kind": "patrol",
        "target_id": None,
        "target_zone_id": "toilet",
        "reason": "r",
        "status": "completed",
        "start_minute": 0,
        "arrival_minute": 0,
        "travel_minutes": 0,
        "causation_event_ids": [],
    }
    history = world.model_state[key]["history"]
    history.append({**entry, "staff_id": "S0", "complete_minute": 60})
    history.append({**entry, "staff_id": "S1", "complete_minute": 59})
    calls["recent_work"] = made.assigned("recent_work", world, 300)

    world = staff_world(staff=[member(f"S{n}", "toilet", "patrol") for n in range(2)])
    spread(world, "toilet", 0.9)
    state = world.model_state["staff_dynamics_v1"]["staff"]
    state["S0"]["fatigue"] = 0.0504
    state["S1"]["fatigue"] = 0.0498
    calls["fatigue_rounds"] = made.assigned("fatigue_rounds", world, 0)

    for label, policy in (
        ("travel_first", "staff-travel-first-v1"),
        ("urgency_first", "staff-urgency-first-v1"),
    ):
        world = staff_world(
            staff=[member("S01", "toilet", "cleaning")],
            configs={"policy_registry": {"schema_version": 2, "staff_operations": policy}},
        )
        for zone, record in world.model_state[key]["zones"].items():
            record["sanitation"] = 0.8 if zone == "toilet" else 0.2
        calls[label] = made.assigned(label, world, 0)

    world = staff_world(staff=[member("S01", "toilet", "cleaning", "patrol")])
    spread(world, "toilet", 0.8)
    for zone, record in world.model_state[key]["zones"].items():
        record["sanitation"] = 0.6 if zone == "toilet" else 1.0
    calls["equal_ranks"] = made.assigned("equal_ranks", world, 0)

    world = staff_world(
        staff=[member("S01", "toilet", "cleaning")],
        configs={
            "staff_operations": {
                "thresholds_by_zone": {"toilet": {"cleaning_sanitation_below": 0.00001}}
            }
        },
    )
    for zone, record in world.model_state[key]["zones"].items():
        record["sanitation"] = 0.0 if zone == "toilet" else 1.0
    calls["small_threshold"] = made.assigned("small_threshold", world, 0)

    world = staff_world(
        staff=[member("S01", "toilet", "patrol"), member("S02", "toilet", "patrol")],
        configs={"layout_routing": routing({})},
    )
    spread(world, "sleeping", 0.9)
    router = router_of(world, {edge: 1 for edge in world.zone_edges})
    calls["full_passage"] = made.assigned("full_passage", world, 0, router)

    world = staff_world(staff=[member(f"S{n}", "toilet", "patrol") for n in range(3)])
    spread(world, "sleeping", 0.9)
    calls["passage_loads"] = made.assigned("passage_loads", world, 0)

    world = staff_world(
        duration=3000,
        staff=[member("S01", "corridor", "patrol"), member("S09", "sleeping", "cleaning")],
    )
    spread(world, "toilet", 0.9)
    for edge in ("sleeping_corridor", "corridor_toilet"):
        world.zone_edges[edge].travel_minutes = 500
    world.minute_index = 496
    world.staff["S09"].active_task_id = "walking"
    state = world.model_state[key]["staff"]
    state["S09"] = {
        **state["S09"],
        "phase": "travel",
        "task_id": "walking",
        "task_kind": "cleaning",
        "target_zone_id": "toilet",
        "reason": "r",
        "started_minute": 0,
        "travel_due_minute": 1000,
        "due_minute": 1010,
        "route_zone_ids": ["sleeping", "corridor", "toilet"],
        "route_edge_ids": ["sleeping_corridor", "corridor_toilet"],
        "travel_minutes": 1000,
    }
    router = router_of(world, {"corridor_toilet": 1})
    calls["trip_under_way"] = made.assigned("trip_under_way", world, 496, router)
    made.inputs["assign_operations"] = calls

    world = on_a_task()
    made.inputs["on_a_task"] = {"world": made.add("on_a_task", world)}
    world.minute_index = 1
    made.inputs["relief"] = {"world": made.add("on_a_task_at_1", world)}
    world.model_state["staff_dynamics_v1"]["staff"]["S02"]["fatigue"] = 0.5
    made.inputs["relief_least_work"] = {"world": made.add("on_a_task_s02_tired", world)}
    world = on_a_task()
    world.model_state[key]["staff"]["S01"]["due_minute"] = 2
    world.minute_index = 1
    made.inputs["no_relief_near_the_end"] = {"world": made.add("on_a_task_due_2", world)}


def _support(world: Any, zone: str = "sleeping") -> list[str]:
    """Everyone in the zone; the first person's pending request."""
    people = sorted(world.agents)
    for agent in world.agents.values():
        agent.zone_id = zone
    pending_request(world, 1, people[0])
    return people


def _wait_worlds(made: _StaffWorlds) -> None:
    """The worlds of `_support_wait_snapshot`, by test, each with the reasons the pytest checks."""
    from sheltersim.domain.models import Activity

    support = [member("S01", "toilet", "support")]
    waits: dict[str, Any] = {}
    world = staff_world(staff=support, duration=200)
    _support(world)
    steps = [made.add("waits_not_attempted", world)]
    world.metric_state["support_assignment_metrics_minute"] = 0.0
    steps.append(made.add("waits_not_recorded", world))
    request = world.support_requests["support-1"]
    request.last_assignment_attempt_minute = 0
    steps.append(made.add("waits_attempted", world))
    request.last_assignment_wait_reason_code = "route_unavailable_at_assignment"
    steps.append(made.add("waits_recorded", world))
    waits["free_worker"] = steps

    world = staff_world(staff=support, duration=3)
    _support(world)
    waits["past_the_end"] = made.add("waits_short_run", world)

    route = router_of(staff_world(staff=support)).route("sleeping", "toilet")
    last = route.travel_minutes + 10
    ends = []
    for duration, too_late in ((last, True), (last + 1, False)):
        world = staff_world(staff=support, duration=duration)
        _support(world)
        ends.append([made.add(f"waits_run_of_{duration}", world), too_late])
    waits["ends_exactly"] = ends

    world = staff_world(staff=support)
    people = sorted(world.agents)
    for agent in world.agents.values():
        agent.zone_id = "sleeping"
    world.agents[people[1]].zone_id = "toilet"
    world.agents[people[2]].activity = Activity.MOVING
    world.agents[people[2]].target_id = "toilet"
    pending_request(world, 1, people[0])
    full = []
    for capacity in (2, 3):
        world.zones["toilet"].capacity = capacity
        full.append(made.add(f"waits_toilet_holds_{capacity}", world))
    waits["full_destination"] = full

    world = staff_world(staff=support)
    _support(world)
    world.zones["toilet"].capacity = 0
    steps = [made.add("waits_toilet_full", world)]
    world.zones["toilet"].capacity = None
    world.model_configs["layout_routing"] = routing({edge: 1 for edge in world.zone_edges})
    for edge in world.zone_edges.values():
        world.schedule_event(trip_event(world, edge.id))
    steps.append(made.add("waits_passages_full", world))
    for edge in world.zone_edges.values():
        edge.closed = True
    steps.append(made.add("waits_passages_closed", world))
    waits["full_passage"] = steps
    made.inputs["waits"] = waits
    made.inputs["many_worlds"] = _many_worlds(made)


def _many_worlds(made: _StaffWorlds) -> dict[str, Any]:
    """`test_many_worlds_read_the_same_reasons_with_free_and_busy_workers`: the draws of its
    `random.Random(2615)`. A world is its base (the staff's zones and the run's length: the
    worlds the extensions start from) and what the test set after: the people's zones (`t` for
    the toilet, `s` for sleeping, in ID order), the busy staff, the requests' rows, the metric and
    a zone's capacity. The builder checks that these make the world the test made."""
    generator = random.Random(2615)
    worlds = []
    for _ in range(120):
        staff = [
            member("S01", generator.choice(["toilet", "sleeping"]), "support"),
            member("S02", generator.choice(["toilet", "sleeping"]), "medical"),
            member("S03", generator.choice(["toilet", "sleeping"]), "support", "nursing"),
        ]
        duration = generator.choice([3, 5, 200])
        world = staff_world(staff=staff, duration=duration)
        base_name = "many_" + "".join(item["zone_id"][0] for item in staff) + f"_{duration}"
        base = made.add(base_name, world)
        before = json.loads(world._canonical_text(world._canonical_data()))
        for agent in world.agents.values():
            agent.zone_id = generator.choice(["toilet", "sleeping"])
        for person in world.staff.values():
            if generator.random() < 0.4:
                person.active_task_id = "busy"
        people = sorted(world.agents)
        for number in range(4):
            request = pending_request(world, number, people[number])
            if generator.random() < 0.3:
                request.last_assignment_attempt_minute = 0
                request.last_assignment_wait_reason_code = generator.choice(
                    [None, "", "insufficient_run_time"]
                )
        metric = generator.random() < 0.5
        if metric:
            world.metric_state["support_assignment_metrics_minute"] = 0.0
        capacity = None
        if generator.random() < 0.3:
            capacity = [generator.choice(sorted(world.zones)), generator.choice([0, 1, 300])]
            world.zones[capacity[0]].capacity = capacity[1]
        after = json.loads(world._canonical_text(world._canonical_data()))
        drawn = {
            "base": base,
            "zones": "".join(after_agent["zone_id"][0] for after_agent in after["agents"]),
            "busy": [row["id"] for row in after["staff"] if row["active_task_id"] == "busy"],
            "requests": after["support_requests"],
            "metric": metric,
            "capacity": capacity,
        }
        assert _drawn_world(before, drawn) == after
        worlds.append(drawn)
    return {"worlds": worlds}


def _drawn_world(document: dict[str, Any], drawn: dict[str, Any]) -> dict[str, Any]:
    """A canonical document with the draws of `_many_worlds` set, as the Rust test sets them."""
    document = copy.deepcopy(document)
    zones = {"t": "toilet", "s": "sleeping"}
    for agent, zone in zip(document["agents"], drawn["zones"], strict=True):
        agent["zone_id"] = zones[zone]
    for row in document["staff"]:
        if row["id"] in drawn["busy"]:
            row["active_task_id"] = "busy"
    document["support_requests"] = drawn["requests"]
    if drawn["metric"]:
        document["metric_state"]["support_assignment_metrics_minute"] = 0.0
        document["metric_state"] = dict(sorted(document["metric_state"].items()))
    if drawn["capacity"] is not None:
        zone, capacity = drawn["capacity"]
        next(row for row in document["zones"] if row["id"] == zone)["capacity"] = capacity
    return document


def _route_worlds(made: _StaffWorlds, root: str) -> None:
    """The worlds of a controller whose Run has route capacities, and its staff service's own
    route and trip."""
    controller = capacity_controller(root, "full", started=False)
    world = controller.world
    for agent in world.agents.values():
        agent.zone_id = "sleeping"
    for edge in world.zone_edges:
        world.schedule_event(trip_event(world, edge))
    made.inputs["route_full_edge"] = {"world": made.add("edges_full", world, controller)}

    controller = capacity_controller(root, "room", capacity=2, started=False)
    world = controller.world
    for edge in world.zone_edges:
        world.schedule_event(trip_event(world, edge))
    made.inputs["route_room"] = {"world": made.add("edges_with_room", world, controller)}

    own = {}
    for label in ("full", "late", "trip"):
        controller = capacity_controller(root, f"own-{label}", started=False)
        world = controller.world
        people = sorted(world.agents)
        for agent in world.agents.values():
            agent.zone_id = "sleeping"
        if label == "full":
            for edge in world.zone_edges:
                world.schedule_event(trip_event(world, edge))
        pending_request(world, 1, people[0])

        def late(entry: dict[str, Any], world: Any = world) -> dict[str, Any]:
            return _service_attr(
                entry, "staff_service", "task_duration_minutes", world.duration_minutes
            )

        name = made.add(f"own_step_{label}", world, controller, late if label == "late" else None)
        own[label] = {"world": name}
    made.inputs["own_route_and_trip"] = own


def _plain_worlds(made: _StaffWorlds) -> None:
    """The requests at once of `scenario_100_agents.json`, which has no floor plan."""
    from sheltersim.core.world import WorldState
    from sheltersim.experiments.scenario import ScenarioSpec
    from support.paths import SCENARIO_100_AGENTS

    def plain() -> Any:
        scenario = ScenarioSpec.model_validate_json(SCENARIO_100_AGENTS.read_text(encoding="utf-8"))
        return WorldState.from_scenario(scenario)

    world = plain()
    agent_id = sorted(world.agents)[0]
    facility_id = facility_of(world, "toilet")
    place_at(world, agent_id, facility_id)
    made.inputs["plain_at_toilet"] = {
        "world": made.add("plain_at_toilet", world, _controller(world)),
        "agent": agent_id,
        "facility": facility_id,
    }

    world = plain()
    world.model_configs["layout_routing"] = {
        "max_agents_in_transit_by_edge": {"toilet_corridor": 1}
    }
    world.facilities["toilet_4"].zone_id = "sleeping"
    for agent_id in ("A001", "A002", "A003", "A004", "A005"):
        world.agents[agent_id].zone_id = "toilet"
    made.inputs["capacity_wait"] = {
        "world": made.add("plain_toilet_passage", world, _controller(world))
    }


def staff_assignment_fixture() -> dict[str, Any]:
    """`staff_assignment.json`."""
    made = _StaffWorlds()
    made.add("staff_base", staff_world())
    _assign_worlds(made)
    _wait_worlds(made)
    with tempfile.TemporaryDirectory(prefix="core-fixture-staff-") as root:
        _route_worlds(made, root)
    _plain_worlds(made)
    return made.document()
