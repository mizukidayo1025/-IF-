"""The saved texts of population.py and population_lifecycle.py are their rules', byte for byte:
one test of each Python entry of their rules.

The rules' texts, case by case, are tested in the core without Python
(native/core/tests/population_texts.rs, Issue #225). These tests keep each entry of the PyO3 crate
reaching the kernel and saying the rule's text, and the text of the command contract's own error
(pydantic's). A rule's `EventSuppressed` and `EventApplicationError` texts go to the event's record
as `detail`, so the kernel must raise what the rule raises, in the rule's order of checks, after the
rule's writes and no others. Each case reaches one check: the outcome must be the kernel's own (not
a refusal), with the rule's text. The Python rules are gone (lane L10, `kernel_only`): each case
was run with them once (2d8fb46), and the class they raised and the world they left are pinned in
PYTHON_GAVE, per test and in call order, as a digest compared on every OS, as the gate's hashes
are. docs/rust-port/lanes/l8-texts-events.md.
"""

from __future__ import annotations

import copy
import hashlib
import json
from typing import Any

import pytest
from support.world import NATIVE_OUTCOME

from sheltersim.contracts.presence import PRESENCE_NAMESPACE, PresenceState
from sheltersim.core.world import WorldState
from sheltersim.domain import population, population_lifecycle
from sheltersim.domain.models import Activity
from sheltersim.domain.population_lifecycle import (
    PopulationPresenceCommand,
    initialize_population_lifecycle_state,
)
from sheltersim.events.effects import (
    EffectProgram,
    SupportMode,
    TargetSelector,
    TimedAction,
    schedule_effect_program,
)
from sheltersim.events.handlers import EffectProgramPopulationHandler
from sheltersim.events.models import (
    EventApplicationError,
    EventKind,
    EventSuppressed,
    ScheduledEvent,
)

DYNAMICS = population.POPULATION_DYNAMICS_STATE_KEY
ROUTE = "arrival-route-test"


def _document(world: WorldState) -> str:
    text = world._canonical_text(world._canonical_data())
    return hashlib.sha256(text.encode()).hexdigest()


# ---- What the Python rules gave ---------------------------------------------------------------

PYTHON_GAVE: dict[str, list[str]] = {
    "test_a_command_the_contract_refuses_says_the_rule_s_text": [
        "0717aceb0e0c255a",
        "0717aceb0e0c255a",
        "0717aceb0e0c255a",
        "0717aceb0e0c255a",
        "0717aceb0e0c255a",
    ],
    "test_a_departure_the_rule_refuses_says_its_text": ["0717aceb0e0c255a"],
    "test_a_relocation_completion_without_its_route_says_the_rule_s_text": ["0717aceb0e0c255a"],
    "test_a_relocation_of_someone_busy_says_the_rule_s_text": ["0717aceb0e0c255a"],
    "test_an_approach_completion_the_rule_refuses_says_its_text": ["41712a52628b2785"],
    "test_the_same_arrival_again_says_the_rule_s_text": ["b111d5818c2946b7"],
}
_SCOPE: list[Any] = ["", 0]


@pytest.fixture(autouse=True)
def _pinned_scope(request: Any) -> None:
    """The test's name, with the index of each parameter: its key in PYTHON_GAVE."""
    name = request.node.originalname
    callspec = getattr(request.node, "callspec", None)
    if callspec is not None:
        name += "[" + "-".join(str(index) for index in callspec.indices.values()) + "]"
    _SCOPE[:] = [name, 0]


def _digest(*values: Any) -> str:
    parts = [
        _document(value)
        if isinstance(value, WorldState)
        else json.dumps(value, sort_keys=True, default=repr)
        for value in values
    ]
    return hashlib.sha256("\n".join(parts).encode()).hexdigest()[:16]


def _python_gave(*values: Any) -> None:
    """These values (an outcome's class, worlds) are what the Python rule gave at this call."""
    name, index = _SCOPE
    _SCOPE[1] += 1
    assert _digest(*values) == PYTHON_GAVE[name][index]


def _says(rule: Any, world: WorldState, text: str, *arguments: Any, **options: Any) -> None:
    """The rule raises `text`, its kernel's own outcome, of the class the Python rule raised and
    after the writes it made: the class and the world it left are pinned."""
    with pytest.raises((EventSuppressed, EventApplicationError)) as made:
        rule(world, *arguments, **options)
    assert str(made.value) == text
    _python_gave(type(made.value).__name__, world)
    assert getattr(made.value, NATIVE_OUTCOME, False), f"the kernel refused: {made.value}"


def _unchanged(rule: Any, world: WorldState, text: str, *arguments: Any, **options: Any) -> None:
    """`_says`, for a check before any write: the world is as it was."""
    before = _document(world)
    _says(rule, world, text, *arguments, **options)
    assert _document(world) == before


def _handler_says(
    world: WorldState, event: ScheduledEvent, text: str, kind: type = EventApplicationError
) -> None:
    """The population handler's kernel raises `kind` with `text`, its Python rule's outcome
    (written down from it: the rule is gone, lane L10), with the world unchanged."""
    handler = EffectProgramPopulationHandler()
    before = _document(world)
    with pytest.raises((EventSuppressed, EventApplicationError)) as made:
        handler.apply(world, event)
    assert (type(made.value), str(made.value)) == (kind, text)
    assert getattr(made.value, NATIVE_OUTCOME, False), f"the kernel refused: {made.value}"
    assert _document(world) == before


def _event(
    event_id: str = "arrival-event",
    minute: int = 0,
    *,
    event_type: str = "effect_program_population",
    source_id: str | None = "ASM-TEST-POPULATION-TEXTS",
    causation_id: str | None = None,
    provenance: dict[str, Any] | None = None,
    **payload: Any,
) -> ScheduledEvent:
    return ScheduledEvent(
        event_id=event_id,
        kind=EventKind.INTERVENTION,
        due_minute=minute,
        priority=0,
        sequence=900_000 + int(hashlib.sha256(event_id.encode()).hexdigest()[:4], 16),
        event_type=event_type,
        payload=payload,
        source_id=source_id,
        causation_id=causation_id,
        provenance=provenance or {},
    )


@pytest.fixture
def world(generated_scenario) -> WorldState:
    made = WorldState.from_scenario(generated_scenario)
    made.model_state.setdefault(
        PRESENCE_NAMESPACE,
        PresenceState(
            people={
                key: {
                    "agent_id": key,
                    "household_id": agent.profile.group_id,
                    "status": "admitted",
                    "since_minute": 0,
                    "transition_zone_id": agent.zone_id,
                }
                for key, agent in made.agents.items()
            }
        ).model_dump(mode="json"),
    )
    initialize_population_lifecycle_state(made)
    return made


def _zone(world: WorldState) -> str:
    return next(iter(world.agents.values())).zone_id


def _program(world: WorldState, op: str, zone: str, **parameters: Any) -> ScheduledEvent:
    """An approved EffectProgram of one population primitive at minute 1, and its event."""
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
    program = EffectProgram(
        program_id=f"population-texts-{op}",
        narrative="A population primitive whose refusals are compared.",
        source_text=op,
        simulation_start_minute=0,
        effects=[effect],
        assumptions=[],
        support_mode=SupportMode.NATIVE,
        compiler_provenance={"compiler_kind": "offline_rule", "compiler_version": "test-v1"},
    )
    schedule_effect_program(world, program)
    return next(
        event
        for event in world.event_queue.pending()
        if event.event_type == "effect_program_population"
        and event.payload.get("program_id") == program.program_id
    )


def _arrival_program(world: WorldState, zone: str, **parameters: Any) -> ScheduledEvent:
    return _program(
        world,
        "add_evacuees",
        zone,
        **{"count": 1, "household_size": 1, "mobility": "independent", "language": "ja"},
        **parameters,
    )


def _add_route(world: WorldState, zone: str) -> None:
    """An approach route from outside the building into the middle of `zone`."""
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


def _changed(event: ScheduledEvent, **fields: Any) -> ScheduledEvent:
    return ScheduledEvent(**{**event.to_dict(), "kind": event.kind, **fields})


# ---- queue_arrivals --------------------------------------------------------------------------


def test_the_same_arrival_again_says_the_rule_s_text(world) -> None:
    world.zones[_zone(world)].capacity = None
    arguments = {"event": _event("arrival-twice"), "zone_id": _zone(world), "count": 2}
    assert population.queue_arrivals(world, **arguments) == 2
    _unchanged(
        population.queue_arrivals,
        world,
        "population arrival generated a duplicate stable ID",
        **arguments,
    )


# ---- apply_arrival_approach_completion ------------------------------------------------------


def _walking(world: WorldState) -> tuple[ScheduledEvent, dict[str, Any]]:
    """One arrival walking the approach route, and its completion event."""
    zone = _zone(world)
    _add_route(world, zone)
    event = _arrival_program(world, zone, approach_route_id=ROUTE)
    world.minute_index = event.due_minute
    population.queue_arrivals(world, event=event, zone_id=zone, count=1, approach_route_id=ROUTE)
    (pending,) = world.model_state[DYNAMICS]["pending_arrivals"]
    completion = next(
        item
        for item in world.event_queue.pending()
        if item.event_id == pending["approach_completion_event_id"]
    )
    return completion, pending


def test_an_approach_completion_the_rule_refuses_says_its_text(world) -> None:
    completion, _ = _walking(world)
    _unchanged(
        population.apply_arrival_approach_completion,
        world,
        "arrival approach completion payload fields do not match",
        _changed(completion, event_type="population_approach_finished"),
    )


# ---- queue_departures ------------------------------------------------------------------------


def test_a_departure_the_rule_refuses_says_its_text(world) -> None:
    arguments = {"event": _event("departure"), "zone_id": "no-such-zone", "count": 1}
    _unchanged(
        population.queue_departures,
        world,
        "population departure references an unknown zone",
        **arguments,
    )


# ---- apply_population_presence_command ------------------------------------------------------


def _free_person(world: WorldState) -> str:
    return next(iter(world.agents))


def test_a_command_the_contract_refuses_says_the_rule_s_text(world) -> None:
    rule = population_lifecycle.apply_population_presence_command
    event = _event("command-invalid")
    for command in (
        {"action": "fly"},
        {"action": "relocate", "agent_id": "someone"},
        {"action": "reunite", "agent_ids": ["only-one"], "household_id": "h"},
        {"action": "transfer"},
        "not a command",
    ):
        typed_error = None
        try:
            command_model = population_lifecycle.PopulationPresenceCommand
            (
                command_model.model_validate(command).selected_ids()
                if not isinstance(command, command_model)
                else None
            )
        except (TypeError, ValueError) as error:
            typed_error = error
        assert typed_error is not None
        _unchanged(rule, world, f"population command is invalid: {typed_error}", event, command)


def test_a_relocation_of_someone_busy_says_the_rule_s_text(world) -> None:
    # A queued person is in a facility's queue in a whole world; the check reads the activity.
    person = _free_person(world)
    here = world.agents[person].zone_id
    there = next(edge.to_zone_id for edge in world.zone_edges.values() if edge.from_zone_id == here)
    candidate = copy.deepcopy(world)
    candidate.agents[person].activity = Activity.QUEUED
    typed = PopulationPresenceCommand.model_validate(
        {"action": "relocate", "agent_id": person, "zone_id": there}
    )
    _unchanged(
        population_lifecycle._apply_presence_action,
        world,
        "relocation requires a person who is not in an active task",
        candidate,
        _event("command-relocate"),
        typed,
        typed.selected_ids(),
    )


# ---- complete_presence_relocation -----------------------------------------------------------


def test_a_relocation_completion_without_its_route_says_the_rule_s_text(world) -> None:
    event = _event(
        "relocation-arrival",
        event_type="agent_travel_complete",
        agent_id=_free_person(world),
        purpose="presence_relocation",
    )
    _unchanged(
        population_lifecycle.complete_presence_relocation,
        world,
        "relocation completion does not match its pending route",
        event,
    )


# ---- EffectProgramPopulationHandler ---------------------------------------------------------


def test_a_departure_primitive_the_rule_refuses_says_its_text(world) -> None:
    event = _program(world, "depart_evacuees", _zone(world), count=1, include_initial=False)
    _handler_says(
        world,
        _changed(event, payload={**event.payload, "include_initial": None}),
        "population departure include_initial must be boolean",
    )
    _handler_says(world, event, "only 0 admitted dynamic residents are available to leave")
