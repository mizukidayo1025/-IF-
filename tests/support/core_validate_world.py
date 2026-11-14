"""Broken worlds for the rules of `WorldState.validate` that #347 moves into the core (#250).

Each builder returns a case file of `support.core_validate` (`cases_fixture`):

- `validate_selection.json`: `selected_models(model_configs)` and the declared state contracts
  (`validate_declared_states`, the presence ledger);
- `validate_service.json`: the resource service's config, WorldState v6 and `resource_service_v1`
  with its histories;
- `validate_information.json`: the information_spread config and runtime state, and the
  psychological distress state;
- `validate_events.json`: the event rules (the ID counters, the log's audit, the queue against the
  log and the clock).

`native/core/tests/validate_world.rs` checks each file.
"""

from __future__ import annotations

import copy
from collections.abc import Callable
from typing import Any

from support.core_validate import Case, cases_fixture

Edit = Callable[[dict[str, Any]], None]


def _node(document: dict[str, Any], path: tuple[Any, ...]) -> Any:
    node = document
    for key in path:
        node = node[key]
    return node


def put(*path: Any, value: Any) -> Edit:
    """Set the value at a path of the world's document."""

    def edit(document: dict[str, Any]) -> None:
        _node(document, path[:-1])[path[-1]] = copy.deepcopy(value)

    return edit


def drop(*path: Any) -> Edit:
    """Remove a key (or an index) at a path of the world's document."""

    def edit(document: dict[str, Any]) -> None:
        del _node(document, path[:-1])[path[-1]]

    return edit


def both(*edits: Edit) -> Edit:
    """Several edits, in order."""

    def edit(document: dict[str, Any]) -> None:
        for each in edits:
            each(document)

    return edit


def apply(function: Callable[[dict[str, Any]], None]) -> Edit:
    """An edit written as a function of the document."""
    return function


def config(*path: Any, value: Any) -> Edit:
    return put("model_configs", *path, value=value)


def state(*path: Any, value: Any) -> Edit:
    return put("model_state", *path, value=value)


G = "generated"
S = "small_replay"


# ---- selected_models and the declared states -------------------------------------------------


def _first_agent(document: dict[str, Any]) -> str:
    return document["agents"][0]["profile"]["id"]


def _presence(edit: Callable[[dict[str, Any], dict[str, Any]], None]) -> Edit:
    def run(document: dict[str, Any]) -> None:
        edit(document["model_state"]["population_presence_v1"], document)

    return run


def _first_person(presence: dict[str, Any]) -> dict[str, Any]:
    return next(iter(presence["people"].values()))


def _without_suite(document: dict[str, Any], *, schema: int = 3, **models: str) -> None:
    """The generated registry without its suite (the suite's models as overrides), with these."""
    registry = document["model_configs"]["policy_registry"]
    registry.pop("suite_id")
    registry["schema_version"] = schema
    registry.update(models)


def _proximity(enabled: bool) -> dict[str, Any]:
    return {
        "schema_version": 1,
        "enabled": enabled,
        "policy_version": "distance-sampled-proximity-v1",
        "sample_interval_minutes": 5,
        "max_distance_m": 2.0,
        "max_pairs_per_sample": 100,
    }


def _receipt(document: dict[str, Any], **changes: Any) -> dict[str, Any]:
    agent = _first_agent(document)
    receipt = {
        "schema_version": 2,
        "transition_id": "fixture-transition",
        "event_id": "fixture-event",
        "agent_id": agent,
        "minute": 1,
        "action": "relocated",
        "from_zone_id": None,
        "to_zone_id": None,
        "from_household_id": None,
        "to_household_id": None,
        "causation_id": None,
        "source_ids": [],
        "state_snapshot_hash": None,
        "time_semantics": "recorded_event_minute",
    }
    receipt.update(changes)
    return receipt


SELECTION_CASES = [
    Case("configs_value_not_a_dict", G, edit=config("toilet_service", value=[1])),
    Case("behavior_unknown_key", G, edit=config("behavior", "foo", value=1)),
    Case(
        "behavior_interval_zero", G, edit=config("behavior", "decision_interval_minutes", value=0)
    ),
    Case(
        "behavior_interval_float",
        G,
        edit=config("behavior", "decision_interval_minutes", value=15.0),
    ),
    Case(
        "behavior_interval_bool",
        S,
        edit=config("behavior", "decision_interval_minutes", value=True),
    ),
    Case("behavior_temperature_zero", G, edit=config("behavior", "temperature", value=0)),
    Case("behavior_temperature_text", S, edit=config("behavior", "temperature", value="0.2")),
    Case("behavior_temperature_int", S, edit=config("behavior", "temperature", value=1)),
    Case("registry_unknown_key", G, edit=config("policy_registry", "nope", value="x")),
    Case("registry_schema_4", G, edit=config("policy_registry", "schema_version", value=4)),
    Case("registry_schema_text", G, edit=config("policy_registry", "schema_version", value="3")),
    Case("registry_schema_float", G, edit=config("policy_registry", "schema_version", value=3.0)),
    Case("registry_suite_unknown", G, edit=config("policy_registry", "suite_id", value="nope")),
    Case("registry_suite_list", G, edit=config("policy_registry", "suite_id", value=["a"])),
    Case("registry_suite_dict", G, edit=config("policy_registry", "suite_id", value={"a": 1})),
    Case("registry_suite_int", G, edit=config("policy_registry", "suite_id", value=2)),
    Case("registry_suite_schema_2", G, edit=config("policy_registry", "schema_version", value=2)),
    Case(
        "registry_suite_conflict",
        G,
        edit=config("policy_registry", "psychology", value="causal-stress-v1"),
    ),
    Case(
        "registry_suite_behavior_mismatch",
        G,
        edit=config("behavior", "policy", value="utility-softmax-v1"),
    ),
    Case(
        "registry_without_suite_unknown_model",
        G,
        edit=both(
            drop("model_configs", "policy_registry", "suite_id"),
            config("policy_registry", "affect", value="nope"),
        ),
    ),
    Case(
        "registry_without_suite_model_of_other_domain",
        G,
        edit=both(
            drop("model_configs", "policy_registry", "suite_id"),
            config("policy_registry", "affect", value="causal-stress-v1"),
        ),
    ),
    Case(
        "registry_without_suite_model_not_a_str",
        G,
        edit=both(
            drop("model_configs", "policy_registry", "suite_id"),
            config("policy_registry", "queue", value=[1, "a"]),
        ),
    ),
    Case("behavior_policy_unknown_small", S, edit=config("behavior", "policy", value="nope-v1")),
    Case("behavior_policy_list_small", S, edit=config("behavior", "policy", value=["x"])),
    Case(
        "behavior_selector_disagrees",
        S,
        edit=both(
            config("policy_registry", value={"schema_version": 1, "behavior": "priority-rule-v1"}),
        ),
    ),
    Case(
        "sleep_weight_unsupported_small",
        S,
        edit=config("behavior", "sleep_debt_utility_weight", value=1.0),
    ),
    Case("sleep_weight_text", G, edit=config("behavior", "sleep_debt_utility_weight", value="1")),
    Case("sleep_weight_bool", G, edit=config("behavior", "sleep_debt_utility_weight", value=True)),
    Case("sleep_weight_above", G, edit=config("behavior", "sleep_debt_utility_weight", value=5.5)),
    Case("sleep_weight_below", G, edit=config("behavior", "sleep_debt_utility_weight", value=-1)),
    Case(
        "sleep_weight_bound_int", G, edit=config("behavior", "sleep_debt_utility_weight", value=5)
    ),
    Case(
        "intervals_not_a_map",
        G,
        edit=config("behavior", "decision_interval_minutes_by_agent", value=[1]),
    ),
    Case(
        "intervals_too_many",
        G,
        edit=config(
            "behavior",
            "decision_interval_minutes_by_agent",
            value={f"A{index}": 15 for index in range(101)},
        ),
    ),
    Case(
        "intervals_empty_id",
        G,
        edit=config("behavior", "decision_interval_minutes_by_agent", value={"": 15}),
    ),
    Case(
        "intervals_long_id",
        G,
        edit=config("behavior", "decision_interval_minutes_by_agent", value={"x" * 65: 15}),
    ),
    Case(
        "intervals_text_value",
        G,
        edit=config("behavior", "decision_interval_minutes_by_agent", value={"A001": "15"}),
    ),
    Case(
        "intervals_float_value",
        G,
        edit=config("behavior", "decision_interval_minutes_by_agent", value={"A001": 15.0}),
    ),
    Case(
        "intervals_zero",
        G,
        edit=config("behavior", "decision_interval_minutes_by_agent", value={"A001": 0}),
    ),
    Case(
        "intervals_above",
        G,
        edit=config("behavior", "decision_interval_minutes_by_agent", value={"A001": 1441}),
    ),
    Case(
        "intervals_fit",
        G,
        edit=config("behavior", "decision_interval_minutes_by_agent", value={"A001": 1440}),
    ),
    Case(
        "temperatures_at_lower",
        G,
        edit=config("behavior", "temperature_by_agent", value={"A001": 0}),
    ),
    Case(
        "temperatures_above",
        G,
        edit=config("behavior", "temperature_by_agent", value={"A001": 10.5}),
    ),
    Case(
        "temperatures_fit", G, edit=config("behavior", "temperature_by_agent", value={"A001": 0.5})
    ),
    Case(
        "temperatures_unsupported",
        S,
        edit=both(
            config("behavior", "policy", value="priority-rule-v1"),
            config("behavior", "temperature_by_agent", value={"A001": 0.5}),
        ),
    ),
    Case(
        "affect_without_life_state",
        G,
        edit=apply(
            lambda document: _without_suite(document, psychology="mean-reverting-distress-v1")
        ),
    ),
    Case("body_dynamics_config_missing", G, edit=drop("model_configs", "body_dynamics")),
    Case("staff_operations_config_missing", G, edit=drop("model_configs", "staff_operations")),
    Case("staff_dynamics_config_missing", G, edit=drop("model_configs", "staff_dynamics")),
    Case("zone_environment_config_missing", G, edit=drop("model_configs", "zone_environment")),
    Case(
        "zone_environment_config_invalid",
        G,
        edit=config("zone_environment", "policy_version", value="nope"),
    ),
    Case("zone_environment_config_extra", G, edit=config("zone_environment", "extra", value=1)),
    Case("zone_environment_config_empty", G, edit=config("zone_environment", value={})),
    Case(
        "zone_environment_config_list_value",
        G,
        edit=config("zone_environment", "policy_version", value=["zone-environment-v2"]),
    ),
    Case("zone_environment_config_list", G, edit=config("zone_environment", value=[1])),
    Case(
        "formula_needs_schema_2", G, edit=apply(lambda document: _without_suite(document, schema=1))
    ),
    Case(
        "proximity_enabled_but_disabled_model",
        G,
        edit=config("social_proximity", value=_proximity(True)),
    ),
    Case(
        "proximity_disabled_config_fits",
        G,
        edit=config("social_proximity", value=_proximity(False)),
    ),
    Case("proximity_config_fields", G, edit=config("social_proximity", value={"enabled": True})),
    Case(
        "proximity_config_version",
        G,
        edit=config("social_proximity", value={**_proximity(True), "schema_version": 2}),
    ),
    Case(
        "proximity_config_bounds",
        G,
        edit=config("social_proximity", value={**_proximity(True), "max_distance_m": 11}),
    ),
    Case(
        "proximity_sampled_without_config",
        G,
        edit=apply(
            lambda document: _without_suite(
                document, social_proximity="distance-sampled-proximity-v1"
            )
        ),
    ),
    Case("state_contracts_fields", G, edit=config("state_contracts", "extra", value=1)),
    Case("state_contracts_schema", G, edit=config("state_contracts", "schema_version", value=2)),
    Case(
        "state_contracts_namespaces_list", G, edit=config("state_contracts", "namespaces", value=[])
    ),
    Case(
        "state_contracts_unknown_namespace",
        G,
        edit=config("state_contracts", "namespaces", "nope_v1", value={}),
    ),
    Case(
        "state_contracts_signature_mismatch",
        G,
        edit=config(
            "state_contracts",
            "namespaces",
            "population_presence_v1",
            "implementation_version",
            value="2",
        ),
    ),
    Case("presence_missing", G, edit=drop("model_state", "population_presence_v1")),
    Case("presence_schema", G, edit=state("population_presence_v1", "schema_version", value=2)),
    Case("presence_extra_field", G, edit=state("population_presence_v1", "extra", value=1)),
    Case(
        "presence_bad_status",
        G,
        edit=_presence(lambda presence, _: _first_person(presence).update(status="gone")),
    ),
    Case(
        "presence_future_since",
        G,
        edit=_presence(lambda presence, _: _first_person(presence).update(since_minute=10_000)),
    ),
    Case(
        "presence_admitted_without_zone",
        G,
        edit=_presence(lambda presence, _: _first_person(presence).update(transition_zone_id=None)),
    ),
    Case(
        "presence_unknown_zone",
        G,
        edit=_presence(
            lambda presence, _: _first_person(presence).update(transition_zone_id="nowhere")
        ),
    ),
    Case(
        "presence_departed_but_here",
        G,
        edit=_presence(lambda presence, _: _first_person(presence).update(status="departed")),
    ),
    Case(
        "presence_omits_a_person",
        G,
        edit=_presence(lambda presence, _: presence["people"].pop(next(iter(presence["people"])))),
    ),
    Case(
        "presence_receipt_hash",
        G,
        edit=_presence(
            lambda presence, document: presence["history"].append(
                _receipt(document, state_snapshot_hash="XYZ")
            )
        ),
    ),
    Case(
        "presence_receipt_fits",
        G,
        edit=_presence(
            lambda presence, document: presence["history"].append(
                _receipt(document, state_snapshot_hash="a" * 64)
            )
        ),
    ),
    Case(
        "presence_receipt_future",
        G,
        edit=_presence(
            lambda presence, document: presence["history"].append(_receipt(document, minute=10_000))
        ),
    ),
    Case(
        "presence_archive_path_missing",
        G,
        edit=_presence(
            lambda presence, _: _first_person(presence).update(archive_path=["nope_v1"])
        ),
    ),
    Case(
        "presence_archive_path_fits",
        G,
        edit=_presence(
            lambda presence, _: _first_person(presence).update(
                archive_path=["population_dynamics_v1", "schema_version"]
            )
        ),
    ),
]


def validate_selection_fixture() -> dict[str, Any]:
    """`validate_selection.json`."""
    return cases_fixture(SELECTION_CASES)


# ---- The resource service ------------------------------------------------------------------------


def _service(*path: Any, value: Any) -> Edit:
    return state("resource_service_v1", *path, value=value)


def _request(edit: Callable[[dict[str, Any]], None]) -> Edit:
    def run(document: dict[str, Any]) -> None:
        edit(document["model_state"]["resource_service_v1"]["request_history"][-1])

    return run


def _stall_record(document: dict[str, Any], **changes: Any) -> dict[str, Any]:
    record = {
        "minute": 1,
        "facility_id": "water_station_main",
        "agent_id": _first_agent(document),
        "reason": "capacity",
    }
    record.update(changes)
    return record


def _add_stall(**changes: Any) -> Edit:
    def run(document: dict[str, Any]) -> None:
        stalls = document["model_state"]["resource_service_v1"]["queue_stall_history"]
        stalls.append(_stall_record(document, **changes))

    return run


def _stall_state(facility: str = "water_station_main", **changes: Any) -> Edit:
    def run(document: dict[str, Any]) -> None:
        record = {"agent_id": _first_agent(document), "reason": "capacity"}
        record.update(changes)
        document["model_state"]["resource_service_v1"]["stall_state"][facility] = record

    return run


def _old_request(document: dict[str, Any]) -> None:
    """A bad record well before the last tick: only the full history reads it."""
    history = document["model_state"]["resource_service_v1"]["request_history"]
    record = copy.deepcopy(history[0])
    record["minute"] = 0
    record["reason"] = "nope"
    history.insert(0, record)


SERVICE_CASES = [
    Case("service_config_on_v5", S, edit=config("resource_service", value={})),
    Case("service_config_missing_on_v6", G, edit=drop("model_configs", "resource_service")),
    Case("service_state_without_v6", S, edit=state("resource_service_v1", value={})),
    Case("service_config_schema", G, edit=config("resource_service", "schema_version", value=2)),
    Case("service_config_extra", G, edit=config("resource_service", "extra", value=1)),
    Case(
        "service_config_minutes_zero",
        G,
        edit=config("resource_service", "service_minutes", value=0),
    ),
    Case(
        "service_config_minutes_text",
        G,
        edit=config("resource_service", "service_minutes", value="12"),
    ),
    Case(
        "service_config_minutes_float",
        G,
        edit=config("resource_service", "service_minutes", value=12.0),
    ),
    Case(
        "service_config_many_errors",
        G,
        edit=config("resource_service", value={"schema_version": 1, "water_ml_per_service": -1}),
    ),
    Case(
        "service_config_threshold_high",
        G,
        edit=config("resource_service", "hydration_action_threshold", value=1.5),
    ),
    Case(
        "service_config_skill_spaces",
        G,
        edit=config("resource_service", "water_staff_skill", value=" water_service "),
    ),
    Case(
        "service_config_skill_bad",
        G,
        edit=config("resource_service", "water_staff_skill", value="-bad"),
    ),
    Case(
        "service_config_not_canonical",
        G,
        edit=drop("model_configs", "resource_service", "hunger_relief"),
    ),
    Case(
        "service_config_int_for_float", G, edit=config("resource_service", "hunger_relief", value=1)
    ),
    Case("service_without_ledger", G, edit=drop("model_state", "resource_ledger_v1")),
    Case("service_state_missing", G, edit=drop("model_state", "resource_service_v1")),
    Case("service_state_extra", G, edit=_service("extra", value=1)),
    Case("service_state_schema", G, edit=_service("schema_version", value=2)),
    Case("service_state_schema_float", G, edit=_service("schema_version", value=1.0)),
    Case("service_cancelled_empty_id", G, edit=_service("cancelled_task_ids", value=[""])),
    Case("service_cancelled_twice", G, edit=_service("cancelled_task_ids", value=["t", "t"])),
    Case("service_reservations_not_a_list", G, edit=_service("active_reservation_ids", value={})),
    Case("service_requests_not_a_list", G, edit=_service("request_history", value={})),
    Case("service_stall_state_list", G, edit=_service("stall_state", value=[])),
    Case("service_request_extra_field", G, edit=_request(lambda record: record.update(extra=1))),
    Case(
        "service_request_minute_future",
        G,
        edit=_request(lambda record: record.update(minute=10_000)),
    ),
    Case(
        "service_request_minute_bool", G, edit=_request(lambda record: record.update(minute=True))
    ),
    Case(
        "service_request_unknown_agent",
        G,
        edit=_request(lambda record: record.update(agent_id="nobody")),
    ),
    Case(
        "service_request_agent_list",
        G,
        edit=_request(lambda record: record.update(agent_id=["A001"])),
    ),
    Case(
        "service_request_action_dict",
        G,
        edit=_request(lambda record: record.update(action={"a": 1})),
    ),
    Case(
        "service_request_unknown_action",
        G,
        edit=_request(lambda record: record.update(action="eat")),
    ),
    Case(
        "service_request_unknown_zone",
        G,
        edit=_request(lambda record: record.update(origin_zone_id="nowhere")),
    ),
    Case(
        "service_request_unknown_facility",
        G,
        edit=_request(lambda record: record.update(facility_id="nowhere")),
    ),
    Case("service_request_detail_int", G, edit=_request(lambda record: record.update(detail=1))),
    Case(
        "service_request_wrong_facility",
        G,
        edit=_request(lambda record: record.update(facility_id="water_station_main")),
    ),
    Case(
        "service_request_inconsistent",
        G,
        edit=_request(lambda record: record.update(reason="no_route")),
    ),
    Case("service_request_old_bad", G, edit=_old_request),
    Case("service_stall_fits", G, edit=_add_stall()),
    Case("service_stall_unknown_reason", G, edit=_add_stall(reason="tired")),
    Case("service_stall_reason_list", G, edit=_add_stall(reason=["capacity"])),
    Case("service_stall_minute_future", G, edit=_add_stall(minute=10_000)),
    Case("service_stall_unknown_facility", G, edit=_add_stall(facility_id="nowhere")),
    Case(
        "service_stall_out_of_order",
        G,
        edit=both(_add_stall(minute=10), _add_stall(minute=5)),
    ),
    Case("service_stall_state_fits_not_head", G, edit=_stall_state()),
    Case("service_stall_state_unknown_facility", G, edit=_stall_state("nowhere")),
    Case("service_stall_state_not_resource", G, edit=_stall_state("toilet_01")),
    Case("service_stall_state_bad_reason", G, edit=_stall_state(reason="tired")),
    Case("service_stall_state_agent_list", G, edit=_stall_state(agent_id=["A001"])),
    Case("service_stall_state_extra", G, edit=_stall_state(extra=1)),
]


def validate_service_fixture() -> dict[str, Any]:
    """`validate_service.json`."""
    return cases_fixture(SERVICE_CASES)


# ---- The information spread and the psychological distress ---------------------------------------


def _information(edit: Callable[[dict[str, Any], dict[str, Any]], None]) -> Edit:
    def run(document: dict[str, Any]) -> None:
        edit(document["model_state"]["information_spread_v1"], document)

    return run


def _message(state: dict[str, Any], document: dict[str, Any], **changes: Any) -> dict[str, Any]:
    message = {
        "id": "fixture-message",
        "truth_status": "false",
        "source_id": "fixture-source",
        "zone_id": document["zones"][0]["id"],
        "time": 1,
        "origin_event_id": "fixture-event",
        "origin_agent_id": _first_agent(document),
    }
    message.update(changes)
    state["messages"][message["id"] if isinstance(message["id"], str) else "fixture-message"] = (
        message
    )
    return message


def _belief(state: dict[str, Any], document: dict[str, Any], **changes: Any) -> None:
    _message(state, document)
    belief = {
        "message_id": "fixture-message",
        "belief": "heard",
        "source": "fixture-source",
        "last_contact": 1,
        "last_event_id": "fixture-event",
        "exposed": True,
        "belief_strength": 0.6,
        "share_propensity": 0.5,
        "correction_exposed": False,
        "correction_accepted": False,
        "ever_misbelieved": False,
        "ever_forgotten": False,
        "last_correction_accepted": False,
    }
    belief.update(changes)
    for key in [key for key, value in changes.items() if value is ...]:
        del belief[key]
    state["beliefs"][_first_agent(document)]["fixture-message"] = belief


def _contact(key: str, record: Any = None) -> Edit:
    return _information(
        lambda information, _: information["contacts"].update(
            {
                key: record
                if record is not None
                else {"last_seen_minute": 1, "consecutive_minutes": 1}
            }
        )
    )


def _pair(document: dict[str, Any]) -> str:
    first, second = (agent["profile"]["id"] for agent in document["agents"][:2])
    return f'["{first}","{second}"]'


def _contact_with(record: Any) -> Edit:
    def run(document: dict[str, Any]) -> None:
        document["model_state"]["information_spread_v1"]["contacts"][_pair(document)] = record

    return run


def _support_request(**changes: Any) -> Edit:
    def run(document: dict[str, Any]) -> None:
        state = document["model_state"]["information_spread_v1"]
        _message(state, document)
        request = {
            "request_id": "fixture-request",
            "agent_id": _first_agent(document),
            "minute": 1,
            "message_ids": ["fixture-message"],
        }
        request.update(changes)
        state["support_requests"].append(request)
        state["counters"]["support_requests"] += 1

    return run


def _psychology(edit: Callable[[dict[str, Any]], None]) -> Edit:
    def run(document: dict[str, Any]) -> None:
        state = document["model_state"]["psychological_distress_v1"]
        edit(next(iter(state["agents"].values())))

    return run


INFORMATION_CASES = [
    Case("information_config_unknown", G, edit=config("information_spread", "nope", value=1)),
    Case(
        "information_config_enabled_int", G, edit=config("information_spread", "enabled", value=1)
    ),
    Case(
        "information_config_contacts_high",
        G,
        edit=config("information_spread", "max_contacts_per_agent", value=17),
    ),
    Case(
        "information_config_contacts_float",
        G,
        edit=config("information_spread", "max_contacts_per_agent", value=2.0),
    ),
    Case(
        "information_config_probability_high",
        G,
        edit=config("information_spread", "query_utility_weight", value=4.5),
    ),
    Case(
        "information_config_probability_bool",
        G,
        edit=config("information_spread", "trust_sensitivity", value=True),
    ),
    Case(
        "information_config_reliability_bad_id",
        G,
        edit=config("information_spread", "correction_source_reliability_by_id", value={"-x": 0.5}),
    ),
    Case(
        "information_config_reliability_int",
        G,
        edit=config("information_spread", "correction_source_reliability_by_id", value=5),
    ),
    Case(
        "information_config_reliability_null",
        G,
        edit=config("information_spread", "correction_source_reliability_by_id", value=None),
    ),
    Case(
        "information_config_schema", G, edit=config("information_spread", "schema_version", value=2)
    ),
    Case(
        "information_config_source",
        G,
        edit=config("information_spread", "source_id", value="ASM-OTHER"),
    ),
    Case(
        "information_config_partial",
        G,
        edit=drop("model_configs", "information_spread", "query_threshold"),
    ),
    Case(
        "information_config_disabled_with_state",
        G,
        edit=config("information_spread", "enabled", value=False),
    ),
    Case("information_state_on_small", S, edit=state("information_spread_v1", value={})),
    Case("information_state_missing", G, edit=drop("model_state", "information_spread_v1")),
    Case("information_state_extra", G, edit=state("information_spread_v1", "extra", value=1)),
    Case(
        "information_state_schema",
        G,
        edit=state("information_spread_v1", "schema_version", value=3),
    ),
    Case(
        "information_message_fits",
        G,
        edit=_information(lambda information, document: _message(information, document)),
    ),
    Case(
        "information_message_bad_id",
        G,
        edit=_information(
            lambda information, document: _message(information, document, source_id="-x")
        ),
    ),
    Case(
        "information_message_truth",
        G,
        edit=_information(
            lambda information, document: _message(information, document, truth_status="maybe")
        ),
    ),
    Case(
        "information_message_truth_list",
        G,
        edit=_information(
            lambda information, document: _message(information, document, truth_status=["true"])
        ),
    ),
    Case(
        "information_message_time",
        G,
        edit=_information(lambda information, document: _message(information, document, time=-1)),
    ),
    Case(
        "information_message_late",
        G,
        edit=_information(
            lambda information, document: _message(information, document, time=100_000)
        ),
    ),
    Case(
        "information_message_zone",
        G,
        edit=_information(
            lambda information, document: _message(information, document, zone_id="nowhere")
        ),
    ),
    Case(
        "information_message_key",
        G,
        edit=_information(
            lambda information, document: information["messages"].update(
                other=_message({"messages": {}}, document)
            )
        ),
    ),
    Case(
        "information_beliefs_missing_agent",
        G,
        edit=_information(
            lambda information, document: information["beliefs"].pop(_first_agent(document))
        ),
    ),
    Case(
        "information_belief_fits",
        G,
        edit=_information(lambda information, document: _belief(information, document)),
    ),
    Case(
        "information_belief_fields",
        G,
        edit=_information(
            lambda information, document: _belief(information, document, ever_forgotten=...)
        ),
    ),
    Case(
        "information_belief_status",
        G,
        edit=_information(
            lambda information, document: _belief(information, document, belief="known")
        ),
    ),
    Case(
        "information_belief_strength",
        G,
        edit=_information(
            lambda information, document: _belief(information, document, belief_strength=1.5)
        ),
    ),
    Case(
        "information_belief_exposed_int",
        G,
        edit=_information(lambda information, document: _belief(information, document, exposed=1)),
    ),
    Case(
        "information_belief_accepted_unexposed",
        G,
        edit=_information(
            lambda information, document: _belief(information, document, correction_accepted=True)
        ),
    ),
    Case(
        "information_belief_unknown_shares",
        G,
        edit=_information(
            lambda information, document: _belief(information, document, belief="unknown")
        ),
    ),
    Case(
        "information_belief_late",
        G,
        edit=_information(
            lambda information, document: _belief(information, document, last_contact=100_000)
        ),
    ),
    Case(
        "information_belief_other_message",
        G,
        edit=_information(
            lambda information, document: _belief(information, document, message_id="other")
        ),
    ),
    Case(
        "information_contact_fits",
        G,
        edit=apply(
            lambda document: _contact_with({"last_seen_minute": 1, "consecutive_minutes": 1})(
                document
            )
        ),
    ),
    Case("information_contact_key_text", G, edit=_contact("not json")),
    Case(
        "information_contact_key_spaces",
        G,
        edit=apply(lambda document: _contact(_pair(document).replace(",", ", "))(document)),
    ),
    Case(
        "information_contact_key_same",
        G,
        edit=apply(
            lambda document: _contact(f'["{_first_agent(document)}","{_first_agent(document)}"]')(
                document
            )
        ),
    ),
    Case(
        "information_contact_record",
        G,
        edit=apply(lambda document: _contact_with({"last_seen_minute": 1})(document)),
    ),
    Case(
        "information_contact_negative",
        G,
        edit=apply(
            lambda document: _contact_with({"last_seen_minute": 1, "consecutive_minutes": -1})(
                document
            )
        ),
    ),
    Case(
        "information_contact_duration",
        G,
        edit=apply(
            lambda document: _contact_with({"last_seen_minute": 1, "consecutive_minutes": 3})(
                document
            )
        ),
    ),
    Case(
        "information_pressure_high",
        G,
        edit=_information(
            lambda information, document: information["query_pressure"].update(
                {_first_agent(document): 2}
            )
        ),
    ),
    Case(
        "information_requests_dict",
        G,
        edit=state("information_spread_v1", "support_requests", value={}),
    ),
    Case("information_request_fits", G, edit=_support_request()),
    Case("information_request_fields", G, edit=_support_request(extra=1)),
    Case("information_request_unknown_message", G, edit=_support_request(message_ids=["other"])),
    Case("information_request_twice", G, edit=both(_support_request(), _support_request())),
    Case(
        "information_counter_negative",
        G,
        edit=state("information_spread_v1", "counters", "contact_attempts", value=-1),
    ),
    Case(
        "information_counter_mismatch",
        G,
        edit=state("information_spread_v1", "counters", "support_requests", value=3),
    ),
    Case("psychology_state_on_small", S, edit=state("psychological_distress_v1", value={})),
    Case("psychology_state_missing", G, edit=drop("model_state", "psychological_distress_v1")),
    Case(
        "psychology_state_schema",
        G,
        edit=state("psychological_distress_v1", "schema_version", value=2),
    ),
    Case(
        "psychology_state_schema_true",
        G,
        edit=state("psychological_distress_v1", "schema_version", value=True),
    ),
    Case(
        "psychology_state_minute",
        G,
        edit=state("psychological_distress_v1", "last_updated_minute", value=-2),
    ),
    Case(
        "psychology_agent_baseline",
        G,
        edit=_psychology(lambda record: record.update(baseline_distress=1.5)),
    ),
    Case(
        "psychology_agent_fields", G, edit=_psychology(lambda record: record.pop("completed_days"))
    ),
    Case(
        "psychology_agent_sum",
        G,
        edit=_psychology(lambda record: record.update(daily_acute_sum="0")),
    ),
    Case(
        "psychology_agent_observed",
        G,
        edit=_psychology(lambda record: record.update(daily_observed_minutes=1440)),
    ),
    Case(
        "psychology_agent_high",
        G,
        edit=_psychology(lambda record: record.update(daily_high_distress_minutes=-1)),
    ),
    Case(
        "psychology_agent_accumulation",
        G,
        edit=_psychology(lambda record: record.update(daily_acute_sum=10_000.0)),
    ),
]


def validate_information_fixture() -> dict[str, Any]:
    """`validate_information.json`."""
    return cases_fixture(INFORMATION_CASES)


# ---- The event rules -----------------------------------------------------------------------------


def _first_pending(edit: Callable[[dict[str, Any]], None]) -> Edit:
    def run(document: dict[str, Any]) -> None:
        edit(document["event_queue"][0])

    return run


def _last_record(edit: Callable[[dict[str, Any]], None]) -> Edit:
    def run(document: dict[str, Any]) -> None:
        edit(document["event_log"][-1])

    return run


def _realized_pending(document: dict[str, Any]) -> None:
    """The first pending event also realized, with a sequence of its own."""
    record = copy.deepcopy(document["event_log"][-1])
    record["event"] = copy.deepcopy(document["event_queue"][0])
    record["event"]["sequence"] = 9_000_000
    document["event_log"].append(record)
    document["id_counters"]["event"] = 9_000_001


def _sequence_of_log(document: dict[str, Any]) -> None:
    document["event_queue"][0]["sequence"] = document["event_log"][0]["event"]["sequence"]


EVENT_CASES = [
    Case("counter_negative", G, edit=put("id_counters", "fixture", value=-1)),
    Case("counter_text", G, edit=put("id_counters", "fixture", value="1")),
    Case("counter_list", G, edit=put("id_counters", "fixture", value=[1])),
    Case("event_counter_low", G, edit=put("id_counters", "event", value=1)),
    Case("event_counter_text", G, edit=put("id_counters", "event", value="9")),
    Case("event_counter_missing", S, edit=drop("id_counters", "event")),
    Case(
        "log_future_realization",
        G,
        edit=_last_record(lambda record: record.update(realized_minute=100)),
    ),
    Case(
        "log_outside_duration",
        S,
        edit=both(
            put("minute_index", value=1440),
            _last_record(lambda record: record.update(realized_minute=1440)),
        ),
    ),
    Case("pending_and_realized", G, edit=_realized_pending),
    Case("sequence_in_queue_and_log", G, edit=_sequence_of_log),
    Case(
        "pending_outside_scenario",
        S,
        edit=_first_pending(lambda event: event.update(due_minute=1440)),
    ),
    Case(
        "in_flight_event",
        G,
        edit=put("id_counters", "fixture", value=0),
        in_flight=lambda document: document["event_queue"][0],
    ),
]


def validate_events_fixture() -> dict[str, Any]:
    """`validate_events.json`."""
    return cases_fixture(EVENT_CASES)
