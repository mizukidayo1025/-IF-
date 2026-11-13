"""The table and the worlds of the core's minute-0 builder (Issue #249).

The core builds the entry of a Scenario's world at minute 0 (`native/core/src/initial/`) from the
Scenario's canonical JSON, the bytes `export_world` writes for the browser
(`scripts/build_browser_sample.py`). What it does not compute it reads from a table Python writes,
`native/core/src/initial/table.json` (`initial_table`): for each fingerprint of the configs
(`world_export.configs_fingerprint`), the Run's policies, the sections Python's contracts parse
and the namespaces' templates (`world_export.initial_table_entry`). The table is made from the
Scenarios of `TABLE_REQUESTS`, the requests of the Scenario screen (the structured form and the
example text's), with edge values; two Scenarios of one fingerprint must give the same entry, so a
config the fingerprint leaves out that changes what the table holds fails here.

`initial_worlds.json` (`initial_worlds_fixture`) holds generated Scenarios' canonical JSON with the
sha256 of the entry `export_world` writes of their worlds at minute 0, and the digests of the
worlds after `ticks` ticks through `TickEngine`: the core's tests (`native/core/tests/initial.rs`)
and the WASM build's (`native/wasm/tests/node_initial.mjs`) build the entry in the core and run it.
"""

from __future__ import annotations

import hashlib
import json
from typing import Any

from sheltersim.core.world_export import configs_fingerprint, export_world, initial_table_entry
from support.core_fixtures import run_world

TABLE_SCHEMA_VERSION = 1
# The requests whose Scenarios the table is made of: the screen's form with edge values and the
# example text's request (墨田区, earthquake, 100 people, 7 days). The screen sends no policy IDs,
# plan, selected shelter or overrides.
BASE_REQUEST: dict[str, Any] = {
    "municipality": "墨田区",
    "disaster": "地震",
    "evacuees": 100,
    "duration_days": 7.0,
    "seed": 2026,
}
TABLE_REQUESTS: dict[str, dict[str, Any]] = {
    "example": {},
    "three": {"evacuees": 3, "municipality": "合成市"},
    "twenty": {"evacuees": 20},
    "no_staff": {"staff_count": 0},
    "staff_12": {"staff_count": 12},
    "no_accessible_toilet": {"accessible_toilet_count": 0},
    "twenty_accessible_toilets": {"accessible_toilet_count": 20},
    "half_day": {"duration_days": 0.5},
    "two_and_a_half_days": {"duration_days": 2.5, "evacuees": 30},
    "small_area": {"gross_area_m2": 400.0, "evacuees": 20},
    "large_area": {"gross_area_m2": 2400.0, "evacuees": 250},
    "conditions": {
        "facility_conditions": ("ペット同伴あり", "授乳室あり", "個室あり", "車いす利用者あり")
    },
    "flood": {"disaster": "水害", "seed": 7},
    "stock_entered": {
        "operational_conditions": {
            "initial_resources": {"status": "entered", "water_ml": 300000, "food_meal_units": 250}
        }
    },
    "stock_incomplete": {
        "operational_conditions": {"initial_resources": {"status": "incomplete", "water_ml": 1000}}
    },
}
# The worlds of `initial_worlds.json`: the request (over `BASE_REQUEST`) and the ticks to run.
WORLDS: dict[str, tuple[dict[str, Any], int]] = {
    "twenty": ({"evacuees": 20, "duration_days": 1.0}, 360),
    "hundred": ({"duration_days": 1.0}, 60),
}


def scenario_of(request: dict[str, Any]) -> Any:
    """The Scenario the builder makes of `BASE_REQUEST` updated with `request`."""
    from sheltersim.experiments.initial_scenario import (
        InitialScenarioBuilder,
        InitialScenarioRequest,
    )

    built = InitialScenarioBuilder().build(InitialScenarioRequest(**{**BASE_REQUEST, **request}))
    return built.scenario


def entry_text(entry: dict[str, Any]) -> bytes:
    """The bytes of an entry as the browser's sample writes them (`export.json`)."""
    return json.dumps(entry, sort_keys=True, ensure_ascii=False, separators=(",", ":")).encode()


def minute_zero_entry(scenario: Any) -> dict[str, Any]:
    """`export_world` of the Scenario's world at minute 0, set up for a Run."""
    world, controller, _ = run_world(scenario)
    return export_world(world, controller)


def initial_table() -> dict[str, Any]:
    """`native/core/src/initial/table.json`: the table entry of each fingerprint of the Scenarios
    of `TABLE_REQUESTS`. Two Scenarios of one fingerprint that give different entries raise."""
    entries: dict[str, Any] = {}
    made_by: dict[str, str] = {}
    for name, request in TABLE_REQUESTS.items():
        scenario = scenario_of(request)
        fingerprint = configs_fingerprint(json.loads(scenario.canonical_json())["model_configs"])
        made = json.loads(json.dumps(initial_table_entry(minute_zero_entry(scenario))))
        if fingerprint in entries and entries[fingerprint] != made:
            raise ValueError(
                f"{name} and {made_by[fingerprint]} have one fingerprint but different entries"
            )
        entries.setdefault(fingerprint, made)
        made_by.setdefault(fingerprint, name)
    return {"schema_version": TABLE_SCHEMA_VERSION, "entries": entries}


def _sha(raw: bytes) -> str:
    return hashlib.sha256(raw).hexdigest()


def initial_worlds_fixture() -> dict[str, Any]:
    """`initial_worlds.json`: for each of `WORLDS`, the Scenario's canonical JSON (`scenario`, a
    str), the sha256 and length of the entry of its world at minute 0, and the digests of its world
    after the ticks."""
    from support.core_tick import metrics_digest, world_digests

    worlds = {}
    for name, (request, ticks) in WORLDS.items():
        scenario = scenario_of(request)
        text = scenario.canonical_json()
        world, controller, _engine = run_world(scenario)
        entry = entry_text(export_world(world, controller))
        initial = world_digests(world)
        del world, controller, _engine
        world, controller, _engine = run_world(scenario, ticks)
        snapshots = controller.artifacts.metrics.snapshots
        worlds[name] = {
            "scenario": text,
            "expected": {
                "entry_sha256": _sha(entry),
                "entry_bytes": len(entry),
                "ticks": ticks,
                "initial": initial,
                "world": world_digests(world),
                "metrics": metrics_digest(snapshots),
                "metric_count": len(snapshots),
            },
        }
    return {"initial_worlds": worlds}
