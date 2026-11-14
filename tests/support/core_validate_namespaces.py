"""Broken extension namespaces and Python's errors for the core's validation (#250, lane D, #349).

`validate_simulator_extensions` checks each namespace with its own validator; the core ports seven
of them (`native/core/src/validate/<area>.rs`): agent_dynamics_v1, body_state_v1 (with its
history), staff_dynamics_v1, staff_operations_v1, social_network_v1, population_dynamics_v1 and
population_lifecycle_v1. Each case below changes one value of the `generated` world of
`validate_worlds.json` so that one rule refuses it (or, for an accepting case, so that a rule must
let it through), and `cases_fixture` records what Python raises at each entry point.

A case of an active staff task (`_ordered`) orders one on the live world first, as an operator's
order does (`order_staff_operation`), and changes its record after.
"""

from __future__ import annotations

from collections.abc import Callable
from typing import Any

from support.core_validate import Case, cases_fixture

Edit = Callable[[dict[str, Any]], None]

AD = "agent_dynamics_v1"
BODY = "body_state_v1"
SD = "staff_dynamics_v1"
SO = "staff_operations_v1"
SN = "social_network_v1"
PD = "population_dynamics_v1"
PL = "population_lifecycle_v1"


def _node(document: dict[str, Any], path: tuple[Any, ...]) -> Any:
    node = document
    for key in path:
        node = node[key]
    return node


def state(*path: Any, value: Any) -> Edit:
    """Set a value of `model_state` at a path."""

    def edit(document: dict[str, Any]) -> None:
        _node(document["model_state"], path[:-1])[path[-1]] = value

    return edit


def drop(*path: Any) -> Edit:
    """Remove a key of `model_state` at a path."""

    def edit(document: dict[str, Any]) -> None:
        del _node(document["model_state"], path[:-1])[path[-1]]

    return edit


def change(*path: Any, by: Callable[[Any], Any]) -> Edit:
    """Replace a value of `model_state` at a path with what `by` makes of it."""

    def edit(document: dict[str, Any]) -> None:
        node = _node(document["model_state"], path[:-1])
        node[path[-1]] = by(node[path[-1]])

    return edit


def both(*edits: Edit) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        for one in edits:
            one(document)

    return edit


def case(name: str, edit: Edit) -> Case:
    return Case(name, "generated", edit=edit)


# ---- agent_dynamics_v1 ---------------------------------------------------------------------------

AGENT_DYNAMICS = [
    case("ad_not_a_dict", state(AD, value=[])),
    case("ad_extra_field", state(AD, "extra", value=1)),
    case("ad_schema_2", state(AD, "schema_version", value=2)),
    case("ad_schema_true", state(AD, "schema_version", value=True)),
    case("ad_policy_v2", state(AD, "policy_version", value="need-led-autonomy-memory-v2")),
    case("ad_agent_missing", drop(AD, "agents", "A010")),
    case("ad_record_not_a_dict", state(AD, "agents", "A001", value=1)),
    case("ad_record_extra_field", state(AD, "agents", "A001", "extra", value=1)),
    case(
        "ad_known_repeated",
        change(AD, "agents", "A001", "known_facility_ids", by=lambda ids: ids + ids[:1]),
    ),
    case("ad_known_unhashable", state(AD, "agents", "A001", "known_facility_ids", value=[[1]])),
    case("ad_known_unknown", state(AD, "agents", "A001", "known_facility_ids", value=["nowhere"])),
    case(
        "ad_known_not_a_list", state(AD, "agents", "A001", "known_facility_ids", value="toilet_01")
    ),
    case("ad_due_true", state(AD, "agents", "A001", "decision_due_minute", value=True)),
    case("ad_due_negative", state(AD, "agents", "A001", "decision_due_minute", value=-1)),
    case("ad_due_float", state(AD, "agents", "A001", "decision_due_minute", value=15.0)),
    case("ad_last_true", state(AD, "agents", "A001", "last_decision_minute", value=True)),
    case("ad_last_none", state(AD, "agents", "A001", "last_decision_minute", value=None)),
    case("ad_last_negative", state(AD, "agents", "A002", "last_decision_minute", value=-3)),
]

# ---- body_state_v1 -------------------------------------------------------------------------------


def _relationship(**changes: Any) -> dict[str, Any]:
    record = {
        "contact_count": 1,
        "trust": 0.5,
        "contact_minutes": 5,
        "last_contact_minute": 10,
        "last_interaction": None,
        "topic_ids": ["water"],
    }
    record.update(changes)
    return record


def relationship(key: str, **changes: Any) -> Edit:
    return state(BODY, "relationships", key, value=_relationship(**changes))


def _item(**changes: Any) -> dict[str, Any]:
    item = {
        "event_id": "social-1",
        "minute": 15,
        "kind": "conversation",
        "actor_ids": ["A001", "A002"],
        "topic_id": None,
        "causation_id": None,
        "deltas": {},
    }
    item.update(changes)
    return item


def history(*items: dict[str, Any]) -> Edit:
    return state(BODY, "history", value=list(items))


def body_config(name: str, value: Any) -> Edit:
    return state(BODY, "config", name, value=value)


PAIR = '["A001","A002"]'

BODY_STATE = [
    case("body_not_a_dict", state(BODY, value="body")),
    case("body_source", state(BODY, "source_id", value="ASM-OTHER")),
    case("body_policy", state(BODY, "policy_version", value=None)),
    case("body_agent_extra", state(BODY, "agents", "Z999", value={})),
    case("body_relationships_list", state(BODY, "relationships", value=[])),
    case("body_history_dict", state(BODY, "history", value={})),
    case("body_config_none", state(BODY, "config", value=None)),
    case("body_config_list", state(BODY, "config", value=[])),
    case("body_config_extra", body_config("extra", 1)),
    case("body_config_schema", body_config("schema_version", 2)),
    case("body_config_rate_true", body_config("sleep_debt_stress_rate", True)),
    case("body_config_rate_high", body_config("correction_reassurance_gain", 1.5)),
    case("body_config_rate_int", body_config("sleep_debt_stress_rate", 1)),
    case("body_config_minutes_float", body_config("social_interaction_minutes", 5.0)),
    case("body_config_minutes_zero", body_config("social_interaction_minutes", 0)),
    case("body_config_affect_int", body_config("affect_policy", 1)),
    case("body_config_affect_unknown", body_config("affect_policy", "x")),
    # The Policy Registry selects the affect model: the saved config must name it, and has it.
    case(
        "body_config_affect_other",
        body_config("affect_policy", "trust-weighted-positive-negative-v1"),
    ),
    case("body_config_affect_missing", drop(BODY, "config", "affect_policy")),
    case("body_config_distance", body_config("relationship_distance_scale_m", 0.05)),
    case("body_config_distance_int", body_config("relationship_distance_scale_m", 3)),
    case("body_config_per_agent_empty", body_config("sleep_debt_stress_rate_by_agent", {})),
    case("body_config_per_agent_list", body_config("hygiene_support_relief_by_agent", [])),
    case(
        "body_config_per_agent_bad_id", body_config("sleep_debt_stress_rate_by_agent", {"-A": 0.1})
    ),
    case(
        "body_config_per_agent_bool", body_config("discontent_stress_rate_by_agent", {"A001": True})
    ),
    case("body_config_per_agent_none", body_config("sleep_debt_stress_rate_by_agent", None)),
    case(
        "body_config_per_agent_unknown",
        body_config("sleep_debt_stress_rate_by_agent", {"Z999": 0.1}),
    ),
    case(
        "body_config_per_agent_known",
        body_config("medical_assessment_uncertainty_relief_by_agent", {"A001": 0.1}),
    ),
    case(
        "body_config_population_none",
        both(body_config("sleep_debt_stress_rate_by_agent", {"Z999": 0.1}), state(PD, value=None)),
    ),
    case(
        "body_config_population_traits_int",
        both(
            body_config("sleep_debt_stress_rate_by_agent", {"Z999": 0.1}),
            state(PD, "profile_traits", value=3),
        ),
    ),
    case(
        "body_config_population_ids_unhashable",
        both(
            body_config("sleep_debt_stress_rate_by_agent", {"Z999": 0.1}),
            change(PD, "initial_agent_ids", by=lambda ids: [*ids, ["Z999"]]),
        ),
    ),
    case("body_config_optional_missing", drop(BODY, "config", "hygiene_support_relief")),
    case("body_config_required_missing", drop(BODY, "config", "sleep_debt_stress_rate")),
    case("body_record_not_a_dict", state(BODY, "agents", "A001", value=None)),
    case("body_record_extra", state(BODY, "agents", "A001", "extra", value=0.0)),
    case("body_record_true", state(BODY, "agents", "A001", "uncertainty", value=True)),
    case("body_record_int", state(BODY, "agents", "A001", "uncertainty", value=1)),
    case(
        "body_record_sleep_high", state(BODY, "agents", "A001", "sleep_debt_minutes", value=1440.5)
    ),
    case("body_record_sleep_max", state(BODY, "agents", "A001", "sleep_debt_minutes", value=1440)),
    case("body_record_negative", state(BODY, "agents", "A003", "hygiene_need", value=-0.1)),
    case("body_relationship_ok", relationship(PAIR)),
    case("body_relationship_unparsable", relationship("[")),
    case("body_relationship_not_json", relationship("A001|A002")),
    case("body_relationship_single", relationship('["A001"]')),
    case("body_relationship_dict", relationship('{"A001":"A002"}')),
    case("body_relationship_unhashable_first", relationship('[["A001"],"A002"]')),
    case("body_relationship_unhashable_second", relationship('["A001",{"a":1}]')),
    case("body_relationship_unknown_then_list", relationship('["Z9",["A002"]]')),
    case("body_relationship_spaced", relationship('["A001", "A002"]')),
    case("body_relationship_escaped", relationship('["\\u0041001","A002"]')),
    case("body_relationship_same", relationship('["A001","A001"]')),
    case("body_relationship_unknown", relationship('["A001","Z999"]')),
    case("body_relationship_number", relationship('[1e5000,"A002"]')),
    case("body_relationship_nan", relationship('[NaN,"A002"]')),
    case("body_relationship_surrogate", relationship('["\\ud800","A002"]')),
    case("body_relationship_long_int", relationship("[" + "9" * 4301 + ',"A002"]')),
    case("body_relationship_count_true", relationship(PAIR, contact_count=True)),
    case("body_relationship_minutes_negative", relationship(PAIR, contact_minutes=-1)),
    case("body_relationship_topics", relationship(PAIR, topic_ids=["t"] * 33)),
    case("body_relationship_topic_long", relationship(PAIR, topic_ids=["é" * 64])),
    case("body_relationship_topic_too_long", relationship(PAIR, topic_ids=["é" * 65])),
    case("body_relationship_topic_int", relationship(PAIR, topic_ids=[1])),
    case("body_relationship_trust_high", relationship(PAIR, trust=1.5)),
    case("body_relationship_trust_true", relationship(PAIR, trust=True)),
    case("body_relationship_trust_int", relationship(PAIR, trust=1)),
    case("body_relationship_minute_float", relationship(PAIR, last_contact_minute=10.0)),
    case("body_relationship_record_list", state(BODY, "relationships", PAIR, value=[])),
    case("body_history_ok", history(_item(minute=3), _item(minute=14), _item())),
    case("body_history_not_a_dict", history(_item(), 3)),
    case("body_history_missing", history({k: v for k, v in _item().items() if k != "deltas"})),
    case("body_history_minute_true", history(_item(minute=True))),
    case("body_history_minute_negative", history(_item(minute=-1))),
    case("body_history_actor_unknown", history(_item(actor_ids=["Z999"]))),
    case("body_history_actor_unhashable", history(_item(actor_ids=[["A001"]]))),
    case(
        "body_history_actor_departed",
        both(
            history(_item(actor_ids=["Z999"])),
            state(PD, "departures", value=[{"agent_id": "Z999"}]),
        ),
    ),
    case("body_history_departures_int", both(history(_item()), state(PD, "departures", value=7))),
    case("body_history_deltas_list", history(_item(deltas=[]))),
    # Only the whole history reads an old item: the check of a tick reads the last tick's.
    case(
        "body_history_old_bad",
        history(_item(minute=2, deltas=None), _item(minute=10), _item(minute=14), _item()),
    ),
    case(
        "body_history_old_bad_kept",
        history(_item(minute=2), _item(minute=13, deltas=None), _item(minute=14), _item()),
    ),
]

# ---- staff_dynamics_v1 ---------------------------------------------------------------------------


def sd_config(name: str, value: Any) -> Edit:
    return state(SD, "config", name, value=value)


def sd_v2(**config: Any) -> Edit:
    """The saved state of a v2 config with these fields."""
    return both(
        state(SD, "policy_version", value="continuous-duty-fatigue-v2"),
        state(SD, "source_id", value="ASM-STAFF-DYNAMICS-V2"),
        sd_config("policy_version", "continuous-duty-fatigue-v2"),
        sd_config("source_id", "ASM-STAFF-DYNAMICS-V2"),
        *(sd_config(name, value) for name, value in config.items()),
    )


def windows(*rows: tuple[int, int]) -> list[dict[str, int]]:
    return [{"start_minute": start, "end_minute": end} for start, end in rows]


STAFF_DYNAMICS = [
    case("sd_not_a_dict", state(SD, value=1)),
    case("sd_schema_true", state(SD, "schema_version", value=True)),
    case("sd_schema_float", state(SD, "schema_version", value=1.5)),
    case("sd_staff_missing", drop(SD, "staff", "staff_003")),
    case("sd_config_extra", sd_config("extra", 1)),
    case("sd_config_not_a_dict", state(SD, "config", value=[])),
    case("sd_config_none", state(SD, "config", value=None)),
    case("sd_config_policy_list", sd_config("policy_version", [])),
    case("sd_config_policy_other", sd_config("policy_version", "x")),
    case("sd_config_source_empty", sd_config("source_id", "")),
    case("sd_config_v1_windows", sd_config("shift_windows_by_staff", {"staff_001": []})),
    case("sd_config_v1_recovery", sd_config("off_shift_fatigue_recovery_per_minute", 0.1)),
    case("sd_config_rest_zero", sd_config("rest_minutes", 0)),
    case("sd_config_rest_true", sd_config("rest_minutes", True)),
    case("sd_config_max_float", sd_config("max_continuous_work_minutes", 240.0)),
    case("sd_config_maxima_list", sd_config("max_continuous_work_minutes_by_staff", [])),
    case(
        "sd_config_maxima_value",
        sd_config("max_continuous_work_minutes_by_staff", {"staff_001": 0}),
    ),
    case(
        "sd_config_maxima_ok", sd_config("max_continuous_work_minutes_by_staff", {"staff_001": 120})
    ),
    case(
        "sd_config_maxima_unknown",
        sd_config("max_continuous_work_minutes_by_staff", {"staff_999": 120}),
    ),
    case(
        "sd_v2_ok",
        sd_v2(
            off_shift_fatigue_recovery_per_minute=0.005,
            shift_windows_by_staff={"staff_001": windows((0, 480))},
        ),
    ),
    case(
        "sd_v2_windows_list",
        sd_v2(off_shift_fatigue_recovery_per_minute=0.005, shift_windows_by_staff=[]),
    ),
    case(
        "sd_v2_windows_many",
        sd_v2(
            off_shift_fatigue_recovery_per_minute=0.005,
            shift_windows_by_staff={
                "staff_001": windows(*((i * 10, i * 10 + 5) for i in range(22)))
            },
        ),
    ),
    case(
        "sd_v2_windows_row",
        sd_v2(
            off_shift_fatigue_recovery_per_minute=0.005,
            shift_windows_by_staff={"staff_001": windows((10, 10))},
        ),
    ),
    case(
        "sd_v2_windows_overlap",
        sd_v2(
            off_shift_fatigue_recovery_per_minute=0.005,
            shift_windows_by_staff={"staff_001": windows((100, 200), (0, 120))},
        ),
    ),
    case(
        "sd_v2_windows_touch",
        sd_v2(
            off_shift_fatigue_recovery_per_minute=0.005,
            shift_windows_by_staff={"staff_001": windows((120, 200), (0, 120))},
        ),
    ),
    case(
        "sd_v2_windows_unknown",
        sd_v2(
            off_shift_fatigue_recovery_per_minute=0.005,
            shift_windows_by_staff={"staff_999": windows((0, 480))},
        ),
    ),
    case("sd_v2_recovery_high", sd_v2(off_shift_fatigue_recovery_per_minute=2.0)),
    case("sd_identity", state(SD, "source_id", value="ASM-OTHER")),
    case("sd_config_not_canonical", drop(SD, "config", "rest_minutes")),
    case(
        "sd_config_legacy",
        both(
            drop(SD, "config", "shift_windows_by_staff"),
            drop(SD, "config", "off_shift_fatigue_recovery_per_minute"),
        ),
    ),
    case("sd_record_not_a_dict", state(SD, "staff", "staff_001", value=[])),
    case(
        "sd_record_minutes_true", state(SD, "staff", "staff_001", "total_work_minutes", value=True)
    ),
    case(
        "sd_record_minutes_negative", state(SD, "staff", "staff_002", "rest_until_minute", value=-1)
    ),
    case("sd_record_fatigue_high", state(SD, "staff", "staff_001", "fatigue", value=1.5)),
    case("sd_record_fatigue_int", state(SD, "staff", "staff_001", "fatigue", value=1)),
    case("sd_record_activity", state(SD, "staff", "staff_001", "last_activity", value="sleeping")),
    case(
        "sd_record_activity_list",
        state(SD, "staff", "staff_001", "last_activity", value=["working"]),
    ),
]

# ---- staff_operations_v1 -------------------------------------------------------------------------


def so_config(name: str, value: Any) -> Edit:
    return state(SO, "config", name, value=value)


def _so_item(**changes: Any) -> dict[str, Any]:
    item = {
        "task_id": "staff-operation-1",
        "staff_id": "staff_002",
        "task_kind": "patrol",
        "target_id": None,
        "target_zone_id": "sleeping",
        "reason": "a test order",
        "status": "completed",
        "start_minute": 1,
        "arrival_minute": 3,
        "complete_minute": 10,
        "travel_minutes": 2,
        "causation_event_ids": ["order-1"],
    }
    item.update(changes)
    return item


def so_history(**changes: Any) -> Edit:
    return state(SO, "history", value=[_so_item(**changes)])


def _ordered(*changes: Callable[[Any], None], arrive: bool = False) -> Callable[[Any], None]:
    """An edit of the live world: staff_002 (given the support skill) is ordered to patrol the
    sleeping area, has arrived there if `arrive`, and its record is changed after."""

    def edit(world: Any) -> None:
        from sheltersim.domain.staff_operations import order_staff_operation
        from sheltersim.events.handlers import StaffOperationArrivalHandler

        world.staff["staff_002"].skills = ("support", *world.staff["staff_002"].skills)
        order_staff_operation(
            world,
            staff_id="staff_002",
            task_kind="patrol",
            target_zone_id="sleeping",
            target_id=None,
            reason="a test order",
            causation_event_id="order-1",
            source_id=None,
        )
        if arrive:
            arrival = next(
                e for e in world.event_queue.pending() if e.event_type == "staff_operation_arrive"
            )
            world.event_queue.remove(arrival.event_id)
            world.minute_index = arrival.due_minute
            StaffOperationArrivalHandler().apply(world, arrival)
        for one in changes:
            one(world)

    return edit


def record(name: str, value: Any) -> Callable[[Any], None]:
    def edit(world: Any) -> None:
        world.model_state[SO]["staff"]["staff_002"][name] = value

    return edit


def record_by(name: str, by: Callable[[Any], Any]) -> Callable[[Any], None]:
    def edit(world: Any) -> None:
        held = world.model_state[SO]["staff"]["staff_002"]
        held[name] = by(held[name])

    return edit


def ordered(name: str, *changes: Callable[[Any], None], arrive: bool = False) -> Case:
    return Case(name, "generated", edit_world=_ordered(*changes, arrive=arrive))


STAFF_OPERATIONS = [
    case("so_not_a_dict", state(SO, value=[])),
    case("so_zone_missing", drop(SO, "zones", "toilet")),
    case("so_history_dict", state(SO, "history", value={})),
    case("so_config_none", state(SO, "config", value=None)),
    case("so_config_extra", so_config("extra", 1)),
    case("so_config_source", so_config("source_id", "x")),
    case(
        "so_config_durations_missing",
        change(
            SO,
            "config",
            "task_duration_minutes",
            by=lambda d: {k: v for k, v in d.items() if k != "intake"},
        ),
    ),
    case(
        "so_config_duration_zero",
        state(SO, "config", "task_duration_minutes", "maintenance", value=0),
    ),
    case(
        "so_config_duration_float",
        state(SO, "config", "task_duration_minutes", "cleaning", value=15.0),
    ),
    case("so_config_rate_high", so_config("patrol_reassurance", 2)),
    case("so_config_rate_true", so_config("cleaning_sanitation_recovery", True)),
    case("so_config_thresholds_list", so_config("thresholds_by_zone", [])),
    case("so_config_threshold_empty", so_config("thresholds_by_zone", {"food": {}})),
    case("so_config_threshold_name", so_config("thresholds_by_zone", {"food": {"x": 0.1}})),
    case(
        "so_config_threshold_value",
        so_config("thresholds_by_zone", {"food": {"patrol_stress_at_least": 1.5}}),
    ),
    case(
        "so_config_threshold_ok",
        so_config("thresholds_by_zone", {"food": {"patrol_stress_at_least": 0.5}}),
    ),
    case(
        "so_config_threshold_unknown_zone",
        so_config("thresholds_by_zone", {"roof": {"patrol_stress_at_least": 0.5}}),
    ),
    case("so_config_effects_list", so_config("effects_by_zone", 1)),
    case(
        "so_config_effect_sanitation",
        so_config("effects_by_zone", {"food": {"sanitation_use_per_person_minute": 0.5}}),
    ),
    case(
        "so_config_effect_patrol",
        so_config("effects_by_zone", {"food": {"patrol_reassurance": -0.5}}),
    ),
    case(
        "so_config_effect_unknown_zone",
        so_config("effects_by_zone", {"roof": {"patrol_reassurance": 0.5}}),
    ),
    case(
        "so_config_legacy",
        both(drop(SO, "config", "thresholds_by_zone"), drop(SO, "config", "effects_by_zone")),
    ),
    case("so_config_not_canonical", drop(SO, "config", "patrol_reassurance")),
    case("so_zone_sanitation", state(SO, "zones", "food", "sanitation", value=1.5)),
    case("so_zone_minute_true", state(SO, "zones", "food", "updated_minute", value=True)),
    case("so_record_phase", state(SO, "staff", "staff_002", "phase", value="resting")),
    case("so_record_phase_list", state(SO, "staff", "staff_002", "phase", value=["idle"])),
    case("so_record_extra", state(SO, "staff", "staff_002", "extra", value=None)),
    case("so_idle_retains", state(SO, "staff", "staff_002", "task_id", value="t")),
    case("so_idle_travel_float", state(SO, "staff", "staff_002", "travel_minutes", value=0.0)),
    case("so_history_ok", so_history()),
    case("so_history_interrupted", so_history(status="interrupted", arrival_minute=None)),
    case("so_history_no_arrival", so_history(arrival_minute=None)),
    case("so_history_status", so_history(status="done")),
    case("so_history_staff_list", so_history(staff_id=["staff_002"])),
    case("so_history_zone_dict", so_history(target_zone_id={})),
    case("so_history_target_unknown", so_history(target_id="nowhere")),
    case("so_history_minutes", so_history(complete_minute=0)),
    case("so_history_arrival_late", so_history(arrival_minute=11)),
    case("so_history_causes", so_history(causation_event_ids=[""])),
]

# A staff member on an ordered task (a file of its own: each world is the generated one with the
# order's events and tasks, which make the larger variants).
STAFF_TASKS = [
    ordered("so_travel_ok"),
    ordered("so_working_ok", arrive=True),
    ordered("so_travel_due_moved", record_by("due_minute", lambda due: due + 1)),
    ordered("so_travel_kind_list", record("task_kind", ["patrol"])),
    ordered(
        "so_travel_route_zone_list",
        record_by("route_zone_ids", lambda zones: [[zone] for zone in zones]),
    ),
    ordered(
        "so_travel_route_edge_unknown",
        record_by("route_edge_ids", lambda edges: ["nowhere", *edges[1:]]),
    ),
    ordered("so_travel_reason_empty", record("reason", "")),
    ordered("so_travel_target_dict", record("target_id", {})),
    ordered(
        "so_travel_arrival_set",
        record_by("travel_due_minute", lambda due: due),
        record_by("arrival_minute", lambda _: 3),
    ),
    ordered("so_travel_due_wrong", record_by("travel_due_minute", lambda due: due + 1)),
    ordered("so_travel_route_short", record_by("route_zone_ids", lambda zones: zones[:-1])),
    ordered("so_travel_kind_cleaning", record("task_kind", "cleaning")),
    ordered("so_travel_kind_intake", record("task_kind", "intake")),
    ordered("so_working_route_kept", record("route_edge_ids", ["e"]), arrive=True),
    ordered(
        "so_working_arrival_wrong",
        record_by("arrival_minute", lambda minute: minute + 1),
        arrive=True,
    ),
]

# ---- social_network_v1 ---------------------------------------------------------------------------


def _tie(**changes: Any) -> dict[str, Any]:
    tie = {
        "contact_count": 1,
        "conversation_count": 0,
        "cooperation_count": 0,
        "trust": None,
        "shared_message_ids": [],
        "last_contact_minute": 5,
        "last_interaction": None,
    }
    tie.update(changes)
    return tie


def tie(key: str, **changes: Any) -> Edit:
    return state(SN, "relationships", key, value=_tie(**changes))


SOCIAL_NETWORK = [
    case("sn_not_a_dict", state(SN, value=1)),
    case("sn_policy", state(SN, "policy_version", value="x")),
    case("sn_schema_float", state(SN, "schema_version", value=1.0)),
    case("sn_count_missing", drop(SN, "interaction_count")),
    case("sn_relationships_list", state(SN, "relationships", value=[])),
    case("sn_tie_ok", tie('["A001","A002"]')),
    case("sn_key_unparsable", tie('["A001"')),
    case("sn_key_empty", tie("")),
    case("sn_key_single", tie('["A001"]')),
    case("sn_key_int", tie('["A001",2]')),
    case("sn_key_same", tie('["A001","A001"]')),
    case("sn_key_spaced", tie(' ["A001","A002"]')),
    case("sn_key_unknown_people", tie('["Z1","Z2"]')),
    case("sn_key_surrogate", tie('["\\ud800","A002"]')),
    case("sn_key_escaped_quote", tie('["A\\"1","A002"]')),
    case("sn_key_escaped_slash", tie('["A\\/1","A002"]')),
    case("sn_key_long_int", tie("[" + "1" * 4400 + "]")),
    case("sn_key_infinity", tie("-Infinity")),
    case("sn_count_true", tie('["A001","A002"]', contact_count=True)),
    case("sn_count_negative", tie('["A001","A002"]', cooperation_count=-1)),
    case("sn_messages_dict", tie('["A001","A002"]', shared_message_ids={})),
    case("sn_tie_extra", state(SN, "relationships", '["A001","A002"]', value={**_tie(), "x": 1})),
    case("sn_trust_high", tie('["A001","A002"]', trust=1.01)),
    case("sn_trust_true", tie('["A001","A002"]', trust=False)),
    case("sn_trust_int", tie('["A001","A002"]', trust=0)),
    case("sn_minute_true", tie('["A001","A002"]', last_contact_minute=True)),
    case("sn_minute_negative", tie('["A001","A002"]', last_contact_minute=-5)),
]

# ---- population_dynamics_v1 and population_lifecycle_v1 (kernels of Python's) --------------------

POPULATION = [
    case("pd_not_a_dict", state(PD, value=[])),
    case("pd_field_missing", drop(PD, "arrivals")),
    case("pd_initial_repeated", change(PD, "initial_agent_ids", by=lambda ids: ids + ids[:1])),
    case("pd_traits_int", state(PD, "profile_traits", "A001", value=1)),
    case("pd_context_unknown", state(PD, "profile_context", "Z999", value={})),
    case("pd_pending_bad", state(PD, "pending_arrivals", value=[{}])),
    case("pl_not_a_dict", state(PL, value=1)),
    case("pl_field_missing", drop(PL, "messages")),
    case("pl_schema", state(PL, "schema_version", value=2)),
    case("pl_person_bad", state(PL, "people", "A001", value={})),
    case("pl_return_bad", state(PL, "pending_returns", value=[{}])),
    case("pl_relocation_bad", state(PL, "pending_relocations", "A001", value={})),
    case("pl_archive_bad", state(PL, "relationship_archive", value=[{}])),
    case("pl_message_bad", state(PL, "messages", "m1", value={})),
    case("pl_beliefs_bad", state(PL, "deferred_beliefs", "A001", value=[])),
    case("pl_message_unhashable", state(PL, "messages", "m1", value={"message_id": []})),
]

CASES = (
    AGENT_DYNAMICS + BODY_STATE + STAFF_DYNAMICS + STAFF_OPERATIONS + SOCIAL_NETWORK + POPULATION
)


def validate_namespaces_fixture() -> dict[str, Any]:
    """`validate_namespaces.json`: the cases of the namespaces lane D ports."""
    return cases_fixture(CASES)


def validate_staff_tasks_fixture() -> dict[str, Any]:
    """`validate_staff_tasks.json`: the cases of `staff_operations_v1` with an ordered task."""
    return cases_fixture(STAFF_TASKS)
