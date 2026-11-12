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

from sheltersim.core.world_export import export_world
from support.core_fixtures import run_world

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


# ---- The proposals ------------------------------------------------------------------------------

BEHAVIOR = "sheltersim.policies.behavior"
# The seed of the drawn world of the test of seeds past 64 bits.
PAST_64_SEED = 5


def behavior_controller(world: Any) -> Any:
    from sheltersim.experiments.metrics import MetricsRecorder
    from sheltersim.experiments.runner import OfflineSimulationController
    from sheltersim.experiments.timeline import _MetricSink

    return OfflineSimulationController(world, _MetricSink(MetricsRecorder()))


def behavior_world(scenario: Any) -> tuple[Any, Any]:
    """The world a drawn world starts from, with its body state, and its controller."""
    from sheltersim.core.world import WorldState
    from sheltersim.domain.body_state import create_body_state

    world = WorldState.from_scenario(scenario)
    world.model_state.setdefault("body_state_v1", create_body_state(world))
    return world, behavior_controller(world)


def draw_world(draws: random.Random, scenario: Any) -> tuple[Any, Any, Any]:
    """A world with values spread over each threshold of the rule, and most of it due, its
    controller, and the person contexts drawn (None for none). The values are the rule's to read
    and need not make a world the validators accept, so the controller is made before them."""
    from sheltersim.domain.models import Activity

    world, controller = behavior_world(scenario)
    world.minute_index = draws.randrange(0, 2880)

    def value() -> float:
        return draws.choice((0.0, 0.25, 0.5, 0.65, 0.8, 0.9, 1.0, draws.random(), draws.random()))

    people = sorted(world.agents)
    records = world.model_state["body_state_v1"]["agents"]
    groups = (None, "", "G1", "G2")
    context: dict[str, Any] = {}
    for agent_id in people:
        agent = world.agents[agent_id]
        agent.needs.toilet_urgency = value()
        agent.needs.hunger = value()
        agent.needs.fatigue = value()
        agent.needs.hydration_need = value()
        agent.stress = value()
        agent.trust = value()
        agent.next_decision_at = draws.choice((0, 0, 0, world.minute_index + 1))
        agent.activity = draws.choice(
            (Activity.IDLE, Activity.RESTING, Activity.SLEEPING, Activity.MOVING)
        )
        record = records[agent_id]
        record["sleep_debt_minutes"] = draws.choice((0.0, 240.0, draws.random() * 900.0))
        for name in ("uncertainty", "reassurance", "discontent", "negative_affect", "hygiene_need"):
            record[name] = value()
        if draws.random() < 0.5:
            others = draws.sample(people, min(9, len(people)))
            context[agent_id] = {
                "care_for_ids": others[:3] if draws.random() < 0.5 else [],
                "caregiver_ids": others[3:6] if draws.random() < 0.5 else [],
                "parent_ids": others[6:] if draws.random() < 0.5 else [],
                "usual_wake_minute": draws.randrange(0, 1440),
                "usual_sleep_minute": draws.randrange(0, 1440),
                "required_sleep_minutes": draws.choice((60, 480, 600)),
            }
    world.model_state["population_presence_v1"] = {
        "people": {
            agent_id: {"household_id": draws.choice(groups)}
            for agent_id in people
            if draws.random() < 0.7
        }
    }
    if context:
        configs = world.model_configs.setdefault("population_context", {"people": {}})
        configs["people"] = context
    zones = sorted(world.zones)
    environment = world.model_state.get("zone_environment_v1")
    if isinstance(environment, dict):
        for number, action in enumerate(("sleep", "rest", "wait", "sleep")):
            if draws.random() < 0.5:
                environment["modifiers"][f"m{number}"] = {
                    "zone_id": draws.choice(zones),
                    "start_minute": 0,
                    "op": "modify_demand_rate",
                    "parameters": {
                        "action": action,
                        "probability_multiplier": draws.choice((0.5, 1.0, 1.5, 2.0)),
                        "duration_multiplier": (
                            draws.choice((0.5, 1.0, 1.25, 2.0))
                            if action in ("rest", "sleep")
                            else 1.0
                        ),
                    },
                    "start_event_id": f"e{number}",
                    "source_id": "s",
                    "assumption_ids": ["ASM-A", "ASM-B"][: number % 3],
                }
    return world, controller, context or None


def draw_policy(draws: random.Random, ids: list[str]) -> Any:
    """A behavior policy with its values drawn: a utility policy, or a priority rule over one."""
    from sheltersim.policies.behavior import PriorityRuleBehaviorPolicy, UtilityBehaviorPolicy

    def weight() -> float:
        return draws.choice((0.0, 0.25, 1, 1.0, draws.random()))

    autonomy = draws.random() < 0.7
    parameters: dict[str, Any] = {
        "temperature": draws.choice((0.2, 0.05, 1, 3.0, draws.random() + 0.01)),
        "decision_interval_minutes": draws.choice((1, 15, 30)),
        "decision_interval_minutes_by_agent": {ids[0]: 7} if draws.random() < 0.5 else {},
        "temperature_by_agent": {ids[1]: draws.choice((0.35, 2))} if draws.random() < 0.5 else {},
        "sleep_debt_utility_weight": draws.choice((1.0, 0.5, draws.random())),
        "resource_service_enabled": draws.random() < 0.7,
        "hydration_action_threshold": draws.choice((0.25, 0.30000000000000004, 1e-07, 1)),
        "hunger_action_threshold": draws.choice((0.7, 0.123456789, 1.0)),
        "water_utility_base": weight(),
        "water_utility_need_weight": weight(),
        "meal_utility_base": weight(),
        "meal_utility_need_weight": weight(),
        "information_query_threshold": draws.choice((1.1, 0.5, 1, 0.123456789)),
        "information_query_utility_weight": weight(),
        "autonomy_enabled": autonomy,
    }
    version = draws.choice(
        (
            "utility-softmax-v1",
            "sleep-aware-utility-softmax-v2",
            "sleep-aware-utility-softmax-v2-autonomy-v2",
            'quote"\\version-é',
        )
    )
    scorer = UtilityBehaviorPolicy(**parameters, policy_version=version)
    if draws.random() < 0.3:
        return PriorityRuleBehaviorPolicy(
            scorer, policy_version=draws.choice((version, "priority-rule-v1"))
        )
    return scorer


def _compact(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, separators=(",", ":"))


# The activities `draw_world` draws from, by their place in a record.
ACTIVITIES = ("idle", "resting", "sleeping", "moving")
# The households `draw_world` draws from, by their place in a record.
HOUSEHOLDS = (None, "", "G1", "G2")
# The body record's values `draw_world` draws, in a record's order.
BODY_VALUES = (
    "sleep_debt_minutes",
    "uncertainty",
    "reassurance",
    "discontent",
    "negative_affect",
    "hygiene_need",
)
# The actions of the zone modifiers `draw_world` may add, `m0` to `m3`.
MODIFIER_ACTIONS = ("sleep", "rest", "wait", "sleep")


def drawn_record(world: Any, drawn_context: Any) -> str:
    """What `draw_world` drew, as one JSON text of numbers only (a person is their place in ID
    order): `[minute, people, presence, context, modifiers]`. Each person is `[toilet, hunger,
    fatigue, hydration, stress, trust, next decision, activity, *body values]` (the activity's and
    the household's places in `ACTIVITIES` and `HOUSEHOLDS`); the presence is each person's
    household, -1 for no record; the context is None or `[person, care_for, caregivers, parents,
    wake, sleep, required]` in the dict's order; the modifiers are None or `[number, zone, the
    probability's, the duration's multiplier]` (the zone's place in ID order), in order."""
    people = sorted(world.agents)
    place = {agent_id: index for index, agent_id in enumerate(people)}
    zones = sorted(world.zones)
    records = world.model_state["body_state_v1"]["agents"]
    agents = []
    for agent_id in people:
        agent = world.agents[agent_id]
        needs = agent.needs
        record = records[agent_id]
        agents.append(
            [
                needs.toilet_urgency,
                needs.hunger,
                needs.fatigue,
                needs.hydration_need,
                agent.stress,
                agent.trust,
                agent.next_decision_at,
                ACTIVITIES.index(agent.activity.value),
                *(record[name] for name in BODY_VALUES),
            ]
        )
    presence = world.model_state["population_presence_v1"]["people"]
    assert list(presence) == [agent_id for agent_id in people if agent_id in presence]
    households = [
        HOUSEHOLDS.index(presence[agent_id]["household_id"]) if agent_id in presence else -1
        for agent_id in people
    ]
    context = None
    if drawn_context is not None:
        assert world.model_configs["population_context"]["people"] == drawn_context
        context = [
            [
                place[agent_id],
                [place[other] for other in person["care_for_ids"]],
                [place[other] for other in person["caregiver_ids"]],
                [place[other] for other in person["parent_ids"]],
                person["usual_wake_minute"],
                person["usual_sleep_minute"],
                person["required_sleep_minutes"],
            ]
            for agent_id, person in drawn_context.items()
        ]
    modifiers = None
    environment = world.model_state.get("zone_environment_v1")
    if isinstance(environment, dict):
        modifiers = []
        for name, modifier in environment["modifiers"].items():
            number = int(name[1:])
            parameters = modifier["parameters"]
            assert modifier == {
                "zone_id": modifier["zone_id"],
                "start_minute": 0,
                "op": "modify_demand_rate",
                "parameters": {
                    "action": MODIFIER_ACTIONS[number],
                    "probability_multiplier": parameters["probability_multiplier"],
                    "duration_multiplier": parameters["duration_multiplier"],
                },
                "start_event_id": f"e{number}",
                "source_id": "s",
                "assumption_ids": ["ASM-A", "ASM-B"][: number % 3],
            }, name
            modifiers.append(
                [
                    number,
                    zones.index(modifier["zone_id"]),
                    parameters["probability_multiplier"],
                    parameters["duration_multiplier"],
                ]
            )
    return _compact([world.minute_index, agents, households, context, modifiers])


# The values the Run reads of a `UtilityBehaviorPolicy` (`Spec::with_fields` of the core's
# `run::behavior`), in the order of a policy's record.
UTILITY_FIELDS = (
    "policy_version",
    "temperature",
    "decision_interval_minutes",
    "decision_interval_minutes_by_agent",
    "temperature_by_agent",
    "sleep_debt_utility_weight",
    "resource_service_enabled",
    "hydration_action_threshold",
    "hunger_action_threshold",
    "water_utility_base",
    "water_utility_need_weight",
    "meal_utility_base",
    "meal_utility_need_weight",
    "information_query_threshold",
    "information_query_utility_weight",
    "autonomy_enabled",
)
# What reading `scorer` of a utility policy raised.
NO_SCORER = ["builtins.AttributeError", "'UtilityBehaviorPolicy' object has no attribute 'scorer'"]


def _behavior_object(qualname: str, attrs: dict[str, Any]) -> dict[str, Any]:
    return {
        "object": {"class": [BEHAVIOR, qualname, True], "attrs": attrs, "class_own": {}, "own": {}}
    }


def _utility_object(values: list[Any], *, as_policy: bool) -> dict[str, Any]:
    attrs = {name: {"value": value} for name, value in zip(UTILITY_FIELDS, values, strict=True)}
    if as_policy:
        attrs = {
            "policy_version": attrs.pop("policy_version"),
            "scorer": {"fault": NO_SCORER},
            **attrs,
        }
    return _behavior_object("UtilityBehaviorPolicy", attrs)


def decisions_of(record: Any) -> dict[str, Any]:
    """The document of `controller.decisions` (`policies_document`) a policy's record stands for:
    the values of a utility policy, or `[version, values]` of a priority rule over one."""
    if len(record) == 2:
        policy = _behavior_object(
            "PriorityRuleBehaviorPolicy",
            {
                "policy_version": {"value": record[0]},
                "scorer": _utility_object(record[1], as_policy=False),
            },
        )
    else:
        policy = _utility_object(record, as_policy=True)
    return _behavior_object("BehaviorDecisionRunner", {"policy": policy})


class PolicyRecords:
    """The behavior policies as the Run reads them (`controller.decisions`), each as one JSON text:
    the values of its utility policy (`UTILITY_FIELDS`), with the priority rule's version before
    them for a priority rule. `decisions_of` makes the Run's document again from it, the same as
    the Run's."""

    def __init__(self, scenario: Any) -> None:
        self.controller = behavior_world(scenario)[1]

    def record(self, policy: Any) -> str:
        from sheltersim.policies.behavior import BehaviorDecisionRunner, PriorityRuleBehaviorPolicy

        self.controller.decisions = BehaviorDecisionRunner(policy)
        self.controller._run_core.refresh_policies(self.controller)
        document = self.controller._run_core.policies_document()["controller"]["attrs"]
        scorer = policy.scorer if isinstance(policy, PriorityRuleBehaviorPolicy) else policy
        values = [getattr(scorer, name) for name in UTILITY_FIELDS]
        record = (
            [policy.policy_version, values]
            if isinstance(policy, PriorityRuleBehaviorPolicy)
            else values
        )
        assert decisions_of(json.loads(_compact(record))) == document["decisions"], policy
        return _compact(record)


def _seeded(draws: random.Random, scenario: Any, policies: PolicyRecords, count: int) -> Any:
    """A drawn world and `count` drawn policies, each with the seed of its draws."""
    world, _, context = draw_world(draws, scenario)
    record = drawn_record(world, context)
    drawn = []
    for _ in range(count):
        policy = draw_policy(draws, sorted(world.agents))
        drawn.append([policies.record(policy), draws.randrange(0, 2**40)])
    return record, drawn


def requests_world(scenario: Any) -> tuple[Any, Any]:
    """The world of `test_requests_cooldowns_and_queued_tasks_are_read_as_python_reads_them`:
    everyone due at minute 40, a request of each status, cooldowns, and queued tasks."""
    from sheltersim.core.world import WorldState
    from sheltersim.domain.models import (
        ActiveTask,
        Activity,
        SupportRequestState,
        SupportRequestStatus,
    )

    world = WorldState.from_scenario(scenario)
    controller = behavior_controller(world)
    for agent in world.agents.values():
        agent.activity = Activity.IDLE
        agent.next_decision_at = 0
    people = sorted(world.agents)
    world.minute_index = 40
    for number, status in enumerate(SupportRequestStatus):
        request = SupportRequestState(f"R{number}", people[number], 5, 10, status=status)
        world.support_requests[request.id] = request
        world.agents[people[number]].support_request_id = request.id
        # Enough for the help candidate but for the request.
        world.agents[people[number]].needs.fatigue = 0.9
    world.agents[people[4]].support_request_id = "nothing"
    world.agents[people[5]].support_cooldown_until = 55
    world.agents[people[6]].support_cooldown_until = 25
    world.active_tasks["T1"] = ActiveTask("T1", "toilet_queue", people[7], "F1", 10, 90)
    world.active_tasks["T2"] = ActiveTask("T2", "resource_service_queue", people[8], None, 50, 90)
    world.active_tasks["T3"] = ActiveTask("T3", "toilet_queue", people[9], "F1", 20, 90)
    world.active_tasks["T4"] = ActiveTask("T4", "resource_service_queue", people[9], "F2", 30, 90)
    return world, controller


# The minutes of the Run whose proposals are compared.
RUN_MINUTES = 90


def run_scenario_of(scenario: Any) -> Any:
    """The Scenario of `test_each_proposal_of_a_run_has_the_bits_of_python`: `RUN_MINUTES` long."""
    import copy

    from sheltersim.experiments.scenario import ScenarioSpec

    document = copy.deepcopy(scenario.model_dump(mode="json"))
    document["duration_minutes"] = RUN_MINUTES
    return ScenarioSpec.model_validate(document)


def behavior_rows_fixture() -> dict[str, Any]:
    """The world a drawn world starts from (`generated`), the requests' world, the world of a Run
    of `RUN_MINUTES` set up as `run_scenario` sets it up (`run`), and what the tests drew:
    `past_64` (a drawn world and a policy) and `requests` (the two policies). The tests of many
    drawn worlds stay in pytest (#225, decision 5)."""
    from sheltersim.policies.behavior import UtilityBehaviorPolicy
    from support.core_fixtures_queues import generated
    from support.core_texts import Fixture

    scenario = generated()
    fixture = Fixture()
    fixture.world("generated", behavior_world(scenario)[0], None, full=True)
    fixture.world("requests", requests_world(scenario)[0], None)
    world, controller, _ = run_world(run_scenario_of(scenario))
    fixture.world("run", world, controller)
    policies = PolicyRecords(scenario)
    record, made = _seeded(random.Random(PAST_64_SEED), scenario, policies, 1)
    requests = [
        policies.record(UtilityBehaviorPolicy(information_query_threshold=threshold))
        for threshold in (1.1, 0.0)
    ]
    return {
        "worlds": fixture.worlds,
        "past_64": {"world": record, "policy": made[0][0]},
        "requests": requests,
    }
