"""The fixtures of the core's tests without Python: worlds the Python side built, as JSON (#225).

The tests of `native/core/tests/` (cargo test) read worlds that Python built: a Scenario run some
ticks through `TickEngine` and edited in Python. `sheltersim.core.world_export.export_world`
writes what such a test needs of one world, at any minute (the world, its slots, what the Run read
of the controller and the Python-parsed sections of its configs), and the crate's loader
(`sheltersim_core::load`, through `native/core/tests/support/load.rs` and `sections.rs`) reads it
back into a World, a Run and a `TestHost` whose sections and floor plan are this world's.

A fixture file holds named worlds, `{"worlds": {name: entry}}`; a builder may add its own keys to
an entry (the values its test expects). `FIXTURES` lists every file and the function that builds
it; `benchmarks/core_fixtures.py` writes them all again, and `integration/test_core_fixtures.py`
checks that each file is what its builder makes today.
"""

from __future__ import annotations

import importlib
import json
import tempfile
from collections.abc import Callable
from pathlib import Path
from typing import Any

from sheltersim.core.world_export import export_world
from support.paths import CORE_TEST_DATA, INITIAL_TABLE

# Each fixture file under native/core/tests/data/ (its stem) and the function that builds its
# document, as "module:function". A lane adds a file here and its builder in a support module.
FIXTURES: dict[str, str] = {
    "tick_core": "support.core_tick:tick_core_fixture",
    "sections": "support.core_fixtures:sections_fixture",
    "typed_rows": "support.core_typed_rows:typed_rows_document",
    "queue_rules": "support.core_fixtures_queues:queue_rules_fixture",
    "overlay_rules": "support.core_fixtures_overlay:overlay_rules_fixture",
    "overlay_queues": "support.core_fixtures_overlay_queues:overlay_queues_fixture",
    "replan_journeys": "support.core_fixtures_movement:replan_journeys_fixture",
    "movement_walk_order": "support.core_fixtures_movement:movement_walk_order_fixture",
    "movement_rules": "support.core_fixtures_movement:movement_rules_fixture",
    "event_texts": "support.core_texts:event_texts_fixture",
    "rumor_texts": "support.core_texts:rumor_texts_fixture",
    "travel_texts": "support.core_texts:travel_texts_fixture",
    "resource_service_texts": "support.core_fixtures_texts:resource_service_texts_fixture",
    "generated_texts": "support.core_fixtures_texts:generated_texts_fixture",
    "handoff_texts": "support.core_fixtures_texts:handoff_texts_fixture",
    "service_rules": "support.core_fixtures_service:service_rules_fixture",
    "staff_assignment": "support.core_fixtures_service:staff_assignment_fixture",
    "people_rows": "support.core_people_rows:people_rows_fixture",
    "handler_rules": "support.core_fixtures_handlers:handler_rules_fixture",
    "staff_rules": "support.core_fixtures_staff:staff_rules_fixture",
    "population_rules": "support.core_fixtures_population:population_rules_fixture",
    "model_steps": "support.core_model_steps:model_steps_fixture",
    "resource_ledger": "support.core_fixtures_reads:resource_ledger_fixture",
    "proximity": "support.core_fixtures_reads:proximity_fixture",
    "dispatch_rows": "support.core_fixtures_reads:dispatch_rows_fixture",
    "behavior_rows": "support.core_fixtures_reads:behavior_rows_fixture",
    "staff_dynamics": "support.core_fixtures_staff_work:staff_dynamics_fixture",
    "staff_order_texts": "support.core_fixtures_staff_work:staff_order_texts_fixture",
    "frames": "support.core_frames:frames_fixture",
    "api_answers": "support.core_api_answers:api_answers_fixture",
    "scenario_facility": "support.core_scenario_facility:scenario_facility_fixture",
    "offline_interpreter": "support.core_interpret:offline_interpreter_fixture",
    "interventions": "support.core_interventions:interventions_fixture",
    "scenario_request": "support.core_scenario_request:scenario_request_fixture",
    "scenario_population": "support.core_scenario_population:scenario_population_fixture",
    "scenario_conditions": "support.core_scenario_conditions:scenario_conditions_fixture",
    "summaries": "support.core_summaries:summaries_fixture",
    "research_cases": "support.core_research:research_cases_fixture",
    "programs": "support.core_programs:programs_fixture",
    "initial_worlds": "support.core_initial:initial_worlds_fixture",
    # Not a test's data: the table the core's minute-0 builder reads (#249), at its own path.
    "initial_table": "support.core_initial:initial_table",
    "scenario_builder": "support.core_scenario_builder:scenario_builder_fixture",
    # The whole world's validation (#250): the valid worlds, and a file of cases per area.
    "validate_worlds": "support.core_validate:validate_worlds_fixture",
    "validate_entry": "support.core_validate:validate_entry_fixture",
    # The input contracts (#346).
    "contracts_effects": "support.core_contracts:contracts_effects_fixture",
    "contracts_overlay": "support.core_contracts:contracts_overlay_fixture",
    "contracts_geometry": "support.core_contracts:contracts_geometry_fixture",
    "contracts_population": "support.core_contracts:contracts_population_fixture",
}
# The fixtures whose file is not under native/core/tests/data/.
PATHS: dict[str, Path] = {"initial_table": INITIAL_TABLE}
# The fixtures the core's build embeds (`include_str!`), written without spaces.
COMPACT = frozenset({"initial_table"})

# ---- The registry ------------------------------------------------------------------------------


def fixture_path(name: str) -> Path:
    """The file of a registered fixture."""
    return PATHS.get(name, CORE_TEST_DATA / f"{name}.json")


def build(name: str) -> dict[str, Any]:
    """The document of a registered fixture, as its builder makes it now."""
    module, function = FIXTURES[name].split(":")
    builder: Callable[[], dict[str, Any]] = getattr(importlib.import_module(module), function)
    return builder()


def render(document: dict[str, Any], name: str | None = None) -> str:
    """The text of a fixture file: indented, but the files the core embeds (`COMPACT`)."""
    if name in COMPACT:
        return json.dumps(document, sort_keys=True, ensure_ascii=False, separators=(",", ":"))
    return json.dumps(document, sort_keys=True, ensure_ascii=False, indent=1) + "\n"


def write(name: str) -> Path:
    """Write a registered fixture again."""
    path = fixture_path(name)
    path.write_text(render(build(name), name), encoding="utf-8")
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
