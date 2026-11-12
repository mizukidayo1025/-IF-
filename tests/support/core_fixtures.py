"""The fixtures of the core's tests without Python: worlds the Python side built, as JSON (#225).

The tests of `native/core/tests/` (cargo test) read worlds that Python built: a Scenario run some
ticks through `TickEngine` and edited in Python. `export_world` writes what such a test needs of
one world, at any minute: the world's canonical document, the slots its core owns, what the Run
read of the controller (`RunCore.policies_document`, the route capacities, the zone neighbors),
and the sections of the configs that Python's contracts and factories parse
(`SectionId::parser() == Parser::Host` in `native/core/src/config/mod.rs`; the core parses the
others from the configs' `V` itself). `native/core/tests/support/load.rs` and `sections.rs` read it
back into a World, a Run and a `TestHost` whose sections and floor plan are this world's.

A fixture file holds named worlds, `{"worlds": {name: entry}}`; a builder may add its own keys to
an entry (the values its test expects). `FIXTURES` lists every file and the function that builds
it; `benchmarks/core_fixtures.py` writes them all again, and `integration/test_core_fixtures.py`
checks that each file is what its builder makes today.

Each section is what the PyO3 crate's `PyHost::fill_section` reads from Python
(`native/src/core/configs.rs`, `native/src/core/geometry.rs`, `rules/after_tick.rs`'s
`fill_tick_plan`), before it turns it into the core's value: `{"value": ...}`, or
`{"fault": [class, text]}` for what the parse raised (the class as `config::FaultClass`: `Value`,
`Type`, `Key`, `Validation`, `Refused`, or the module and name of another class), or
`{"absent": true}` (a facility geometry the configs don't have), or `{"host_error": text}` where
the PyO3 crate raises out of `fill_section` instead of keeping a fault. A dict loses its order
here (the file's keys are sorted), as the world's canonical document does.
"""

from __future__ import annotations

import importlib
import json
import tempfile
from collections.abc import Callable
from pathlib import Path
from typing import Any

from support.paths import CORE_TEST_DATA

# Each fixture file under native/core/tests/data/ (its stem) and the function that builds its
# document, as "module:function". A lane adds a file here and its builder in a support module.
FIXTURES: dict[str, str] = {
    "tick_core": "support.core_tick:tick_core_fixture",
    "sections": "support.core_fixtures:sections_fixture",
    "typed_rows": "support.core_typed_rows:typed_rows_document",
    "event_texts": "support.core_texts:event_texts_fixture",
    "rumor_texts": "support.core_texts:rumor_texts_fixture",
    "travel_texts": "support.core_texts:travel_texts_fixture",
}

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


# ---- The registry ------------------------------------------------------------------------------


def fixture_path(name: str) -> Path:
    """The file of a registered fixture."""
    return CORE_TEST_DATA / f"{name}.json"


def build(name: str) -> dict[str, Any]:
    """The document of a registered fixture, as its builder makes it now."""
    module, function = FIXTURES[name].split(":")
    builder: Callable[[], dict[str, Any]] = getattr(importlib.import_module(module), function)
    return builder()


def render(document: dict[str, Any]) -> str:
    """The text of a fixture file."""
    return json.dumps(document, sort_keys=True, ensure_ascii=False, indent=1) + "\n"


def write(name: str) -> Path:
    """Write a registered fixture again."""
    path = fixture_path(name)
    path.write_text(render(build(name)), encoding="utf-8")
    return path


# ---- A Run's world ------------------------------------------------------------------------------


def run_world(scenario: Any, ticks: int = 0) -> tuple[Any, Any, Any]:
    """The world, its controller and its engine as `run_scenario` sets them up (offline), after
    `ticks` ticks through `TickEngine`."""
    from sheltersim.core.engine import TickEngine, TraceLevel
    from sheltersim.core.world import WorldState
    from sheltersim.domain.simulator_state import initialize_simulator_extensions
    from sheltersim.experiments.runner import OfflineSimulationController
    from sheltersim.inspection.traces import RunArtifactWriter

    world = WorldState.from_scenario(scenario)
    initialize_simulator_extensions(world)
    artifacts = RunArtifactWriter(
        scenario, tempfile.mkdtemp(prefix="core-fixture-"), trace_level=TraceLevel.OFF
    )
    artifacts.write_initial(world, execution_mode="offline")
    controller = OfflineSimulationController(world, artifacts)
    engine = TickEngine(world, phase_handler=controller)
    for _ in range(ticks):
        engine.step()
    return world, controller, engine


def export_world(
    world: Any, controller: Any | None = None, *, validate: bool = True
) -> dict[str, Any]:
    """What a core test reads of one world as it is now: its canonical document, the slots its
    core owns (`[name, is_field]`), what the Run read of the controller (without a controller,
    nothing), and the Python-parsed sections of its configs (`sections`). Without `validate`, a
    world the test edited into a state `WorldState.validate` turns down is written as it is (the
    same text as `canonical_json`, which validates first)."""
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
    import sheltersim_native

    kind = type(error)
    if refusal and kind is sheltersim_native.Unsupported:
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
    import sheltersim_native

    return sheltersim_native.Unsupported(text)


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


def _facility_geometry(configs: Any) -> dict[str, Any]:
    """`FacilityGeometry.model_validate(model_configs["facility_geometry"])`, as JSON, with each
    approach route's `travel_minutes` (a property, which may raise)."""
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
        try:
            dumped["travel_minutes"] = {"value": _exact_int(route.travel_minutes)}
        except Exception as error:
            dumped["travel_minutes"] = _fault(error)
    return {"value": value}


def _spatial_constraints(configs: Any) -> dict[str, Any]:
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
        "facility_geometry": _facility_geometry(configs),
        "spatial_constraints": _spatial_constraints(configs),
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


# ---- The fixture of the sections and the floor plan ---------------------------------------------

# The generated Scenario of the sections' fixture: the UI's, with three people.
SECTIONS_TICKS = 10


def sections_world() -> tuple[Any, Any]:
    """A generated Scenario's world (facility geometry, spatial constraints, every extension) after
    `SECTIONS_TICKS` ticks, with a facility moved and a partition set in its overlay by hand."""
    from sheltersim.experiments.initial_scenario import (
        InitialScenarioBuilder,
        InitialScenarioRequest,
    )

    request = InitialScenarioRequest(
        municipality="合成市", disaster="地震", evacuees=3, duration_days=0.25, seed=2026
    )
    world, controller, _ = run_world(
        InitialScenarioBuilder().build(request).scenario, SECTIONS_TICKS
    )
    geometry = world.model_configs["facility_geometry"]
    moved = geometry["service_points"][0]["facility_id"]
    point = geometry["facility_points"][moved]
    overlay = world.model_state["geometry_overlay_v1"]
    overlay["facility_points"][moved] = {
        "point_m": {"x": point["x"] + 0.25, "y": point["y"] - 0.25},
        "minute": SECTIONS_TICKS,
        "source_ids": ["ASM-TEST"],
        "event_id": "fixture-move",
    }
    zone = geometry["zones"][0]
    x = min(corner["x"] for corner in zone["polygon"]) + 0.5
    y = min(corner["y"] for corner in zone["polygon"]) + 0.5
    overlay["partitions"]["fixture-partition"] = {
        "zone_id": zone["zone_id"],
        "polygon_m": [{"x": x, "y": y}, {"x": x + 0.5, "y": y}, {"x": x + 0.5, "y": y + 0.5}],
        "event_id": "fixture-partition",
        "source_ids": ["ASM-TEST"],
        "minute": SECTIONS_TICKS,
        "end_minute": SECTIONS_TICKS + 60,
    }
    return world, controller


def _read_section(world: Any, name: str) -> dict[str, Any]:
    """`config_section` of the native path: what its read site gives, or the class and text of
    what it raises."""
    import sheltersim_native

    try:
        return {"value": sheltersim_native.config_section(world, name)}
    except Exception as error:
        return {"raises": [type(error).__name__, str(error)]}


def sections_fixture() -> dict[str, Any]:
    """`sections.json`: the world of `sections_world`, and what the native path reads of it: the
    floor plan (`geo_summary_of`), the approach routes, whether queues are independent, and the
    sections' values as their read sites read them (`config_section`)."""
    from sheltersim import native

    world, controller = sections_world()
    geometry = native.rules("movement_geometry")
    summary = geometry.geo_summary_of(world)
    names = (
        "staff_operations",
        "support_cooldown_minutes",
        "information_spread",
        "body_models",
        "care_outcome",
        "zone_environment",
        "infrastructure",
        "proximity",
        "transit_capacities",
        "overlay_rooms",
        "extensions_enabled",
    )
    entry = export_world(world, controller)
    entry["expected"] = json.loads(
        json.dumps(
            {
                "floor_plan": summary,
                "approach_routes": geometry.approach_routes_of(world),
                "independent_queues": geometry.independent_queues_of(world),
                "config_sections": {name: _read_section(world, name) for name in names},
            }
        )
    )
    return {"worlds": {"generated_moved": entry}}
