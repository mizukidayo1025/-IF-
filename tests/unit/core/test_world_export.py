"""`sheltersim.core.world_export`: one world as the JSON the core's loader reads (#244).

The core's tests check what the loader builds of an export (`native/core/tests/`, with the files
`integration/test_core_fixtures.py` keeps current); here, the parts of an export and that
exporting changes nothing of the world.
"""

from __future__ import annotations

import json
from collections.abc import Callable
from typing import Any

from sheltersim.core.world import WorldState
from sheltersim.core.world_export import export_world

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
