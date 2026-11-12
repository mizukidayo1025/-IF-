"""The worlds of the tests of the ledger's operations, the proximity sampling, the dispatch phase
and the proposals, for pytest and for the core's tests (Issue #225, lane M10b).

Each builder writes one file under `native/core/tests/data/` (`core_fixtures.FIXTURES`), built as
the pytest file built its world, with the inputs that file drew with `random.Random`:

- `resource_ledger.json` (`native/core/tests/resource_ledger_ops.rs`): the world of
  `integration/test_resource_ledger_row.py` (`ledger_world`) and the operations of its random
  sequences (`ledger_sequences`).
"""

from __future__ import annotations

import json
import random
from typing import Any

from support.core_fixtures import export_world

# ---- The resource ledger ------------------------------------------------------------------------

LEDGER_SOURCE = "ASM-TEST-RESOURCE-OPERATION"
# The seed and the size of the random sequences of operations.
LEDGER_SEED = 20261101
LEDGER_WORLDS = 25
LEDGER_MINUTES = range(1, 60)


def ledger_world() -> Any:
    """The small replay Scenario with a stock and no events, at minute 0."""
    from sheltersim.core.world import WorldState
    from sheltersim.experiments.scenario import ScenarioSpec
    from support.paths import SMALL_REPLAY

    document = json.loads(SMALL_REPLAY.read_text(encoding="utf-8"))
    document["schema_version"] = 3
    document["duration_minutes"] = 300
    document["scheduled_events"] = []
    document["initial_stock"] = {
        "schema_version": 1,
        "warehouse_id": "warehouse-main",
        "zone_id": "sleeping",
        "water_ml": 1_000,
        "food_meal_units": 4,
        "source_ids": ["ASM-TEST-INITIAL-STOCK"],
    }
    return WorldState.from_scenario(ScenarioSpec.model_validate(document))


def ledger_sequences(people: list[str]) -> list[list[str]]:
    """The random operations of each world, one per minute from 1: `"operation source resource
    quantity agent reservation"`, with `-` for what the payload leaves out. The draws are in the
    order the pytest file drew them; each world's events are `op-1`, `op-2`... in order."""
    randomizer = random.Random(LEDGER_SEED)
    reservations = [f"r-{index}" for index in range(12)]
    out = []
    for _ in range(LEDGER_WORLDS):
        operations = []
        for _minute in LEDGER_MINUTES:
            operation = randomizer.choice(["delivery", "loss", "reserve", "release", "consume"])
            resource = randomizer.choice(["water_ml", "food_meal_units"])
            quantity = "-"
            if operation != "release":
                quantity = str(randomizer.choice([1, 1, 2, 3, 50, 2000]))
            agent = reservation = "-"
            if operation in {"reserve", "release", "consume"}:
                agent = randomizer.choice(people)
                reservation = randomizer.choice(reservations)
            source = randomizer.choice([LEDGER_SOURCE, "ASM-SECOND", "ASM-THIRD"])
            operations.append(f"{operation} {source} {resource} {quantity} {agent} {reservation}")
        out.append(operations)
    return out


def resource_ledger_fixture() -> dict[str, Any]:
    world = ledger_world()
    entry = export_world(world)
    entry["sequences"] = ledger_sequences(sorted(world.agents)[:3])
    return {"worlds": {"ledger": entry}}


# ---- The proximity sampling ---------------------------------------------------------------------

PROXIMITY = "distance-sampled-proximity-v1"
# The ticks the world of the step's test has run.
PROXIMITY_TICKS = 5
# The seed of the positions of `test_the_pairs_of_a_sample_are_the_python_rules_from_the_row`, and
# the limits of its samples, 20 each.
PROXIMITY_SEED = 7
PROXIMITY_LIMITS = (1, 5, 10_000)


def proximity_world(scenario: Any) -> Any:
    """The world the proximity tests start from: the Scenario at minute 0."""
    from sheltersim.core.world import WorldState

    return WorldState.from_scenario(scenario)


def proximity_draws(world: Any) -> list[str]:
    """The positions of each sample of the pairs' test, as it drew them for every placed person, in
    the order of the positions: `"k,y,z"` for each, a space between, where the point is
    `(k * radius / 4, y)` and the zone is the `z`-th of the world's zones in ID order."""
    rng = random.Random(PROXIMITY_SEED)
    positions = world.model_state["movement_v1"]["positions"]
    zones = sorted(world.zones)
    out = []
    for _limit in PROXIMITY_LIMITS:
        for _ in range(20):
            drawn = []
            for record in positions.values():
                if record["point"] is None:
                    continue
                k = rng.randint(-8, 8)
                y = rng.uniform(-3.0, 3.0)
                zone = zones[:2].index(rng.choice(zones[:2]))
                drawn.append(f"{k},{y!r},{zone}")
            out.append(" ".join(drawn))
    return out


def _orders(world: Any) -> dict[str, Any]:
    """The orders of Python's dicts the canonical document sorts: the people's positions and the ID
    counters."""
    return {
        "positions": list(world.model_state["movement_v1"]["positions"]),
        "id_counters": list(world.id_counters),
    }


def proximity_fixture() -> dict[str, Any]:
    """The world at minute 0 (`start`), and `stepped`: what the proximity step reads of the world
    after `PROXIMITY_TICKS` ticks (the positions, the ID counters and the minute; the sampler reads
    no other part that changed), written as the changes from `start`."""
    from support.core_fixtures_queues import generated, run_minutes
    from support.core_texts import changes

    scenario = generated(duration_days=1 / 24, social_proximity_policy_id=PROXIMITY)
    world = proximity_world(scenario)
    start = json.loads(json.dumps(export_world(world)))
    stepped = run_minutes(scenario, PROXIMITY_TICKS)[0]
    document = json.loads(stepped.canonical_json())
    read = [
        ["model_state", "movement_v1", "positions"],
        ["id_counters"],
        ["minute_index"],
    ]
    made: list[Any] = []
    for path in read:
        base, value = start["world"], document
        for key in path:
            base, value = base[key], value[key]
        made += changes(base, value, ["world", *path])
    start["orders"] = _orders(world)
    start["draws"] = proximity_draws(world)
    return {
        "worlds": {
            "start": start,
            "stepped": {"base": "start", "changes": made, "orders": _orders(stepped)},
        }
    }
