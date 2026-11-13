"""`sheltersim.core.world_export`: one world as the JSON the core's loader reads (#244).

The core's tests check what the loader builds of an export (`native/core/tests/`, with the files
`integration/test_core_fixtures.py` keeps current); here, the parts of an export, that exporting
changes nothing of the world, and that the core's own builder of the world at minute 0
(`sheltersim_core::initial`, #249) writes the bytes Python exports for the screen's Scenarios.
"""

from __future__ import annotations

import json
from collections.abc import Callable
from typing import Any

import pytest
from support.core_initial import TABLE_REQUESTS, entry_text, minute_zero_entry, scenario_of

from sheltersim.core.world import WorldState
from sheltersim.core.world_export import configs_fingerprint, export_world

# The sections the host parses (`SectionId::parser() == Parser::Host`), by their `Configs` field.
HOST_SECTIONS = {
    "selection",
    "tick_plan",
    "facility_geometry",
    "spatial_constraints",
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
}


def test_export_has_the_world_its_slots_and_the_host_sections(
    small_replay_scenario: Callable[..., Any],
) -> None:
    world = WorldState.from_scenario(small_replay_scenario())
    before = world.canonical_json()

    entry = export_world(world)

    assert entry["world"] == json.loads(before)
    assert world.canonical_json() == before
    assert "run" not in entry
    names = [name for name, _ in entry["slots"]]
    assert len(names) == len(set(names))
    assert {"resources", "metric_state", "model_configs", "id_counters"} <= set(names)
    assert set(entry["sections"]) == HOST_SECTIONS
    # The small replay has no facility geometry.
    assert entry["sections"]["facility_geometry"] == {"absent": True}
    json.dumps(entry, allow_nan=False)


# ---- The core's minute-0 builder (#249) ----------------------------------------------------------


@pytest.mark.parametrize("name", sorted(TABLE_REQUESTS))
def test_the_core_builds_the_minute_zero_entry_python_exports(name: str) -> None:
    """`sheltersim_core::initial::entry` of a generated Scenario's canonical JSON is the bytes of
    `export_world` of its world at minute 0, set up for a Run (the screen's requests, #249)."""
    import sheltersim_native

    scenario = scenario_of(TABLE_REQUESTS[name])
    made = sheltersim_native.initial_entry(scenario.canonical_json().encode())

    assert made == entry_text(minute_zero_entry(scenario))


def test_the_core_refuses_configs_its_table_lacks_and_what_is_not_a_scenario() -> None:
    """A fingerprint the table lacks is Unsupported; bytes that are no Scenario are a ValueError."""
    import sheltersim_native

    from sheltersim.native import Unsupported

    document = json.loads(scenario_of({"evacuees": 3}).canonical_json())
    document["model_configs"]["behavior"]["temperature"] = 0.3
    with pytest.raises(Unsupported, match="fingerprint"):
        sheltersim_native.initial_entry(json.dumps(document).encode())
    for raw in (b"", b"[]", b"{}", b'{"seed": "1"}', b"\xff"):
        with pytest.raises(ValueError):
            sheltersim_native.initial_entry(raw)


def test_the_fingerprint_keeps_only_that_the_data_configs_are_there() -> None:
    first = json.loads(scenario_of({"evacuees": 3}).canonical_json())["model_configs"]
    second = json.loads(scenario_of({"evacuees": 20, "gross_area_m2": 900.0}).canonical_json())
    second_configs = second["model_configs"]

    assert first["facility_geometry"] != second_configs["facility_geometry"]
    assert configs_fingerprint(first) == configs_fingerprint(second_configs)
    del second_configs["population_context"]
    assert configs_fingerprint(first) != configs_fingerprint(second_configs)
