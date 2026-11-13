"""One world as JSON for the core without Python (`sheltersim_core::load`, #225, #244).

The core's loader (`native/core/src/load/`) builds a World, a Run and the sections and floor
plan a host fills from what `export_world` writes of one world, at any minute: the world's
canonical document, the slots its core owns, what the Run read of the controller
(`RunCore.policies_document`, the route capacities, the zone neighbors), and the sections of the
configs that Python's contracts and factories parse (`SectionId::parser() == Parser::Host` in
`native/core/src/config/mod.rs`; the core parses the others from the configs' `V` itself). The
core's tests read worlds written with it (`tests/support/core_fixtures.py`), and a host without
Python (the browser's, stage 4 of #132) runs ticks on one. Exporting reads the world and the
controller and changes neither.

Each section is what the PyO3 crate's `PyHost::fill_section` reads from Python
(`native/src/core/configs.rs`, `native/src/core/geometry.rs`, `rules/after_tick.rs`'s
`fill_tick_plan`), before it turns it into the core's value: `{"value": ...}`, or
`{"fault": [class, text]}` for what the parse raised (the class as `config::FaultClass`: `Value`,
`Type`, `Key`, `Validation`, `Refused`, or the module and name of another class), or
`{"absent": true}` (a facility geometry the configs don't have), or `{"host_error": text}` where
the PyO3 crate raises out of `fill_section` instead of keeping a fault. A dict loses its order
in a file written with sorted keys, as the world's canonical document does.
"""

from __future__ import annotations

import json
from collections.abc import Callable
from typing import Any

# The attributes of `InformationSpreadConfig` the PyO3 crate reads (`InformationSpread::FIELDS` of
# the core, but `present`, which is whether the configs have the key).
INFORMATION_FIELDS = (
    "enabled",
    "minimum_contact_minutes",
    "default_source_reliability",
    "correction_source_reliability_by_id",
    "rumor_belief_probability",
    "rumor_trust_sensitivity",
    "rumor_source_sensitivity",
    "query_pressure_per_claim",
    "query_pressure_relief_per_correction",
    "correction_acceptance_probability",
    "correction_trust_sensitivity",
    "correction_source_sensitivity",
)


def export_world(
    world: Any, controller: Any | None = None, *, validate: bool = True
) -> dict[str, Any]:
    """What the core's loader reads of one world as it is now: its canonical document, the slots
    its core owns (`[name, is_field]`), what the Run read of the controller (without a
    controller, nothing), and the Python-parsed sections of its configs (`sections`). Without
    `validate`, a world a test edited into a state `WorldState.validate` turns down is written as
    it is (the same text as `canonical_json`, which validates first)."""
    from sheltersim.core.state_mirror import OWNED_FIELDS

    core = world.__dict__["_core"]
    text = world.canonical_json() if validate else world._canonical_text(world._canonical_data())
    entry: dict[str, Any] = {
        "world": json.loads(text),
        "slots": [[name, name in OWNED_FIELDS] for name in core.owned_names()],
        "sections": sections(world),
    }
    if controller is not None:
        entry["run"] = {
            "route_capacities": controller.route_capacities,
            "zone_neighbors": {
                zone: list(neighbors) for zone, neighbors in controller.zone_neighbors.items()
            },
            "policies": controller._run_core.policies_document(),
        }
    return entry


# ---- The sections Python parses -----------------------------------------------------------------


def _fault(error: BaseException, *, refusal: bool = False) -> dict[str, Any]:
    """`fault_of` of the PyO3 crate, and with `refusal` its `refusal_or_fault`, which keeps an
    `Unsupported` as a refusal."""
    from sheltersim.native import Unsupported

    kind = type(error)
    if refusal and kind is Unsupported:
        return {"fault": ["Refused", str(error)]}
    known = {ValueError: "Value", TypeError: "Type", KeyError: "Key"}
    if kind in known:
        name = known[kind]
    elif kind.__name__ == "ValidationError" and kind.__module__.startswith("pydantic"):
        name = "Validation"
    else:
        name = f"{kind.__module__}.{kind.__name__}"
    return {"fault": [name, str(error)]}


def _refused(text: str) -> Exception:
    from sheltersim.native import Unsupported

    return Unsupported(text)


def _exact_str(value: Any) -> str:
    if type(value) is not str:
        raise _refused("a value is not a str")
    return value


def _exact_int(value: Any) -> int:
    if type(value) is not int:
        raise _refused("a value is not an int")
    if not -(2**63) <= value < 2**63:
        raise _refused("an int is too large")
    return value


def _attribute(value: Any, name: str) -> Any:
    """`row::attribute`: an attribute, or a refusal."""
    try:
        return getattr(value, name)
    except AttributeError:
        raise _refused("an input has no such attribute") from None


def _parsed(parse: Callable[[], Any]) -> dict[str, Any]:
    """`configs::parsed`: the value, or what the parse raised, a refusal kept as one."""
    try:
        return {"value": parse()}
    except Exception as error:
        return _fault(error, refusal=True)


def _section_of(parse: Callable[[], Any]) -> dict[str, Any]:
    """`configs::section_of` and the geometry's parses: the value, or `fault_of` what it raised."""
    try:
        return {"value": parse()}
    except Exception as error:
        return _fault(error)


def _selection(configs: Any) -> dict[str, Any]:
    from sheltersim.contracts.policies import selected_models

    try:
        selection = selected_models(configs)
    except Exception as error:
        return _fault(error)
    if type(selection) is not dict or any(
        type(key) is not str or type(value) is not str for key, value in selection.items()
    ):
        return {"fault": ["Refused", "not a selection"]}
    return {"value": [[domain, model] for domain, model in selection.items()]}


def _tick_plan(world: Any) -> dict[str, Any]:
    from sheltersim.core.tick_validation import tick_plan_fields

    def parse() -> dict[str, Any]:
        fields = tick_plan_fields(world)
        resource = fields["resource_config"]
        if resource is not None:
            resource = {
                "water_ml_per_service": _exact_int(_attribute(resource, "water_ml_per_service")),
                "food_meal_units_per_service": _exact_int(
                    _attribute(resource, "food_meal_units_per_service")
                ),
                "water_staff_skill": _exact_str(_attribute(resource, "water_staff_skill")),
                "meal_staff_skill": _exact_str(_attribute(resource, "meal_staff_skill")),
            }
        return {
            **fields,
            "declared": [list(item) for item in fields["declared"]],
            "operational_events": sorted(fields["operational_events"]),
            "infrastructure_events": sorted(fields["infrastructure_events"]),
            "resource_config": resource,
        }

    return _section_of(parse)


def facility_geometry_section(configs: Any) -> dict[str, Any]:
    """`FacilityGeometry.model_validate(model_configs["facility_geometry"])`, as JSON, with each
    approach route's `distance_m` and `travel_minutes` (properties; the second may raise)."""
    from sheltersim.contracts.extensions import FacilityGeometry

    raw = configs.get("facility_geometry")
    if raw is None:
        return {"absent": True}
    try:
        model = FacilityGeometry.model_validate(raw)
    except Exception as error:
        return _fault(error)
    value = model.model_dump(mode="json")
    for route, dumped in zip(model.arrival_approach_routes, value["arrival_approach_routes"]):
        dumped["distance_m"] = route.distance_m
        try:
            dumped["travel_minutes"] = {"value": _exact_int(route.travel_minutes)}
        except Exception as error:
            dumped["travel_minutes"] = _fault(error)
    return {"value": value}


def spatial_constraints_section(configs: Any) -> dict[str, Any]:
    """`SpatialConstraints.model_validate(model_configs.get("spatial_constraints", {}))`."""
    from sheltersim.contracts.spatial import SpatialConstraints

    raw = configs["spatial_constraints"] if "spatial_constraints" in configs else {}
    return _section_of(lambda: SpatialConstraints.model_validate(raw).model_dump(mode="json"))


def _staff_operations(configs: Any) -> dict[str, Any]:
    from sheltersim.policies.registry import create_staff_operations_formula

    def parse() -> dict[str, Any]:
        formula = create_staff_operations_formula(configs)
        selected = configs.get("policy_registry")
        selected = selected is not None and "staff_operations" in selected
        return {
            "model_id": _exact_str(_attribute(formula, "model_id")),
            "selected_source": _exact_str(_attribute(formula, "source_id")) if selected else None,
        }

    return _parsed(parse)


def _information_spread(configs: Any) -> dict[str, Any]:
    from sheltersim.domain.models import InformationSpreadConfig

    def parse() -> dict[str, Any]:
        config = InformationSpreadConfig.from_mapping(configs.get("information_spread"))
        fields = {name: getattr(config, name) for name in INFORMATION_FIELDS}
        return {**fields, "present": "information_spread" in configs}

    return _parsed(parse)


def _body_models(world: Any, selection: dict[str, Any]) -> dict[str, Any]:
    """The affect model's name and the relationship model the selection names; the core parses
    the operators from them and the configs (`BodyModels::parse`)."""
    from sheltersim.domain.body_state import canonical_body_config

    if "value" not in selection:
        text = "not a selection" if selection["fault"][0] == "Refused" else None
        return {"host_error": text or "the model selection raises"}
    models = dict(selection["value"])

    def affect() -> str:
        if "affect" in models:
            return models["affect"]
        configs = world.model_configs
        if not isinstance(configs, dict):
            raise _refused("a value is not a plain dict")
        try:
            config = canonical_body_config(configs.get("body_dynamics"))
        except Exception:
            raise _refused("the body config raises") from None
        return _exact_str(config["affect_policy"])

    try:
        name = affect()
    except Exception as error:
        return _fault(error, refusal=True)
    return {"value": {"affect": name, "relationship": models.get("relationship")}}


def _care_outcome(configs: Any) -> dict[str, Any]:
    from sheltersim.policies.registry import create_care_outcome_formula

    def parse() -> dict[str, Any]:
        try:
            formula = create_care_outcome_formula(configs)
        except Exception:
            raise _refused("the formula refuses") from None
        return {
            "model_id": _exact_str(_attribute(formula, "model_id")),
            "source_id": _exact_str(_attribute(formula, "source_id")),
            "config": json.loads(json.dumps(_attribute(formula, "config"))),
        }

    return _parsed(parse)


def _validated(configs: Any, key: str, contract: Any) -> dict[str, Any]:
    """`contract.model_validate(model_configs[key]).model_dump(mode="json")`."""
    return _parsed(lambda: contract.model_validate(configs[key]).model_dump(mode="json"))


def _proximity(configs: Any) -> dict[str, Any]:
    from sheltersim.domain.social_network import canonical_proximity_contact_config

    return _parsed(lambda: canonical_proximity_contact_config(configs.get("social_proximity")))


def _overlay_rooms(configs: Any) -> dict[str, Any]:
    config = configs.get("geometry_overlay", {})
    is_dict = isinstance(config, dict)
    schema: dict[str, Any] = {"value": False}
    if is_dict:
        try:
            schema = {"value": config.get("schema_version") in {3, 4}}
        except Exception as error:
            schema = _fault(error)
    return {"value": {"is_dict": is_dict, "room_schema": schema}}


def _extensions_enabled(configs: Any) -> dict[str, Any]:
    def parse() -> bool:
        if not isinstance(configs, dict):
            raise _refused("the configs are not a plain dict")
        if "simulator_extensions" not in configs:
            return False
        enabled = configs["simulator_extensions"].get("enabled")
        return enabled is not None and bool(enabled)

    return _parsed(parse)


def sections(world: Any) -> dict[str, Any]:
    """The sections of the world's configs that Python parses, by the name of their field of the
    core's `Configs` (`SectionId::name`)."""
    from sheltersim.domain.environment import ScenarioZoneEnvironmentConfig
    from sheltersim.domain.infrastructure import ScenarioInfrastructureSpec

    configs = world.model_configs
    selection = _selection(configs)
    return {
        "selection": selection,
        "tick_plan": _tick_plan(world),
        "facility_geometry": facility_geometry_section(configs),
        "spatial_constraints": spatial_constraints_section(configs),
        "staff_operations": _staff_operations(configs),
        "support_cooldown_minutes": _parsed(
            lambda: _exact_int(
                configs.get("staff", {}).get("support_cooldown_minutes", 30),
            )
        ),
        "information_spread": _information_spread(configs),
        "body_models": _body_models(world, selection),
        "care_outcome": _care_outcome(configs),
        "zone_environment": _validated(configs, "zone_environment", ScenarioZoneEnvironmentConfig),
        "infrastructure": _validated(configs, "infrastructure", ScenarioInfrastructureSpec),
        "proximity": _proximity(configs),
        "transit_capacities": _parsed(
            lambda: bool(configs.get("layout_routing", {}).get("max_agents_in_transit_by_edge", {}))
        ),
        "overlay_rooms": _overlay_rooms(configs),
        "extensions_enabled": _extensions_enabled(configs),
    }


# ---- What the core's minute-0 builder takes from Python (#249) ----------------------------------
#
# The core builds the entry of a Scenario's world at minute 0 itself (`native/core/src/initial/`,
# for the browser, #285): the rows, the namespaces' records of each person, staff member and zone,
# the floor plan's sections, the zone neighbors. What it does not compute (the Run's
# `policies_document`, the sections Python's contracts parse, the namespaces' configs) it reads
# from a table that Python writes (`native/core/src/initial/table.json`, by
# `tests/support/core_initial.py`), keyed by a fingerprint of the configs that leaves out the
# configs that hold the Scenario's own data. A Scenario whose fingerprint the table lacks is
# refused there.

# The configs whose contents are the Scenario's own data (its floor plan and its people): the
# fingerprint keeps only that they are there. Every other config is the same for every Scenario the
# Scenario screen makes, and so is what Python derives from them.
DATA_CONFIGS = ("facility_geometry", "population", "population_context", "spatial_constraints")
# The sections the core makes from the Scenario (the contracts' dumps of the configs as they are).
DATA_SECTIONS = ("facility_geometry", "spatial_constraints")
# The namespaces the core makes whole, from the Scenario's stock, events, staff and floor plan.
DATA_NAMESPACES = ("resource_ledger_v1", "operational_events_v1", "facility_queues_v1")
# The members of a namespace that hold a record per person, staff member or zone: the core fills
# them in; the table holds the rest of the namespace (its template), with these empty.
ENTITY_MEMBERS = {
    "agent_dynamics_v1": ("agents",),
    "body_state_v1": ("agents",),
    "information_spread_v1": ("beliefs", "query_pressure"),
    "movement_v1": ("positions", "staff_positions"),
    "population_dynamics_v1": ("initial_agent_ids", "profile_traits"),
    "population_presence_v1": ("people",),
    "psychological_distress_v1": ("agents",),
    "staff_dynamics_v1": ("staff",),
    "staff_operations_v1": ("staff", "zones"),
}


def _canonical_text(value: Any) -> str:
    return json.dumps(
        value, sort_keys=True, separators=(",", ":"), ensure_ascii=False, allow_nan=False
    )


def configs_fingerprint(configs: dict[str, Any]) -> str:
    """The sha256 of the canonical JSON of the configs, each of `DATA_CONFIGS` there as `true`."""
    import hashlib

    masked = {key: True if key in DATA_CONFIGS else value for key, value in configs.items()}
    return hashlib.sha256(_canonical_text(masked).encode("utf-8")).hexdigest()


def initial_table_entry(entry: dict[str, Any]) -> dict[str, Any]:
    """What the core's builder reads from the table for an entry `export_world` wrote of a world at
    minute 0 with its controller: the Run's policies and route capacities, the sections but
    `DATA_SECTIONS` (the tick plan without `independent_queues`, which the floor plan decides),
    and each namespace but `DATA_NAMESPACES`, its `ENTITY_MEMBERS` emptied."""
    sections = {
        name: section for name, section in entry["sections"].items() if name not in DATA_SECTIONS
    }
    plan = sections["tick_plan"]
    if "value" in plan:
        sections["tick_plan"] = {
            "value": {
                key: value for key, value in plan["value"].items() if key != "independent_queues"
            }
        }
    namespaces = {}
    for name, state in entry["world"]["model_state"].items():
        if name in DATA_NAMESPACES:
            continue
        template = dict(state)
        for member in ENTITY_MEMBERS.get(name, ()):
            template[member] = type(template[member])()
        namespaces[name] = template
    return {
        "policies": entry["run"]["policies"],
        "route_capacities": entry["run"]["route_capacities"],
        "sections": sections,
        "namespaces": namespaces,
    }
