"""Broken worlds for the entity rules of `WorldState.validate` (#250, #348): the clock, the entity
maps, zone edges and route capacities, the Agents and the zones' capacity, facilities and their
queues, tasks and the events that serve them, staff, support requests and the stocks
(`_validate_entity_rules` and `_validate_zone_capacity` of `core/world.py`), and Python's error
for each at each entry point.

The core checks them with `native/core/tests/validate_entities.rs`. The cases change a valid world
of `validate_worlds.json` (`small_replay`: WorldState v5 without extensions; `generated`: v6 with
every extension, at minute 15) or this file's own `service` world: the generated Scenario at minute
4, where a meal is being served, one person is in a toilet and one waits for it. Each change breaks
one rule and leaves the earlier validators passing, so Python's first error is the entity rule's.
See `support/core_validate.py`.
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

from support.core_validate import Case, cases_fixture

Edit = Callable[[dict[str, Any]], None]
# The ticks the `service` world is run.
SERVICE_TICKS = 4
# The ticks the `replay_service` world is run: A003 is in toilet_1.
REPLAY_TICKS = 3


def _service() -> Any:
    """The generated Scenario at minute `SERVICE_TICKS`."""
    from support.core_tick import generated_run

    world, _, engine = generated_run()
    for _ in range(SERVICE_TICKS):
        engine.step()
    return world


def _replay_service() -> Any:
    """h00_small_replay (WorldState v5, no extensions) at minute `REPLAY_TICKS`."""
    from support.core_tick import core_tick_run

    world, _, engine = core_tick_run()
    for _ in range(REPLAY_TICKS):
        engine.step()
    return world


BASES = {"service": _service, "replay_service": _replay_service}


# ---- Edits of a world document -------------------------------------------------------------------


def _item(document: dict[str, Any], table: str, item_id: str) -> dict[str, Any]:
    """The item of a list of the document with this ID (an Agent by its profile's ID)."""
    for item in document[table]:
        found = item["profile"]["id"] if table == "agents" else item["id"]
        if found == item_id:
            return item
    raise KeyError(item_id)


def _event(document: dict[str, Any], event_id: str) -> dict[str, Any]:
    return next(event for event in document["event_queue"] if event["event_id"] == event_id)


def change(table: str, item_id: str, **values: Any) -> Edit:
    """Set fields of an item of a table (`agents`, `facilities`, ...); a dotted field name reaches
    into a dict (`needs.hunger`)."""

    def edit(document: dict[str, Any]) -> None:
        item = _item(document, table, item_id)
        for name, value in values.items():
            node = item
            *path, last = name.split(".")
            for key in path:
                node = node[key]
            node[last] = value

    return edit


def attribute(table: str, item_id: str, **values: Any) -> Callable[[Any], None]:
    """An edit of a live world: set attributes of an item of a table."""

    def edit(world: Any) -> None:
        item = getattr(world, table)[item_id]
        for name, value in values.items():
            setattr(item, name, value)

    return edit


def _initial_hydration(world: Any) -> None:
    """A v5 Agent whose initial needs (a frozen profile) have a hydration need."""
    import dataclasses

    agent = world.agents["A003"]
    needs = dataclasses.replace(agent.profile.initial_needs, hydration_need=0.1)
    agent.profile = dataclasses.replace(agent.profile, initial_needs=needs)


def payload(event_id: str, **values: Any) -> Edit:
    """Set fields of a pending event's payload (or drop one, for the value `DROP`)."""

    def edit(document: dict[str, Any]) -> None:
        fields = _event(document, event_id)["payload"]
        for name, value in values.items():
            if value is DROP:
                del fields[name]
            else:
                fields[name] = value

    return edit


def event(event_id: str, **values: Any) -> Edit:
    """Set fields of a pending event."""

    def edit(document: dict[str, Any]) -> None:
        _event(document, event_id).update(values)

    return edit


def drop_event(event_id: str) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        document["event_queue"] = [
            item for item in document["event_queue"] if item["event_id"] != event_id
        ]

    return edit


def copy_event(event_id: str, new_id: str, sequence: int, **values: Any) -> Edit:
    """Schedule a copy of a pending event, under another ID and sequence."""

    def edit(document: dict[str, Any]) -> None:
        import copy

        made = copy.deepcopy(_event(document, event_id))
        made.update(event_id=new_id, sequence=sequence, **values)
        document["event_queue"].append(made)
        document["id_counters"]["event"] = max(document["id_counters"]["event"], sequence + 1)

    return edit


def top(**values: Any) -> Edit:
    """Set top-level fields of the document."""

    def edit(document: dict[str, Any]) -> None:
        document.update(values)

    return edit


def config(name: str, value: Any) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        document["model_configs"][name] = value

    return edit


def state(*path: Any, value: Any) -> Edit:
    """Set a value of `model_state` at a path (or drop it, for `DROP`)."""

    def edit(document: dict[str, Any]) -> None:
        node = document["model_state"]
        for key in path[:-1]:
            node = node[key]
        if value is DROP:
            del node[path[-1]]
        else:
            node[path[-1]] = value

    return edit


def add_task(**fields: Any) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        task = {
            "agent_id": None,
            "due_minute": 0,
            "resource_id": None,
            "staff_id": None,
            "start_minute": 0,
        }
        task.update(fields)
        document["active_tasks"].append(task)

    return edit


def both(*edits: Edit) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        for one in edits:
            one(document)

    return edit


def toilet_queue(*, task: bool) -> Edit:
    """A003 waits for toilet_2 in the `small_replay` world, with its queue task or without."""
    edits = [
        change("facilities", "toilet_2", queue_agent_ids=["A003"]),
        change("agents", "A003", activity="queued", target_id="toilet_2"),
    ]
    if task:
        edits.append(
            add_task(
                id="t1",
                kind="toilet_queue",
                agent_id="A003",
                resource_id="toilet_2",
                start_minute=30,
                due_minute=30,
            )
        )
    return both(*edits)


def two_open_requests(document: dict[str, Any]) -> None:
    """A001 of `small_replay` with two pending requests, each with its timeout event."""
    for index, request in enumerate(document["support_requests"][:2]):
        request.update(
            agent_id="A001",
            status="pending",
            assigned_staff_id=None,
            task_id=None,
            resolved_minute=None,
        )
    agent = _item(document, "agents", "A001")
    agent.update(activity="seeking_help", support_request_id=document["support_requests"][0]["id"])


DROP = object()
ROUTING = {
    "schema_version": 1,
    "source_type": "assumption",
    "source_id": "ASM-A05-ROUTE-EDIT-V1",
    "max_agents_in_transit_by_edge": {"corridor_desk": 3},
}
# The ids of the `service` world.
MEAL_TASK = "resource-service-task-0"
MEAL_DONE = "resource-service-complete-42"
TOILET_TASK = "toilet-service-39"
TOILET_DONE = "toilet-complete-39"
TOILET_QUEUE = "toilet-queue-a6d44ffcbf5470bd56470d06"
SUPPORT_TASK = "staff-support-28"
SUPPORT_DONE = "staff-support-complete-28"
ASSIGNED = "support-1f35c22dd47d70682865bdab"
PENDING = "support-464131df94b1012ff8261017"
PENDING_TIMEOUT = "support-timeout-4"
TRIP = "agent-travel-13"
TRIP_DONE = "agent-travel-complete-13"
REPLAY_TOILET_TASK = "toilet-service-16"
# The ids of the `generated` world.
SUPPORT_TRIP = "agent-travel-76"
SUPPORT_TRIP_DONE = "agent-travel-complete-76"


CASES = [
    # ---- The clock, the seed and the maps' keys.
    Case("clock_past_the_end", "small_replay", edit=top(minute_index=1441)),
    Case("clock_without_duration", "small_replay", edit=top(duration_minutes=0, minute_index=0)),
    Case("seed_negative", "small_replay", edit=top(seed=-1)),
    Case("agent_key_empty", "small_replay", edit=change("agents", "A003", **{"profile.id": ""})),
    Case("zone_key_empty", "small_replay", edit=change("zones", "corridor", id="")),
    Case(
        "edge_key_empty",
        "small_replay",
        edit=lambda document: document["zone_edges"][0].update(id=""),
    ),
    Case("facility_key_empty", "small_replay", edit=change("facilities", "toilet_4", id="")),
    Case("staff_key_empty", "small_replay", edit=change("staff", "S05", id="")),
    # ---- Zone edges and route capacities.
    Case(
        "edge_unknown_zone",
        "small_replay",
        edit=lambda document: document["zone_edges"][0].update(to_zone_id="nowhere"),
    ),
    Case(
        "edge_no_travel_time",
        "small_replay",
        edit=lambda document: document["zone_edges"][0].update(travel_minutes=0),
    ),
    Case(
        "edge_to_itself",
        "small_replay",
        edit=lambda document: document["zone_edges"][0].update(
            to_zone_id=document["zone_edges"][0]["from_zone_id"]
        ),
    ),
    Case("routing_accepted", "small_replay", edit=config("layout_routing", ROUTING)),
    Case(
        "routing_wrong_source",
        "small_replay",
        edit=config("layout_routing", {**ROUTING, "source_id": "OTHER"}),
    ),
    Case(
        "routing_extra_key",
        "small_replay",
        edit=config("layout_routing", {**ROUTING, "extra": 1}),
    ),
    Case(
        "routing_unknown_edge",
        "small_replay",
        edit=config("layout_routing", {**ROUTING, "max_agents_in_transit_by_edge": {"nowhere": 3}}),
    ),
    Case(
        "routing_capacity_too_large",
        "small_replay",
        edit=config(
            "layout_routing",
            {**ROUTING, "max_agents_in_transit_by_edge": {"corridor_desk": 101}},
        ),
    ),
    Case(
        "routing_capacity_bool",
        "small_replay",
        edit=config(
            "layout_routing",
            {**ROUTING, "max_agents_in_transit_by_edge": {"corridor_desk": True}},
        ),
    ),
    Case(
        "routing_version_float_accepted",
        "small_replay",
        edit=config("layout_routing", {**ROUTING, "schema_version": 1.0}),
    ),
    # ---- Agents and zones.
    Case("agent_unknown_zone", "small_replay", edit=change("agents", "A003", zone_id="nowhere")),
    Case(
        "agent_hunger_above_one",
        "small_replay",
        edit=change("agents", "A003", **{"needs.hunger": 1.5}),
    ),
    Case(
        "agent_fatigue_negative",
        "small_replay",
        edit=change("agents", "A003", **{"needs.fatigue": -0.1}),
    ),
    # WorldState.from_dict refuses these values; a live Agent may hold them.
    Case(
        "agent_stress_above_one", "small_replay", edit_world=attribute("agents", "A003", stress=2.0)
    ),
    Case(
        "agent_trust_negative", "small_replay", edit_world=attribute("agents", "A003", trust=-1.0)
    ),
    Case(
        "agent_decision_negative",
        "small_replay",
        edit_world=attribute("agents", "A003", next_decision_at=-1),
    ),
    Case(
        "agent_cooldown_negative",
        "small_replay",
        edit_world=attribute("agents", "A003", support_cooldown_until=-5),
    ),
    Case(
        "zone_over_capacity", "small_replay", edit=change("zones", "information_desk", capacity=2)
    ),
    Case(
        "zone_at_capacity_accepted",
        "small_replay",
        edit=change("zones", "information_desk", capacity=3),
    ),
    Case("zone_over_capacity_v6", "generated", edit=change("zones", "toilet", capacity=2)),
    # ---- Facilities and their queues.
    Case(
        "facility_unknown_zone",
        "small_replay",
        edit=change("facilities", "toilet_4", zone_id="nowhere"),
    ),
    Case(
        "facility_capacity_negative",
        "small_replay",
        edit=change("facilities", "toilet_4", capacity=-1, working_units=0),
    ),
    Case(
        "facility_units_above_capacity",
        "small_replay",
        edit=change("facilities", "toilet_4", working_units=2),
    ),
    Case(
        "facility_over_capacity",
        "service",
        edit=change("facilities", "accessible_toilet_01", capacity=0, working_units=0),
    ),
    Case(
        "facility_queue_repeats",
        "service",
        edit=change("facilities", "accessible_toilet_01", queue_agent_ids=["A010", "A010"]),
    ),
    Case(
        "facility_service_repeats",
        "service",
        edit=change(
            "facilities", "accessible_toilet_01", active_agent_ids=["A003", "A003"], capacity=2
        ),
    ),
    Case(
        "facility_queue_unknown_agent",
        "small_replay",
        edit=change("facilities", "toilet_4", queue_agent_ids=["A999"]),
    ),
    Case(
        "facility_queue_twice",
        "service",
        edit=change("facilities", "toilet_01", queue_agent_ids=["A010"]),
    ),
    Case(
        "facility_queue_not_queued",
        "small_replay",
        edit=change("facilities", "toilet_4", queue_agent_ids=["A003"]),
    ),
    Case(
        "facility_service_unknown_agent",
        "small_replay",
        edit=change("facilities", "toilet_4", active_agent_ids=["A999"]),
    ),
    Case(
        "facility_served_and_queued",
        "service",
        edit=change("facilities", "toilet_01", active_agent_ids=["A010"]),
    ),
    Case(
        "facility_service_not_in_service",
        "small_replay",
        edit=change("facilities", "toilet_4", active_agent_ids=["A003"]),
    ),
    Case(
        "resource_facility_legacy_queue",
        "service",
        edit=change("facilities", "water_station_main", queue_agent_ids=["A010"]),
    ),
    Case(
        "toilet_resource_slots",
        "service",
        edit=change("facilities", "toilet_01", resource_queue_agent_ids=["A010"]),
    ),
    Case(
        "resource_queue_repeats",
        "service",
        edit=change("facilities", "water_station_main", resource_queue_agent_ids=["A1", "A1"]),
    ),
    Case(
        "resource_service_repeats",
        "service",
        edit=change(
            "facilities", "meal_distribution_main", resource_active_agent_ids=["A002", "A002"]
        ),
    ),
    Case(
        "resource_over_capacity",
        "service",
        edit=change(
            "facilities",
            "meal_distribution_main",
            resource_active_agent_ids=["A002", "A001"],
        ),
    ),
    Case(
        "resource_queue_unknown_agent",
        "service",
        edit=change("facilities", "water_station_main", resource_queue_agent_ids=["A999"]),
    ),
    Case(
        "resource_queue_twice",
        "service",
        edit=change("facilities", "water_station_main", resource_queue_agent_ids=["A010"]),
    ),
    Case(
        "resource_queue_inconsistent",
        "service",
        edit=change("facilities", "water_station_main", resource_queue_agent_ids=["A001"]),
    ),
    Case(
        "resource_service_unknown_agent",
        "service",
        edit=change("facilities", "water_station_main", resource_active_agent_ids=["A999"]),
    ),
    Case(
        "resource_served_twice",
        "service",
        edit=change("facilities", "water_station_main", resource_active_agent_ids=["A002"]),
    ),
    Case(
        "resource_service_inconsistent",
        "service",
        edit=change("facilities", "water_station_main", resource_active_agent_ids=["A001"]),
    ),
    Case(
        "queued_agent_without_queue",
        "small_replay",
        edit=change("agents", "A003", activity="queued"),
    ),
    Case(
        "served_agent_without_service",
        "small_replay",
        edit=change("agents", "A003", activity="in_service"),
    ),
    # ---- Tasks.
    Case(
        "travel_task_with_facility",
        "service",
        edit=change("active_tasks", TRIP, resource_id="toilet_01"),
    ),
    Case("travel_task_late_start", "service", edit=change("active_tasks", TRIP, start_minute=5)),
    Case("travel_task_no_time", "service", edit=change("active_tasks", TRIP, due_minute=0)),
    Case(
        "travel_task_twice",
        "service",
        edit=change("active_tasks", "agent-travel-34", agent_id="A005"),
    ),
    Case(
        "v5_resource_task",
        "small_replay",
        edit=add_task(
            id="r", kind="resource_service_queue", agent_id="A003", resource_id="toilet_4"
        ),
    ),
    # A changed task of a person in service breaks the facility queues' state first; an added one
    # does not.
    Case(
        "resource_task_without_facility",
        "service",
        edit=add_task(
            id="q", kind="resource_service_queue", agent_id="A001", start_minute=1, due_minute=1
        ),
    ),
    Case(
        "resource_task_at_a_toilet",
        "service",
        edit=add_task(
            id="q",
            kind="resource_service_queue",
            agent_id="A001",
            resource_id="toilet_01",
            start_minute=1,
            due_minute=1,
        ),
    ),
    Case(
        "resource_task_in_future", "service", edit=change("active_tasks", MEAL_TASK, start_minute=5)
    ),
    Case(
        "resource_queue_task_with_staff",
        "service",
        edit=add_task(
            id="q",
            kind="resource_service_queue",
            agent_id="A001",
            resource_id="water_station_main",
            staff_id="staff_003",
            start_minute=1,
            due_minute=1,
        ),
    ),
    Case(
        "resource_task_no_staff",
        "service",
        edit=change("active_tasks", MEAL_TASK, staff_id=None),
    ),
    Case(
        "resource_task_twice",
        "service",
        edit=add_task(
            id="r2",
            kind="resource_service_water",
            agent_id="A002",
            resource_id="meal_distribution_main",
            staff_id="staff_003",
            start_minute=3,
            due_minute=8,
        ),
    ),
    Case(
        "toilet_task_without_agent",
        "replay_service",
        edit=change("active_tasks", REPLAY_TOILET_TASK, agent_id=None),
    ),
    Case(
        "toilet_task_at_a_desk",
        "service",
        edit=add_task(
            id="t",
            kind="toilet_queue",
            agent_id="A001",
            resource_id="intake_desk",
            start_minute=1,
            due_minute=1,
        ),
    ),
    Case(
        "toilet_task_in_future", "service", edit=change("active_tasks", TOILET_TASK, start_minute=5)
    ),
    Case(
        "toilet_queue_task_timing",
        "service",
        edit=change("active_tasks", TOILET_QUEUE, due_minute=4),
    ),
    Case(
        "toilet_service_task_timing",
        "service",
        edit=change("active_tasks", TOILET_TASK, due_minute=3),
    ),
    Case(
        "toilet_task_twice",
        "service",
        edit=add_task(
            id="t2",
            kind="toilet_service",
            agent_id="A003",
            resource_id="accessible_toilet_01",
            start_minute=3,
            due_minute=7,
        ),
    ),
    Case(
        "toilet_kind_casefolded_accepted",
        "service",
        edit=change("facilities", "toilet_01", kind="Toilet"),
    ),
    # ---- Toilets: who is in them and their tasks.
    # WorldState v5 has no facility queues' state, which would find these first.
    Case("toilet_queue_without_task", "small_replay", edit=toilet_queue(task=False)),
    Case("toilet_queue_accepted", "small_replay", edit=toilet_queue(task=True)),
    Case(
        "toilet_queue_without_target",
        "small_replay",
        edit=both(toilet_queue(task=True), change("agents", "A003", target_id="toilet_3")),
    ),
    Case(
        "toilet_queue_task_without_entry",
        "small_replay",
        edit=both(
            toilet_queue(task=True),
            add_task(
                id="t2",
                kind="toilet_queue",
                agent_id="A003",
                resource_id="toilet_3",
                start_minute=30,
                due_minute=30,
            ),
        ),
    ),
    Case(
        "toilet_service_without_task",
        "replay_service",
        edit=change("active_tasks", REPLAY_TOILET_TASK, kind="other"),
    ),
    Case(
        "toilet_service_task_without_slot",
        "replay_service",
        edit=add_task(
            id="t2",
            kind="toilet_service",
            agent_id="A003",
            resource_id="toilet_2",
            start_minute=3,
            due_minute=7,
        ),
    ),
    Case("toilet_completion_missing", "service", edit=drop_event(TOILET_DONE)),
    Case("toilet_completion_late", "service", edit=event(TOILET_DONE, due_minute=8)),
    Case(
        "toilet_completion_other_staff", "service", edit=payload(TOILET_DONE, staff_id="staff_003")
    ),
    Case(
        "toilet_completion_no_staff_accepted",
        "service",
        edit=payload(TOILET_DONE, staff_id=DROP),
    ),
    Case(
        "toilet_completion_twice",
        "service",
        edit=copy_event(TOILET_DONE, "toilet-complete-copy", 900_001),
    ),
    # ---- Resource services: who is in them, their tasks, ledger and staff.
    Case(
        "resource_queue_without_task",
        "service",
        edit=both(
            change("facilities", "water_station_main", resource_queue_agent_ids=["A001"]),
            change(
                "agents", "A001", activity="queued", target_id="water_station_main", zone_id="food"
            ),
        ),
    ),
    Case(
        "resource_service_without_task",
        "service",
        edit=both(
            change("facilities", "water_station_main", resource_active_agent_ids=["A001"]),
            change(
                "agents",
                "A001",
                activity="in_service",
                target_id="water_station_main",
                zone_id="food",
            ),
        ),
    ),
    Case("resource_completion_missing", "service", edit=drop_event(MEAL_DONE)),
    Case(
        "resource_completion_other_resource",
        "service",
        edit=payload(MEAL_DONE, resource_id="water_ml"),
    ),
    Case("resource_completion_quantity", "service", edit=payload(MEAL_DONE, quantity=2)),
    Case(
        "resource_completion_quantity_float_accepted",
        "service",
        edit=payload(MEAL_DONE, quantity=1.0),
    ),
    Case(
        "resource_completion_not_a_completion", "service", edit=event(MEAL_DONE, kind="exogenous")
    ),
    Case(
        "ledger_reservation_missing",
        "service",
        edit=state(
            "resource_ledger_v1",
            "accounts",
            "food_meal_units",
            "reservations",
            MEAL_TASK,
            "agent_id",
            value="A001",
        ),
    ),
    Case(
        "ledger_accounts_missing",
        "service",
        edit=state("resource_ledger_v1", "accounts", "food_meal_units", value=DROP),
    ),
    Case("ledger_accounts_list", "service", edit=state("resource_ledger_v1", "accounts", value=[])),
    Case(
        "ledger_water_missing",
        "generated",
        edit=state("resource_ledger_v1", "accounts", "water_ml", value=DROP),
    ),
    Case(
        "ledger_reservations_unhashable",
        "generated",
        edit=state("resource_ledger_v1", "accounts", "water_ml", "reservations", value=[["x"]]),
    ),
    Case(
        "ledger_reservations_none",
        "generated",
        edit=state("resource_ledger_v1", "accounts", "water_ml", "reservations", value=None),
    ),
    Case(
        "resource_staff_elsewhere",
        "service",
        edit=change("staff", "staff_002", zone_id="staff"),
    ),
    Case(
        "resource_staff_unskilled",
        "service",
        edit=change("staff", "staff_002", skills=["water_service"]),
    ),
    Case(
        "resource_completion_without_task",
        "service",
        edit=both(
            copy_event(MEAL_DONE, "resource-complete-copy", 900_001),
            payload("resource-complete-copy", task_id=None),
        ),
    ),
    Case(
        "resource_completion_inactive_task",
        "service",
        edit=both(
            copy_event(MEAL_DONE, "resource-complete-copy", 900_001),
            payload("resource-complete-copy", task_id="gone"),
        ),
    ),
    Case(
        "resource_completion_cancelled_task_accepted",
        "service",
        edit=both(
            copy_event(MEAL_DONE, "resource-complete-copy", 900_001),
            payload("resource-complete-copy", task_id="gone"),
            state("resource_service_v1", "cancelled_task_ids", value=["gone"]),
        ),
    ),
    Case(
        "service_reservations_extra",
        "service",
        edit=state("resource_service_v1", "active_reservation_ids", value=[MEAL_TASK, "other"]),
    ),
    Case(
        "service_reservations_missing",
        "service",
        edit=state("resource_service_v1", "active_reservation_ids", value=[]),
    ),
    # ---- Travel.
    Case("travel_agent_not_moving", "service", edit=change("agents", "A005", activity="idle")),
    Case("travel_completion_missing", "service", edit=drop_event(TRIP_DONE)),
    Case("travel_completion_late", "service", edit=event(TRIP_DONE, due_minute=5)),
    Case(
        "travel_unknown_destination",
        "service",
        edit=payload(TRIP_DONE, destination_zone_id="nowhere"),
    ),
    Case(
        "travel_destination_unhashable",
        "service",
        edit=payload(TRIP_DONE, destination_zone_id=["x"]),
    ),
    Case(
        "travel_destination_dict",
        "service",
        edit=payload(TRIP_DONE, destination_zone_id={"x": 1}),
    ),
    Case("travel_other_target", "service", edit=payload(TRIP_DONE, facility_id="toilet_01")),
    Case(
        "support_trip_other_purpose",
        "generated",
        edit=payload(SUPPORT_TRIP_DONE, purpose="return"),
    ),
    Case(
        "support_trip_unknown_request",
        "generated",
        edit=payload(SUPPORT_TRIP_DONE, request_id="nothing"),
    ),
    Case(
        "moving_agent_without_trip",
        "small_replay",
        edit=change("agents", "A003", activity="moving"),
    ),
    # ---- Staff.
    Case("staff_unknown_zone", "small_replay", edit=change("staff", "S05", zone_id="nowhere")),
    Case(
        "staff_work_repeats",
        "small_replay",
        edit=change("staff", "S05", work_queue=["t", "t"]),
    ),
    Case(
        "staff_unknown_task",
        "small_replay",
        edit=change("staff", "S05", work_queue=["t2", "t1"]),
    ),
    Case(
        "staff_task_twice",
        "service",
        edit=change(
            "staff",
            "staff_003",
            work_queue=[SUPPORT_TASK],
            active_task_id=SUPPORT_TASK,
            busy_until=12,
        ),
    ),
    Case("staff_busy_negative", "small_replay", edit=change("staff", "S05", busy_until=-1)),
    Case("staff_idle_but_busy", "small_replay", edit=change("staff", "S05", busy_until=3)),
    Case(
        "staff_active_not_queued",
        "service",
        edit=change("staff", "staff_001", work_queue=[]),
    ),
    # ---- Support requests.
    Case(
        "request_unknown_agent",
        "small_replay",
        edit=lambda document: document["support_requests"][0].update(agent_id="A999"),
    ),
    Case(
        "request_timing",
        "small_replay",
        edit=lambda document: document["support_requests"][0].update(requested_minute=31),
    ),
    Case(
        "request_attempt_timing",
        "small_replay",
        edit=lambda document: document["support_requests"][0].update(
            last_assignment_attempt_minute=31
        ),
    ),
    Case(
        "request_wait_reason_unknown",
        "service",
        edit=change("support_requests", PENDING, last_assignment_wait_reason_code="bored"),
    ),
    Case(
        "request_wait_reason_without_attempt",
        "service",
        edit=change(
            "support_requests",
            PENDING,
            last_assignment_wait_reason_code="insufficient_run_time",
            last_assignment_attempt_minute=None,
        ),
    ),
    Case(
        "assigned_request_other_task",
        "service",
        edit=change("support_requests", ASSIGNED, task_id=TOILET_TASK),
    ),
    Case(
        "assigned_request_resting",
        "service",
        edit=change("agents", "A008", activity="resting"),
    ),
    Case(
        "assigned_request_without_completion",
        "service",
        edit=drop_event(SUPPORT_DONE),
    ),
    Case(
        "assigned_request_trip_other_staff",
        "generated",
        edit=payload(SUPPORT_TRIP_DONE, staff_id="staff_002"),
    ),
    Case(
        "request_assignment_without_status",
        "service",
        edit=change("support_requests", PENDING, assigned_staff_id="staff_003"),
    ),
    Case("pending_request_without_timeout", "service", edit=drop_event(PENDING_TIMEOUT)),
    Case(
        "pending_request_timeout_late",
        "service",
        edit=event(PENDING_TIMEOUT, due_minute=91),
    ),
    Case(
        "request_timeout_unhashable",
        "service",
        edit=payload(PENDING_TIMEOUT, request_id=["x"]),
    ),
    Case(
        "request_timeout_dict",
        "service",
        edit=payload(PENDING_TIMEOUT, request_id={"x": 1}),
    ),
    Case("agent_two_open_requests", "small_replay", edit=two_open_requests),
    Case(
        "agent_other_request",
        "service",
        edit=change("agents", "A001", support_request_id=ASSIGNED),
    ),
    Case(
        "agent_open_request_idle",
        "service",
        edit=change("agents", "A001", activity="idle"),
    ),
    Case(
        "agent_settled_request",
        "small_replay",
        edit=change("agents", "A001", support_request_id="support-464131df94b1012ff8261017"),
    ),
    # ---- Every task once more, and the stocks.
    Case(
        "task_unknown_agent",
        "small_replay",
        edit=add_task(id="x", kind="other", agent_id="A999"),
    ),
    Case(
        "task_unknown_facility",
        "small_replay",
        edit=add_task(id="x", kind="other", resource_id="nowhere"),
    ),
    Case(
        "staff_support_task_inconsistent",
        "service",
        edit=change("active_tasks", SUPPORT_TASK, resource_id="intake_desk"),
    ),
    Case(
        "task_negative_start",
        "small_replay",
        edit=add_task(id="x", kind="other", start_minute=-1),
    ),
    Case(
        "task_due_before_start",
        "small_replay",
        edit=add_task(id="x", kind="other", start_minute=5, due_minute=4),
    ),
    Case(
        "stock_negative",
        "generated",
        edit=lambda document: document["resources"].update(water_ml=-1),
    ),
]


# A WorldState v5 document has no hydration need and no resource slots, so the core's loader reads
# none: `native/core/tests/validate_entities.rs` sets these in the loaded world's rows as the edit
# sets them here.
ROW_CASES = [
    Case(
        "v5_hydration_need",
        "small_replay",
        edit_world=lambda world: setattr(world.agents["A003"].needs, "hydration_need", 0.1),
    ),
    Case(
        "v5_initial_hydration_need",
        "small_replay",
        edit_world=_initial_hydration,
    ),
    Case(
        "v5_resource_occupancy",
        "small_replay",
        edit_world=lambda world: world.facilities["toilet_4"].resource_queue_agent_ids.append(
            "A003"
        ),
    ),
]


def validate_entities_fixture() -> dict[str, Any]:
    """`validate_entities.json`: the cases of the entity rules."""
    return cases_fixture(CASES, BASES)


def validate_entity_rows_fixture() -> dict[str, Any]:
    """`validate_entity_rows.json`: the cases a test sets in the loaded world's rows."""
    return cases_fixture(ROW_CASES)
