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


# ---- The dispatch phase -------------------------------------------------------------------------


class Sink:
    """What the controller writes to, which the dispatch tests don't read."""

    def __getattr__(self, name: str) -> Any:
        return lambda *arguments, **options: None


def dispatch_controller() -> Any:
    """The controller of the dispatch tests: the Scenario of 100 people for 600 minutes, at 0."""
    from sheltersim.core.world import WorldState
    from sheltersim.experiments.runner import OfflineSimulationController
    from sheltersim.experiments.scenario import ScenarioSpec
    from support.paths import SCENARIO_100_AGENTS

    world = WorldState.from_scenario(ScenarioSpec.load(SCENARIO_100_AGENTS))
    world.duration_minutes = 600
    return OfflineSimulationController(world, Sink())


def probe(name: str, sequence: int, event_type: str = "probe", **options: Any) -> Any:
    """An event of a test, of a type no handler of the table has unless given."""
    from sheltersim.events.models import EventKind, ScheduledEvent

    return ScheduledEvent(
        event_id=name,
        kind=options.pop("kind", EventKind.ENDOGENOUS),
        due_minute=options.pop("due", 0),
        priority=options.pop("priority", 0),
        sequence=sequence,
        event_type=event_type,
        payload=options.pop("payload", {}),
        source_id=options.pop("source_id", "probe-1"),
        causation_id=None,
        provenance=options.pop("provenance", {"source_type": "assumption"}),
    )


def social_action(controller: Any, **change: Any) -> Any:
    """A social action of A001 and A002, who are put in the same zone; `change` replaces some of
    its payload."""
    from sheltersim.events.models import EventKind, ScheduledEvent

    world = controller.world
    first, second = "A001", "A002"
    world.agents[second].zone_id = world.agents[first].zone_id
    payload = {
        "interaction_type": "reassurance_exchange",
        "agent_id": first,
        "participant_agent_id": second,
        "zone_id": world.agents[first].zone_id,
        "topic_id": "emotional_check_in",
        "action": "offer_reassurance",
        "reason_short": "autonomous_need_choice",
        "interaction_duration_minutes": 5,
    }
    return ScheduledEvent(
        event_id="social-901",
        kind=EventKind.ENDOGENOUS,
        due_minute=0,
        priority=0,
        sequence=901,
        event_type="social_interaction",
        payload={**payload, **change},
        source_id="social-policy-1",
        causation_id=None,
        provenance={
            "source_type": "assumption",
            "source_id": "ASM-EVENT-TRIGGERED-SOCIAL-INTERACTIONS-V1",
            "policy_version": "social-policy-1",
        },
    )


def walk(controller: Any, purpose: str, facility_id: str | None, destination: str) -> Any:
    """A walk of A001 to `destination`, scheduled by the controller: its completion event."""
    world = controller.world
    agent = world.agents["A001"]
    route = controller.space.route(agent.zone_id, destination, agent.id)
    assert route is not None and route.travel_minutes > 0
    assert controller._schedule_travel(
        agent_id=agent.id,
        route=route,
        purpose=purpose,
        facility_id=facility_id,
        minute=world.minute_index,
        validate_state=False,
    )
    return next(
        event
        for event in world.event_queue.pending()
        if event.event_type == "agent_travel_complete" and event.payload["agent_id"] == "A001"
    )


def return_walk(controller: Any) -> Any:
    """A001's walk home to another zone."""
    world = controller.world
    zone = world.agents["A001"].zone_id
    return walk(controller, "return", None, next(name for name in world.zones if name != zone))


def toilet_walk(controller: Any) -> Any:
    """A001's walk to a toilet in another zone."""
    world = controller.world
    zone = world.agents["A001"].zone_id
    facility = next(
        facility
        for facility in world.facilities.values()
        if facility.kind == "toilet" and facility.zone_id != zone
    )
    return walk(controller, "toilet_request", facility.id, facility.zone_id)


def _schedule(event: Any) -> Any:
    return lambda controller: controller.world.schedule_event(event(controller))


def _apart(controller: Any) -> None:
    event = social_action(controller)
    world = controller.world
    zones = [zone for zone in world.zones if zone != world.agents["A001"].zone_id]
    world.agents["A002"].zone_id = zones[0]
    world.schedule_event(event)


# Each world the phase runs on, as the pytest file arranged it: the arrangement, and whether the
# world's minute is then the walk's due minute.
DISPATCHED = {
    "no_handler": (_schedule(lambda controller: probe("x", 901)), False),
    "social": (_schedule(social_action), False),
    "social_apart": (_apart, False),
    "social_other_reason": (
        _schedule(lambda controller: social_action(controller, reason_short="other")),
        False,
    ),
    "return_walk": (return_walk, True),
    "toilet_walk": (toilet_walk, True),
}

# Each spoiled walk with the deleted Python rule's text, written down from it.
SPOILED_WALKS = [
    (
        lambda world, event: world.active_tasks.pop(event.payload["task_id"]),
        "travel completion does not match an active task",
    ),
    (
        lambda world, event: setattr(world.agents["A001"], "target_id", "x"),
        "return travel completion does not match its target",
    ),
    (
        lambda world, event: setattr(
            world.agents["A001"], "activity", type(world.agents["A001"].activity).IDLE
        ),
        "travel completion Agent does not match its active task",
    ),
]

# A walk home named for another purpose, with the deleted Python rule's text.
OTHER_PURPOSES = [
    ("sleep_bed", "sleep-bed travel does not match its live reservation"),
    ("presence_relocation", "presence relocation completion does not match its route"),
    ("another", "unsupported travel purpose 'another'"),
]


def dispatch_rows_fixture() -> dict[str, Any]:
    """The worlds the dispatch phase runs on (`DISPATCHED`, each at the minute it runs), and the
    cases of the travel completion's handler alone (`support/cases.rs`): a spoiled walk home, and
    one named for another purpose."""
    from support.core_texts import Fixture, table

    fixture = Fixture()
    controller = dispatch_controller()
    fixture.world("hundred", controller.world, controller, full=True)
    for name, (arrange, at_due) in DISPATCHED.items():
        controller = dispatch_controller()
        made = arrange(controller)
        if at_due:
            controller.world.minute_index = made.due_minute
        fixture.world(name, controller.world, controller)
    handler = table("agent_travel_complete")
    for number, (spoil, text) in enumerate(SPOILED_WALKS):
        controller = dispatch_controller()
        event = return_walk(controller)
        controller.world.minute_index = event.due_minute
        spoil(controller.world, event)
        name = fixture.world(f"spoiled_walk_{number}", controller.world, controller)
        fixture.says("a_walk_that_does_not_fit", name, handler, event, text)
    for purpose, text in OTHER_PURPOSES:
        controller = dispatch_controller()
        event = return_walk(controller)
        controller.world.minute_index = event.due_minute
        other = probe(
            "walk-other",
            event.sequence + 5000,
            "agent_travel_complete",
            due=event.due_minute,
            payload={**event.payload_dict(), "purpose": purpose},
        )
        name = fixture.world("return_walk", controller.world, controller)
        fixture.says("a_walk_named_for_another_purpose", name, handler, other, text)
    document = fixture.document()
    del document["canonical_sha256"]
    return document
