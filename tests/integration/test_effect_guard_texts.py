"""An EffectProgram marker's guard in the Rust dispatch loop: the Python dispatcher's outcome.

`EventDispatcher.dispatch_one` asks `effect_event_guard_or_suppress` before an event's handler. The
loop in rules/dispatch.rs asks it in Rust: a false guard is a suppressed event, a guard the contract
turns down or with no target a failed one, each with the Python rule's text (saved as the record's
`detail`); a guard that holds goes on to the handler. Each case runs an event through the Rust
loop, compares all that was saved with what the Python step saved from it, and checks the record
against the outcome the guard's Python rule gave. Both are gone (`kernel_only`,
docs/rust-port/lanes/l8-texts-events.md, and lane L10 for the step): the outcome is written down
from the rule, and what the step saved is a digest taken once from it (45a9bc9), pinned on Linux,
as the gate's hashes are.
"""

from __future__ import annotations

import hashlib
import json
import sys
from collections.abc import Callable
from pathlib import Path
from typing import Any

import pytest

from sheltersim import native
from sheltersim.core.world import WorldState
from sheltersim.events.effects import (
    DEFAULT_PRIMITIVE_REGISTRY,
    EffectProgram,
    GuardExpression,
    SupportMode,
    TargetSelector,
    TimedAction,
    schedule_effect_program,
)
from sheltersim.events.models import EventKind, EventStatus, ScheduledEvent
from sheltersim.experiments.runner import OfflineSimulationController

KEY = "effect_program_v1"


class Sink:
    """What the controller writes to, which these tests don't read."""

    def __getattr__(self, name: str) -> Callable[..., None]:
        return lambda *arguments, **options: None


def _gate() -> Any:
    root = str(Path(__file__).resolve().parents[2] / "benchmarks")
    if root not in sys.path:
        sys.path.insert(0, root)
    import gate_scenarios

    return gate_scenarios


@pytest.fixture(scope="module")
def scenario() -> Any:
    return _gate().generated(evacuees=20, minutes=120)


def _counts() -> dict[str, int]:
    return dict(native.rules("dispatch").stats()[0])


def _saved(controller: OfflineSimulationController) -> dict[str, Any]:
    world = controller.world
    return {
        "records": [item.decode() for item in world.event_log.items_bytes(0, len(world.event_log))],
        "pending": [event.to_dict() for event in world.event_queue.pending()],
        "counters": dict(world.id_counters),
        # Not `canonical_json()`: its check turns down a marker of no scheduled program.
        "world": world._canonical_text(world._canonical_data()),
    }


def _marked(event_type: str, payload: dict[str, Any], guard: Any) -> ScheduledEvent:
    return ScheduledEvent(
        event_id="guarded-1",
        kind=EventKind.INTERVENTION,
        due_minute=0,
        priority=0,
        sequence=900_001,
        event_type=event_type,
        payload=payload,
        source_id="TEST-GUARD",
        causation_id=None,
        provenance={KEY: {"guard": guard, "program_id": "no-such-program"}},
    )


def _program(guard: dict[str, Any]) -> EffectProgram:
    """One facility availability change at minute 0, with this guard."""
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


def _scheduled(guard: dict[str, Any]) -> Callable[[WorldState], str]:
    """The program's start event, scheduled by `schedule_effect_program` in a valid world."""

    def arrange(world: WorldState) -> str:
        schedule_effect_program(world, _program(guard))
        return next(
            event.event_id
            for event in world.event_queue.pending()
            if event.event_type == "effect_program_apply"
        )

    return arrange


def _dispatched(
    scenario: Any, event: ScheduledEvent | Callable[[WorldState], str], python_saved: str
) -> tuple[Any, dict[str, int], dict[str, int]]:
    """The event's record from the Rust loop, which saves what the Python step saved: the digest
    `python_saved` of `_saved`.

    A crafted marker of no program makes a world `validate()` turns down, which neither the guard
    nor the dispatch loop reads; an event whose guard holds goes to its handler, so it comes from a
    program scheduled as an operator's would be.
    """
    controller = OfflineSimulationController(WorldState.from_scenario(scenario), Sink())
    if isinstance(event, ScheduledEvent):
        controller.world.schedule_event(event)
        event_id = event.event_id
    else:
        event_id = event(controller.world)
    before = _counts()
    controller._run_dispatch_phase(controller.world, 0)
    after = _counts()
    text = json.dumps(_saved(controller), sort_keys=True)
    assert hashlib.sha256(text.encode("utf-8")).hexdigest()[:16] == python_saved
    record = controller.world.event_log.find(event_id)
    return record, before, after


def _grew(before: dict[str, int], after: dict[str, int], *names: str) -> None:
    for name in names:
        assert after[name] == before[name] + 1, name


FACILITY = {"facility_id": "toilet_01", "working_units": 0}


def test_a_false_guard_suppresses_the_event_in_rust(scenario: Any) -> None:
    guard = {"operator": "lt", "field": "simulation.minute", "value": 0}
    record, before, after = _dispatched(
        scenario, _marked("effect_program_apply", FACILITY, guard), "4e2ff7aa6f0fc988"
    )
    assert (record.status, record.detail) == (
        EventStatus.SUPPRESSED,
        "EffectProgram guard evaluated false",
    )
    _grew(before, after, "guard_suppressed", "events_rust")
    assert after["events_python"] == before["events_python"]


@pytest.mark.parametrize(
    ("guard", "python_saved"),
    [
        ({"operator": "eq"}, "85aca6254326f09a"),
        ({"operator": "eq", "field": "no.such.field", "value": 1}, "5e0f37e2c43462d3"),
        ({"operator": "gt", "field": "simulation.minute", "value": "x"}, "27570f25deea21d6"),
        (
            {
                "operator": "and",
                "operands": [{"operator": "eq", "field": "simulation.minute", "value": 0}],
            },
            "e7283102ac28014d",
        ),
        (
            {"operator": "eq", "field": "simulation.minute", "value": 0, "other": 1},
            "dc0e06448ea5b32f",
        ),
    ],
)
def test_a_guard_the_contract_turns_down_fails_with_its_text(
    scenario: Any, guard: dict[str, Any], python_saved: str
) -> None:
    record, before, after = _dispatched(
        scenario, _marked("effect_program_apply", FACILITY, guard), python_saved
    )
    assert record.status is EventStatus.FAILED
    assert record.detail.startswith("EffectProgram guard is invalid: ")
    _grew(before, after, "guard_failed", "events_rust")


@pytest.mark.parametrize(
    ("field", "payload", "text", "python_saved"),
    [
        (
            "target.capacity",
            {},
            "guard field 'target.capacity' has no facility target",
            "0c5b9a97be5e1e4b",
        ),
        (
            "target.working_units",
            {"facility_id": "no-such-facility"},
            "guard field 'target.working_units' has no facility target",
            "8ae7aec5b1626e62",
        ),
        (
            "target.queue_length",
            {"facility_id": ""},
            "guard field 'target.queue_length' has no facility target",
            "6f9d918df8bceef5",
        ),
        (
            "zone.occupancy",
            {},
            "guard zone.occupancy has no known zone target",
            "e8e7ce315ac96ba7",
        ),
        (
            "zone.occupancy",
            {"zone_id": "nowhere"},
            "guard zone.occupancy has no known zone target",
            "149aeef2dba32364",
        ),
        (
            "zone.occupancy",
            {"target_id": "nowhere"},
            "guard zone.occupancy has no known zone target",
            "9955e50f1b9ce2ee",
        ),
        (
            "resource.available_quantity",
            {"resource_id": "other"},
            "guard resource.available_quantity has no ledger target",
            "0ee0ab9b611a9831",
        ),
        (
            "resource.available_quantity",
            {},
            "guard resource.available_quantity has no ledger target",
            "b6928ccb8daaaf5c",
        ),
        (
            "staff.available",
            {"staff_id": "nobody"},
            "guard staff.available has no known staff target",
            "9bfbcfbdfdeb96a5",
        ),
        (
            "staff.available",
            {},
            "guard staff.available has no known staff target",
            "78eb1a948feea5d7",
        ),
        (
            "staff.available",
            {"staff_id": ""},
            "guard staff.available has no known staff target",
            "02cc177610302683",
        ),
    ],
)
def test_a_guard_without_its_target_fails_with_python_s_text(
    scenario: Any, field: str, payload: dict[str, Any], text: str, python_saved: str
) -> None:
    guard = {"operator": "gte", "field": field, "value": 0}
    if field == "staff.available":
        guard["operator"], guard["value"] = "eq", True
    record, before, after = _dispatched(
        scenario, _marked("effect_program_apply", payload, guard), python_saved
    )
    assert (record.status, record.detail) == (EventStatus.FAILED, text)
    _grew(before, after, "guard_failed", "events_rust")


def test_a_later_operand_is_read_only_after_those_before_it(scenario: Any) -> None:
    """`and` stops at a false operand, so the missing target after it is never read."""
    first_false = {
        "operator": "and",
        "operands": [
            {"operator": "lt", "field": "simulation.minute", "value": 0},
            {"operator": "gte", "field": "target.capacity", "value": 0},
        ],
    }
    record, _, _ = _dispatched(
        scenario, _marked("effect_program_apply", {}, first_false), "899ecabfe3dd6d97"
    )
    assert record.detail == "EffectProgram guard evaluated false"
    first_true = {**first_false, "operator": "or"}
    first_true["operands"] = [
        {"operator": "gte", "field": "target.capacity", "value": 0},
        {"operator": "gte", "field": "simulation.minute", "value": 0},
    ]
    record, _, _ = _dispatched(
        scenario, _marked("effect_program_apply", {}, first_true), "3a129a58d52ea5b8"
    )
    assert record.detail == "guard field 'target.capacity' has no facility target"


def test_a_guard_that_holds_goes_on_to_the_handler(scenario: Any) -> None:
    guard = {"operator": "gte", "field": "target.working_units", "value": 1}
    record, before, after = _dispatched(scenario, _scheduled(guard), "af71b64a3089f6b2")
    assert (record.status, record.detail) == (EventStatus.APPLIED, None)
    _grew(before, after, "guard_passed", "others_applied", "events_rust")
    assert after["events_python"] == before["events_python"]


def test_a_false_guard_of_a_scheduled_program_suppresses_its_start(scenario: Any) -> None:
    guard = {"operator": "gt", "field": "target.working_units", "value": 1}
    record, before, after = _dispatched(scenario, _scheduled(guard), "6a554483ede70f5b")
    assert (record.status, record.detail) == (
        EventStatus.SUPPRESSED,
        "EffectProgram guard evaluated false",
    )
    _grew(before, after, "guard_suppressed", "events_rust")


def test_an_event_without_a_handler_is_python_s_to_record(scenario: Any) -> None:
    guard = {"operator": "lt", "field": "simulation.minute", "value": 0}
    record, before, after = _dispatched(
        scenario, _marked("no_such_event", {}, guard), "8a9bb286b8074ae8"
    )
    assert (record.status, record.detail) == (
        EventStatus.FAILED,
        "no handler registered for event type 'no_such_event'",
    )
    _grew(before, after, "events_python")
    assert after["guard_suppressed"] == before["guard_suppressed"]


def test_a_marked_event_of_the_table_is_python_s(scenario: Any) -> None:
    guard = {"operator": "lt", "field": "simulation.minute", "value": 0}
    record, before, after = _dispatched(
        scenario, _marked("support_request_timeout", {}, guard), "5141dc1f7e535ad5"
    )
    assert (record.status, record.detail) == (
        EventStatus.SUPPRESSED,
        "EffectProgram guard evaluated false",
    )
    _grew(before, after, "events_python")
    assert after["guard_suppressed"] == before["guard_suppressed"]


@pytest.mark.parametrize(
    ("value", "applies", "python_saved"),
    [
        (2**70, True, "7570f31750059065"),
        (-(2**70), False, "d520f89364bda8e5"),
        (2**63, True, "9b401386111f6f68"),
        (-(2**63) - 1, False, "1678a1db6ae904e1"),
    ],
)
def test_a_guard_value_past_64_bits_compares_by_value_in_rust(
    scenario: Any, value: int, applies: bool, python_saved: str
) -> None:
    """An int past 64 bits is the contract's to take (StrictInt), and Rust compares it as Python."""
    guard = {"operator": "lt", "field": "simulation.minute", "value": value}
    record, before, after = _dispatched(scenario, _scheduled(guard), python_saved)
    if applies:
        assert (record.status, record.detail) == (EventStatus.APPLIED, None)
        _grew(before, after, "guard_passed", "events_rust")
    else:
        assert (record.status, record.detail) == (
            EventStatus.SUPPRESSED,
            "EffectProgram guard evaluated false",
        )
        _grew(before, after, "guard_suppressed", "events_rust")
    assert after["guard_refused"] == before["guard_refused"]
