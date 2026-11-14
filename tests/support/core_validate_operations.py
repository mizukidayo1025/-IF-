"""Broken worlds for the operational events and the infrastructure of `WorldState.validate` (#250,
#348): `validate_operational_events_state` (`domain/operational.py`, H03: shipments, water
outages, staff absences) and `validate_infrastructure_runtime_state` (`domain/infrastructure.py`,
A03), and Python's error for each at each entry point.

The core checks them with `native/core/tests/validate_operations.rs`. The cases change this file's
`operations` world: the generated Scenario with the gate's operational conditions
(`core_texts.CONDITIONS`: a power outage, an equipment failure, heat, a repeated staff absence and a
delivery) at minute `MINUTE`, when the first absence is over, both outages are on and the heat ends.
Each change breaks one rule and leaves the earlier validators passing. See
`support/core_validate.py`.
"""

from __future__ import annotations

import copy
from collections.abc import Callable
from typing import Any

from support.core_validate import Case, cases_fixture

Edit = Callable[[dict[str, Any]], None]
# The minute of the `operations` world.
MINUTE = 50
DROP = object()


def _operations() -> Any:
    from support.core_texts import CONDITIONS, EVACUEES, _gate, _run

    gate = _gate()
    scenario = gate.generated(evacuees=EVACUEES, minutes=120, operational_conditions=CONDITIONS)
    return _run(scenario, MINUTE)[0]


BASES = {"operations": _operations}
BASE = "operations"

# The ids of the `operations` world.
OPS = "operational_events_v1"
INFRA = "infrastructure_v1"
SHIPMENT = "region-shipment-006"
DELIVERY = "region-condition-006-resource_delivery"
FIRST_ABSENCE = "region-condition-004-staff_unavailable"
FIRST_RETURN = "staff-restored-56"
NEXT_ABSENCE = "region-condition-005-staff_unavailable"
SOURCE = "SRC-OPERATOR-INITIAL-CONDITIONS"
PROVENANCE = {"source_type": "assumption", "source_id": SOURCE}


# ---- Edits of a world document -------------------------------------------------------------------


def both(*edits: Edit) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        for one in edits:
            one(document)

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


def config(name: str, value: Any) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        if value is DROP:
            del document["model_configs"][name]
        else:
            document["model_configs"][name] = value

    return edit


def _event(document: dict[str, Any], event_id: str) -> dict[str, Any]:
    return next(item for item in document["event_queue"] if item["event_id"] == event_id)


def change_event(event_id: str, payload: dict[str, Any] | None = None, **fields: Any) -> Edit:
    """Change a pending event: fields, and keys of its payload (`DROP` drops one)."""

    def edit(document: dict[str, Any]) -> None:
        item = _event(document, event_id)
        item.update(fields)
        for key, value in (payload or {}).items():
            if value is DROP:
                del item["payload"][key]
            else:
                item["payload"][key] = value

    return edit


def event_of(event_type: str, payload: dict[str, Any], **fields: Any) -> dict[str, Any]:
    """A pending event as the document holds one; its sequence is set when it is added."""
    made = {
        "event_id": f"test-{event_type.replace('_', '-')}",
        "kind": "exogenous",
        "due_minute": 80,
        "priority": 0,
        "sequence": 0,
        "event_type": event_type,
        "payload": payload,
        "source_id": SOURCE,
        "causation_id": None,
        "provenance": PROVENANCE,
    }
    made.update(fields)
    return made


def add(*events: dict[str, Any]) -> Edit:
    """Schedule events, each with a new sequence past the event counter."""

    def edit(document: dict[str, Any]) -> None:
        for made in events:
            made = copy.deepcopy(made)
            made["sequence"] = document["id_counters"]["event"]
            document["id_counters"]["event"] += 1
            document["event_queue"].append(made)

    return edit


def first_due(document: dict[str, Any]) -> dict[str, Any]:
    """The pending social interaction due now: an event in flight that no rule here reads."""
    return next(
        item
        for item in document["event_queue"]
        if item["event_type"] == "social_interaction" and item["due_minute"] <= MINUTE
    )


def pending_by_id(event_id: str) -> Callable[[dict[str, Any]], dict[str, Any]]:
    return lambda document: copy.deepcopy(_event(document, event_id))


# A water outage of the zone `food`, which has the water station, pending at minute 80.
WATER = event_of("water_outage", {"outage_id": "w1", "zone_id": "food"}, event_id="water-1")
WATER_RECORD = {
    "target_kind": "zone",
    "target_id": "food",
    "outage_event_id": "water-1",
    "restore_event_id": None,
    "active": False,
}
RESTORE = event_of(
    "water_restored",
    {"outage_id": "w1"},
    event_id="water-back-1",
    due_minute=90,
    causation_id="water-1",
)


def water(record: dict[str, Any] | None = None, *events: dict[str, Any]) -> Edit:
    """The water outage `w1` scheduled, its record in the state (changed by `record`), and more
    events."""
    made = {**WATER_RECORD, **(record or {})}
    return both(add(WATER, *events), state(OPS, "water_outages", "w1", value=made))


# staff_002 away from minute 50 to 80, the absence in flight: its return is scheduled.
AWAY = event_of(
    "staff_unavailable",
    {"staff_id": "staff_002", "until_minute": 80},
    event_id="away-2",
    due_minute=50,
)
BACK = event_of(
    "staff_restored",
    {"staff_id": "staff_002", "unavailability_event_id": "away-2"},
    event_id="back-2",
    kind="completion",
    causation_id="away-2",
)
AWAY_RECORD = {
    "available": False,
    "unavailability_event_id": "away-2",
    "until_minute": 80,
    "restore_event_id": "back-2",
}


def away(*more: Edit) -> Edit:
    return both(add(AWAY, BACK), state(OPS, "staff_status", "staff_002", value=AWAY_RECORD), *more)


def away_in_flight(document: dict[str, Any]) -> dict[str, Any]:
    return copy.deepcopy(_event(document, "away-2"))


OPERATIONAL_CASES = [
    # ---- The exact shape of the state, and the config.
    Case("ops_state_not_a_dict", BASE, edit=state(OPS, value=[])),
    Case("ops_state_extra_key", BASE, edit=state(OPS, "extra", value=1)),
    Case("ops_state_version_2", BASE, edit=state(OPS, "schema_version", value=2)),
    Case("ops_state_version_float_accepted", BASE, edit=state(OPS, "schema_version", value=1.0)),
    Case("ops_shipments_a_list", BASE, edit=state(OPS, "shipments", value=[])),
    Case("ops_cancelled_a_dict", BASE, edit=state(OPS, "cancelled_staff_task_ids", value={})),
    Case("ops_state_missing", BASE, edit=state(OPS, value=DROP)),
    Case("ops_state_none", BASE, edit=state(OPS, value=None)),
    Case(
        "ops_config_not_a_flag",
        BASE,
        edit=both(state(OPS, value=DROP), config("operational_events", {"enabled": 1})),
    ),
    Case(
        "ops_disabled_without_state",
        BASE,
        edit=both(state(OPS, value=DROP), config("operational_events", {"enabled": False})),
    ),
    # ---- The event in flight.
    Case("in_flight_not_due", BASE, in_flight=pending_by_id(NEXT_ABSENCE)),
    Case(
        "in_flight_not_pending",
        BASE,
        in_flight=lambda document: {
            **first_due(document),
            "event_id": "nowhere",
            "sequence": 999_999,
        },
    ),
    Case(
        "in_flight_changed",
        BASE,
        in_flight=lambda document: {**first_due(document), "priority": 7},
    ),
    Case("in_flight_due_accepted", BASE, in_flight=first_due),
    # ---- The events the check reads.
    Case(
        "facility_outage_fields",
        BASE,
        edit=add(event_of("facility_outage", {"facility_id": "toilet_01"})),
    ),
    Case(
        "facility_recovery_unknown_facility",
        BASE,
        edit=add(event_of("facility_recovery", {"facility_id": "nowhere", "units_restored": 1})),
    ),
    Case(
        "facility_outage_amount_negative",
        BASE,
        edit=add(event_of("facility_outage", {"facility_id": "toilet_01", "units_out": -1})),
    ),
    Case(
        "facility_outage_amount_bool",
        BASE,
        edit=add(event_of("facility_outage", {"facility_id": "toilet_01", "units_out": True})),
    ),
    Case(
        "facility_outage_facility_dict",
        BASE,
        edit=add(event_of("facility_outage", {"facility_id": {"x": 1}, "units_out": 1})),
    ),
    Case(
        "resource_loss_fields",
        BASE,
        edit=add(
            event_of(
                "resource_loss",
                {
                    "warehouse_id": "main_warehouse",
                    "resource_id": "water_ml",
                    "quantity": 1,
                    "x": 1,
                },
            )
        ),
    ),
    Case(
        "delivery_delay_accepted",
        BASE,
        edit=add(event_of("delivery_delayed", {"shipment_id": SHIPMENT, "to_minute": 70})),
    ),
    Case(
        "delivery_delay_infrastructure_accepted",
        BASE,
        edit=add(
            event_of(
                "delivery_delayed",
                {"shipment_id": SHIPMENT, "to_minute": 70},
                kind="completion",
                causation_id="region-condition-001-power_outage",
                provenance={
                    "policy_version": "infrastructure-events-v1",
                    "source_id": "GATE-SYNTHETIC-INPUT",
                },
            )
        ),
    ),
    Case(
        "delivery_delay_completion",
        BASE,
        edit=add(
            event_of(
                "delivery_delayed",
                {"shipment_id": SHIPMENT, "to_minute": 70},
                kind="completion",
                causation_id="x",
            )
        ),
    ),
    Case(
        "delivery_delay_without_source",
        BASE,
        edit=add(
            event_of("delivery_delayed", {"shipment_id": SHIPMENT, "to_minute": 70}, source_id=None)
        ),
    ),
    Case(
        "delivery_delay_bad_shipment",
        BASE,
        edit=add(event_of("delivery_delayed", {"shipment_id": "-x", "to_minute": 70})),
    ),
    Case(
        "delivery_delay_minute_text",
        BASE,
        edit=add(event_of("delivery_delayed", {"shipment_id": SHIPMENT, "to_minute": "70"})),
    ),
    Case(
        "staff_absence_ends_early",
        BASE,
        edit=change_event(NEXT_ABSENCE, {"until_minute": 55}),
    ),
    Case(
        "staff_absence_unknown_member",
        BASE,
        edit=change_event(NEXT_ABSENCE, {"staff_id": "staff_999"}),
    ),
    Case(
        "staff_return_not_a_completion",
        BASE,
        edit=add(
            {
                **BACK,
                "kind": "exogenous",
                "payload": {"staff_id": "staff_001", "unavailability_event_id": FIRST_ABSENCE},
            }
        ),
    ),
    Case(
        "staff_return_other_cause",
        BASE,
        edit=add(
            {
                **BACK,
                "payload": {"staff_id": "staff_001", "unavailability_event_id": NEXT_ABSENCE},
                "causation_id": NEXT_ABSENCE,
            }
        ),
    ),
    # ---- Shipments.
    Case("shipment_bad_id", BASE, edit=change_event(DELIVERY, {"shipment_id": "-x"})),
    Case(
        "shipment_twice",
        BASE,
        edit=add(
            event_of(
                "resource_delivery",
                {
                    "warehouse_id": "main_warehouse",
                    "resource_id": "food_meal_units",
                    "quantity": 1,
                    "shipment_id": SHIPMENT,
                },
            )
        ),
    ),
    Case("shipment_state_missing", BASE, edit=state(OPS, "shipments", SHIPMENT, value=DROP)),
    Case(
        "shipment_record_late",
        BASE,
        edit=state(OPS, "shipments", SHIPMENT, "due_minute", value=61),
    ),
    Case(
        "shipment_record_float_accepted",
        BASE,
        edit=state(OPS, "shipments", SHIPMENT, "due_minute", value=60.0),
    ),
    # ---- Water outages.
    Case("water_outage_accepted", BASE, edit=water()),
    Case(
        "water_outage_unknown_zone",
        BASE,
        edit=add(event_of("water_outage", {"outage_id": "w1", "zone_id": "nowhere"})),
    ),
    Case(
        "water_outage_not_a_station",
        BASE,
        edit=add(event_of("water_outage", {"outage_id": "w1", "facility_id": "toilet_01"})),
    ),
    Case(
        "water_outage_bad_id",
        BASE,
        edit=add(event_of("water_outage", {"outage_id": "", "zone_id": "food"})),
    ),
    Case(
        "water_outage_zone_without_station",
        BASE,
        edit=add(event_of("water_outage", {"outage_id": "w1", "zone_id": "sleeping"})),
    ),
    Case(
        "water_outage_twice",
        BASE,
        edit=water(None, {**WATER, "event_id": "water-2"}),
    ),
    Case("water_outage_without_record", BASE, edit=add(WATER)),
    Case("water_outage_record_other_kind", BASE, edit=water({"target_kind": "facility"})),
    Case("water_outage_record_active_text", BASE, edit=water({"active": "no"})),
    Case("water_restore_unrecorded", BASE, edit=water(None, RESTORE)),
    Case(
        "water_restore_accepted",
        BASE,
        edit=water({"restore_event_id": "water-back-1"}, RESTORE),
    ),
    Case(
        "water_restore_unknown_outage",
        BASE,
        edit=add({**RESTORE, "payload": {"outage_id": "w9"}}),
    ),
    Case(
        "water_restore_bad_id",
        BASE,
        edit=add({**RESTORE, "payload": {"outage_id": "?"}}),
    ),
    Case(
        "water_restore_other_cause",
        BASE,
        edit=water({"restore_event_id": "water-back-1"}, {**RESTORE, "causation_id": "other"}),
    ),
    Case(
        "water_restore_before_outage",
        BASE,
        edit=water({"restore_event_id": "water-back-1"}, {**RESTORE, "due_minute": 70}),
    ),
    Case("water_outage_active_early", BASE, edit=water({"active": True})),
    # ---- Staff status.
    Case("staff_status_missing", BASE, edit=state(OPS, "staff_status", "staff_003", value=DROP)),
    Case("staff_status_not_a_dict", BASE, edit=state(OPS, "staff_status", "staff_003", value=[])),
    Case(
        "staff_status_available_text",
        BASE,
        edit=state(OPS, "staff_status", "staff_003", "available", value="yes"),
    ),
    Case(
        "staff_status_available_with_time",
        BASE,
        edit=state(OPS, "staff_status", "staff_003", "until_minute", value=60),
    ),
    Case(
        "staff_status_other_member_s_absence",
        BASE,
        edit=state(
            OPS, "staff_status", "staff_003", "unavailability_event_id", value=FIRST_ABSENCE
        ),
    ),
    Case(
        "staff_status_absence_list",
        BASE,
        edit=state(OPS, "staff_status", "staff_003", "unavailability_event_id", value=["x"]),
    ),
    Case(
        "staff_status_not_back",
        BASE,
        edit=state(OPS, "staff_status", "staff_001", "available", value=False),
    ),
    Case(
        "staff_status_without_return",
        BASE,
        edit=state(OPS, "staff_status", "staff_001", "restore_event_id", value=None),
    ),
    Case(
        "staff_status_two_returns",
        BASE,
        edit=add(
            {
                **BACK,
                "event_id": "back-1",
                "due_minute": 45,
                "causation_id": FIRST_ABSENCE,
                "payload": {"staff_id": "staff_001", "unavailability_event_id": FIRST_ABSENCE},
            }
        ),
    ),
    Case("staff_away_in_flight_accepted", BASE, edit=away(), in_flight=away_in_flight),
    Case(
        "staff_away_but_busy",
        BASE,
        edit=away(
            lambda document: next(
                item for item in document["staff"] if item["id"] == "staff_002"
            ).update(busy_until=3)
        ),
        in_flight=away_in_flight,
    ),
    # ---- Cancelled tasks.
    Case(
        "cancelled_task_empty",
        BASE,
        edit=state(OPS, "cancelled_staff_task_ids", value=["staff-support-51", ""]),
    ),
    Case(
        "cancelled_task_twice",
        BASE,
        edit=state(OPS, "cancelled_staff_task_ids", value=["staff-support-51", "staff-support-51"]),
    ),
]

POWER = "region-power-outage-000-000"
EQUIPMENT = "region-equipment-failure-001-000"
HEAT = "region-stressor-002-000"
POWER_BACK = "power-restored-74"
EQUIPMENT_BACK = "equipment-repaired-92"
HEAT_END = "environmental-stressor-end-44"
# A route interruption pending at minute 80, until 90, that holds the delivery back.
ROUTE = event_of(
    "supply_route_interruption",
    {"outage_id": "r1", "route_id": "road-1", "shipment_ids": [SHIPMENT], "until_minute": 90},
    event_id="route-1",
)
ROUTE_RECORD = {
    "route_id": "road-1",
    "start_event_id": "route-1",
    "restore_event_id": None,
    "until_minute": 90,
    "shipment_ids": [SHIPMENT],
    "active": False,
}


def route(record: dict[str, Any] | None = None, **payload: Any) -> Edit:
    """The route interruption `r1` scheduled (its payload changed by `payload`) and its record
    (changed by `record`)."""
    made = {**ROUTE, "payload": {**ROUTE["payload"], **payload}}
    return both(
        add(made),
        state(INFRA, "route_interruptions", "r1", value={**ROUTE_RECORD, **(record or {})}),
    )


def rename_outage(document: dict[str, Any]) -> None:
    outages = document["model_state"][INFRA]["outages"]
    outages["-bad"] = outages.pop(POWER)


def config_change(**values: Any) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        config = document["model_configs"]["infrastructure"]
        for key, value in values.items():
            if value is DROP:
                del config[key]
            else:
                config[key] = value

    return edit


def facility(facility_id: str, **values: Any) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        next(item for item in document["facilities"] if item["id"] == facility_id).update(values)

    return edit


INFRASTRUCTURE_CASES = [
    # ---- The config and the state's shape.
    Case("infra_config_missing", BASE, edit=config("infrastructure", DROP)),
    Case("infra_config_rate_too_high", BASE, edit=config_change(heat_stress_per_minute=0.5)),
    Case("infra_config_extra_field", BASE, edit=config_change(extra=1)),
    Case("infra_config_not_complete", BASE, edit=config_change(policy_version=DROP)),
    Case(
        "infra_config_edges_unsorted",
        BASE,
        edit=lambda document: document["model_configs"]["infrastructure"][
            "dependency_edges"
        ].reverse(),
    ),
    Case("infra_state_missing", BASE, edit=state(INFRA, value=DROP)),
    Case("infra_state_version_2", BASE, edit=state(INFRA, "schema_version", value=2)),
    Case("infra_outages_a_list", BASE, edit=state(INFRA, "outages", value=[])),
    Case(
        "infra_base_unknown_facility",
        BASE,
        edit=state(INFRA, "base_working_units", "nowhere", value=1),
    ),
    Case(
        "infra_base_above_capacity",
        BASE,
        edit=state(INFRA, "base_working_units", "toilet_01", value=2),
    ),
    Case(
        "infra_base_bool",
        BASE,
        edit=state(INFRA, "base_working_units", "intake_desk", value=True),
    ),
    # ---- The event in flight: the heat's end applies now.
    Case("infra_end_in_flight", BASE, in_flight=pending_by_id(HEAT_END)),
    # ---- Outages.
    Case("infra_outage_record_missing", BASE, edit=state(INFRA, "outages", POWER, value=DROP)),
    Case("infra_outage_bad_id", BASE, edit=rename_outage),
    Case(
        "infra_outage_other_type",
        BASE,
        edit=state(INFRA, "outages", POWER, "event_type", value="equipment_failure"),
    ),
    Case(
        "infra_outage_other_target",
        BASE,
        edit=state(INFRA, "outages", POWER, "target_id", value="grid-2"),
    ),
    Case(
        "infra_outage_closure",
        BASE,
        edit=state(INFRA, "outages", POWER, "affected_facility_ids", value=[]),
    ),
    Case("infra_outage_units", BASE, edit=state(INFRA, "outages", EQUIPMENT, "units_out", value=2)),
    Case(
        "infra_outage_other_restore",
        BASE,
        edit=state(INFRA, "outages", EQUIPMENT, "restore_event_id", value=HEAT_END),
    ),
    Case(
        "infra_outage_shared_restore",
        BASE,
        edit=state(INFRA, "outages", POWER, "restore_event_id", value=EQUIPMENT_BACK),
    ),
    Case(
        "infra_outage_restore_list",
        BASE,
        edit=state(INFRA, "outages", POWER, "restore_event_id", value=["x"]),
    ),
    Case(
        "infra_outage_without_restore",
        BASE,
        edit=state(INFRA, "outages", POWER, "restore_event_id", value=None),
    ),
    Case("infra_outage_inactive", BASE, edit=state(INFRA, "outages", POWER, "active", value=False)),
    # ---- Environmental stressors.
    Case(
        "infra_stressor_record_missing",
        BASE,
        edit=state(INFRA, "environmental_stressors", HEAT, value=DROP),
    ),
    Case(
        "infra_stressor_other_source",
        BASE,
        edit=state(INFRA, "environmental_stressors", HEAT, "source_id", value="OTHER"),
    ),
    Case(
        "infra_stressor_rate",
        BASE,
        edit=state(INFRA, "environmental_stressors", HEAT, "stress_per_minute", value=0.002),
    ),
    Case(
        "infra_stressor_shared_end",
        BASE,
        edit=state(INFRA, "environmental_stressors", HEAT, "end_event_id", value=POWER_BACK),
    ),
    Case(
        "infra_stressor_unknown_end",
        BASE,
        edit=state(INFRA, "environmental_stressors", HEAT, "end_event_id", value="nothing"),
    ),
    Case(
        "infra_stressor_without_end",
        BASE,
        edit=state(INFRA, "environmental_stressors", HEAT, "end_event_id", value=None),
    ),
    Case(
        "infra_stressor_inactive",
        BASE,
        edit=state(INFRA, "environmental_stressors", HEAT, "active", value=False),
    ),
    # ---- Route interruptions.
    Case("infra_route_accepted", BASE, edit=route()),
    Case(
        "infra_route_unknown_shipment",
        BASE,
        edit=route({"shipment_ids": ["other"]}, shipment_ids=["other"]),
    ),
    Case("infra_route_other_until", BASE, edit=route({"until_minute": 95})),
    Case("infra_route_unknown_restore", BASE, edit=route({"restore_event_id": "nothing"})),
    Case("infra_route_active_early", BASE, edit=route({"active": True})),
    Case("infra_route_without_record", BASE, edit=add(ROUTE)),
    # ---- The facilities under the outages, and the completion events.
    Case("infra_capacity_disagrees", BASE, edit=facility("toilet_01", working_units=1)),
    Case(
        "infra_failures_exceed_units",
        BASE,
        edit=state(INFRA, "base_working_units", "water_station_main", value=0),
    ),
    Case(
        "infra_base_lacks_affected_facility",
        BASE,
        edit=state(INFRA, "base_working_units", "toilet_01", value=DROP),
    ),
    Case(
        "infra_completion_unrecorded",
        BASE,
        edit=add(
            event_of(
                "power_restored",
                {"outage_id": POWER},
                event_id="power-back-2",
                kind="completion",
                causation_id="region-condition-001-power_outage",
            )
        ),
    ),
]

CASES = OPERATIONAL_CASES + INFRASTRUCTURE_CASES


def validate_operations_fixture() -> dict[str, Any]:
    """`validate_operations.json`: the cases of the operational events and the infrastructure."""
    return cases_fixture(CASES, BASES)
