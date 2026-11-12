"""An EffectProgram event's failed texts that are a Python check's own, said by the kernel: the
text of a contract's error, as each handler wraps it, and of the world's check.

The handlers' texts, case by case, are tested in the core without Python
(native/core/tests/effect_program_texts.rs, Issue #225). Its host's contracts give their input
back, so the texts of pydantic's errors stay here: each is also the test of a handler's Python
entry (the environment, temporary-space, geometry and structure handlers of events/effects.py,
native/src/rules/effects.rs). These texts are saved: the dispatcher writes them to the event's
record as `detail`. Each case checks that the kernel raises the outcome itself, that nothing
changed, and the class and text the deleted Python rule gave (support/world.py
`kernel_says_python_s_text`); a contract's text is the contract's own, as the rule wrapped it. A
handoff needs a staff operation under way, so its world is run with TickEngine to that point
(docs/rust-port/lanes/l8-texts-events.md).
"""

from __future__ import annotations

import copy
from typing import Any

import pytest
from support.world import kernel_says_python_s_text

from sheltersim.core.engine import TickEngine
from sheltersim.core.world import WorldState
from sheltersim.domain.geometry_overlay import RouteAccessParameters
from sheltersim.events.effects import (
    _PARAMETER_MODELS,
    DEFAULT_PRIMITIVE_REGISTRY,
    EffectProgram,
    EffectProgramEnvironmentHandler,
    EffectProgramGeometryHandler,
    EffectProgramStructureHandler,
    EffectProgramTemporarySpaceHandler,
    EffectProgramValidationError,
    GuardExpression,
    SupportMode,
    TargetSelector,
    TimedAction,
    schedule_effect_program,
)
from sheltersim.events.models import (
    EventApplicationError,
    EventSuppressed,
    ScheduledEvent,
)
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.experiments.timeline import _MetricSink

START, END = 5, 30
FAILED, SUPPRESSED = "EventApplicationError", "EventSuppressed"


@pytest.fixture
def world(generated_scenario_preview: dict[str, Any]) -> WorldState:
    return WorldState.from_scenario(
        ScenarioSpec.model_validate(copy.deepcopy(generated_scenario_preview["scenario"]))
    )


def _action(
    op: str,
    kind: str,
    target: str,
    parameters: dict[str, Any],
    *,
    effect_id: str = "effect-1",
    start: int = START,
    end: int | None = END,
    guard: GuardExpression | None = None,
) -> TimedAction:
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


def _program(*actions: TimedAction, program_id: str = "program-1") -> EffectProgram:
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


def _event(world: WorldState, event_type: str, op: str | None = None) -> ScheduledEvent:
    found = [
        item
        for item in world.event_queue.pending()
        if item.event_type == event_type and (op is None or item.payload.get("op") == op)
    ]
    assert len(found) == 1, event_type
    return found[0]


def _changed(event: ScheduledEvent, payload: dict[str, Any] | None = None, **fields: Any) -> Any:
    """The event with payload fields replaced (a value of `...` drops the field)."""
    document = event.to_dict()
    for key, value in (payload or {}).items():
        if value is ...:
            del document["payload"][key]
        else:
            document["payload"][key] = value
    document.update(fields)
    return ScheduledEvent(**document)


def _says(handler: Any, world: WorldState, event: Any, outcome: str, text: str) -> None:
    """The handler's outcome is `outcome` with `text`, the kernel's."""
    classes = {FAILED: EventApplicationError, SUPPRESSED: EventSuppressed}
    said = kernel_says_python_s_text(handler, world, event, (classes[outcome], text))
    assert said == (outcome, text), said


def _says_the_contract_s(
    handler: Any, world: WorldState, event: Any, head: str, op: str, raw: Any
) -> None:
    """The failure is `head` and the text of the contract's error for `raw`, as the rule wrote it
    (`f"{head}{exc}"` of the TypeError or ValueError of `model_validate`)."""
    with pytest.raises((TypeError, ValueError)) as contract:
        _PARAMETER_MODELS[op].model_validate(raw)
    _says(handler, world, event, FAILED, f"{head}{contract.value}")


def _a_zone(world: WorldState) -> str:
    return sorted(world.zones)[0]


def _a_toilet(world: WorldState) -> str:
    return sorted(id for id, item in world.facilities.items() if item.kind == "toilet")[0]


# ---- EffectProgramEnvironmentHandler -------------------------------------------------------------


def test_a_zone_environment_parameter_the_contract_refuses_is_its_text(world: WorldState) -> None:
    schedule_effect_program(
        world, _program(_action("set_ambient_noise", "zone", _a_zone(world), {"level": 0.5}))
    )
    start = _event(world, "effect_program_environment")
    with pytest.raises(ValueError) as contract:
        _PARAMETER_MODELS["set_ambient_noise"].model_validate({"level": 9.0})
    _says(
        EffectProgramEnvironmentHandler(),
        world,
        _changed(start, {"parameters": {"level": 9.0}}),
        FAILED,
        f"zone environment parameters are invalid: {contract.value}",
    )


def test_a_start_the_check_of_the_world_turns_down_is_python_s_text(world: WorldState) -> None:
    """The handler wraps a ValueError of the copy's check: here the record of a start whose
    parameters differ from its event's in the queue. The check is a Python validator's
    (domain/environment.py), which the core's test host does not run."""
    schedule_effect_program(
        world, _program(_action("set_lighting", "zone", _a_zone(world), {"level": 0.5}))
    )
    start = _event(world, "effect_program_environment")
    _says(
        EffectProgramEnvironmentHandler(),
        world,
        _changed(start, {"parameters": {"level": 0.25}}),
        FAILED,
        "zone environment modifier is invalid: invalid zone environment state: zone environment "
        "modifier has invalid start/expiry event links",
    )


# ---- EffectProgramTemporarySpaceHandler ----------------------------------------------------------


def test_a_temporary_space_parameter_the_contract_refuses_is_its_text(world: WorldState) -> None:
    zone = _a_zone(world)
    schedule_effect_program(
        world,
        _program(
            _action(
                "add_temporary_space", "zone", zone, {"people_capacity": 5, "sleeping_capacity": 2}
            )
        ),
    )
    start = _event(world, "effect_program_temporary_space")
    invalid = _changed(start, {"parameters": {"people_capacity": 0}})
    _says_the_contract_s(
        EffectProgramTemporarySpaceHandler(),
        world,
        invalid,
        "temporary-space parameters are invalid: ",
        "add_temporary_space",
        invalid.payload["parameters"],
    )


# ---- EffectProgramGeometryHandler ----------------------------------------------------------------


def test_a_route_access_the_contract_refuses_is_its_text(world: WorldState) -> None:
    """What `apply_geometry_change` raises is the contract's text, as the Python rule words it."""
    edge = sorted(world.zone_edges)[0]
    schedule_effect_program(
        world, _program(_action("set_route_access", "route", edge, {"accessible": True}))
    )
    start = _event(world, "effect_program_geometry")
    with pytest.raises(ValueError) as contract:
        RouteAccessParameters.model_validate({"accessible": "yes"})
    _says(
        EffectProgramGeometryHandler(),
        world,
        _changed(start, {"parameters": {"accessible": "yes"}}),
        FAILED,
        str(contract.value),
    )


# ---- EffectProgramStructureHandler: additions, roles, repairs ------------------------------------


def test_a_role_change_the_contract_refuses_is_its_text(world: WorldState) -> None:
    staff_id = sorted(world.staff)[0]
    schedule_effect_program(
        world,
        _program(_action("set_staff_role", "staff", staff_id, {"skills": ["support"]}, end=None)),
    )
    event = _event(world, "effect_program_structure", "set_staff_role")
    twice = _changed(event, {"skills": ["same", "same"]})
    _says_the_contract_s(
        EffectProgramStructureHandler(),
        world,
        twice,
        "staff-role event skills are invalid: ",
        "set_staff_role",
        {"skills": twice.payload["skills"]},
    )


def test_a_repair_the_contract_refuses_is_its_text(world: WorldState) -> None:
    facility = _a_toilet(world)
    world.facilities[facility].working_units = 0
    if "infrastructure_v1" in world.model_state:
        world.model_state["infrastructure_v1"]["base_working_units"][facility] = 0
    capacity = world.facilities[facility].capacity
    parameters = {
        "facility_id": facility,
        "component_id": facility,
        "expected_working_units": 0,
        "restored_working_units": capacity,
    }
    schedule_effect_program(
        world, _program(_action("facility_repair", "facility", facility, parameters, end=None))
    )
    event = _event(world, "effect_program_structure", "facility_repair")
    partial = _changed(event, {"parameters": {"facility_id": facility}})
    _says_the_contract_s(
        EffectProgramStructureHandler(),
        world,
        partial,
        "facility repair parameters are invalid: ",
        "facility_repair",
        partial.payload["parameters"],
    )


# ---- EffectProgramStructureHandler: a handoff ----------------------------------------------------


@pytest.fixture(scope="module")
def handing_off(
    generated_scenario_preview: dict[str, Any],
) -> tuple[WorldState, ScheduledEvent, ScheduledEvent]:
    """A world where a patrol is under way and an idle patroller waits in its zone; the handoff,
    and a role change of the worker on patrol."""
    world = WorldState.from_scenario(
        ScenarioSpec.model_validate(copy.deepcopy(generated_scenario_preview["scenario"]))
    )
    engine = TickEngine(
        world, phase_handler=OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    )
    operations = world.model_state["staff_operations_v1"]["staff"]
    idle = [id for id, staff in sorted(world.staff.items()) if operations[id]["phase"] == "idle"]
    worker = next(id for id in idle if "support" in world.staff[id].skills)
    # One who does nothing but patrol: sent to the patrol's zone first, it waits there idle.
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
        """Order a patrol at the first minute the worker may take one."""
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
        pytest.fail(f"{staff_id} took no order")

    order(replacement, "send-replacement")
    for _ in range(60):
        engine.step()
        phase = world.model_state["staff_operations_v1"]["staff"][replacement]["phase"]
        if phase == "idle" and world.staff[replacement].zone_id == zone:
            break
    else:
        pytest.fail("the replacement did not come to the zone")
    order(worker, "patrol")
    for _ in range(60):
        engine.step()
        record = world.model_state["staff_operations_v1"]["staff"][worker]
        if record["phase"] == "working" and world.staff[replacement].active_task_id is None:
            break
    else:
        pytest.fail("no patrol came to work")
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
    return (
        world,
        _event(world, "effect_program_structure", "staff_handoff"),
        _event(world, "effect_program_structure", "set_staff_role"),
    )


def test_a_handoff_the_contract_refuses_is_its_text(
    handing_off: tuple[WorldState, ScheduledEvent, ScheduledEvent],
) -> None:
    base, event, _ = handing_off
    worker = event.payload["staff_id"]
    partial = _changed(event, {"parameters": {"staff_id": worker}})
    _says_the_contract_s(
        EffectProgramStructureHandler(),
        WorldState.from_dict(base.to_dict()),
        partial,
        "staff handoff parameters are invalid: ",
        "staff_handoff",
        partial.payload["parameters"],
    )
