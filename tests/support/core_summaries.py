"""The summaries of the core's summary test (#302): what the intervention's reading is given.

`native/core/tests/summaries.rs` (cargo test) and `native/wasm/tests/node_summaries.mjs` (Node)
run worlds in the core from minute 0 and check at each minute here that the core's summary
(`sheltersim_core::summary`) has the sha256 of the canonical JSON of the summary Python makes of
the same world, `scenario_summary_from_world(world).model_dump(mode="json")`, as the server's
`intervention-preview` reads it. `summaries_fixture` builds `summaries.json`;
`benchmarks/core_fixtures.py summaries` writes it.

- `small_replay` (`tick_core.json`) has four zones and no namespace but the core's.
- `generated` (`tick_core.json`) is a generated Scenario with every namespace, a resource ledger
  and the zone environment's settings.
- `operations` is a generated Scenario of `OPERATIONS_EVACUEES` people with the gate's outages,
  absence and shipment (`world_order.OPERATIONS`), and people who come, go and step out
  (`MOVES`, `EXIT_MINUTE`): at its minutes a staff member is away, a shipment is pending and then
  delivered, and the lifecycle keeps a person who is out of the world.
- `one_zone` is `small_replay` with everything in one zone: the zone is the default zone.

The builder checks that each of these is in the summaries it records. `FULL` names the world whose
first summary the fixture also holds whole, for a reader to see the form.
"""

from __future__ import annotations

import hashlib
from typing import Any

from sheltersim.core.world_export import export_world
from support.core_fixtures import run_world
from support.core_tick import TICKS, core_tick_run, core_tick_scenario, generated_run

OPERATIONS_EVACUEES = 30
# The population's moves of `operations`: (op, minute, parameters).
MOVES: tuple[tuple[str, int, dict[str, Any]], ...] = (
    ("add_evacuees", 20, {"count": 3, "household_size": 1, "mobility": "independent"}),
    ("depart_evacuees", 40, {"count": 4, "include_initial": True}),
)
MOVE_ZONE = "sleeping"
# The minute the first person goes out for a while (`population_presence`, temporary_exit): the
# lifecycle keeps them while they are out of the world.
EXIT_MINUTE = 30
ONE_ZONE = "sleeping"

# The minutes of each world's summaries.
MINUTES: dict[str, tuple[int, ...]] = {
    "small_replay": (0, 600, TICKS),
    "generated": (0, 152, 360),
    "operations": (0, 10, 41, 151, 359),
    "one_zone": (0, 30),
}
FULL = "operations"


def operations_run() -> tuple[Any, Any, Any]:
    """The world of `operations` at minute 0, its controller and its engine."""
    from sheltersim.events.effects import (
        EffectProgram,
        SupportMode,
        TargetSelector,
        TimedAction,
        schedule_effect_program,
    )
    from sheltersim.experiments.initial_scenario import (
        InitialScenarioBuilder,
        InitialScenarioRequest,
    )
    from support.world_order import OPERATIONS

    request = InitialScenarioRequest(
        municipality="合成市",
        disaster="地震",
        evacuees=OPERATIONS_EVACUEES,
        duration_days=0.25,
        seed=2026,
        operational_conditions=OPERATIONS,
    )
    world, controller, engine = run_world(InitialScenarioBuilder().build(request).scenario)
    effects = [
        TimedAction(
            effect_id=f"summary-{op}",
            op=op,
            target_selector=TargetSelector(kind="zone", target_id=MOVE_ZONE),
            start_minute=minute,
            parameters=parameters,
            units="evacuee",
            source_id="ASM-TEST-SUMMARY-POPULATION",
            policy_dependencies=["population_dynamics_v1"],
            reversible=False,
        )
        for op, minute, parameters in MOVES
    ]
    effects.append(
        TimedAction(
            effect_id="summary-exit",
            op="population_presence",
            target_selector=TargetSelector(kind="person", target_id=sorted(world.agents)[0]),
            start_minute=EXIT_MINUTE,
            parameters={"action": "temporary_exit"},
            units="population_transition",
            source_id="ASM-TEST-SUMMARY-POPULATION",
            policy_dependencies=["population_lifecycle_v1"],
            reversible=False,
        )
    )
    schedule_effect_program(
        world,
        EffectProgram(
            program_id="summary-population",
            narrative="People come and go.",
            source_text="People come and go.",
            simulation_start_minute=0,
            effects=effects,
            assumptions=[],
            support_mode=SupportMode.NATIVE,
            compiler_provenance={"compiler_kind": "offline_rule", "compiler_version": "test-v1"},
        ),
    )
    return world, controller, engine


def one_zone_run() -> tuple[Any, Any, Any]:
    """`small_replay` with its zones made one (`ONE_ZONE`): no routes, everything there."""
    from sheltersim.experiments.scenario import ScenarioSpec

    document = core_tick_scenario().model_dump(mode="json")
    document["zones"] = [zone for zone in document["zones"] if zone["id"] == ONE_ZONE]
    document["zone_edges"] = []
    for key in ("facilities", "staff", "profiles"):
        for item in document.get(key, []):
            if "zone_id" in item:
                item["zone_id"] = ONE_ZONE
    return run_world(ScenarioSpec.model_validate(document))


def summary_bytes(world: Any) -> bytes:
    """The canonical JSON of the summary the server's `intervention-preview` makes."""
    from sheltersim.llm.natural_language import scenario_summary_from_world
    from sheltersim.storage.projects import canonical

    return canonical(scenario_summary_from_world(world).model_dump(mode="json")).encode()


def summaries(run: tuple[Any, Any, Any], minutes: tuple[int, ...]) -> tuple[list[Any], list[Any]]:
    """The world run on to each minute: the minute and the sha256 of its summary there, and the
    summaries."""
    import json

    world, _, engine = run
    recorded, made = [], []
    for minute in minutes:
        while world.minute_index < minute:
            engine.step()
        raw = summary_bytes(world)
        recorded.append([minute, hashlib.sha256(raw).hexdigest()])
        made.append(json.loads(raw))
    return recorded, made


def _shown(made: list[Any]) -> dict[str, bool]:
    """What the summaries show of the cases the fixture is for."""
    return {
        "away": any(item["available_staff_ids"] != item["staff_ids"] for item in made),
        "shipment": any(item["pending_shipments"] for item in made),
        "delivered": any(not item["pending_shipments"] for item in made),
        "left": any(len(item["person_ids"]) > item["agent_count"] for item in made),
        "occupied": any(
            facility["active_occupancy"] for item in made for facility in item["facilities"]
        ),
        "water_station": any(zone["has_water_station"] for item in made for zone in item["zones"]),
        "environment": any(item["zone_environment_settings"] for item in made),
        "resources": any(item["resource_ids"] for item in made),
        "default_zone": any(item["default_zone_id"] for item in made),
        "several_zones": any(len(item["zones"]) > 1 for item in made),
    }


def summaries_fixture() -> dict[str, Any]:
    """`summaries.json`: the worlds `operations` and `one_zone` at minute 0, and each world's
    summaries at its minutes."""
    runs = {
        "small_replay": core_tick_run(),
        "generated": generated_run(),
        "operations": operations_run(),
        "one_zone": one_zone_run(),
    }
    worlds = {name: export_world(*runs[name][:2]) for name in ("operations", "one_zone")}
    expected, shown, full = {}, {}, None
    for name, run in runs.items():
        expected[name], made = summaries(run, MINUTES[name])
        shown[name] = _shown(made)
        if name == FULL:
            full = made[0]
    for case in ("away", "shipment", "delivered", "left", "occupied", "water_station"):
        assert shown["operations"][case], (case, shown)
    for case in ("environment", "resources", "several_zones"):
        assert shown["generated"][case], (case, shown)
    assert shown["one_zone"]["default_zone"], shown
    assert not shown["small_replay"]["default_zone"], shown
    return {
        "worlds": worlds,
        "files": {
            "small_replay": "tick_core",
            "generated": "tick_core",
            "operations": "summaries",
            "one_zone": "summaries",
        },
        "expected": expected,
        "full": {"world": FULL, "minute": MINUTES[FULL][0], "summary": full},
    }
