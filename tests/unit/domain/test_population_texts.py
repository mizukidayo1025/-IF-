"""The saved texts of population.py and population_lifecycle.py are their rules', byte for byte.

A rule's `EventSuppressed` and `EventApplicationError` texts go to the event's record as `detail`,
so the kernel must raise what the rule raises, in the rule's order of checks, after the rule's
writes and no others. Each case reaches one check: the outcome must be the kernel's own (not a
refusal), with the rule's text. The Python rules are gone (lane L10, `kernel_only`): each case was
run with them once (2d8fb46), and the class they raised and the world they left are pinned in
PYTHON_GAVE, per test and in call order, as a digest compared on every OS, as the gate's
hashes are. docs/rust-port/lanes/l8-texts-events.md.
"""

from __future__ import annotations

import copy
import hashlib
import json
from typing import Any

import pytest
from support.world import NATIVE_OUTCOME

from sheltersim.contracts.presence import PRESENCE_NAMESPACE, PresenceRecord, PresenceState
from sheltersim.core.world import WorldState
from sheltersim.domain import population, population_lifecycle
from sheltersim.domain.models import Activity
from sheltersim.domain.population_lifecycle import (
    POPULATION_LIFECYCLE_STATE_KEY,
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
    "test_a_command_outside_its_contract_or_minute_says_the_rule_s_text": [
        "bbded8a4299217b9",
        "330cb0ce4aca5606",
        "a34be4debf2ed930",
    ],
    "test_a_command_the_contract_refuses_says_the_rule_s_text": [
        "0717aceb0e0c255a",
        "0717aceb0e0c255a",
        "0717aceb0e0c255a",
        "0717aceb0e0c255a",
        "0717aceb0e0c255a",
    ],
    "test_a_departure_the_rule_refuses_says_its_text[0-0]": ["0717aceb0e0c255a"],
    "test_a_departure_the_rule_refuses_says_its_text[1-1]": ["0717aceb0e0c255a"],
    "test_a_departure_the_rule_refuses_says_its_text[2-2]": ["0717aceb0e0c255a"],
    "test_a_departure_the_rule_refuses_says_its_text[3-3]": ["0717aceb0e0c255a"],
    "test_a_departure_the_rule_refuses_says_its_text[4-4]": ["0717aceb0e0c255a"],
    "test_a_departure_the_rule_refuses_says_its_text[5-5]": ["0717aceb0e0c255a"],
    "test_a_departure_the_rule_refuses_says_its_text[6-6]": ["0717aceb0e0c255a"],
    "test_a_first_departure_keeps_the_state_it_made_when_too_few_can_leave": ["0717aceb0e0c255a"],
    "test_a_relocation_completion_without_its_route_says_the_rule_s_text": ["0717aceb0e0c255a"],
    "test_a_relocation_of_someone_busy_says_the_rule_s_text": ["0717aceb0e0c255a"],
    "test_a_relocation_the_rule_refuses_says_its_text": [
        "0717aceb0e0c255a",
        "0717aceb0e0c255a",
        "62c908964806b306",
        "1a2791ee21c6d1f5",
    ],
    "test_a_relocation_without_a_route_says_the_rule_s_text": ["1ebe95b9e59bbd40"],
    "test_a_return_the_rule_refuses_says_its_text": [
        "0717aceb0e0c255a",
        "0717aceb0e0c255a",
        "1f944ffd14f66dcd",
        "1f944ffd14f66dcd",
    ],
    "test_a_reunion_the_rule_refuses_says_its_text": ["0717aceb0e0c255a", "6416570b8ed318d5"],
    "test_a_route_into_another_zone_says_the_rule_s_text": ["75e22870dd41adf1"],
    "test_a_waiting_approach_with_a_record_stops_after_it_joins_the_waiting": ["868d27782577864d"],
    "test_a_waiting_arrival_with_a_record_stops_after_it_joins_the_waiting": ["96d651595c7e09bf"],
    "test_alternate_zones_the_rule_refuses_say_its_texts": [
        "0717aceb0e0c255a",
        "0717aceb0e0c255a",
        "0717aceb0e0c255a",
        "f51474277b1eedfb",
    ],
    "test_an_approach_completion_the_rule_refuses_says_its_text": [
        "41712a52628b2785",
        "41712a52628b2785",
        "41712a52628b2785",
        "41712a52628b2785",
        "41712a52628b2785",
        "41712a52628b2785",
        "41712a52628b2785",
        "5f7a1afad2f1df22",
    ],
    "test_an_approach_completion_without_its_presence_stops_after_two_writes": ["b17ba2fd2b969f08"],
    "test_an_approach_outside_an_approved_program_stops_after_the_first_writes": [
        "0459e0eae290c15a"
    ],
    "test_an_approach_without_its_program_s_provenance_stops_after_its_sequence": [
        "eb2e36d9115c46a1"
    ],
    "test_an_arrival_the_rule_refuses_says_its_text[0-0]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[1-1]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[10-10]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[11-11]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[12-12]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[13-13]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[14-14]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[15-15]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[16-16]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[17-17]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[18-18]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[19-19]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[2-2]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[20-20]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[21-21]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[22-22]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[23-23]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[24-24]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[25-25]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[3-3]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[4-4]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[5-5]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[6-6]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[7-7]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[8-8]": ["0717aceb0e0c255a"],
    "test_an_arrival_the_rule_refuses_says_its_text[9-9]": ["0717aceb0e0c255a"],
    "test_an_exit_or_transfer_of_someone_absent_says_the_rule_s_text": [
        "0717aceb0e0c255a",
        "0717aceb0e0c255a",
    ],
    "test_the_bounds_say_the_rule_s_texts": [
        "2f8abb4cf454e76a",
        "86452fe232195d46",
        "2adaba550e488fa6",
    ],
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


def _other_zone(world: WorldState) -> str:
    here = _zone(world)
    return next(zone for zone in sorted(world.zones) if zone != here)


def _arrival_id(world: WorldState, event_id: str, index: int = 0) -> str:
    digest = hashlib.sha256(f"{world.seed}\0{event_id}\0{index}".encode()).hexdigest()
    return f"arrival-{digest[:28]}"


def _departed_record(world: WorldState, agent_id: str) -> None:
    """A presence record for someone not in the shelter, under an ID an arrival will generate."""
    world.model_state[PRESENCE_NAMESPACE]["people"][agent_id] = PresenceRecord(
        agent_id=agent_id, status="departed", since_minute=0
    ).model_dump(mode="json")


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


@pytest.mark.parametrize(
    ("options", "text"),
    [
        ({"zone_id": "no-such-zone"}, "population arrival references an unknown zone"),
        ({"zone_id": 7}, "population arrival references an unknown zone"),
        ({"admission_zone_ids": "abc"}, "population arrival alternate admission zones are invalid"),
        ({"admission_zone_ids": None}, "population arrival alternate admission zones are invalid"),
        ({"admission_zone_ids": (5,)}, "population arrival alternate admission zones are invalid"),
        (
            {"admission_zone_ids": ["no-such-zone"]},
            "population arrival alternate admission zones are invalid",
        ),
        ({"approach_route_id": "no-such-route"}, "population arrival approach route is invalid"),
        ({"approach_route_id": 5}, "population arrival approach route is invalid"),
        ({"count": 0}, "population arrival count is outside its supported range"),
        ({"count": 1001}, "population arrival count is outside its supported range"),
        ({"count": True}, "population arrival count is outside its supported range"),
        ({"count": "3"}, "population arrival count is outside its supported range"),
        ({"count": 2.0}, "population arrival count is outside its supported range"),
        (
            {"household_size": 13},
            "population arrival household_size is outside its supported range",
        ),
        (
            {"household_size": None},
            "population arrival household_size is outside its supported range",
        ),
        ({"mobility": "flying"}, "population arrival mobility is invalid"),
        ({"mobility": None}, "population arrival mobility is invalid"),
        ({"mobility": 5}, "population arrival mobility is invalid"),
        ({"language": "not a language"}, "population arrival language is invalid"),
        ({"language": ""}, "population arrival language is invalid"),
        ({"language": None}, "population arrival language is invalid"),
        ({"language": 5}, "population arrival language is invalid"),
        (
            {"person_context_overrides": ({"member_index": 0},)},
            "population arrival person context is invalid",
        ),
        ({"person_context_overrides": 5}, "population arrival person context is invalid"),
        (
            {
                "person_context_overrides": (
                    {"member_index": 1, "care_support_needs": ["medical_assessment"]},
                )
            },
            "population arrival person context index is invalid",
        ),
        (
            {
                "count": 3,
                "person_context_overrides": (
                    {"member_index": 1, "care_support_needs": ["medical_assessment"]},
                    {"member_index": 1, "care_support_needs": ["language_access"]},
                ),
            },
            "population arrival person context index is invalid",
        ),
    ],
)
def test_an_arrival_the_rule_refuses_says_its_text(world, options: dict[str, Any], text) -> None:
    arguments = {"event": _event(), "zone_id": _zone(world), "count": 1, **options}
    _unchanged(population.queue_arrivals, world, text, **arguments)


def test_alternate_zones_the_rule_refuses_say_its_texts(world) -> None:
    here, other = _zone(world), _other_zone(world)
    invalid = "population arrival alternate admission zones are invalid"
    for zones in ([here], [other, other]):
        _unchanged(
            population.queue_arrivals,
            world,
            invalid,
            event=_event(),
            zone_id=here,
            count=1,
            admission_zone_ids=zones,
        )
    _unchanged(
        population.queue_arrivals,
        world,
        "one arrival approach route cannot combine with alternate admission zones",
        event=_event(),
        zone_id=here,
        count=1,
        admission_zone_ids=(other,),
        approach_route_id="any-route",
    )
    world.model_configs.pop("geometry_overlay", None)
    _unchanged(
        population.queue_arrivals,
        world,
        "alternate room admission requires geometry_overlay schema_version=3 or 4",
        event=_event(),
        zone_id=here,
        count=1,
        admission_zone_ids=(other,),
    )


def test_a_route_into_another_zone_says_the_rule_s_text(world) -> None:
    _add_route(world, _other_zone(world))
    _unchanged(
        population.queue_arrivals,
        world,
        "population arrival approach route targets a different zone",
        event=_event(),
        zone_id=_zone(world),
        count=1,
        approach_route_id=ROUTE,
    )


def test_the_bounds_say_the_rule_s_texts(world) -> None:
    state = world.model_state[DYNAMICS]
    arguments = {"event": _event(), "zone_id": _zone(world), "count": 1000}
    state["pending_arrivals"] = [{} for _ in range(9001)]
    _unchanged(
        population.queue_arrivals,
        world,
        "population arrival exceeds the pending capacity bound",
        **arguments,
    )
    state["pending_arrivals"] = [{} for _ in range(9000)]
    _unchanged(
        population.queue_arrivals,
        world,
        "population arrival exceeds the 10000-resident runtime bound",
        **arguments,
    )
    state["pending_arrivals"] = []
    state["profile_traits"].update({f"person-{index}": {} for index in range(9001)})
    _unchanged(
        population.queue_arrivals,
        world,
        "population arrival exceeds the 10000-profile history bound",
        **arguments,
    )


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


def test_a_waiting_arrival_with_a_record_stops_after_it_joins_the_waiting(world) -> None:
    zone = _zone(world)
    world.zones[zone].capacity = 0
    event = _event("arrival-recorded")
    _departed_record(world, _arrival_id(world, event.event_id, 1))
    # The first waits and is written; the second has a record: the rule stops there.
    _says(
        population.queue_arrivals,
        world,
        "waiting arrival already has a presence record",
        event=event,
        zone_id=zone,
        count=2,
    )
    assert len(world.model_state[DYNAMICS]["pending_arrivals"]) == 2


def test_an_approach_outside_an_approved_program_stops_after_the_first_writes(world) -> None:
    zone = _zone(world)
    _add_route(world, zone)
    _says(
        population.queue_arrivals,
        world,
        "arrival approach is not part of an approved EffectProgram",
        event=_event("arrival-unapproved", program_id="none", effect_id="none"),
        zone_id=zone,
        count=2,
        approach_route_id=ROUTE,
    )


def test_an_approach_without_its_program_s_provenance_stops_after_its_sequence(world) -> None:
    zone = _zone(world)
    _add_route(world, zone)
    event = _changed(_arrival_program(world, zone, approach_route_id=ROUTE), provenance={})
    sequence = world.id_counters.get("event")
    _says(
        population.queue_arrivals,
        world,
        "arrival event lacks its approved EffectProgram provenance",
        event=event,
        zone_id=zone,
        count=2,
        approach_route_id=ROUTE,
    )
    assert world.id_counters.get("event") != sequence


def test_a_waiting_approach_with_a_record_stops_after_it_joins_the_waiting(world) -> None:
    zone = _zone(world)
    _add_route(world, zone)
    event = _arrival_program(world, zone, approach_route_id=ROUTE)
    _departed_record(world, _arrival_id(world, event.event_id))
    _says(
        population.queue_arrivals,
        world,
        "waiting arrival already has a presence record",
        event=event,
        zone_id=zone,
        count=1,
        approach_route_id=ROUTE,
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
    completion, pending = _walking(world)
    rule = population.apply_arrival_approach_completion
    payload = dict(completion.payload)
    _unchanged(
        rule,
        world,
        "arrival approach completion payload fields do not match",
        _changed(completion, event_type="population_approach_finished"),
    )
    _unchanged(
        rule,
        world,
        "arrival approach completion payload fields do not match",
        _changed(completion, payload={**payload, "extra": 1}),
    )
    _unchanged(
        rule,
        world,
        "arrival approach completion is outside its approved program",
        _changed(completion, payload={**payload, "program_id": "none"}),
    )
    _unchanged(
        rule,
        world,
        "arrival approach completion is outside its approved program",
        _changed(completion, causation_id="another-event"),
    )
    _unchanged(
        rule,
        world,
        "arrival approach completion does not match pending arrival",
        _changed(completion, payload={**payload, "agent_id": "nobody"}),
    )
    _unchanged(
        rule,
        world,
        "arrival approach completion does not match pending arrival",
        _changed(completion, due_minute=completion.due_minute + 1),
    )
    _unchanged(
        rule,
        world,
        "arrival approach route provenance does not match",
        _changed(completion, source_id="SRC-ANOTHER"),
    )
    pending["approach_route_id"] = "another-route"
    _unchanged(
        rule,
        world,
        "arrival approach route provenance does not match",
        _changed(completion, payload={**payload, "route_id": "another-route"}),
    )


def test_an_approach_completion_without_its_presence_stops_after_two_writes(world) -> None:
    completion, pending = _walking(world)
    agent_id = pending["profile"]["id"]
    world.model_state[PRESENCE_NAMESPACE]["people"][agent_id]["status"] = "awaiting_admission"
    _says(
        population.apply_arrival_approach_completion,
        world,
        "arrival approach has no matching presence state",
        completion,
    )
    assert pending["approach_completed_minute"] == completion.due_minute
    assert "approach_completion_event_id" not in pending


# ---- queue_departures ------------------------------------------------------------------------


@pytest.mark.parametrize(
    ("options", "text"),
    [
        ({"zone_id": "no-such-zone"}, "population departure references an unknown zone"),
        ({"count": 0}, "population departure count is outside its supported range"),
        ({"count": False}, "population departure count is outside its supported range"),
        ({"include_initial": None}, "population departure include_initial must be boolean"),
        ({"include_initial": 1}, "population departure include_initial must be boolean"),
        ({"count": 1}, "only 0 admitted dynamic residents are available to leave"),
        (
            {"count": 1000, "include_initial": True},
            "only {people} admitted dynamic residents are available to leave",
        ),
    ],
)
def test_a_departure_the_rule_refuses_says_its_text(world, options: dict[str, Any], text) -> None:
    zone = _zone(world)
    people = sum(agent.zone_id == zone for agent in world.agents.values())
    arguments = {"event": _event("departure"), "zone_id": zone, "count": 1, **options}
    _unchanged(population.queue_departures, world, text.format(people=people), **arguments)


def test_a_first_departure_keeps_the_state_it_made_when_too_few_can_leave(world) -> None:
    world.model_state.pop(DYNAMICS)
    _says(
        population.queue_departures,
        world,
        "only 0 admitted dynamic residents are available to leave",
        event=_event("departure"),
        zone_id=_zone(world),
        count=1,
    )
    assert DYNAMICS in world.model_state


# ---- apply_population_presence_command ------------------------------------------------------


def _command(
    world: WorldState, text: str, minute: int = 0, event_minute: int | None = None, **fields
):
    world.minute_index = minute
    event = _event(
        f"command-{fields.get('action')}", minute if event_minute is None else event_minute
    )
    _unchanged(population_lifecycle.apply_population_presence_command, world, text, event, fields)


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


def test_a_command_outside_its_contract_or_minute_says_the_rule_s_text(world) -> None:
    person = _free_person(world)
    _command(
        world,
        "population command minute must match the current event boundary",
        minute=1,
        event_minute=2,
        action="transfer",
        agent_id=person,
    )
    _command(
        world,
        "expected return minute precedes the departure event",
        minute=5,
        action="temporary_exit",
        agent_id=person,
        expected_return_minute=2,
    )
    world.model_state.pop(POPULATION_LIFECYCLE_STATE_KEY)
    _command(
        world,
        "population lifecycle contract is not installed",
        action="transfer",
        agent_id=person,
    )


def test_an_exit_or_transfer_of_someone_absent_says_the_rule_s_text(world) -> None:
    _command(
        world,
        "temporary exit requires an admitted person",
        action="temporary_exit",
        agent_id="nobody",
    )
    _command(world, "transfer requires an admitted person", action="transfer", agent_id="nobody")


def test_a_return_the_rule_refuses_says_its_text(world) -> None:
    person = _free_person(world)
    _command(world, "return requires a retained absent person", action="return", agent_id="nobody")
    _command(world, "return requires a retained absent person", action="return", agent_id=person)
    world.minute_index = 1
    population_lifecycle.apply_population_presence_command(
        world, _event("command-exit", 1), {"action": "transfer", "agent_id": person}
    )
    assert person not in world.agents
    _command(
        world,
        "return requires a valid destination zone",
        minute=2,
        action="return",
        agent_id=person,
        zone_id="nowhere",
    )
    _command(
        world,
        "return requires a valid destination zone",
        minute=2,
        action="return",
        agent_id=person,
    )


def test_a_relocation_the_rule_refuses_says_its_text(world) -> None:
    person = _free_person(world)
    here = world.agents[person].zone_id
    there = next(edge.to_zone_id for edge in world.zone_edges.values() if edge.from_zone_id == here)
    invalid = "relocation requires an admitted person and valid destination"
    _command(world, invalid, action="relocate", agent_id="nobody", zone_id=there)
    _command(world, invalid, action="relocate", agent_id=person, zone_id="nowhere")
    world.zones[there].capacity = 0
    _command(
        world,
        "relocation destination has no available capacity",
        action="relocate",
        agent_id=person,
        zone_id=there,
    )
    world.zones[there].capacity = None
    world.duration_minutes = 2
    _command(
        world,
        "relocation route would finish after the scenario boundary",
        minute=1,
        action="relocate",
        agent_id=person,
        zone_id=there,
    )


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


def test_a_relocation_without_a_route_says_the_rule_s_text(world) -> None:
    person = _free_person(world)
    here = world.agents[person].zone_id
    there = _other_zone(world)
    world.zones[there].capacity = None
    for edge_id, edge in list(world.zone_edges.items()):
        if there in (edge.from_zone_id, edge.to_zone_id):
            world.zone_edges.pop(edge_id)
    assert here != there
    _command(
        world,
        "no accessible, physically clear relocation route is available",
        action="relocate",
        agent_id=person,
        zone_id=there,
    )


def test_a_reunion_the_rule_refuses_says_its_text(world) -> None:
    first, second = sorted(world.agents)[:2]
    _command(
        world,
        "reunion requires every person to be admitted",
        action="reunite",
        agent_ids=(first, "nobody"),
        household_id="household-new",
    )
    world.agents[second].zone_id = next(
        zone for zone in sorted(world.zones) if zone != world.agents[first].zone_id
    )
    _command(
        world,
        "reunion requires people to be physically co-located",
        action="reunite",
        agent_ids=(first, second),
        household_id="household-new",
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


def test_a_primitive_s_argument_the_rule_turns_into_its_text(world) -> None:
    zone = _zone(world)
    event = _arrival_program(world, zone)
    payload = dict(event.payload)
    _handler_says(
        world,
        _changed(event, payload={**payload, "admission_zone_ids": 5}),
        "'int' object is not iterable",
    )
    _handler_says(
        world,
        _changed(event, payload={**payload, "person_context_overrides": None}),
        "'NoneType' object is not iterable",
    )
    _handler_says(
        world,
        _changed(event, payload={**payload, "household_size": None}),
        "population arrival household_size is outside its supported range",
    )


def test_a_mobility_of_another_type_is_invalid_as_the_enum_says(world) -> None:
    """A list is no Mobility: the enum raised ValueError, which the rule worded. The kernel of
    `queue_arrivals` used to leave it to that rule; now it says the same itself."""
    zone = _zone(world)
    event = _arrival_program(world, zone)
    changed = _changed(event, payload={**event.payload, "mobility": [1]})
    _handler_says(world, changed, "population arrival mobility is invalid")


def test_a_departure_primitive_the_rule_refuses_says_its_text(world) -> None:
    event = _program(world, "depart_evacuees", _zone(world), count=1, include_initial=False)
    _handler_says(
        world,
        _changed(event, payload={**event.payload, "include_initial": None}),
        "population departure include_initial must be boolean",
    )
    _handler_says(world, event, "only 0 admitted dynamic residents are available to leave")
