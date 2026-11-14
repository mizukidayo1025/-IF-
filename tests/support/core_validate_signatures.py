"""The extensions' signatures and the head of their validation, and Python's answers (#250, lane D,
#349).

`validate_simulator_extensions` first reads `simulator_signatures(world.model_configs)`: the
states the configs require (`simulator:<key>`), which must be in `model_state`, and the errors of
the configs it reads. The core ports both (`native/core/src/validate/signatures.rs`,
`extension_head.rs`). This file has two kinds of cases:

- `cases`: a change to the `generated` world that one rule of the head or of the signatures
  refuses (or lets through), with Python's class and text at each entry point (`cases_fixture`);
- `signatures`: a change to a world's configs and the signatures Python makes of them, as a list
  of `[key, value]` in the dict's order (or `[class, text]` of its error). These worlds may break
  other rules: only the signatures are compared.
"""

from __future__ import annotations

import copy
from collections.abc import Callable
from typing import Any

from support.core_validate import Case, base_entry, cases_fixture

Edit = Callable[[dict[str, Any]], None]

LEGACY = "facility_queues_legacy_mode_v1"
MARKER = {
    "schema_version": 1,
    "mode": "pre_independent_queue_checkpoint",
    "policy_version": "facility_fifo_legacy-v1",
}


def both(*edits: Edit) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        for one in edits:
            one(document)

    return edit


def config(name: str, value: Any) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        document["model_configs"][name] = copy.deepcopy(value)

    return edit


def config_field(name: str, field: str, value: Any) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        document["model_configs"][name][field] = value

    return edit


def drop_config(name: str) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        del document["model_configs"][name]

    return edit


def state(name: str, value: Any) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        document["model_state"][name] = copy.deepcopy(value)

    return edit


def drop_state(*names: str) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        for name in names:
            del document["model_state"][name]

    return edit


def case(name: str, edit: Edit, base: str = "generated") -> Case:
    return Case(name, base, edit=edit)


PROXIMITY = {
    "schema_version": 1,
    "enabled": True,
    "policy_version": "distance-sampled-proximity-v1",
    "sample_interval_minutes": 5,
    "max_distance_m": 1,
    "max_pairs_per_sample": 200,
}

CASES = [
    # The head: the required states, `temporary_spaces_v1` and the legacy queue marker.
    case("head_missing_one", drop_state("agent_dynamics_v1")),
    case("head_missing_two", drop_state("social_network_v1", "movement_v1")),
    case("head_none_is_held", state("social_network_v1", None)),
    case("head_temporary_spaces_unsigned", config_field("simulator_extensions", "enabled", False)),
    case("head_extensions_enabled_one", config_field("simulator_extensions", "enabled", 1)),
    case("head_legacy_mixed", state(LEGACY, MARKER)),
    case(
        "head_legacy_wrong",
        both(drop_state("facility_queues_v1"), state(LEGACY, {**MARKER, "mode": "x"})),
    ),
    case("head_legacy_none_marker", both(drop_state("facility_queues_v1"), state(LEGACY, None))),
    case("head_queues_missing", drop_state("facility_queues_v1")),
    # The configs the signatures read.
    case("sig_overlay_without_geometry", drop_config("facility_geometry")),
    case("sig_shared_operations_bad", config("shared_operations", {"schema_version": 2})),
    case("sig_shared_operations_extra", config_field("shared_operations", "x", 1)),
    case("sig_shared_operations_true", config("shared_operations", {"schema_version": True})),
    case("sig_shared_operations_list", config("shared_operations", [])),
    case("sig_overlay_schema_5", config("geometry_overlay", {"schema_version": 5})),
    case("sig_overlay_extra", config("geometry_overlay", {"schema_version": 4, "x": 1})),
    case("sig_overlay_true", config("geometry_overlay", {"schema_version": True})),
    case("sig_agent_dynamics_config", config("agent_dynamics", {"x": 1})),
    case("sig_agent_dynamics_unhashable", config_field("agent_dynamics", "policy_version", [])),
    case("sig_staff_dynamics_config", config_field("staff_dynamics", "rest_minutes", 0)),
    case("sig_staff_dynamics_unhashable", config_field("staff_dynamics", "policy_version", {})),
    case("sig_proximity_fields", config("social_proximity", {})),
    case("sig_proximity_bounds", config("social_proximity", {**PROXIMITY, "max_distance_m": 20})),
    case(
        "sig_awareness_ok",
        config("facility_awareness", {"initial_known_facility_ids": ["toilet_01"]}),
    ),
]


def _geometry_service_points(points: Callable[[list[Any]], list[Any]]) -> Edit:
    def edit(document: dict[str, Any]) -> None:
        geometry = document["model_configs"]["facility_geometry"]
        geometry["service_points"] = points(geometry["service_points"])

    return edit


def proximity_selected(document: dict[str, Any]) -> None:
    """The registry selects the contact sampler, each domain by itself (no suite)."""
    registry = document["model_configs"]["policy_registry"]
    del registry["suite_id"]
    registry["social_proximity"] = "distance-sampled-proximity-v1"


SIGNATURE_CASES = [
    case("signed_generated", lambda document: None),
    case("signed_small", lambda document: None, base="small_replay"),
    case("signed_small_context", config("population_context", {}), base="small_replay"),
    case(
        "signed_staff_v2",
        config("staff_dynamics", {"policy_version": "continuous-duty-fatigue-v2"}),
    ),
    case("signed_staff_absent", drop_config("staff_dynamics")),
    case("signed_staff_empty", config("staff_dynamics", {})),
    case("signed_agent_v1", drop_config("agent_dynamics")),
    case("signed_no_shared_operations", drop_config("shared_operations")),
    case("signed_no_overlay", drop_config("geometry_overlay")),
    case("signed_overlay_2", config("geometry_overlay", {"schema_version": 2})),
    case("signed_overlay_3", config("geometry_overlay", {"schema_version": 3})),
    case("signed_extensions_off", config_field("simulator_extensions", "enabled", False)),
    case("signed_no_extensions", drop_config("simulator_extensions")),
    case("signed_no_zone_environment", drop_config("zone_environment")),
    # pydantic's ValidationError, which the core raises as a ValueError with the same text.
    case("signed_geometry_invalid", config_field("facility_geometry", "schema_version", 2)),
    case(
        "signed_environment_invalid",
        config_field("zone_environment", "max_rest_decision_minutes", "x"),
    ),
    case(
        "signed_environment_dict", config_field("zone_environment", "max_rest_decision_minutes", {})
    ),
    case("signed_no_service_points", _geometry_service_points(lambda points: [])),
    case("signed_one_service_point", _geometry_service_points(lambda points: points[:1])),
    case(
        "signed_awareness",
        config(
            "facility_awareness",
            {
                "initial_known_facility_ids": ["toilet_01", "intake_desk"],
                "discover_co_located_facilities_on_arrival": False,
            },
        ),
    ),
    case("signed_proximity_unselected", config("social_proximity", PROXIMITY)),
    case("signed_proximity", both(config("social_proximity", PROXIMITY), proximity_selected)),
    case("signed_proximity_off", config("social_proximity", {**PROXIMITY, "enabled": False})),
    case(
        "signed_proximity_float",
        both(config("social_proximity", {**PROXIMITY, "max_distance_m": 2.75}), proximity_selected),
    ),
]


def _signatures(document: dict[str, Any]) -> Any:
    """What `simulator_signatures` makes of the configs of the world the document builds (which
    holds them as the mirror's tracked values, as the validation reads them)."""
    from sheltersim.core.world import WorldState
    from sheltersim.domain.simulator_state import simulator_signatures

    world = WorldState.from_dict(copy.deepcopy(document), _validate=False)
    try:
        signed = simulator_signatures(world.model_configs)
    except Exception as error:  # every class, as Python raised it
        return [type(error).__name__, str(error)]
    return [[key, value] for key, value in signed.items()]


def validate_signatures_fixture() -> dict[str, Any]:
    """`validate_signatures.json`: the cases of the head and the signatures, and the signatures
    Python makes of other configs."""
    fixture = cases_fixture(CASES + SIGNATURE_CASES)
    signed = {case.name for case in SIGNATURE_CASES}
    fixture["signatures"] = []
    for one in CASES + SIGNATURE_CASES:
        document = base_entry(one.base)["world"]
        assert one.edit is not None
        one.edit(document)
        fixture["signatures"].append(
            {"name": one.name, "world": one.name, "expected": _signatures(document)}
        )
    fixture["cases"] = [item for item in fixture["cases"] if item["name"] not in signed]
    return fixture
