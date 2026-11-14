"""Broken resource ledgers and Python's errors for the core's whole-world validation (#250, #351).

The resource ledger block of `WorldState.validate` (core/world.py): the identity history of
`population_dynamics_v1`, then `validate_resource_ledger` (with the history: the whole audit
replayed) or `validate_resource_ledger_state` (without: the balances and the newest records) of
domain/resources.py, wrapped as "invalid resource ledger: {exc}". `native/core/tests/
validate_resource_ledger.rs` compares the core's answers with these (`support/core_validate.py`).

The generated world's ledger has only a reserve and a consume, so many cases first add valid
operations as the rules of domain/resources.py would write them: one delivery (`_delivery`), one
reservation left active (`_active`), or every kind (`_extended`: a delivery, a loss, a reservation
released, one consumed in two parts). Each case then breaks one rule.

Not here: a ledger, or its accounts, that is not a dict or lacks an account. The entity rules
(`_validate_entity_rules`) read the accounts first and raise their TypeError or KeyError, and the
population_dynamics_v1 identity checks other than the unknown profile ID are reached by
`validate_population_dynamics_state` first; those are the other areas' cases.
"""

from __future__ import annotations

import copy
from collections.abc import Callable
from typing import Any

from support.core_validate import Case, cases_fixture

LEDGER = "resource_ledger_v1"
SERVICE_SOURCE = "ASM-REGION-RESOURCE-SERVICE-V1"
BALANCES = ("initial", "delivered", "served", "losses", "remaining", "reserved")
# The minute of the generated world: records of this minute are the last tick's.
MINUTE = 15

Edit = Callable[[dict[str, Any]], None]


def _ledger(document: dict[str, Any]) -> dict[str, Any]:
    return document["model_state"][LEDGER]


def _operate(
    document: dict[str, Any],
    operation: str,
    resource_id: str,
    quantity: int,
    event_id: str,
    *,
    reservation_id: str | None = None,
    agent_id: str | None = None,
    minute: int = MINUTE,
) -> dict[str, Any]:
    """Apply one valid operation as domain/resources.py does, and append its audit record."""
    ledger = _ledger(document)
    account = ledger["accounts"][resource_id]
    if operation == "delivery":
        account["delivered"] += quantity
        account["remaining"] += quantity
    elif operation == "loss":
        account["losses"] += quantity
        account["remaining"] -= quantity
    elif operation == "reserve":
        assert reservation_id is not None and agent_id is not None
        ledger["used_reservation_ids"].append(reservation_id)
        account["reservations"][reservation_id] = {
            "reservation_id": reservation_id,
            "resource_id": resource_id,
            "agent_id": agent_id,
            "quantity": quantity,
        }
        account["reserved"] += quantity
    else:
        reservation = account["reservations"][reservation_id]
        account["reserved"] -= quantity
        if operation == "consume":
            account["served"] += quantity
            account["remaining"] -= quantity
        if quantity == reservation["quantity"]:
            del account["reservations"][reservation_id]
        else:
            reservation["quantity"] -= quantity
    if SERVICE_SOURCE not in account["source_ids"]:
        account["source_ids"].append(SERVICE_SOURCE)
    record = {
        "schema_version": 1,
        "minute": minute,
        "event_id": event_id,
        "operation": operation,
        "resource_id": resource_id,
        "quantity": quantity,
        "reservation_id": reservation_id,
        "agent_id": agent_id,
        "source_id": SERVICE_SOURCE,
        "source_ids": list(account["source_ids"]),
        "warehouse_id": ledger["warehouse_id"],
        **{name: account[name] for name in BALANCES},
    }
    ledger["audit"].append(record)
    document["resources"][resource_id] = account["remaining"]
    return record


def _extended(document: dict[str, Any]) -> None:
    """Valid operations of every kind added to the generated world's ledger, none left active."""
    _operate(document, "delivery", "water_ml", 500, "f-delivery-1")
    _operate(document, "loss", "water_ml", 200, "f-loss-1")
    _operate(
        document,
        "reserve",
        "water_ml",
        300,
        "f-reserve-1",
        reservation_id="f-res-1",
        agent_id="A001",
    )
    _operate(
        document,
        "consume",
        "water_ml",
        100,
        "f-consume-1",
        reservation_id="f-res-1",
        agent_id="A001",
    )
    _operate(
        document,
        "release",
        "water_ml",
        200,
        "f-release-1",
        reservation_id="f-res-1",
        agent_id="A001",
    )
    _operate(
        document,
        "reserve",
        "food_meal_units",
        2,
        "f-reserve-2",
        reservation_id="f-res-2",
        agent_id="A003",
    )
    _operate(
        document,
        "consume",
        "food_meal_units",
        1,
        "f-consume-2",
        reservation_id="f-res-2",
        agent_id="A003",
    )
    _operate(
        document,
        "consume",
        "food_meal_units",
        1,
        "f-consume-3",
        reservation_id="f-res-2",
        agent_id="A003",
    )


def _delivery(document: dict[str, Any]) -> None:
    """One delivery of the last tick added to the generated world's ledger."""
    _operate(document, "delivery", "water_ml", 500, "f-delivery-1")


def _active(document: dict[str, Any]) -> None:
    """A reservation of the last tick left active in the generated world's ledger."""
    _operate(
        document,
        "reserve",
        "water_ml",
        50,
        "f-reserve-3",
        reservation_id="f-res-3",
        agent_id="A004",
    )


def _then(*edits: Edit) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        for each in edits:
            each(document)

    return edit


def _at(*path: Any, value: Any) -> Edit:
    """Set the value at a path inside the ledger (copied, so a case never shares it)."""

    def edit(document: dict[str, Any]) -> None:
        node: Any = _ledger(document)
        for key in path[:-1]:
            node = node[key]
        node[path[-1]] = copy.deepcopy(value)

    return edit


def _drop(*path: Any) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        node: Any = _ledger(document)
        for key in path[:-1]:
            node = node[key]
        del node[path[-1]]

    return edit


def _shift(*path: Any, by: int) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        node: Any = _ledger(document)
        for key in path[:-1]:
            node = node[key]
        node[path[-1]] += by

    return edit


def _resources(**values: Any) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        document["resources"].update(values)

    return edit


def _population(*path: Any, value: Any) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        node: Any = document["model_state"]["population_dynamics_v1"]
        for key in path[:-1]:
            node = node[key]
        node[path[-1]] = copy.deepcopy(value)

    return edit


def _audit_edit(index: int, **values: Any) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        _ledger(document)["audit"][index].update(copy.deepcopy(values))

    return edit


def _case(name: str, *edits: Edit) -> Case:
    return Case(name, "generated", edit=_then(*edits))


def _extended_case(name: str, *edits: Edit) -> Case:
    return Case(name, "generated", edit=_then(_extended, *edits))


def _delivery_case(name: str, *edits: Edit) -> Case:
    return Case(name, "generated", edit=_then(_delivery, *edits))


def _active_case(name: str, *edits: Edit) -> Case:
    return Case(name, "generated", edit=_then(_active, *edits))


# The audit index of each record `_extended` adds: 4 delivery, 5 loss, 6 reserve, 7 consume,
# 8 release, 9 food reserve, 10 and 11 food consumes. `_delivery` adds only the delivery (4) and
# `_active` only a reservation (4): a case file holds each case's changes, so most cases add one.
DELIVERY, LOSS, RESERVE, CONSUME, RELEASE, FOOD_RESERVE = range(4, 10)
ACTIVE = 4

CASES = [
    # Accepted: every kind of operation, and a reservation left active.
    _extended_case("ledger_extended_valid"),
    _active_case("ledger_active_valid"),
    _case("ledger_zone_id_known_valid", _at("zone_id", value="food")),
    # The header and the balances (`_validate_ledger_balances`).
    _case("ledger_extra_field", _at("extra", value=1)),
    _case("ledger_missing_audit", _drop("audit")),
    _case("ledger_schema_version_2", _at("schema_version", value=2)),
    _case("ledger_schema_version_true", _at("schema_version", value=True)),
    _case("ledger_schema_version_float", _at("schema_version", value=1.0)),
    _case("ledger_warehouse_invalid", _at("warehouse_id", value="bad id")),
    _case("ledger_warehouse_none", _at("warehouse_id", value=None)),
    _case("ledger_zone_unknown", _at("zone_id", value="nowhere")),
    _case("ledger_zone_invalid", _at("zone_id", value=7)),
    _case("ledger_used_ids_not_list", _at("used_reservation_ids", value="resource-service-task-0")),
    _case(
        "ledger_used_id_invalid",
        _at("used_reservation_ids", value=["resource-service-task-0", "bad id"]),
    ),
    _case(
        "ledger_used_id_not_text", _at("used_reservation_ids", value=["resource-service-task-0", 3])
    ),
    _case(
        "ledger_used_ids_repeat",
        _at("used_reservation_ids", value=["resource-service-task-0", "resource-service-task-0"]),
    ),
    _case("account_extra_field", _at("accounts", "water_ml", "extra", value=0)),
    _case(
        "account_negative_losses",
        _shift("accounts", "water_ml", "losses", by=-1),
    ),
    _case("account_float_initial", _at("accounts", "water_ml", "initial", value=90000.0)),
    _case("account_bool_reserved", _at("accounts", "water_ml", "reserved", value=False)),
    _case("account_initial_big", _at("accounts", "water_ml", "initial", value=10**20)),
    _case("account_unbalanced", _shift("accounts", "water_ml", "delivered", by=5)),
    _case(
        "account_remaining_not_resources",
        _shift("accounts", "water_ml", "delivered", by=5),
        _shift("accounts", "water_ml", "remaining", by=5),
    ),
    _case("account_reservations_list", _at("accounts", "water_ml", "reservations", value=[])),
    _active_case(
        "reservation_key_invalid",
        _at("accounts", "water_ml", "reservations", value={"bad id": {}}),
    ),
    _active_case(
        "reservation_not_in_history",
        _at("used_reservation_ids", value=["resource-service-task-0", "f-res-1", "f-res-2"]),
    ),
    _active_case(
        "reservation_record_fields",
        _drop("accounts", "water_ml", "reservations", "f-res-3", "agent_id"),
    ),
    _active_case(
        "reservation_wrong_resource",
        _at(
            "accounts",
            "water_ml",
            "reservations",
            "f-res-3",
            "resource_id",
            value="food_meal_units",
        ),
    ),
    _active_case(
        "reservation_unknown_agent",
        _at("accounts", "water_ml", "reservations", "f-res-3", "agent_id", value="A999"),
    ),
    _active_case(
        "reservation_quantity_zero",
        _at("accounts", "water_ml", "reservations", "f-res-3", "quantity", value=0),
    ),
    _active_case(
        "reservation_quantity_float",
        _at("accounts", "water_ml", "reservations", "f-res-3", "quantity", value=50.0),
    ),
    _active_case(
        "reservation_total_mismatch",
        _at("accounts", "water_ml", "reservations", "f-res-3", "quantity", value=49),
    ),
    _case(
        "account_reserves_more_than_remains",
        _at("accounts", "food_meal_units", "reserved", value=90),
        _at(
            "accounts",
            "food_meal_units",
            "reservations",
            value={
                "resource-service-task-0": {
                    "reservation_id": "resource-service-task-0",
                    "resource_id": "food_meal_units",
                    "agent_id": "A002",
                    "quantity": 90,
                }
            },
        ),
    ),
    _case(
        "account_source_ids_invalid", _at("accounts", "water_ml", "source_ids", value=["bad id"])
    ),
    _case("account_source_ids_not_list", _at("accounts", "water_ml", "source_ids", value="x")),
    _case(
        "account_source_ids_repeat",
        _at(
            "accounts",
            "water_ml",
            "source_ids",
            value=["ASM-STANDARD-PROVISIONING-V1", "ASM-STANDARD-PROVISIONING-V1"],
        ),
    ),
    _case("resources_missing_food", lambda document: document["resources"].pop("food_meal_units")),
    _case("resources_extra", _resources(extra=1)),
    # The audit trail's records (`_validate_audit_record`).
    _case("audit_not_list", _at("audit", value={})),
    _case(
        "audit_too_short", lambda document: _ledger(document)["audit"].__delitem__(slice(1, None))
    ),
    _delivery_case("audit_record_not_dict", _at("audit", DELIVERY, value="loss")),
    _delivery_case("audit_record_extra_field", _audit_edit(DELIVERY, extra=None)),
    _delivery_case("audit_record_schema_2", _audit_edit(DELIVERY, schema_version=2)),
    _case("audit_record_minute_negative", _audit_edit(0, minute=-1)),
    _delivery_case("audit_record_minute_float", _audit_edit(DELIVERY, minute=15.0)),
    _delivery_case("audit_record_resource_unknown", _audit_edit(DELIVERY, resource_id="soap")),
    _delivery_case("audit_record_operation_unknown", _audit_edit(DELIVERY, operation="steal")),
    _delivery_case("audit_record_operation_list", _audit_edit(DELIVERY, operation=["loss"])),
    _delivery_case("audit_record_operation_dict", _audit_edit(DELIVERY, operation={"loss": 1})),
    _delivery_case("audit_record_quantity_negative", _audit_edit(DELIVERY, quantity=-500)),
    _delivery_case("audit_record_quantity_bool", _audit_edit(DELIVERY, quantity=True)),
    _delivery_case("audit_record_event_id_invalid", _audit_edit(DELIVERY, event_id="bad id")),
    _delivery_case("audit_record_agent_id_number", _audit_edit(DELIVERY, agent_id=1)),
    _delivery_case("audit_record_source_ids_invalid", _audit_edit(DELIVERY, source_ids=["bad id"])),
    _delivery_case(
        "audit_record_source_ids_repeat",
        _audit_edit(
            DELIVERY,
            source_ids=["ASM-STANDARD-PROVISIONING-V1", "ASM-STANDARD-PROVISIONING-V1"],
        ),
    ),
    _delivery_case("audit_record_warehouse_other", _audit_edit(DELIVERY, warehouse_id="other")),
    _delivery_case("audit_record_balance_negative", _audit_edit(DELIVERY, served=-1)),
    _delivery_case("audit_record_balance_float", _audit_edit(DELIVERY, remaining=90500.0)),
    _delivery_case("audit_minutes_decrease", _audit_edit(DELIVERY, minute=2)),
    _case("audit_old_minutes_decrease", _audit_edit(3, minute=2)),
    # The replay (`validate_resource_ledger`).
    _case(
        "audit_initial_order",
        lambda document: _ledger(document)["audit"].insert(0, _ledger(document)["audit"].pop(1)),
    ),
    _case("audit_initial_event", _audit_edit(0, event_id="e-1")),
    _case("audit_initial_minute", _audit_edit(0, minute=1)),
    _case("audit_initial_quantity", _audit_edit(0, quantity=1)),
    _case(
        "audit_initial_sources_unsorted",
        _audit_edit(0, source_ids=["Z-SOURCE", "A-SOURCE"]),
    ),
    _case(
        "audit_event_before_initial",
        lambda document: _ledger(document)["audit"].insert(1, _ledger(document)["audit"].pop(2)),
    ),
    _delivery_case("audit_event_quantity_zero", _audit_edit(DELIVERY, quantity=0)),
    _delivery_case("audit_event_without_event_id", _audit_edit(DELIVERY, event_id=None)),
    _delivery_case("audit_event_without_source", _audit_edit(DELIVERY, source_id=None)),
    _delivery_case("audit_event_id_repeat", _audit_edit(DELIVERY, event_id="resource-reserve-43")),
    _active_case("audit_reserve_without_agent", _audit_edit(ACTIVE, agent_id=None)),
    _delivery_case("audit_loss_with_reservation", _audit_edit(DELIVERY, reservation_id="f-res-9")),
    _delivery_case("audit_source_not_listed", _audit_edit(DELIVERY, source_id="OTHER-SOURCE")),
    _extended_case("audit_loss_exceeds", _audit_edit(LOSS, quantity=10**6)),
    _extended_case("audit_reserve_reused", _audit_edit(FOOD_RESERVE, reservation_id="f-res-1")),
    _active_case(
        "audit_reserve_reused_old",
        _audit_edit(ACTIVE, reservation_id="resource-service-task-0"),
    ),
    _active_case("audit_reserve_exceeds", _audit_edit(ACTIVE, quantity=10**6)),
    _active_case("audit_reserve_unknown_agent", _audit_edit(ACTIVE, agent_id="A999")),
    _extended_case("audit_consume_wrong_agent", _audit_edit(CONSUME, agent_id="A002")),
    _extended_case(
        "audit_consume_unknown_reservation", _audit_edit(CONSUME, reservation_id="f-res-8")
    ),
    _extended_case("audit_release_quantity", _audit_edit(RELEASE, quantity=199)),
    _extended_case("audit_consume_exceeds", _audit_edit(CONSUME, quantity=301)),
    _delivery_case(
        "audit_sources_not_replayed",
        _audit_edit(DELIVERY, source_ids=["ASM-REGION-RESOURCE-SERVICE-V1"]),
    ),
    _delivery_case("audit_after_state", _audit_edit(DELIVERY, served=3)),
    _case("audit_initial_after_state", _audit_edit(1, reserved=1)),
    # The replay against the ledger.
    _extended_case(
        "ledger_used_ids_not_replayed",
        _at("used_reservation_ids", value=["resource-service-task-0", "f-res-2", "f-res-1"]),
    ),
    _case(
        "account_balance_not_replayed",
        _shift("accounts", "water_ml", "initial", by=5),
        _shift("accounts", "water_ml", "remaining", by=5),
        lambda document: document["resources"].__setitem__(
            "water_ml", _ledger(document)["accounts"]["water_ml"]["remaining"]
        ),
    ),
    _case(
        "account_sources_not_replayed",
        _at(
            "accounts",
            "food_meal_units",
            "source_ids",
            value=["ASM-STANDARD-PROVISIONING-V1", "ASM-REGION-RESOURCE-SERVICE-V1", "EXTRA"],
        ),
    ),
    _active_case(
        "account_reservations_not_replayed",
        _at("accounts", "water_ml", "reservations", "f-res-3", "agent_id", value="A005"),
    ),
    _active_case("audit_active_reserve_agent", _audit_edit(ACTIVE, agent_id="A005")),
    # The identity history of population_dynamics_v1.
    _case(
        "population_unknown_profile",
        lambda document: document["model_state"]["population_dynamics_v1"][
            "profile_traits"
        ].__setitem__(
            "A999",
            copy.deepcopy(
                document["model_state"]["population_dynamics_v1"]["profile_traits"]["A001"]
            ),
        ),
    ),
    # schema_version 1.0 equals 1 for the identity check (and for the extension's own).
    _case("population_version_float_valid", _population("schema_version", value=1.0)),
]


def validate_resource_ledger_fixture() -> dict[str, Any]:
    """`validate_resource_ledger.json`: the cases of the resource ledger block (#351)."""
    return cases_fixture(CASES)
