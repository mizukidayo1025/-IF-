"""The worlds of the core's tests of saved texts (Issue #225, lane M2).

`native/core/tests/resource_service_texts.rs`, `effect_program_texts.rs` and
`population_texts.rs` check the texts of the EventSuppressed, EventApplicationError and ValueError
the core's rules raise (they are saved as an event's or a request's `detail`), on the worlds the
pytest files of the same names built. This module builds those worlds and exports them
(`core_fixtures.export_world`):

- `resource_service_texts.json`: `serving`, h00_small_replay with the water station and the meal
  point, A001 being served water at minute 0; `visiting`, the gate's generated Scenario at the
  first minute someone is served at a toilet; each with the Run's policies (the resource
  manager's config, the dispatch table).
- `generated_texts.json`: `generated`, the generated Scenario the Project screen makes at minute 0
  with the Run's policies (`load::loaded` reads them, and `load::following_host` the world), and
  its variants: the EffectPrograms the tests schedule (built and validated with pydantic, as
  `schedule_effect_program` writes them into the world) and the floor plan with an arrival
  approach route.
- `handoff_texts.json`: the world where a patrol is under way and its handoff is scheduled.

A variant is stored as the changes from its base world, as `core_texts.py` stores its worlds
(`{"base": name, "changes": [...]}`, `core_texts.changes`); `native/core/tests/support/cases.rs`
(`cases::entry`) makes it again. A builder adds its own keys to an entry: the IDs a test picked in
Python's order, the commands `PopulationPresenceCommand` validated.
"""

from __future__ import annotations

import copy
import sys
import tempfile
from pathlib import Path
from typing import Any

from support.core_fixtures import export_world
from support.core_fixtures_service import at_once_worlds
from support.core_texts import changes


def variant(base_name: str, base: dict[str, Any], entry: dict[str, Any]) -> dict[str, Any]:
    """`entry` as the changes from `base`, the entry of `base_name` in the same file."""
    return {"base": base_name, "changes": changes(base, entry)}


def _controller(world: Any) -> Any:
    from sheltersim.experiments.metrics import MetricsRecorder
    from sheltersim.experiments.runner import OfflineSimulationController
    from sheltersim.experiments.timeline import _MetricSink

    return OfflineSimulationController(world, _MetricSink(MetricsRecorder()))


# ---- resource_service_texts.json ---------------------------------------------------------------


def _serving() -> tuple[Any, Any]:
    """`_started` of test_resource_service_texts.py: A001 being served water at minute 0."""
    from sheltersim.core.world import WorldState
    from sheltersim.domain.resource_service import ResourceServiceManager
    from support.fixture_scenarios import _resource_service_scenario

    world = WorldState.from_scenario(_resource_service_scenario())
    manager = ResourceServiceManager(world)
    world.agents["A001"].zone_id = world.facilities["water_station_1"].zone_id
    manager.request("A001", "water_station_1", minute=0)
    started = manager.arbitrate(0)
    assert len(started) == 1
    world.validate()
    return world, started[0]


def _visiting() -> tuple[Any, Any, str]:
    """The `visiting` fixture: the gate's generated world at the first minute someone is served
    at a toilet, its controller and that service's task."""
    from sheltersim.core.engine import TickEngine
    from sheltersim.core.world import WorldState

    root = str(Path(__file__).resolve().parents[2] / "benchmarks")
    if root not in sys.path:
        sys.path.insert(0, root)
    import gate_scenarios

    world = WorldState.from_scenario(gate_scenarios.generated(evacuees=20, minutes=120))
    controller = _controller(world)
    engine = TickEngine(world, phase_handler=controller)
    for _ in range(120):
        engine.step()
        served = [key for key, task in world.active_tasks.items() if task.kind == "toilet_service"]
        if served:
            return world, controller, served[0]
    raise AssertionError("nobody was served at a toilet")


def _first(world: Any, kind: str) -> str:
    """The first facility of a kind in the world's order, as `next(...)` of the tests picks it."""
    return next(key for key, item in world.facilities.items() if item.kind == kind)


def resource_service_texts_fixture() -> dict[str, Any]:
    """`resource_service_texts.json`."""
    world, completion = _serving()
    serving = export_world(world, _controller(world))
    serving["completion_id"] = completion.event_id
    serving["toilet"] = _first(world, "toilet")
    world, controller, task_id = _visiting()
    visiting = export_world(world, controller)
    visiting["task_id"] = task_id
    visiting["water"] = _first(world, "water_station")
    return {"worlds": {"serving": serving, "visiting": visiting}}


# ---- generated_texts.json ----------------------------------------------------------------------


def _generated_scenario() -> Any:
    """`generated_scenario`: the Scenario the Project screen's preview makes of the form."""
    from sheltersim.experiments.scenario import ScenarioSpec
    from support.generated import _generated_request, _ui_preview

    with tempfile.TemporaryDirectory(prefix="core-fixture-texts-") as root:
        preview = _ui_preview(Path(root), _generated_request())
    return ScenarioSpec.model_validate(copy.deepcopy(preview["scenario"]))


def _generated_world() -> Any:
    from sheltersim.core.world import WorldState

    return WorldState.from_scenario(_generated_scenario())


def _action(
    op: str,
    kind: str,
    target: str,
    parameters: dict[str, Any],
    *,
    effect_id: str = "effect-1",
    start: int = 5,
    end: int | None = 30,
    guard: Any = None,
) -> Any:
    """`_action` of test_effect_program_texts.py."""
    from sheltersim.events.effects import DEFAULT_PRIMITIVE_REGISTRY, TargetSelector, TimedAction

    spec = DEFAULT_PRIMITIVE_REGISTRY.get(op)
    assert spec is not None
    return TimedAction(
        effect_id=effect_id,
        op=op,
        target_selector=TargetSelector(kind=kind, target_id=target),
        start_minute=start,
        end_minute=end,
        guard=guard,
        parameters=parameters,
        units=sorted(spec.units)[0],
        source_id="TEST-EFFECT-TEXTS",
        assumption_ids=[],
        policy_dependencies=sorted(spec.required_policies),
        reversible=end is not None,
    )


def _program(*actions: Any, program_id: str = "program-1") -> Any:
    """`_program` of test_effect_program_texts.py."""
    from sheltersim.events.effects import EffectProgram, SupportMode

    return EffectProgram(
        program_id=program_id,
        narrative="a test program",
        source_text="a test program",
        simulation_start_minute=min(action.start_minute for action in actions),
        effects=list(actions),
        assumptions=[],
        support_mode=SupportMode.NATIVE,
        compiler_provenance={"compiler_kind": "offline_rule", "compiler_version": "test-v1"},
    )


def _scheduled(*actions: Any, edit: Any = None) -> Any:
    """The generated world with a program of these actions scheduled; `edit` runs first."""
    from sheltersim.events.effects import schedule_effect_program

    world = _generated_world()
    if edit is not None:
        edit(world)
    schedule_effect_program(world, _program(*actions))
    return world


def _a_zone(world: Any) -> str:
    return sorted(world.zones)[0]


def _effect_worlds(base: Any) -> dict[str, Any]:
    """The worlds of test_effect_program_texts.py, by name."""
    from sheltersim.events.effects import GuardExpression

    zone = _a_zone(base)
    edge = sorted(base.zone_edges)[0]
    staff = sorted(base.staff)[0]
    toilet = sorted(id for id, item in base.facilities.items() if item.kind == "toilet")[0]
    worlds = {
        "environment": _scheduled(_action("set_ambient_noise", "zone", zone, {"level": 0.5})),
        "lighting": _scheduled(_action("set_lighting", "zone", zone, {"level": 0.5})),
        "temporary_space": _scheduled(
            _action(
                "add_temporary_space", "zone", zone, {"people_capacity": 5, "sleeping_capacity": 2}
            )
        ),
        "temporary_space_guard": _scheduled(
            _action(
                "add_temporary_space",
                "zone",
                zone,
                {"people_capacity": 5},
                guard=GuardExpression(operator="gt", field="simulation.minute", value=100_000),
            )
        ),
        "geometry": _scheduled(_action("set_route_access", "route", edge, {"accessible": True})),
        "add_toilet_plain": _scheduled(_action("add_toilet", "zone", zone, {"count": 1}, end=None)),
        "role": _scheduled(
            _action("set_staff_role", "staff", staff, {"skills": ["support"]}, end=None)
        ),
    }
    for op in ("add_staff", "add_toilet"):
        world = _scheduled(_action(op, "zone", zone, {"count": 1}, end=None))
        # The floor plan has no place for so many toilets; the test is about the bound.
        world.model_configs["geometry_overlay"]["schema_version"] = 3
        worlds[op] = world

    def broken(world: Any) -> None:
        world.facilities[toilet].working_units = 0
        if "infrastructure_v1" in world.model_state:
            world.model_state["infrastructure_v1"]["base_working_units"][toilet] = 0

    capacity = base.facilities[toilet].capacity
    parameters = {
        "facility_id": toilet,
        "component_id": toilet,
        "expected_working_units": 0,
        "restored_working_units": capacity,
    }
    worlds["repair"] = _scheduled(
        _action("facility_repair", "facility", toilet, parameters, end=None), edit=broken
    )
    return worlds


def handing_off() -> tuple[Any, Any]:
    """The `handing_off` fixture of test_effect_program_texts.py: a patrol under way, an idle
    patroller in its zone, the handoff scheduled, and a role change of the worker on patrol; and
    the controller of its Run."""
    from sheltersim.core.engine import TickEngine
    from sheltersim.events.effects import EffectProgramValidationError, schedule_effect_program

    world = _generated_world()
    controller = _controller(world)
    engine = TickEngine(world, phase_handler=controller)
    operations = world.model_state["staff_operations_v1"]["staff"]
    idle = [id for id, staff in sorted(world.staff.items()) if operations[id]["phase"] == "idle"]
    worker = next(id for id in idle if "support" in world.staff[id].skills)
    replacement = next(id for id in idle if "support" not in world.staff[id].skills)
    zone = sorted({agent.zone_id for agent in world.agents.values()})[0]
    schedule_effect_program(
        world,
        _program(
            _action(
                "set_staff_role",
                "staff",
                worker,
                {"skills": ["patrol", "support"]},
                start=world.minute_index + 300,
                end=None,
            ),
            _action(
                "set_staff_role",
                "staff",
                replacement,
                {"skills": ["patrol"]},
                effect_id="effect-2",
                start=world.minute_index,
                end=None,
            ),
            program_id="roles",
        ),
    )
    engine.step()

    def order(staff_id: str, program_id: str) -> None:
        for _ in range(240):
            try:
                schedule_effect_program(
                    world,
                    _program(
                        _action(
                            "staff_task_order",
                            "staff",
                            staff_id,
                            {
                                "task_kind": "patrol",
                                "target_zone_id": zone,
                                "reason": "a test patrol",
                            },
                            start=world.minute_index,
                            end=None,
                        ),
                        program_id=program_id,
                    ),
                )
                return
            except EffectProgramValidationError:
                engine.step()
        raise AssertionError(f"{staff_id} took no order")

    order(replacement, "send-replacement")
    for _ in range(60):
        engine.step()
        phase = world.model_state["staff_operations_v1"]["staff"][replacement]["phase"]
        if phase == "idle" and world.staff[replacement].zone_id == zone:
            break
    else:
        raise AssertionError("the replacement did not come to the zone")
    order(worker, "patrol")
    for _ in range(60):
        engine.step()
        record = world.model_state["staff_operations_v1"]["staff"][worker]
        if record["phase"] == "working" and world.staff[replacement].active_task_id is None:
            break
    else:
        raise AssertionError("no patrol came to work")
    schedule_effect_program(
        world,
        _program(
            _action(
                "staff_handoff",
                "staff",
                worker,
                {
                    "staff_id": worker,
                    "replacement_staff_id": replacement,
                    "task_id": record["task_id"],
                    "reason_code": "reassigned",
                },
                start=world.minute_index,
                end=None,
            ),
            program_id="handoff",
        ),
    )
    return world, controller


# Population: an approach route's ID, and the world's zones as test_population_texts.py picks them.
ROUTE = "arrival-route-test"


def _zone(world: Any) -> str:
    return next(iter(world.agents.values())).zone_id


def _other_zone(world: Any) -> str:
    here = _zone(world)
    return next(zone for zone in sorted(world.zones) if zone != here)


def _add_route(world: Any, zone: str) -> None:
    """`_add_route` of test_population_texts.py: an approach route into the middle of `zone`."""
    geometry = world.model_configs["facility_geometry"]
    polygon = next(item["polygon"] for item in geometry["zones"] if item["zone_id"] == zone)
    x = sum(point["x"] for point in polygon) / len(polygon)
    y = sum(point["y"] for point in polygon) / len(polygon)
    outside = geometry["height_m"] + 2.0
    geometry["height_m"] = outside + 2.0
    geometry["arrival_approach_routes"] = [
        {
            "route_id": ROUTE,
            "destination_zone_id": zone,
            "points_m": [{"x": x, "y": outside}, {"x": x, "y": y}],
            "walking_speed_m_per_min": 5.0,
            "source_kind": "operator",
            "source_ids": ["SRC-TEST-APPROACH"],
        }
    ]


def _population_program(world: Any, op: str, zone: str, **parameters: Any) -> None:
    """`_program` of test_population_texts.py: an approved program of one population primitive."""
    from sheltersim.events.effects import (
        EffectProgram,
        SupportMode,
        TargetSelector,
        TimedAction,
        schedule_effect_program,
    )

    effect = TimedAction(
        effect_id=f"population-texts-{op}",
        op=op,
        target_selector=TargetSelector(kind="zone", target_id=zone),
        start_minute=1,
        parameters=parameters,
        units="evacuee",
        source_id="ASM-TEST-POPULATION-TEXTS",
        policy_dependencies=["population_dynamics_v1"],
        reversible=False,
    )
    schedule_effect_program(
        world,
        EffectProgram(
            program_id=f"population-texts-{op}",
            narrative="A population primitive whose refusals are compared.",
            source_text=op,
            simulation_start_minute=0,
            effects=[effect],
            assumptions=[],
            support_mode=SupportMode.NATIVE,
            compiler_provenance={"compiler_kind": "offline_rule", "compiler_version": "test-v1"},
        ),
    )


def _arrival_program(world: Any, zone: str, **parameters: Any) -> None:
    _population_program(
        world,
        "add_evacuees",
        zone,
        **{"count": 1, "household_size": 1, "mobility": "independent", "language": "ja"},
        **parameters,
    )


def _population_worlds(base: Any) -> dict[str, Any]:
    """The worlds of test_population_texts.py besides the generated one, by name."""
    here, other = _zone(base), _other_zone(base)
    worlds: dict[str, Any] = {}
    world = _generated_world()
    world.model_configs.pop("geometry_overlay", None)
    worlds["no_overlay_config"] = world
    world = _generated_world()
    _add_route(world, other)
    worlds["route_other"] = world
    world = _generated_world()
    _add_route(world, here)
    worlds["route_here"] = world
    world = _generated_world()
    _add_route(world, here)
    _arrival_program(world, here, approach_route_id=ROUTE)
    worlds["route_here_program"] = world
    world = _generated_world()
    _arrival_program(world, here)
    worlds["arrival_program"] = world
    world = _generated_world()
    _population_program(world, "depart_evacuees", here, count=1, include_initial=False)
    worlds["departure_program"] = world
    return worlds


def _population_inputs(world: Any) -> dict[str, Any]:
    """What test_population_texts.py picks in Python's order, and what the contracts made of its
    commands (`PopulationPresenceCommand.model_validate(...).model_dump(mode="json")`) and of a
    departed person's record (`PresenceRecord`)."""
    from sheltersim.contracts.presence import PresenceRecord
    from sheltersim.domain.population_lifecycle import PopulationPresenceCommand

    person = next(iter(world.agents))
    here = world.agents[person].zone_id
    there = next(edge.to_zone_id for edge in world.zone_edges.values() if edge.from_zone_id == here)
    first, second = sorted(world.agents)[:2]
    other = _other_zone(world)
    commands = {
        "transfer": {"action": "transfer", "agent_id": person},
        "exit_returning_early": {
            "action": "temporary_exit",
            "agent_id": person,
            "expected_return_minute": 2,
        },
        "exit_nobody": {"action": "temporary_exit", "agent_id": "nobody"},
        "transfer_nobody": {"action": "transfer", "agent_id": "nobody"},
        "return_nobody": {"action": "return", "agent_id": "nobody"},
        "return": {"action": "return", "agent_id": person},
        "return_nowhere": {"action": "return", "agent_id": person, "zone_id": "nowhere"},
        "relocate_nobody": {"action": "relocate", "agent_id": "nobody", "zone_id": there},
        "relocate_nowhere": {"action": "relocate", "agent_id": person, "zone_id": "nowhere"},
        "relocate": {"action": "relocate", "agent_id": person, "zone_id": there},
        "relocate_other": {"action": "relocate", "agent_id": person, "zone_id": other},
        "reunite_nobody": {
            "action": "reunite",
            "agent_ids": (first, "nobody"),
            "household_id": "household-new",
        },
        "reunite": {
            "action": "reunite",
            "agent_ids": (first, second),
            "household_id": "household-new",
        },
    }
    record = PresenceRecord(agent_id="departed", status="departed", since_minute=0)
    return {
        "contracts": _arrival_contracts(),
        "person": person,
        "there": there,
        "commands": {
            name: PopulationPresenceCommand.model_validate(fields).model_dump(mode="json")
            for name, fields in commands.items()
        },
        "departed_record": record.model_dump(mode="json"),
    }


def _contract(model: Any, value: Any) -> dict[str, Any]:
    """What the native host's `contract` gives for `value`: the model's dump, or the class and
    text of what `model_validate` raised (a ValidationError is a ValueError)."""
    try:
        return {"dump": model.model_validate(value).model_dump(mode="json")}
    except (TypeError, ValueError) as error:
        return {"raises": ["Type" if isinstance(error, TypeError) else "Value", str(error)]}


def _arrival_contracts() -> list[dict[str, Any]]:
    """The contracts `queue_arrivals` asks of the arguments of
    `an_arrival_the_rule_refuses_says_its_text` (population_texts.rs): `PersonTraits` of a
    language that is no identifier (the rule's own fields around it), and
    `DynamicArrivalPersonContext` of each person's context."""
    from sheltersim.contracts.extensions import PersonTraits
    from sheltersim.contracts.population import DynamicArrivalPersonContext

    asked: list[dict[str, Any]] = []
    for language in ("not a language", "", None, 5):
        fields = {"age_years": 30, "household_role": "adult", "languages": [language]}
        asked.append({"model": "PersonTraits", "input": fields, **_contract(PersonTraits, fields)})
    for item in (
        {"member_index": 0},
        {"member_index": 1, "care_support_needs": ["medical_assessment"]},
        {"member_index": 1, "care_support_needs": ["language_access"]},
    ):
        asked.append(
            {
                "model": "DynamicArrivalPersonContext",
                "input": item,
                **_contract(DynamicArrivalPersonContext, item),
            }
        )
    return asked


def generated_texts_fixture() -> dict[str, Any]:
    """`generated_texts.json`: the generated world, and the variants of the effect and population
    tests and of the requests at once, with the inputs of the requests at once."""
    base = _generated_world()
    generated = export_world(base, _controller(base))
    generated["population"] = _population_inputs(base)
    worlds: dict[str, Any] = {"generated": generated}
    for name, world in {**_effect_worlds(base), **_population_worlds(base)}.items():
        worlds[name] = variant("generated", generated, export_world(world, _controller(world)))
    # The requests at once of lane M6 (`core_fixtures_service.at_once_worlds`).
    plain = {key: value for key, value in generated.items() if key != "population"}
    at_once, inputs = at_once_worlds(plain)
    assert not set(at_once) & set(worlds)
    worlds.update(at_once)
    return {"worlds": worlds, "inputs": inputs}


def handoff_texts_fixture() -> dict[str, Any]:
    """`handoff_texts.json`: the world of `handing_off`, with the Run's policies, at the minute
    its handoff is due. Its EventLog is left out: the handoff's rules do not read it, and its
    records (some 1.4 MB of JSON) would be most of the file."""
    world, controller = handing_off()
    entry = export_world(world, controller)
    entry["world"]["event_log"] = []
    return {"worlds": {"handing_off": entry}}
