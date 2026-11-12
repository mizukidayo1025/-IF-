"""The worlds of the core's tests of the population rules (Issue #225, lane M8).

`native/core/tests/population_rules.rs` runs the kernels of population.py and
population_lifecycle.py on the worlds `test_population_rules.py` built for them before they
moved to the core: the generated Scenario the Project screen makes, at minute 0 (`generated` of
`generated_texts.json`, which `core_fixtures_texts.py` exports), and its variants. This module
writes `population_rules.json`: the variants, each stored as the changes from that world
(`{"file": "generated_texts", "base": "generated", "changes": [...]}`;
`native/core/tests/support/cases.rs` makes it again), and what the pytest file picked in Python's
order and what the contracts made of its commands (`inputs`).

- `overlay_3`: the configs with `geometry_overlay` at schema_version 3.
- `room`: `_world_with_a_room`, the medical and quiet areas full and a surveyed room in the quiet
  area, applied at minute 1.
- `out_and_back`: the program that takes the first person out at minute 1 and back at minute 3.
"""

from __future__ import annotations

from typing import Any

from sheltersim.core.world_export import export_world
from support.core_fixtures_texts import (
    _contract,
    _controller,
    _generated_scenario,
    _generated_world,
)
from support.core_texts import changes

# The world of generated_texts.json the worlds here are changes of.
BASE_FILE = "generated_texts"
BASE = "generated"


def _world_with_a_room(scenario: Any) -> Any:
    """`_world_with_a_room` of test_population_rules.py: everyone full but one surveyed room in
    the quiet area, applied at minute 1."""
    from sheltersim.core.world import WorldState
    from sheltersim.events.effects import (
        EffectProgram,
        EffectProgramGeometryHandler,
        SupportMode,
        TargetSelector,
        TimedAction,
        schedule_effect_program,
    )
    from sheltersim.events.handlers import EventDispatcher
    from sheltersim.experiments.scenario import ScenarioSpec

    document = scenario.model_dump(mode="json")
    for zone in document["zones"]:
        if zone["id"] in {"medical", "quiet"}:
            zone["capacity"] = 0
    made = WorldState.from_scenario(ScenarioSpec.model_validate(document))
    room = TimedAction(
        effect_id="room",
        op="temporary_room_geometry",
        target_selector=TargetSelector(kind="zone", target_id="quiet"),
        start_minute=1,
        end_minute=20,
        parameters={
            "schema_version": 1,
            "room_id": "room",
            "zone_id": "quiet",
            "polygon_m": [
                {"x": 8.0, "y": 2.4},
                {"x": 9.3, "y": 2.4},
                {"x": 9.3, "y": 4.4},
                {"x": 8.0, "y": 4.4},
            ],
            "people_capacity": 4,
            "sleeping_capacity": 0,
            "bed_points_m": [],
            "route_edge_ids": ["route_corridor_quiet", "route_quiet_corridor"],
            "source_ids": ["ASM-TEST-ROOM"],
        },
        units="metre_2d",
        source_id="ASM-TEST-ROOM",
        policy_dependencies=["geometry_overlay_v1"],
        reversible=False,
    )
    schedule_effect_program(
        made,
        EffectProgram(
            program_id="room-program",
            narrative="A surveyed room.",
            source_text="room",
            simulation_start_minute=1,
            effects=[room],
            assumptions=[],
            support_mode=SupportMode.NATIVE,
            compiler_provenance={"compiler_kind": "offline_rule", "compiler_version": "test-v1"},
        ),
    )
    made.minute_index = 1
    dispatcher = EventDispatcher({"effect_program_geometry": EffectProgramGeometryHandler()})
    assert dispatcher.dispatch_due(made, 1)[0].status.value == "applied"
    return made


def _out_and_back(agent_id: str, exit_minute: int, return_minute: int) -> Any:
    """`_out_and_back` of test_population_rules.py: one program that takes a person out and brings
    them back."""
    from sheltersim.events.effects import EffectProgram, SupportMode, TargetSelector, TimedAction

    effects = [
        TimedAction(
            effect_id=effect_id,
            op="population_presence",
            target_selector=TargetSelector(kind="person", target_id=agent_id),
            start_minute=minute,
            parameters={"action": action},
            units="population_transition",
            source_id="TEST-PRESENCE",
            assumption_ids=[],
            policy_dependencies=["population_lifecycle_v1"],
            reversible=False,
        )
        for effect_id, minute, action in (
            ("out", exit_minute, "temporary_exit"),
            ("back", return_minute, "return"),
        )
    ]
    return EffectProgram(
        program_id="program-out-and-back",
        narrative="out and back",
        source_text="out and back",
        simulation_start_minute=exit_minute,
        effects=effects,
        assumptions=[],
        support_mode=SupportMode.NATIVE,
        compiler_provenance={"compiler_kind": "offline_rule", "compiler_version": "test-v1"},
    )


def _worlds() -> dict[str, Any]:
    from sheltersim.events.effects import schedule_effect_program

    worlds: dict[str, Any] = {}
    world = _generated_world()
    world.model_configs["geometry_overlay"] = {"schema_version": 3}
    worlds["overlay_3"] = world
    worlds["room"] = _world_with_a_room(_generated_scenario())
    world = _generated_world()
    schedule_effect_program(world, _out_and_back(sorted(world.agents)[0], 1, 3))
    worlds["out_and_back"] = world
    return worlds


def _inputs(world: Any, out_and_back: Any) -> dict[str, Any]:
    """What test_population_rules.py picks in Python's order, and what
    `PopulationPresenceCommand` made of its commands (`model_dump(mode="json")`)."""
    from sheltersim.domain.population_lifecycle import PopulationPresenceCommand

    person = next(iter(world.agents))
    zone = world.agents[person].zone_id
    other = next(item for item in sorted(world.zones) if item != zone)

    def first_step(origin: str) -> str:
        return next(
            edge.to_zone_id for edge in world.zone_edges.values() if edge.from_zone_id == origin
        )

    first, second = [agent.id for agent in world.agents.values() if agent.zone_id == zone][:2]
    walker = sorted(world.agents)[1]
    commands: dict[str, dict[str, Any]] = {
        "exit": {"action": "temporary_exit", "agent_id": person},
        "exit_9": {"action": "temporary_exit", "agent_id": person, "expected_return_minute": 9},
        "exit_9_sources": {
            "action": "temporary_exit",
            "agent_id": person,
            "expected_return_minute": 9,
            "source_ids": ("ASM-TEST-A", "ASM-TEST-B"),
        },
        "exit_5": {"action": "temporary_exit", "agent_id": person, "expected_return_minute": 5},
        "return": {"action": "return", "agent_id": person},
        "return_here": {"action": "return", "agent_id": person, "zone_id": zone},
        "transfer": {"action": "transfer", "agent_id": person},
        "transfer_other": {"action": "transfer", "agent_id": person, "zone_id": other},
        "relocate": {"action": "relocate", "agent_id": person, "zone_id": first_step(zone)},
        "relocate_walker": {
            "action": "relocate",
            "agent_id": walker,
            "zone_id": first_step(world.agents[walker].zone_id),
        },
        "relocate_in_place": {"action": "relocate", "agent_id": first, "zone_id": zone},
        "reunite": {
            "action": "reunite",
            "agent_ids": (first, second),
            "household_id": "household-new",
        },
        "refused_temporary_exit": {"action": "temporary_exit", "agent_id": "nobody"},
        "refused_transfer": {"action": "transfer", "agent_id": "nobody"},
        "refused_return": {"action": "return", "agent_id": "nobody"},
        "refused_relocate": {"action": "relocate", "agent_id": "nobody", "zone_id": "nowhere"},
        "refused_reunite": {
            "action": "reunite",
            "agent_ids": ("nobody", "nor-them"),
            "household_id": "h",
        },
    }
    dumped = {
        name: PopulationPresenceCommand.model_validate(fields).model_dump(mode="json")
        for name, fields in commands.items()
    }
    exit_event = next(
        event
        for event in out_and_back.event_queue.pending()
        if event.event_type == "effect_program_population"
        and event.payload["command"]["action"] == "temporary_exit"
    )
    dumped["out"] = PopulationPresenceCommand.model_validate(
        exit_event.payload["command"]
    ).model_dump(mode="json")
    return {
        "contracts": _arrival_contracts(),
        "person": person,
        "zone": zone,
        "other": other,
        "first": first,
        "second": second,
        "walker": walker,
        "commands": dumped,
    }


def _arrival_contracts() -> list[dict[str, Any]]:
    """What the contracts gave for the refused arrivals of
    `test_a_refused_arrival_changes_nothing_and_says_the_rule_s_text`: `PersonTraits` of a
    language that is no identifier (the rule's own fields around it), and
    `DynamicArrivalPersonContext` of a person's context."""
    from sheltersim.contracts.extensions import PersonTraits
    from sheltersim.contracts.population import DynamicArrivalPersonContext

    traits = {"age_years": 30, "household_role": "adult", "languages": ["not a language"]}
    context = {"member_index": 9}
    return [
        {"model": "PersonTraits", "input": traits, **_contract(PersonTraits, traits)},
        {
            "model": "DynamicArrivalPersonContext",
            "input": context,
            **_contract(DynamicArrivalPersonContext, context),
        },
    ]


def population_rules_fixture() -> dict[str, Any]:
    """`population_rules.json`: the worlds of test_population_rules.py besides `generated` of
    `generated_texts.json`, as its variants, and the inputs."""
    base = _generated_world()
    generated = export_world(base, _controller(base))
    made = _worlds()
    worlds = {
        name: {
            "file": BASE_FILE,
            "base": BASE,
            "changes": changes(generated, export_world(world, _controller(world))),
        }
        for name, world in made.items()
    }
    return {"worlds": worlds, "inputs": _inputs(base, made["out_and_back"])}
