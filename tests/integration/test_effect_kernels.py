"""The Python entries of events/effects.py's rules in Rust, and what stays Python's.

native/src/rules/effects.rs (docs/rust-port/lanes/m6-effects.md). The handlers, the guard and the
modifier checks have no Python rule left (`kernel_only`); their cases are the core's tests
(native/core/tests/effect_kernels.rs, Issue #225): each handler's success, outcome and texts. Here
each entry reaches its kernel; a contract's own text (pydantic's) is the contract's; the guard is
compared with `_guard_answer` below, the rule restated, and what the contract turns down with the
contract. `validate_effect_program_state` keeps its Python rule, the full check of a loaded state
(decision D3), and is still compared with it; the state's row and its mirror are Python's.
"""

from __future__ import annotations

import copy
import json
import random
from collections.abc import Mapping
from typing import Any

import pytest

from sheltersim import native
from sheltersim.core.state_mirror import rust_validated
from sheltersim.core.world import WorldState
from sheltersim.domain.operational import staff_is_available
from sheltersim.events.effects import (
    _PARAMETER_MODELS,
    DEFAULT_PRIMITIVE_REGISTRY,
    EFFECT_EVENT_TYPES,
    EffectProgram,
    EffectProgramEnvironmentHandler,
    EffectProgramFacilityHandler,
    EffectProgramStructureHandler,
    EffectProgramTemporarySpaceHandler,
    GuardExpression,
    SupportMode,
    TargetSelector,
    TimedAction,
    has_pending_facility_modifier,
    schedule_effect_program,
    should_apply_effect_guard,
    validate_effect_program_state,
)
from sheltersim.events.models import EventApplicationError, EventSuppressed, ScheduledEvent
from sheltersim.experiments.scenario import ScenarioSpec

KEY = "effect_program_v1"
START, END = 5, 30


@pytest.fixture(scope="module")
def rules() -> Any:
    return native.rules("effects")


@pytest.fixture
def world(generated_scenario_preview) -> WorldState:
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
        source_id="TEST-EFFECT-KERNELS",
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


def _event(world: WorldState, event_type: str) -> ScheduledEvent:
    found = [item for item in world.event_queue.pending() if item.event_type == event_type]
    assert len(found) == 1, event_type
    return found[0]


def _copy(world: WorldState) -> WorldState:
    return WorldState.from_dict(world.to_dict())


def _text(world: WorldState) -> str:
    return json.dumps(world.to_dict(), ensure_ascii=False)


def _python_s_outcome(
    kernel: Any,
    handler: Any,
    world: WorldState,
    event: ScheduledEvent,
    expected: tuple[type, str],
) -> str:
    """The kernel raises the deleted Python rule's outcome itself, changing nothing.

    `expected` is that rule's class and text, written down from it.
    """
    before = _text(world)
    with pytest.raises((EventSuppressed, EventApplicationError)) as made:
        kernel(handler, world, event)
    assert _text(world) == before
    assert (type(made.value), str(made.value)) == expected
    return str(made.value)


def _a_facility(world: WorldState, kind: str = "toilet") -> str:
    return sorted(id for id, item in world.facilities.items() if item.kind == kind)[0]


def _a_zone(world: WorldState) -> str:
    return sorted(world.zones)[0]


def test_the_kernel_is_the_one_that_runs(world: WorldState, rules: Any) -> None:
    """A kernel can say Unsupported on every call while everything else passes."""
    facility = _a_facility(world)
    schedule_effect_program(
        world,
        _program(_action("set_facility_availability", "facility", facility, {"working_units": 0})),
    )
    before, _ = rules.stats()
    EffectProgramFacilityHandler().apply(world, _event(world, "effect_program_apply"))
    after, _ = rules.stats()
    assert after["facility"] == before.get("facility", 0) + 1
    assert world.facilities[facility].working_units == 0


# ---- The handlers ----------------------------------------------------------------------------


def test_invalid_zone_parameters_are_the_contract_s_text(world: WorldState, rules: Any) -> None:
    schedule_effect_program(
        world, _program(_action("set_lighting", "zone", _a_zone(world), {"level": 1.0}))
    )
    document = _event(world, "effect_program_environment").to_dict()
    document["payload"]["parameters"] = {"level": 9.0}
    event = ScheduledEvent(**document)
    with pytest.raises(ValueError) as contract:
        _PARAMETER_MODELS["set_lighting"].model_validate({"level": 9.0})
    text = f"zone environment parameters are invalid: {contract.value}"
    handler = EffectProgramEnvironmentHandler()
    _python_s_outcome(rules.environment, handler, world, event, (EventApplicationError, text))
    assert text.startswith("zone environment parameters are invalid: 1 validation error")


def test_a_temporary_space_starts_through_its_entry(world: WorldState, rules: Any) -> None:
    """The entry of the temporary-space handler reaches the kernel (the rule's cases are
    native/core/tests/effect_kernels.rs)."""
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
    assert rules.temporary_space(EffectProgramTemporarySpaceHandler(), world, start) is None
    space = world.model_state["temporary_spaces_v1"]["spaces"][start.payload["modifier_id"]]
    assert (space["zone_id"], space["active"]) == (zone, True)


def _structure_event(world: WorldState, op: str) -> ScheduledEvent:
    found = [
        item
        for item in world.event_queue.pending()
        if item.event_type == "effect_program_structure" and item.payload.get("op") == op
    ]
    assert len(found) == 1
    return found[0]


def test_a_role_change_s_contract_stays_python_s(world: WorldState, rules: Any) -> None:
    """Skills the contract turns down are its own text, as the rule wrapped its ValueError (the
    change itself is native/core/tests/effect_kernels.rs)."""
    staff_id = sorted(world.staff)[0]
    schedule_effect_program(
        world,
        _program(
            _action("set_staff_role", "staff", staff_id, {"skills": ["zeta", "alpha"]}, end=None)
        ),
    )
    document = _structure_event(world, "set_staff_role").to_dict()
    document["payload"]["skills"] = ["same", "same"]
    changed = ScheduledEvent(**document)
    with pytest.raises(ValueError) as contract:
        _PARAMETER_MODELS["set_staff_role"].model_validate({"skills": changed.payload["skills"]})
    text = f"staff-role event skills are invalid: {contract.value}"
    handler = EffectProgramStructureHandler()
    _python_s_outcome(rules.structure, handler, world, changed, (EventApplicationError, text))
    assert "staff role skills must be unique" in text


# ---- The guard --------------------------------------------------------------------------------


def _guarded(world: WorldState, guard: dict[str, Any], payload: dict[str, Any]) -> ScheduledEvent:
    return ScheduledEvent(
        event_id="guarded-event",
        kind="intervention",
        due_minute=1,
        priority=0,
        sequence=999_999,
        event_type="effect_program_apply",
        payload=payload,
        source_id=None,
        causation_id=None,
        provenance={KEY: {"guard": guard, "program_id": "no-such-program"}},
    )


def _guard_cases(world: WorldState) -> list[tuple[dict[str, Any], dict[str, Any]]]:
    facility = _a_facility(world)
    zone = world.facilities[facility].zone_id
    staff_id = sorted(world.staff)[0]
    capacity = world.facilities[facility].capacity
    values: list[Any] = [
        0,
        1,
        capacity,
        capacity + 1,
        0.0,
        1.0,
        float(capacity),
        capacity + 0.5,
        -1.5,
        True,
        False,
        "1",
        "",
    ]
    payloads = (
        {"facility_id": facility},
        {"zone_id": zone},
        {"staff_id": staff_id},
        {"resource_id": "water_ml"},
        {"resource_id": "other"},
        {"target_id": facility},
        {"facility_id": "no-such-facility"},
        {},
    )
    cases: list[tuple[dict[str, Any], dict[str, Any]]] = []
    for field in (
        "simulation.minute",
        "target.capacity",
        "target.working_units",
        "target.queue_length",
        "zone.occupancy",
        "resource.available_quantity",
        "staff.available",
        "no.such.field",
    ):
        for operator in ("eq", "ne", "gt", "gte", "lt", "lte", "other"):
            for value in values:
                for payload in payloads:
                    cases.append(({"operator": operator, "field": field, "value": value}, payload))
    return cases


def _guard_answer(world: WorldState, event: ScheduledEvent) -> bool:
    """`should_apply_effect_guard` as its Python rule was (events/effects.py before lane L10).

    The rule is gone (`kernel_only`); this restates it as what the kernel must answer.
    """
    marker = event.provenance.get(KEY)
    guard = marker.get("guard") if isinstance(marker, Mapping) else None
    if guard is None:
        return True
    try:
        expression = GuardExpression.model_validate(guard)
    except ValueError as exc:
        raise EventApplicationError(f"EffectProgram guard is invalid: {exc}") from exc
    return _guard_node(world, event, expression)


def _guard_node(world: WorldState, event: ScheduledEvent, guard: GuardExpression) -> bool:
    if guard.operator == "and":
        return all(_guard_node(world, event, item) for item in guard.operands)
    if guard.operator == "or":
        return any(_guard_node(world, event, item) for item in guard.operands)
    field_name = guard.field
    target_id = next(
        (
            event.payload.get(name)
            for name in ("facility_id", "staff_id", "zone_id", "resource_id", "target_id")
            if isinstance(event.payload.get(name), str)
        ),
        None,
    )
    if target_id is None:
        program_id = event.provenance.get(KEY, {}).get("program_id")
        program = world.model_state.get(KEY, {}).get("programs", {}).get(program_id)
        if program and len(program.get("effect_ids", [])) == 1:
            target_id = event.payload.get("shipment_id")
    if field_name == "simulation.minute":
        actual: Any = world.minute_index
    elif field_name in {"target.capacity", "target.working_units", "target.queue_length"}:
        facility = world.facilities.get(target_id or "")
        if facility is None:
            raise EventApplicationError(f"guard field {field_name!r} has no facility target")
        actual = {
            "target.capacity": facility.capacity,
            "target.working_units": facility.working_units,
            "target.queue_length": len(facility.queue_agent_ids)
            + len(facility.resource_queue_agent_ids),
        }[field_name]
    elif field_name == "zone.occupancy":
        zone_id = event.payload.get("zone_id") or (
            world.facilities.get(target_id or "").zone_id
            if target_id in world.facilities
            else target_id
        )
        if zone_id not in world.zones:
            raise EventApplicationError("guard zone.occupancy has no known zone target")
        actual = sum(agent.zone_id == zone_id for agent in world.agents.values())
    elif field_name == "resource.available_quantity":
        ledger = world.model_state.get("resource_ledger_v1")
        resource_id = event.payload.get("resource_id")
        if not isinstance(ledger, dict) or resource_id not in ledger["accounts"]:
            raise EventApplicationError("guard resource.available_quantity has no ledger target")
        account = ledger["accounts"][resource_id]
        actual = account["remaining"] - account["reserved"]
    elif field_name == "staff.available":
        staff_id = event.payload.get("staff_id") or target_id
        if staff_id not in world.staff:
            raise EventApplicationError("guard staff.available has no known staff target")
        actual = staff_is_available(world, staff_id)
    else:
        raise EventApplicationError(f"guard field {field_name!r} is not implemented")
    expected = guard.value
    numeric = all(
        isinstance(item, (int, float)) and not isinstance(item, bool) for item in (actual, expected)
    )
    if guard.operator == "eq":
        return (
            actual == expected if numeric else type(actual) is type(expected) and actual == expected
        )
    if guard.operator == "ne":
        return (
            actual != expected
            if numeric
            else type(actual) is not type(expected) or actual != expected
        )
    if not isinstance(actual, (int, float)) or isinstance(actual, bool):
        return False
    return {
        "gt": actual > expected,
        "gte": actual >= expected,
        "lt": actual < expected,
        "lte": actual <= expected,
    }[guard.operator]


def _guard_outcome(call: Any) -> Any:
    """The answer, or (EventApplicationError, its text)."""
    try:
        return call()
    except EventApplicationError as said:
        return EventApplicationError, str(said)


def test_each_guard_is_python_s_answer_or_python_s_to_give(world: WorldState, rules: Any) -> None:
    generator = random.Random(7)
    cases = _guard_cases(world)
    generator.shuffle(cases)
    answered = said = 0
    for guard, payload in cases[:6000]:
        event = _guarded(world, guard, payload)
        expected = _guard_outcome(lambda event=event: _guard_answer(world, event))
        answer = _guard_outcome(lambda event=event: rules.should_apply_guard(world, event))
        assert answer == expected, (guard, payload, answer, expected)
        if isinstance(answer, bool):
            answered += 1
        else:
            # An invalid guard or a missing target: the Python rule's text (l8-texts-events.md).
            said += 1
    assert answered > 500 and said > 500


def _random_guard(generator: random.Random, depth: int = 1) -> dict[str, Any]:
    if depth < 6 and generator.random() < 0.5:
        count = generator.choice((1, 2, 2, 3, 4, 5))
        return {
            "operator": generator.choice(("and", "or")),
            "operands": [_random_guard(generator, depth + 1) for _ in range(count)],
        }
    guard: dict[str, Any] = {
        "operator": generator.choice(("eq", "ne", "gt", "gte", "lt", "lte")),
        "field": generator.choice(("simulation.minute", "target.capacity", "staff.available")),
        "value": generator.choice((0, 1, 1.0, 0.5, True, False, "x", "y" * 257, None)),
    }
    if generator.random() < 0.05:
        guard["other"] = 1
    if generator.random() < 0.05:
        guard["operands"] = [{"operator": "eq"}]
    return guard


def test_a_guard_the_contract_refuses_has_the_contract_s_text(
    world: WorldState, rules: Any
) -> None:
    generator = random.Random(11)
    facility = _a_facility(world)
    payload = {"facility_id": facility, "staff_id": sorted(world.staff)[0]}
    accepted = refused_by_contract = 0
    for _ in range(1500):
        guard = _random_guard(generator)
        event = _guarded(world, guard, payload)
        answer = _guard_outcome(lambda event=event: should_apply_effect_guard(world, event))
        assert answer == _guard_outcome(lambda event=event: _guard_answer(world, event))
        if not isinstance(answer, bool):
            refused_by_contract += 1
            assert answer[1].startswith("EffectProgram guard is invalid: ")
            continue
        accepted += 1
        # What the kernel takes, the contract takes.
        GuardExpression.model_validate(event.provenance[KEY]["guard"])
    assert accepted > 50 and refused_by_contract > 50


def _nested(depth: int) -> dict[str, Any]:
    leaf = {"operator": "gte", "field": "simulation.minute", "value": 0}
    node = leaf
    for _ in range(depth - 1):
        node = {"operator": "and", "operands": [node, leaf]}
    return node


def _wide(operands: int) -> dict[str, Any]:
    leaf = {"operator": "gte", "field": "simulation.minute", "value": 0}
    return {"operator": "and", "operands": [leaf] * operands}


@pytest.mark.parametrize(
    ("taken", "turned_down"),
    [
        (_nested(4), _nested(5)),
        (_wide(4), _wide(5)),
        # Each of four operands with four leaves is 21 nodes; with three of them, 17.
        (
            {"operator": "or", "operands": [_wide(4), _wide(4), _wide(4)]},
            {"operator": "or", "operands": [_wide(4), _wide(4), _wide(4), _wide(4)]},
        ),
        (
            {"operator": "eq", "field": "simulation.minute", "value": "x" * 256},
            {"operator": "eq", "field": "simulation.minute", "value": "x" * 257},
        ),
    ],
)
def test_the_bounds_of_a_guard_are_the_contract_s(
    world: WorldState, rules: Any, taken: dict[str, Any], turned_down: dict[str, Any]
) -> None:
    event = _guarded(world, taken, {})
    assert rules.should_apply_guard(world, event) is _guard_answer(world, event)
    event = _guarded(world, turned_down, {})
    with pytest.raises(EventApplicationError) as made:
        rules.should_apply_guard(world, event)
    with pytest.raises(EventApplicationError) as said:
        _guard_answer(world, event)
    assert str(made.value) == str(said.value)


def test_the_size_of_a_guard_counts_to_twenty(world: WorldState, rules: Any) -> None:
    def tree(nodes: int) -> dict[str, Any]:
        # Under the root, three-leaf ands of 4 nodes and, for what is left, leaves.
        leaf = {"operator": "gte", "field": "simulation.minute", "value": 0}
        three = {"operator": "and", "operands": [leaf, leaf, leaf]}
        wider = {"operator": "and", "operands": [leaf, leaf, leaf, leaf]}
        children = {20: [wider, wider, wider, three], 21: [wider, wider, wider, wider]}
        return {"operator": "and", "operands": children[nodes]}

    event = _guarded(world, tree(20), {})
    assert rules.should_apply_guard(world, event) is True
    event = _guarded(world, tree(21), {})
    with pytest.raises(EventApplicationError) as made:
        rules.should_apply_guard(world, event)
    with pytest.raises(ValueError, match="guard AST cannot exceed 20 nodes") as said:
        GuardExpression.model_validate(event.provenance[KEY]["guard"])
    assert str(made.value) == f"EffectProgram guard is invalid: {said.value}"


# ---- What other modules ask, and the state ----------------------------------------------------


def test_a_pending_modifier_is_seen(world: WorldState, rules: Any) -> None:
    facility = _a_facility(world)
    assert not rules.has_pending_modifier(world, facility)
    schedule_effect_program(
        world,
        _program(_action("set_facility_availability", "facility", facility, {"working_units": 0})),
    )
    assert rules.has_pending_modifier(world, facility) is True
    assert has_pending_facility_modifier(world, facility) is True
    assert not has_pending_facility_modifier(world, "no-such-facility")


def test_the_operators_and_event_types_are_the_registry_s(rules: Any) -> None:
    assert sorted(rules.OPERATORS) == sorted(
        spec.op for spec in DEFAULT_PRIMITIVE_REGISTRY.operators
    )
    for phase in ("start", "expiry", "derived"):
        for op in rules.OPERATORS:
            assert rules.event_type_of(phase, op) == EFFECT_EVENT_TYPES[phase].get(op), (phase, op)


def test_the_row_writes_the_bytes_of_the_tree(world: WorldState) -> None:
    facility = _a_facility(world)
    schedule_effect_program(
        world,
        _program(_action("set_facility_availability", "facility", facility, {"working_units": 0})),
    )
    mirror = world.model_state._mirrors[KEY]
    assert mirror.verify()
    assert mirror.tainted is None
    assert mirror.row_json() is not None


def test_scheduling_writes_no_whole_namespace_once_it_exists(world: WorldState) -> None:
    facility = _a_facility(world)
    schedule_effect_program(
        world,
        _program(_action("set_facility_availability", "facility", facility, {"working_units": 0})),
    )
    mirror = world.model_state._mirrors[KEY]
    before = mirror.stats()["whole_writes"]
    EffectProgramFacilityHandler().apply(world, _event(world, "effect_program_apply"))
    # The handler's copy is a new world with a new mirror; what it wrote is a record at a time.
    assert world.model_state._mirrors[KEY].stats()["whole_writes"] <= before


def test_a_row_taken_by_a_bad_value_goes_to_python(world: WorldState, rules: Any) -> None:
    facility = _a_facility(world)
    schedule_effect_program(
        world,
        _program(_action("set_facility_availability", "facility", facility, {"working_units": 0})),
    )
    world.model_state[KEY]["schema_version"] = 1.0
    assert world.model_state._mirrors[KEY].tainted is not None
    with pytest.raises(native.Unsupported):
        rules.validate_state(world, validate_event_history=False)
    # 1.0 == 1 passes the Python rule, so only the row is lost.
    validate_effect_program_state(world, validate_event_history=False)
    with pytest.raises(native.Unsupported):
        rules.has_active_modifier(world, facility)


def _random_write(generator: random.Random, world: WorldState) -> None:
    """One value somewhere in the namespace or its events, written as code would write it."""
    state = world.model_state[KEY]
    bad: list[Any] = [None, 0, 1, -1, 1.5, "", "x" * 2000, "bad id!", True, [], {}, [1], {"a": 1}]
    value = generator.choice(bad)
    program = next(iter(state["programs"].values()))
    target = generator.choice(
        [
            (state, "schema_version"),
            (state, "other"),
            (program, "program_hash"),
            (program, "source_text_hash"),
            (program, "narrative"),
            (program, "support_mode"),
            (program, "compiler_provenance"),
            (program, "assumptions"),
            (program, "effect_ids"),
            (program, "operators"),
            (program, "scheduled_event_ids"),
            (program, "other"),
            (program["compiler_provenance"], "compiler_kind"),
            (program["compiler_provenance"], "compiler_version"),
            (program["compiler_provenance"], "model_id"),
            (program["compiler_provenance"], "prompt_hash"),
            (program["compiler_provenance"], "other"),
            (program["operators"], generator.choice(tuple(program["operators"]))),
            (program["operators"], "other"),
        ]
        + [
            (record, key)
            for record in state["active_facility_modifiers"].values()
            for key in ("baseline_working_units", "modifiers", "other")
        ]
        + [
            (item, key)
            for record in state["active_facility_modifiers"].values()
            for item in record["modifiers"]
            for key in ("modifier_id", "working_units", "start_minute", "sequence", "other")
        ]
    )
    target[0][target[1]] = value


def test_a_state_the_check_accepts_passes_the_python_rules(world: WorldState, rules: Any) -> None:
    facility = _a_facility(world)
    schedule_effect_program(
        world,
        _program(_action("set_facility_availability", "facility", facility, {"working_units": 0})),
    )
    EffectProgramFacilityHandler().apply(world, _event(world, "effect_program_apply"))
    generator = random.Random(3)
    accepted = refused = 0
    for _ in range(300):
        broken = _copy(world)
        _random_write(generator, broken)
        for history in (True, False):
            try:
                rules.validate_state(broken, validate_event_history=history)
            except native.Unsupported:
                refused += 1
            else:
                accepted += 1
                validate_effect_program_state(broken, validate_event_history=history)
    assert accepted and refused


def test_the_state_of_a_good_world_is_accepted_with_its_history(
    world: WorldState, rules: Any
) -> None:
    rules.validate_state(world, validate_event_history=True)
    facility = _a_facility(world)
    schedule_effect_program(
        world,
        _program(_action("set_facility_availability", "facility", facility, {"working_units": 0})),
    )
    rules.validate_state(world, validate_event_history=True)
    rules.validate_state(world, validate_event_history=False)
    assert not rust_validated(world, KEY)


def _break_stack(world: WorldState, facility: str, name: str) -> None:
    record = world.model_state[KEY]["active_facility_modifiers"][facility]
    item = record["modifiers"][0]
    if name == "units_not_the_lowest":
        item["working_units"] = record["baseline_working_units"]
    elif name == "facility_not_the_lowest":
        world.facilities[facility].working_units = record["baseline_working_units"]
    elif name == "above_the_baseline":
        item["working_units"] = record["baseline_working_units"] + 1
    elif name == "baseline_above_capacity":
        record["baseline_working_units"] = world.facilities[facility].capacity + 1
    elif name == "negative_baseline":
        record["baseline_working_units"] = -1
    elif name == "empty_stack":
        record["modifiers"] = []
    elif name == "another_modifier_id":
        item["modifier_id"] = "modifier-other"
    elif name == "repeated_modifier":
        record["modifiers"].append(dict(item))
    elif name == "unknown_effect":
        item["effect_id"] = "no-such-effect"
    elif name == "negative_sequence":
        item["sequence"] = -1
    elif name == "negative_minute":
        item["start_minute"] = -1
    else:
        raise AssertionError(name)


@pytest.mark.parametrize(
    "name",
    [
        "units_not_the_lowest",
        "facility_not_the_lowest",
        "above_the_baseline",
        "baseline_above_capacity",
        "negative_baseline",
        "empty_stack",
        "another_modifier_id",
        "repeated_modifier",
        "unknown_effect",
        "negative_sequence",
        "negative_minute",
    ],
)
def test_a_stack_the_python_rule_refuses_is_refused(
    world: WorldState, rules: Any, name: str
) -> None:
    facility = _a_facility(world)
    schedule_effect_program(
        world,
        _program(_action("set_facility_availability", "facility", facility, {"working_units": 0})),
    )
    EffectProgramFacilityHandler().apply(world, _event(world, "effect_program_apply"))
    rules.validate_state(world, validate_event_history=True)
    _break_stack(world, facility, name)
    with pytest.raises(native.Unsupported):
        rules.validate_state(world, validate_event_history=True)
    with pytest.raises(ValueError):
        validate_effect_program_state(world, validate_event_history=True)


@pytest.mark.parametrize(
    ("write", "good"),
    [
        (lambda program: program.__setitem__("narrative", "x" * 1000), True),
        (lambda program: program.__setitem__("narrative", "x" * 1001), False),
        (
            lambda program: program["compiler_provenance"].__setitem__("prompt_hash", "x" * 128),
            True,
        ),
        (
            lambda program: program["compiler_provenance"].__setitem__("prompt_hash", "x" * 129),
            False,
        ),
        (lambda program: program["assumptions"].append(_assumption("x" * 500)), True),
        (lambda program: program["assumptions"].append(_assumption("x" * 501)), False),
    ],
)
def test_the_bounds_of_a_program_record_are_the_python_rule_s(
    world: WorldState, rules: Any, write: Any, good: bool
) -> None:
    facility = _a_facility(world)
    schedule_effect_program(
        world,
        _program(_action("set_facility_availability", "facility", facility, {"working_units": 0})),
    )
    write(world.model_state[KEY]["programs"]["program-1"])
    if good:
        rules.validate_state(world, validate_event_history=True)
        validate_effect_program_state(world, validate_event_history=True)
    else:
        with pytest.raises(native.Unsupported):
            rules.validate_state(world, validate_event_history=True)
        with pytest.raises(ValueError):
            validate_effect_program_state(world, validate_event_history=True)


def _assumption(description: str) -> dict[str, Any]:
    return {"assumption_id": "a-1", "description": description, "source_id": "TEST-SOURCE"}


@pytest.mark.parametrize(
    ("place", "value"),
    [
        ("op", "add_staff"),
        ("phase", "other"),
        ("phase", "derived"),
        ("units", "bad id!"),
        ("assumption_ids", ["1", 2]),
        ("source_text_hash", "0" * 64),
        ("guard", {"operator": "eq"}),
        ("other", 1),
    ],
)
def test_a_marker_the_python_rule_refuses_is_refused(
    world: WorldState, rules: Any, place: str, value: Any
) -> None:
    facility = _a_facility(world)
    schedule_effect_program(
        world,
        _program(_action("set_facility_availability", "facility", facility, {"working_units": 0})),
    )
    event = _event(world, "effect_program_apply")
    document = event.to_dict()
    document["provenance"][KEY][place] = value
    world.event_queue.replace(event.event_id, ScheduledEvent(**document))
    with pytest.raises(native.Unsupported):
        rules.validate_state(world, validate_event_history=True)
    with pytest.raises(ValueError):
        validate_effect_program_state(world, validate_event_history=True)
