"""The worlds of the core's tests of the EffectProgram kernels, the guard in the dispatch and the
views (Issue #225, lane M7).

`native/core/tests/effect_kernels.rs`, `effect_guard_dispatch.rs`, `world_view_reads.rs` and
`world_view_run.rs` check on these worlds what `test_effect_kernels.py`,
`test_effect_guard_texts.py` and `test_world_view_row.py` checked. Each world is a variant of a
world another builder exports (`core_fixtures_texts.variant`), in the same file:

- `generated_texts.json` (variants of `generated`, the generated Scenario the Project screen makes):
  `kernel_*`, the programs of test_effect_kernels.py scheduled by `schedule_effect_program` (each
  EffectProgram built and validated with pydantic), with `contracts`, what the native host's
  contracts give for the effect's parameters (`_PARAMETER_MODELS[op]`), and
  `kernel_zone_<op>_started`, the world the pytest file expected after a zone condition starts;
  `views_*`, the worlds of the views (a Run of 120 minutes, a Run of 60 minutes with false claims,
  and the world without the information namespace or the life context, as the view's hash reads
  each form).
- `resource_service_texts.json` (variants of `visiting`, the gate's generated Scenario): `gate`, its
  world at minute 0 with the controller, as `test_effect_guard_texts.py` dispatched there, with
  `contracts`, what `GuardExpression` says of the guards it turns down; `gate_<name>`, that world
  with a guarded program scheduled.
"""

from __future__ import annotations

import copy
import sys
from pathlib import Path
from typing import Any

from support.core_fixtures import export_world
from support.core_fixtures_texts import _action, _controller, _generated_scenario, variant

# What test_effect_kernels.py names its programs' source.
KERNEL_SOURCE = "TEST-EFFECT-KERNELS"

# The zone conditions of test_effect_kernels.py and their parameters.
ZONE_OPERATORS = {
    "set_ambient_noise": {"level": 1.0},
    "set_lighting": {"level": 1.0},
    "set_ventilation": {"effectiveness": 0.0},
    "set_privacy": {"level": 0.0},
    "set_temperature": {"temperature_c": 35.0},
    "modify_demand_rate": {
        "action": "rest",
        "probability_multiplier": 2.0,
        "duration_multiplier": 2.0,
    },
}


def _contract(model: Any, value: Any) -> dict[str, Any]:
    """What the native host's `contract` gives for `value`: the model's dump, or the class and
    text of what `model_validate` raised (a ValidationError is a ValueError)."""
    try:
        return {"dump": model.model_validate(value).model_dump(mode="json")}
    except (TypeError, ValueError) as error:
        return {"raises": ["Type" if isinstance(error, TypeError) else "Value", str(error)]}


def _parameter_contracts(world: Any) -> list[dict[str, Any]]:
    """What `_PARAMETER_MODELS[op]` gives for the parameters of each pending effect event, as the
    core's rule asks it (`{"skills": ...}` for a role)."""
    from sheltersim.events.effects import _PARAMETER_MODELS

    asked: list[dict[str, Any]] = []
    for event in world.event_queue.pending():
        payload = event.to_dict()["payload"]
        op = payload.get("op")
        if op not in _PARAMETER_MODELS:
            continue
        if op == "set_staff_role":
            value: Any = {"skills": payload["skills"]}
        elif "parameters" in payload:
            value = payload["parameters"]
        else:
            continue
        item = {"model": "_PARAMETER_MODELS", "op": op, "input": value}
        asked.append({**item, **_contract(_PARAMETER_MODELS[op], value)})
    return asked


# ---- generated_texts.json: test_effect_kernels.py ------------------------------------------------


def _kernel_action(
    op: str, kind: str, target: str, parameters: dict[str, Any], **options: Any
) -> Any:
    action = _action(op, kind, target, parameters, **options)
    return action.model_copy(update={"source_id": KERNEL_SOURCE})


def _kernel_program(*actions: Any, program_id: str = "program-1") -> Any:
    """`_program` of test_effect_kernels.py."""
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


def _kernel_scheduled(*actions: Any, edit: Any = None) -> Any:
    from sheltersim.core.world import WorldState
    from sheltersim.events.effects import schedule_effect_program

    world = WorldState.from_scenario(_generated_scenario())
    if edit is not None:
        edit(world)
    schedule_effect_program(world, _kernel_program(*actions))
    return world


def _a_toilet(world: Any) -> str:
    return sorted(id for id, item in world.facilities.items() if item.kind == "toilet")[0]


def _zone_started(world: Any, op: str) -> Any:
    """What test_a_zone_condition_starts_and_expires expected after the start: the world with the
    modifier's record, and nothing else."""
    from sheltersim.core.world import WorldState
    from sheltersim.domain.environment import ScenarioZoneEnvironmentConfig
    from sheltersim.events.effects import _PARAMETER_MODELS

    (start,) = [
        e for e in world.event_queue.pending() if e.event_type == "effect_program_environment"
    ]
    (expiry,) = [
        e
        for e in world.event_queue.pending()
        if e.event_type == "effect_program_environment_expire"
    ]
    marker = start.provenance["effect_program_v1"]
    expected = WorldState.from_dict(world.to_dict())
    expected.model_state["zone_environment_v1"]["modifiers"][start.payload["modifier_id"]] = {
        "modifier_id": start.payload["modifier_id"],
        "program_id": start.payload["program_id"],
        "effect_id": start.payload["effect_id"],
        "op": op,
        "zone_id": start.payload["zone_id"],
        "parameters": _PARAMETER_MODELS[op]
        .model_validate(start.payload["parameters"])
        .model_dump(mode="json"),
        "start_event_id": start.event_id,
        "end_event_id": expiry.event_id,
        "start_minute": start.due_minute,
        "end_minute": start.payload["end_minute"],
        "source_id": ScenarioZoneEnvironmentConfig.model_validate(
            world.model_configs["zone_environment"]
        ).source_id,
        "assumption_ids": sorted(set(marker["assumption_ids"])),
    }
    return expected


def _kernel_worlds(base: Any) -> dict[str, Any]:
    """The worlds of test_effect_kernels.py, by name (`kernel_zone_<op>_started` included)."""
    zone = sorted(base.zones)[0]
    toilet = _a_toilet(base)
    staff = sorted(base.staff)[0]
    unavailable = {"working_units": 0}
    worlds: dict[str, Any] = {
        "kernel_facility": _kernel_scheduled(
            _kernel_action("set_facility_availability", "facility", toilet, unavailable)
        ),
        "kernel_facility_stack": _kernel_scheduled(
            _kernel_action(
                "set_facility_availability",
                "facility",
                toilet,
                unavailable,
                effect_id="first",
                end=20,
            ),
            _kernel_action(
                "set_facility_availability",
                "facility",
                toilet,
                unavailable,
                effect_id="second",
                end=25,
            ),
        ),
        "kernel_temporary_space": _kernel_scheduled(
            _kernel_action(
                "add_temporary_space", "zone", zone, {"people_capacity": 5, "sleeping_capacity": 2}
            )
        ),
        "kernel_role": _kernel_scheduled(
            _kernel_action(
                "set_staff_role", "staff", staff, {"skills": ["zeta", "alpha"]}, end=None
            )
        ),
    }
    for op, parameters in ZONE_OPERATORS.items():
        world = _kernel_scheduled(_kernel_action(op, "zone", zone, parameters))
        worlds[f"kernel_zone_{op}"] = world
        worlds[f"kernel_zone_{op}_started"] = _zone_started(world, op)
    for op in ("add_staff", "add_toilet"):
        worlds[f"kernel_{op}"] = _kernel_scheduled(
            _kernel_action(op, "zone", zone, {"count": 2}, end=None)
        )

        def bounded(world: Any) -> None:
            # The floor plan has no place for so many toilets; the test is about the bound.
            world.model_configs["geometry_overlay"]["schema_version"] = 3

        world = _kernel_scheduled(_kernel_action(op, "zone", zone, {"count": 1}, end=None))
        bounded(world)
        worlds[f"kernel_bound_{op}"] = world

    def broken(world: Any) -> None:
        world.facilities[toilet].working_units = 0
        if "infrastructure_v1" in world.model_state:
            world.model_state["infrastructure_v1"]["base_working_units"][toilet] = 0

    repair = {
        "facility_id": toilet,
        "component_id": toilet,
        "expected_working_units": 0,
        "restored_working_units": base.facilities[toilet].capacity,
    }
    worlds["kernel_repair"] = _kernel_scheduled(
        _kernel_action("facility_repair", "facility", toilet, repair, end=None), edit=broken
    )
    worker = next(id for id, item in sorted(base.staff.items()) if "support" in item.skills)
    gathering = sorted({agent.zone_id for agent in base.agents.values()})[0]
    order = {"task_kind": "patrol", "target_zone_id": gathering, "reason": "a test order"}
    worlds["kernel_order"] = _kernel_scheduled(
        _kernel_action(
            "staff_task_order", "staff", worker, order, start=base.minute_index, end=None
        )
    )
    return worlds


# ---- generated_texts.json: test_world_view_row.py ------------------------------------------------


def _views_run_world() -> tuple[Any, Any]:
    """`test_each_view_of_a_run_has_the_bits_of_the_python_rule`: the generated Scenario, 120
    minutes long."""
    from sheltersim.core.world import WorldState
    from sheltersim.experiments.scenario import ScenarioSpec

    document = _generated_scenario().model_dump(mode="json")
    document["duration_minutes"] = 120
    world = WorldState.from_scenario(ScenarioSpec.model_validate(document))
    return world, _controller(world)


def _views_claims_world() -> tuple[Any, Any]:
    """`test_views_with_false_claims_have_the_bits_of_the_python_rule`: 60 minutes of the generated
    Scenario with a rumor from A001 and its correction, every contact passing a claim on."""
    from sheltersim.core.world import WorldState
    from sheltersim.experiments.scenario import ScenarioSpec
    from support.fixture_scenarios import (
        _information_spread_config,
        _information_spread_scenario,
    )

    document = _generated_scenario().model_dump(mode="json")
    events = _information_spread_scenario(duration=60, correction=True)
    document["duration_minutes"] = 60
    document["scheduled_events"] = events.model_dump(mode="json")["scheduled_events"]
    document["scheduled_events"][0]["payload"]["origin_agent_id"] = "A001"
    configs = document["model_configs"]
    configs["information_spread"] = configs["information_spread"] | _information_spread_config()
    world = WorldState.from_scenario(ScenarioSpec.model_validate(document))
    return world, _controller(world)


def _views_form_world(*, information: bool, life_context: bool) -> tuple[Any, Any]:
    """`test_the_rust_view_hash_covers_each_form_of_the_document`: the generated world, its
    controller made first, without the information namespace or the life context. The pytest
    file's forms of `schema_version` 6 are these worlds again (the generated world is v6 and has
    its resource_service config)."""
    from sheltersim.core.world import WorldState

    world = WorldState.from_scenario(_generated_scenario())
    controller = _controller(world)
    assert world.schema_version == 6 and "resource_service" in world.model_configs
    if not life_context:
        world.model_state.pop("body_state_v1", None)
        world.model_configs.pop("population_context", None)
        world.model_state.pop("population_context_v1", None)
    assert "information_spread_v1" in world.model_state
    if not information:
        world.model_state.pop("information_spread_v1", None)
    return world, controller


def _view_worlds() -> dict[str, tuple[Any, Any]]:
    worlds = {"views_run": _views_run_world(), "views_claims": _views_claims_world()}
    for information in (False, True):
        for life_context in (False, True):
            if information and life_context:
                continue
            parts = (
                "information" if information else "no_information",
                "life" if life_context else "no_life",
            )
            name = "views_" + "_".join(parts)
            worlds[name] = _views_form_world(information=information, life_context=life_context)
    return worlds


def generated_variants(base: Any, generated: dict[str, Any]) -> dict[str, Any]:
    """The entries of lane M7 in `generated_texts.json`, as variants of `generated` (the export of
    `base`)."""
    out: dict[str, Any] = {}
    for name, world in _kernel_worlds(base).items():
        entry = export_world(world, _controller(world))
        out[name] = variant("generated", generated, entry)
        if not name.endswith("_started"):
            out[name]["contracts"] = _parameter_contracts(world)
    for name, (world, controller) in _view_worlds().items():
        # A form without the life context is no world `validate` takes: the view reads it as it is.
        entry = export_world(world, controller, validate=False)
        out[name] = variant("generated", generated, entry)
    return out


# ---- resource_service_texts.json: test_effect_guard_texts.py -------------------------------------

# The guards test_effect_guard_texts.py gives a program it schedules, by name.
SCHEDULED_GUARDS: dict[str, dict[str, Any]] = {
    "holds": {"operator": "gte", "field": "target.working_units", "value": 1},
    "false": {"operator": "gt", "field": "target.working_units", "value": 1},
    "past_64_bits": {"operator": "lt", "field": "simulation.minute", "value": 2**70},
    "below_64_bits": {"operator": "lt", "field": "simulation.minute", "value": -(2**70)},
    "at_2_63": {"operator": "lt", "field": "simulation.minute", "value": 2**63},
    "below_minus_2_63": {"operator": "lt", "field": "simulation.minute", "value": -(2**63) - 1},
}

# The guards the contract turns down in test_effect_guard_texts.py.
REFUSED_GUARDS: list[dict[str, Any]] = [
    {"operator": "eq"},
    {"operator": "eq", "field": "no.such.field", "value": 1},
    {"operator": "gt", "field": "simulation.minute", "value": "x"},
    {
        "operator": "and",
        "operands": [{"operator": "eq", "field": "simulation.minute", "value": 0}],
    },
    {"operator": "eq", "field": "simulation.minute", "value": 0, "other": 1},
]


def _gate_scenario() -> Any:
    """`scenario` of test_effect_guard_texts.py: the gate's generated Scenario."""
    root = str(Path(__file__).resolve().parents[2] / "benchmarks")
    if root not in sys.path:
        sys.path.insert(0, root)
    import gate_scenarios

    return gate_scenarios.generated(evacuees=20, minutes=120)


def _guarded_program(guard: dict[str, Any]) -> Any:
    """`_program` of test_effect_guard_texts.py: one facility availability change at minute 0."""
    from sheltersim.events.effects import (
        DEFAULT_PRIMITIVE_REGISTRY,
        EffectProgram,
        GuardExpression,
        SupportMode,
        TargetSelector,
        TimedAction,
    )

    spec = DEFAULT_PRIMITIVE_REGISTRY.get("set_facility_availability")
    action = TimedAction(
        effect_id="guarded",
        op="set_facility_availability",
        target_selector=TargetSelector(kind="facility", target_id="toilet_01"),
        start_minute=0,
        end_minute=30,
        parameters={"working_units": 0},
        units=sorted(spec.units)[0],
        source_id="TEST-GUARD",
        assumption_ids=[],
        policy_dependencies=sorted(spec.required_policies),
        reversible=True,
        guard=GuardExpression.model_validate(guard),
    )
    return EffectProgram(
        program_id="program-guarded",
        narrative="guarded",
        source_text="guarded",
        simulation_start_minute=0,
        effects=[action],
        assumptions=[],
        support_mode=SupportMode.NATIVE,
        compiler_provenance={"compiler_kind": "offline_rule", "compiler_version": "test-v1"},
    )


def _frozen(guard: dict[str, Any]) -> Any:
    """The guard as the dispatcher read it from an event's marker: frozen, as `ScheduledEvent`
    keeps its provenance (pydantic's text of a refused input shows its type)."""
    from sheltersim.events.models import EventKind, ScheduledEvent

    event = ScheduledEvent(
        event_id="guarded",
        kind=EventKind.INTERVENTION,
        due_minute=0,
        priority=0,
        sequence=0,
        event_type="effect_program_apply",
        payload={},
        provenance={"effect_program_v1": {"guard": guard}},
    )
    return event.provenance["effect_program_v1"]["guard"]


def _gate_world() -> tuple[Any, Any]:
    from sheltersim.core.world import WorldState

    world = WorldState.from_scenario(copy.deepcopy(_gate_scenario()))
    return world, _controller(world)


def gate_variants(visiting: dict[str, Any]) -> dict[str, Any]:
    """The entries of lane M7 in `resource_service_texts.json`, as variants of `visiting`."""
    from sheltersim.events.effects import GuardExpression, schedule_effect_program

    world, controller = _gate_world()
    gate = export_world(world, controller)
    out = {"gate": variant("visiting", visiting, gate)}
    out["gate"]["contracts"] = [
        {"model": "GuardExpression", "input": guard, **_contract(GuardExpression, _frozen(guard))}
        for guard in REFUSED_GUARDS
    ]
    for name, guard in SCHEDULED_GUARDS.items():
        world, controller = _gate_world()
        schedule_effect_program(world, _guarded_program(guard))
        out[f"gate_{name}"] = variant("gate", gate, export_world(world, controller))
    return out
