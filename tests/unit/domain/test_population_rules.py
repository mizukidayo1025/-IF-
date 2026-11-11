"""The kernels of population.py and population_lifecycle.py against what their Python rules gave.

The gate reaches these rules at a few events, so what it can't reach is checked here. Their Python
rules are gone (lane L10, `kernel_only`): each case was run with them once (2d8fb46), and what they
returned and the world they left is pinned in PYTHON_GAVE, per test and in call order, as a digest
compared on every OS, as the gate's hashes are. A kernel raises the rule's own saved text where
the rule raised one (tests/unit/domain/test_population_texts.py); an input outside the rule's
contract is a KernelRefusal, with nothing changed.
"""

from __future__ import annotations

import copy
import hashlib
import json
import os
import re
from typing import Any

import pytest
from support.world import NATIVE_OUTCOME

from sheltersim import native
from sheltersim.contracts.presence import PRESENCE_NAMESPACE, PresenceState
from sheltersim.core import state_mirror
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.world import WorldState
from sheltersim.domain import population, population_lifecycle
from sheltersim.domain.models import ActiveTask, Activity
from sheltersim.domain.population_lifecycle import (
    POPULATION_LIFECYCLE_STATE_KEY,
    PopulationPresenceCommand,
    initialize_population_lifecycle_state,
)
from sheltersim.events.effects import (
    EffectProgram,
    EffectProgramGeometryHandler,
    SupportMode,
    TargetSelector,
    TimedAction,
    schedule_effect_program,
)
from sheltersim.events.handlers import EventDispatcher
from sheltersim.events.models import (
    EventApplicationError,
    EventKind,
    EventSuppressed,
    ScheduledEvent,
)
from sheltersim.experiments.scenario import ScenarioSpec


def _document(world: WorldState) -> str:
    # A digest: pytest's report of two long documents that differ takes minutes.
    text = world._canonical_text(world._canonical_data())
    return hashlib.sha256(text.encode()).hexdigest()


# ---- What the Python rules gave ---------------------------------------------------------------

PYTHON_GAVE: dict[str, list[str]] = {
    "test_a_broken_lifecycle_state_raises_the_rule_s_text[0-0]": ["4e3e1a5cf967fc03"],
    "test_a_broken_lifecycle_state_raises_the_rule_s_text[1-1]": ["4e3e1a5cf967fc03"],
    "test_a_broken_lifecycle_state_raises_the_rule_s_text[2-2]": ["4e3e1a5cf967fc03"],
    "test_a_broken_lifecycle_state_raises_the_rule_s_text[3-3]": ["4e3e1a5cf967fc03"],
    "test_a_broken_population_state_raises_the_rule_s_text[0-0]": ["b11eeb61291f9da4"],
    "test_a_broken_population_state_raises_the_rule_s_text[1-1]": ["b11eeb61291f9da4"],
    "test_a_broken_population_state_raises_the_rule_s_text[2-2]": ["b11eeb61291f9da4"],
    "test_a_broken_population_state_raises_the_rule_s_text[3-3]": ["b11eeb61291f9da4"],
    "test_a_broken_presence_ledger_stops_the_kernels_before_a_write": ["8d05be79883df930"],
    "test_a_departure_of_initial_people_keeps_their_history_and_withdraws_their_events": [
        "66f97af9265b53d7"
    ],
    "test_a_departure_of_someone_busy_waits_as_the_rule_says": ["98f7276d7f198f4a"],
    "test_a_full_lifecycle_history_drops_its_oldest_as_the_rule_does": ["1edd68902f5ee8c6"],
    "test_a_full_presence_history_drops_its_oldest_as_the_rule_does": ["7d7da063297bfc6b"],
    "test_a_person_leaves_and_returns_with_the_same_state": [
        "2fa9ff282291d89f",
        "3d5bcc6d4658ed49",
    ],
    "test_a_relocation_walks_and_its_completion_is_the_rule_s": [
        "36631542c997036d",
        "4e17d33e56d39170",
    ],
    "test_a_retry_at_the_end_of_the_scenario_is_suppressed": ["7c4adaf0460d6974"],
    "test_a_retry_is_the_rule_s_event[0]": ["4e66d9984e4a7b06"],
    "test_a_retry_is_the_rule_s_event[1]": ["6951328349cd462d"],
    "test_a_retry_is_the_rule_s_event[2]": ["e53f4d9c2590002a"],
    "test_a_retry_is_the_rule_s_event[3]": ["6f8c63ae362597a9"],
    "test_a_return_to_a_full_zone_waits_and_the_rule_admits_it_later": [
        "f7b1822a9fe74c25",
        "aacc2e1ff3b22fd9",
        "fe73ec995c8783ac",
        "a9651b487ec72f49",
    ],
    "test_a_reunion_and_a_relocation_in_place_are_the_rule_s": [
        "9f1df5f07bed3c74",
        "c48afd95f22e66cf",
    ],
    "test_a_room_admission_keeps_half_a_metre_from_who_is_there": [
        "1cdd844ce76f2764",
        "aec36ea78bf2ff29",
    ],
    "test_a_whole_command_is_the_rule_s": ["7ed9330305fa2915"],
    "test_an_admission_after_a_relocation_is_the_kernel_s_too": [
        "4dd4085117e6560a",
        "b68db4bb3e463477",
        "6265176cd447a0cc",
        "aa4dc9fb22a285fa",
    ],
    "test_an_exit_of_someone_busy_is_deferred_as_the_rule_says[0-0]": ["523977908ba29f24"],
    "test_an_exit_of_someone_busy_is_deferred_as_the_rule_says[1-1]": ["e6555186da1a9321"],
    "test_an_exit_of_someone_busy_is_deferred_as_the_rule_says[2-2]": ["23bfeb08ef3e8ae6"],
    "test_an_exit_past_its_expected_return_is_dropped": ["9bb91f7d68140e79"],
    "test_arrivals_past_capacity_wait_and_are_admitted_when_room_opens": [
        "8c6f0730f5e57e7d",
        "819214104b4d924c",
        "240aab53b0c2be84",
    ],
    "test_arrivals_take_an_alternate_zone": ["ad8c9fd1a19ab49f"],
    "test_arrivals_with_room_are_the_rule_s_people": ["53cff637f7ac3153"],
    "test_departures_take_the_newest_state_and_admit_who_waits": [
        "4a3ed74258f890f0",
        "9f92e5c5eb8eb682",
    ],
    "test_first_use_by_a_deferred_departure_keeps_the_state": ["98f7276d7f198f4a"],
    "test_first_use_by_a_departure_makes_the_state_as_the_rule_does": ["f4ded4ee92f119d6"],
    "test_first_use_makes_the_population_state_as_the_rule_does": ["1bea8b8df207f7ba"],
    "test_household_context_matches_the_contract_s_dump[0-0]": ["ce993f19108b2a1c"],
    "test_household_context_matches_the_contract_s_dump[1-1]": ["a8ce4ce59860941b"],
    "test_household_context_matches_the_contract_s_dump[2-2]": ["656a6d0522127b63"],
    "test_household_context_matches_the_contract_s_dump[3-3]": ["19343777e3639d24"],
    "test_people_given_a_room_in_one_event_stand_apart": ["648bf16e71dbd33c"],
    "test_room_in_a_zone_with_temporary_spaces[0-0]": ["c36e13eb70283f9d"],
    "test_room_in_a_zone_with_temporary_spaces[0-1]": ["5eeb8bc2aee197a3"],
    "test_room_in_a_zone_with_temporary_spaces[0-2]": ["3d632062ee9f5fae"],
    "test_room_in_a_zone_with_temporary_spaces[0-3]": ["3d632062ee9f5fae"],
    "test_room_in_a_zone_with_temporary_spaces[1-0]": ["3d632062ee9f5fae"],
    "test_room_in_a_zone_with_temporary_spaces[1-1]": ["3d632062ee9f5fae"],
    "test_room_in_a_zone_with_temporary_spaces[1-2]": ["3d632062ee9f5fae"],
    "test_room_in_a_zone_with_temporary_spaces[1-3]": ["3d632062ee9f5fae"],
    "test_room_in_a_zone_with_temporary_spaces[2-0]": ["c36e13eb70283f9d"],
    "test_room_in_a_zone_with_temporary_spaces[2-1]": ["5eeb8bc2aee197a3"],
    "test_room_in_a_zone_with_temporary_spaces[2-2]": ["3d632062ee9f5fae"],
    "test_room_in_a_zone_with_temporary_spaces[2-3]": ["3d632062ee9f5fae"],
    "test_room_in_a_zone_with_temporary_spaces[3-0]": ["5eeb8bc2aee197a3"],
    "test_room_in_a_zone_with_temporary_spaces[3-1]": ["3d632062ee9f5fae"],
    "test_room_in_a_zone_with_temporary_spaces[3-2]": ["3d632062ee9f5fae"],
    "test_room_in_a_zone_with_temporary_spaces[3-3]": ["3d632062ee9f5fae"],
    "test_room_in_a_zone_with_temporary_spaces[4-0]": ["c36e13eb70283f9d"],
    "test_room_in_a_zone_with_temporary_spaces[4-1]": ["5eeb8bc2aee197a3"],
    "test_room_in_a_zone_with_temporary_spaces[4-2]": ["3d632062ee9f5fae"],
    "test_room_in_a_zone_with_temporary_spaces[4-3]": ["3d632062ee9f5fae"],
    "test_someone_saved_without_their_dynamics_gets_a_new_record": [
        "4dd4085117e6560a",
        "9a415f2cee4d209e",
    ],
    "test_the_last_retry_runs_and_the_next_is_the_rule_s_to_refuse": [
        "2931723a931a105a",
        "f24b306a32d91cce",
        "f24b306a32d91cce",
    ],
    "test_who_waits_is_checked_before_a_departure_writes": ["ce1366fb316e196a", "049d6795ea435ef5"],
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
    """These values (results, worlds) are what the Python rule gave at this call of the test."""
    name, index = _SCOPE
    _SCOPE[1] += 1
    assert _digest(*values) == PYTHON_GAVE[name][index]


def _event(event_id: str = "arrival-event", minute: int = 0, **payload: Any) -> ScheduledEvent:
    return ScheduledEvent(
        event_id=event_id,
        kind=EventKind.INTERVENTION,
        due_minute=minute,
        priority=0,
        sequence=900_000 + int(hashlib.sha256(event_id.encode()).hexdigest()[:4], 16),
        event_type="effect_program_population",
        payload=payload,
        source_id="ASM-TEST-POPULATION-RULES",
    )


@pytest.fixture
def world(generated_scenario) -> WorldState:
    made = WorldState.from_scenario(generated_scenario)
    assert population.POPULATION_DYNAMICS_STATE_KEY in made.model_state
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


def _occupancy(world: WorldState, zone_id: str) -> int:
    return sum(agent.zone_id == zone_id for agent in world.agents.values())


def _same_as_rule(kernels: str, rule_name: str, world: WorldState, *args, **options):
    """Run the kernel on the world: it returns and leaves what the Python rule did."""
    result = getattr(native.rules(kernels), rule_name)(world, *args, **options)
    _python_gave(result, world)
    return result


def _arrive(world: WorldState, count: int = 3, household_size: int = 2, **options: Any) -> int:
    return _same_as_rule(
        "population",
        "queue_arrivals",
        world,
        event=_event(minute=world.minute_index),
        zone_id=options.pop("zone_id", _zone(world)),
        count=count,
        household_size=household_size,
        **options,
    )


def test_arrivals_with_room_are_the_rule_s_people(world) -> None:
    world.zones[_zone(world)].capacity = None
    assert _arrive(world, count=5) == 5
    state = world.model_state[population.POPULATION_DYNAMICS_STATE_KEY]
    assert len(state["profile_context"]) == 5


@pytest.mark.parametrize(("size", "count"), [(1, 4), (2, 7), (12, 30), (3, 1)])
def test_household_context_matches_the_contract_s_dump(world, size: int, count: int) -> None:
    _arrive(world, count=count, household_size=size)


def test_arrivals_past_capacity_wait_and_are_admitted_when_room_opens(world) -> None:
    zone = _zone(world)
    world.zones[zone].capacity = _occupancy(world, zone) + 1

    assert _arrive(world, count=4) == 1
    state = world.model_state[population.POPULATION_DYNAMICS_STATE_KEY]
    assert len(state["pending_arrivals"]) == 3
    assert state["arrivals"][-1]["waiting_count"] == 3

    # Nobody can come in: nothing changes but the count.
    assert _same_as_rule("population", "admit_pending_arrivals", world, 0) == ()

    world.zones[zone].capacity += 2
    assert len(_same_as_rule("population", "admit_pending_arrivals", world, 0)) == 2
    assert len(state["pending_arrivals"]) == 1
    assert world.metric_state["population_waiting_count"] == 1.0


def test_arrivals_take_an_alternate_zone(world) -> None:
    zone = _zone(world)
    other = _other_zone(world)
    world.zones[zone].capacity = _occupancy(world, zone)
    world.model_configs["geometry_overlay"] = {"schema_version": 3}

    assert _arrive(world, count=2, admission_zone_ids=(other,)) == 2
    assert {
        agent.zone_id for agent in world.agents.values() if agent.id.startswith("arrival-")
    } == {other}


def test_a_person_walking_to_a_zone_holds_their_place(world) -> None:
    other = _other_zone(world)
    zone = _zone(world)
    walker = next(iter(world.agents.values()))
    walker.zone_id = other
    zone = next(agent.zone_id for agent in list(world.agents.values())[1:])
    walker.activity = type(walker.activity).MOVING
    walker.target_id = zone
    world.zones[zone].capacity = _occupancy(world, zone) + 1

    assert not native.rules("population").zone_has_room(world, zone)
    assert native.rules("population_lifecycle").zone_has_room(world, zone, excluding=walker.id)


@pytest.mark.parametrize("extra", [-1, 0, 1, 3])
@pytest.mark.parametrize(
    "spaces", [(), ((2, True),), ((2, False),), ((3, False), (1, True)), ((1, False), (5, False))]
)
def test_room_in_a_zone_with_temporary_spaces(world, extra: int, spaces: Any) -> None:
    from sheltersim.domain.temporary_spaces import (
        TEMPORARY_SPACES_STATE_KEY,
        create_temporary_spaces_state,
    )

    zone = _zone(world)
    state = create_temporary_spaces_state()
    for index, (people, active) in enumerate(spaces):
        state["spaces"][f"space-{index}"] = {
            "space_id": f"space-{index}",
            "zone_id": zone,
            "people_capacity": people,
            "sleeping_capacity": 0,
            "start_minute": 0,
            "end_minute": 10,
            "active": active,
            "start_event_id": "start",
            "end_event_id": None if active else "end",
            "program_id": "program",
            "effect_id": "effect",
            "source_ids": [],
            "geometry_status": "zone_only_capacity",
        }
    world.model_state[TEMPORARY_SPACES_STATE_KEY] = state
    world.zones[zone].capacity = max(1, _occupancy(world, zone) + extra)

    lifecycle = native.rules("population_lifecycle")
    person = next(iter(world.agents))
    _python_gave(
        native.rules("population").zone_has_room(world, zone),
        lifecycle.zone_has_room(world, zone),
        lifecycle.zone_has_room(world, zone, excluding=person),
    )


def test_a_zone_without_capacity_has_room(world) -> None:
    zone = _zone(world)
    world.zones[zone].capacity = None
    assert native.rules("population").zone_has_room(world, zone)
    assert native.rules("population_lifecycle").zone_has_room(world, zone)


def _depart(world: WorldState, count: int = 1, **options: Any) -> int:
    return _same_as_rule(
        "population",
        "queue_departures",
        world,
        event=_event("departure-event", minute=world.minute_index),
        zone_id=_zone(world),
        count=count,
        **options,
    )


def test_departures_take_the_newest_state_and_admit_who_waits(world) -> None:
    zone = _zone(world)
    world.zones[zone].capacity = _occupancy(world, zone) + 1
    assert _arrive(world, count=3) == 1
    state = world.model_state[population.POPULATION_DYNAMICS_STATE_KEY]
    assert len(state["pending_arrivals"]) == 2

    assert _depart(world, count=1) == 1

    assert len(state["departures"]) == 1
    assert len(state["pending_arrivals"]) == 1


def test_a_departure_of_initial_people_keeps_their_history_and_withdraws_their_events(
    world,
) -> None:
    person = next(iter(world.agents))
    world.schedule_event(_event("mentions-them", minute=5, agent_id=person))
    world.schedule_event(_event("mentions-another", minute=5, agent_id="someone-else"))
    world.schedule_event(_event("mentions-sender", minute=6, sender_agent_id=person))

    count = _occupancy(world, _zone(world))
    assert _depart(world, count=count, include_initial=True) == count

    assert person not in world.agents
    assert {event.event_id for event in world.event_queue.pending()} >= {"mentions-another"}
    assert "mentions-them" not in {event.event_id for event in world.event_queue.pending()}


def test_a_departure_reads_events_scheduled_as_objects_without_a_refusal(world) -> None:
    # Events Python schedules are held as objects, without rows, until a kernel reads them. The
    # departure reads every pending event as a row: the glue makes the rows before the core runs,
    # so the core's "a pending event has no row" is never raised (and counted by the census) for
    # them. Lane c5 (#191) ran the core first and again after that refusal; only the long seed
    # test (tests/integration/test_scenario_seeds.py) saw it.
    import sheltersim_native

    person = next(iter(world.agents))
    world.schedule_event(_event("mentions-them", minute=5, agent_id=person))
    world.schedule_event(_event("mentions-another", minute=5, agent_id="someone-else"))
    # Under a census of the tests, its switch is not this test's to turn.
    counting = "SHELTERSIM_REFUSAL_CENSUS" not in os.environ
    if counting:
        sheltersim_native.census_take()
        sheltersim_native.census_switch(True)
    try:
        departed = native.rules("population").queue_departures(
            world,
            event=_event("departure-event", minute=world.minute_index),
            zone_id=_zone(world),
            count=1,
            include_initial=True,
        )
        assert native.rules("population").validate_population_dynamics_state(world) is None
        raised, _ = sheltersim_native.census_take() if counting else ([], [])
    finally:
        if counting:
            sheltersim_native.census_switch(False)
    assert departed == 1
    assert raised == []
    assert "mentions-another" in {event.event_id for event in world.event_queue.pending()}


def test_the_rule_says_why_a_departure_can_not_happen(world) -> None:
    # The kernel raises the rule's own text (tests/unit/domain/test_population_texts.py).
    kernels = native.rules("population")
    before = _document(world)
    with pytest.raises(EventApplicationError, match="count is outside"):
        kernels.queue_departures(world, event=_event(), zone_id=_zone(world), count=5000)
    with pytest.raises(EventApplicationError, match="unknown zone") as made:
        kernels.queue_departures(world, event=_event(), zone_id="no-such-zone", count=1)
    assert getattr(made.value, NATIVE_OUTCOME)
    assert _document(world) == before


@pytest.mark.parametrize(
    ("options", "text"),
    [
        ({"zone_id": "no-such-zone"}, "unknown zone"),
        ({"count": 0}, "count is outside"),
        ({"household_size": 13}, "household_size is outside"),
        ({"mobility": "flying"}, "mobility is invalid"),
        ({"language": "not a language"}, "language is invalid"),
        ({"admission_zone_ids": ("no-such-zone",)}, "alternate admission zones are invalid"),
        ({"approach_route_id": "no-such-route"}, "approach route is invalid"),
        ({"person_context_overrides": ({"member_index": 9},)}, "person context is invalid"),
    ],
)
def test_a_refused_arrival_changes_nothing_and_says_the_rule_s_text(
    world, options: dict[str, Any], text: str
) -> None:
    arguments = {"event": _event(), "zone_id": _zone(world), "count": 1, **options}
    before = _document(world)
    with pytest.raises(EventApplicationError, match=text) as made:
        native.rules("population").queue_arrivals(world, **arguments)
    assert getattr(made.value, NATIVE_OUTCOME)
    assert _document(world) == before


def _retry_event(**provenance: Any) -> ScheduledEvent:
    return ScheduledEvent(
        event_id="retried",
        kind=EventKind.INTERVENTION,
        due_minute=0,
        priority=4,
        sequence=880_000,
        event_type="effect_program_population",
        payload={"op": "depart_evacuees", "nested": {"a": [1, 2.5]}},
        source_id="ASM-TEST-POPULATION-RULES",
        provenance=provenance,
    )


@pytest.mark.parametrize(
    "provenance",
    [
        {},
        {"population_retry": {"count": 7}, "other": {"kept": True}},
        {"effect_program_v1": {"program_id": "p", "effect_id": "e"}},
        {"effect_program_v1": {"program_id": "not-in-state", "effect_id": "e"}},
    ],
)
def test_a_retry_is_the_rule_s_event(world, provenance: dict[str, Any]) -> None:
    # A program as its row holds one: the kernel reads the namespace's row.
    world.model_state["effect_program_v1"] = {
        "schema_version": 1,
        "programs": {
            "p": {
                "program_hash": "hash",
                "source_text_hash": "text-hash",
                "narrative": "",
                "compiler_provenance": {},
                "assumptions": [],
                "support_mode": "simulation",
                "effect_ids": ["e"],
                "operators": {"e": "depart_evacuees"},
                "scheduled_event_ids": [],
            }
        },
        "active_facility_modifiers": {},
    }
    _same_as_rule(
        "population",
        "schedule_population_retry",
        world,
        _retry_event(**provenance),
        id_prefix="presence-retry",
        subject="population exit",
    )
    pending = world.event_queue.pending()
    retry = next(event for event in pending if event.event_id.startswith("presence-retry-"))
    assert retry.provenance["deferred_from"] == "retried"
    assert dict(retry.payload["nested"])["a"] == (1, 2.5)


def test_the_last_retry_runs_and_the_next_is_the_rule_s_to_refuse(world) -> None:
    _same_as_rule(
        "population",
        "schedule_population_retry",
        world,
        _retry_event(population_retry={"count": 1439}),
    )
    _same_suppression(
        world,
        "departure reached the bounded deferral limit",
        _retry_event(population_retry={"count": 1440}),
    )
    _same_suppression(
        world,
        "departure reached the bounded deferral limit",
        _retry_event(population_retry={"count": "many"}),
    )


def test_a_retry_at_the_end_of_the_scenario_is_suppressed(world) -> None:
    world.minute_index = world.duration_minutes - 1
    _same_suppression(
        world,
        "population exit could not complete before the scenario ended",
        _retry_event(),
        id_prefix="presence-retry",
        subject="population exit",
    )


def _same_suppression(world: WorldState, text: str, *arguments: Any, **options: Any) -> None:
    """The kernel stops with the rule's text, and the world keeps what the rule scheduled."""
    kernel = native.rules("population").schedule_population_retry
    with pytest.raises(EventSuppressed) as ours:
        kernel(world, *arguments, **options)
    assert str(ours.value) == text
    _python_gave(world)


def _busy(world: WorldState, person: str) -> None:
    world.agents[person].activity = Activity.QUEUED


def _waiting_for_the_person(world: WorldState, person: str, zone: str, kernel_name: str) -> None:
    """Depart with `person` selected and busy: the kernel defers as the rule did."""
    event = _event("departure-event", minute=world.minute_index)
    with pytest.raises(EventSuppressed) as ours:
        getattr(native.rules("population"), kernel_name)(
            world, event=event, zone_id=zone, count=1, include_initial=True
        )
    assert str(ours.value) == "departure waits for the selected residents' active tasks"
    _python_gave(world, [event.event_id for event in world.event_queue.pending()])


def test_a_departure_of_someone_busy_waits_as_the_rule_says(world) -> None:
    person = min(world.agents)
    _busy(world, person)
    for agent in world.agents.values():
        if agent.id != person:
            agent.zone_id = _other_zone(world)
    _waiting_for_the_person(world, person, world.agents[person].zone_id, "queue_departures")
    retry = next(
        item for item in world.event_queue.pending() if item.event_id.startswith("departure-retry")
    )
    assert retry.provenance["population_retry"] == {"count": 1}


def test_first_use_makes_the_population_state_as_the_rule_does(world) -> None:
    del world.model_state[population.POPULATION_DYNAMICS_STATE_KEY]
    world.zones[_zone(world)].capacity = None
    assert _arrive(world, count=2) == 2
    assert world.model_state[population.POPULATION_DYNAMICS_STATE_KEY]["schema_version"] == 1


def test_first_use_by_a_departure_makes_the_state_as_the_rule_does(world) -> None:
    del world.model_state[population.POPULATION_DYNAMICS_STATE_KEY]
    assert _depart(world, count=1, include_initial=True) == 1


def test_first_use_by_a_deferred_departure_keeps_the_state(world) -> None:
    del world.model_state[population.POPULATION_DYNAMICS_STATE_KEY]
    person = min(world.agents)
    _busy(world, person)
    for agent in world.agents.values():
        if agent.id != person:
            agent.zone_id = _other_zone(world)
    _waiting_for_the_person(world, person, world.agents[person].zone_id, "queue_departures")
    assert population.POPULATION_DYNAMICS_STATE_KEY in world.model_state


def _full_zone_with_waiters(world: WorldState) -> str:
    """A full zone, someone new in it to leave, and one arrival waiting for their place."""
    zone = _zone(world)
    world.zones[zone].capacity = None
    assert _arrive(world, count=1, household_size=1) == 1
    world.zones[zone].capacity = _occupancy(world, zone)
    _same_as_rule(
        "population",
        "queue_arrivals",
        world,
        event=_event("arrival-waiting"),
        zone_id=zone,
        count=1,
        household_size=1,
    )
    state = world.model_state[population.POPULATION_DYNAMICS_STATE_KEY]
    assert len(state["pending_arrivals"]) == 1
    return zone


def test_who_waits_is_checked_before_a_departure_writes(world) -> None:
    """The admission after a departure used to fall back to the rule, past the writes.

    A waiting arrival whose minute is a bool, which no rule writes, is outside the contract: the
    departure is refused before it writes anything.
    """
    zone = _full_zone_with_waiters(world)
    state = world.model_state[population.POPULATION_DYNAMICS_STATE_KEY]
    state["pending_arrivals"][0]["created_minute"] = True
    arguments = {"event": _event("departure-event"), "zone_id": zone, "count": 1}
    before = _document(world)
    with pytest.raises(native.Unsupported):
        native.rules("population").queue_departures(world, **arguments)
    with pytest.raises(KernelRefusal):
        population.queue_departures(world, **arguments)
    assert _document(world) == before


def _return_waiting(world: WorldState, person: str) -> str:
    zone = world.agents[person].zone_id
    _command(world, minute=1, action="temporary_exit", agent_id=person, expected_return_minute=9)
    world.zones[zone].capacity = _occupancy(world, zone)
    _command(world, minute=2, action="return", agent_id=person)
    assert [
        item["agent_id"]
        for item in world.model_state[POPULATION_LIFECYCLE_STATE_KEY]["pending_returns"]
    ] == [person]
    return zone


def test_an_admission_after_a_relocation_is_the_kernel_s_too(world) -> None:
    """It used to fall back to the rule, past the writes: `_initial_dynamics` was the rule's."""
    from sheltersim.domain.agent_dynamics import (
        AGENT_DYNAMICS_STATE_KEY,
        create_agent_dynamics_state,
    )

    world.model_state.setdefault(AGENT_DYNAMICS_STATE_KEY, create_agent_dynamics_state(world))
    person, walker = sorted(world.agents)[:2]
    zone = _return_waiting(world, person)
    world.zones[zone].capacity = None
    origin = world.agents[walker].zone_id
    destination = next(
        edge.to_zone_id for edge in world.zone_edges.values() if edge.from_zone_id == origin
    )
    world.zones[destination].capacity = None
    _command(world, minute=3, action="relocate", agent_id=walker, zone_id=destination)
    state = world.model_state[POPULATION_LIFECYCLE_STATE_KEY]
    relocation = state["pending_relocations"][walker]
    event = next(
        item for item in world.event_queue.pending() if item.event_id == relocation["event_id"]
    )
    world.minute_index = event.due_minute
    agent = world.agents[walker]
    agent.zone_id = destination
    agent.activity = type(agent.activity).IDLE
    agent.target_id = None
    world.active_tasks.pop(relocation["task_id"])
    # The admission that follows the writes makes this person's record.
    state["people"][person]["agent_dynamics"] = None
    _same_as_rule("population_lifecycle", "complete_presence_relocation", world, event)
    assert person in world.agents
    assert world.model_state[AGENT_DYNAMICS_STATE_KEY]["agents"][person]["decision_due_minute"] == (
        event.due_minute
    )


def test_someone_saved_without_their_dynamics_gets_a_new_record(world) -> None:
    from sheltersim.domain.agent_dynamics import (
        AGENT_DYNAMICS_STATE_KEY,
        create_agent_dynamics_state,
    )

    world.model_state.setdefault(AGENT_DYNAMICS_STATE_KEY, create_agent_dynamics_state(world))
    person = _free_person(world)
    _command(world, minute=1, action="temporary_exit", agent_id=person, expected_return_minute=9)
    state = world.model_state[POPULATION_LIFECYCLE_STATE_KEY]
    state["people"][person]["agent_dynamics"] = None
    _command(world, minute=2, action="return", agent_id=person)
    assert world.model_state[AGENT_DYNAMICS_STATE_KEY]["agents"][person]["decision_due_minute"] == 2


def _command(world: WorldState, minute: int = 0, **fields: Any) -> dict[str, Any]:
    """Apply one presence command through the kernel, on a candidate, as the rule did."""
    world.minute_index = minute
    typed = PopulationPresenceCommand.model_validate(fields)
    ids = typed.selected_ids()
    event = _event(f"command-{fields['action']}-{minute}", minute=minute)
    candidate = copy.deepcopy(world)
    receipt = native.rules("population_lifecycle").apply_presence_action(
        world, candidate, event, typed, ids
    )
    _python_gave(receipt, candidate)
    population_lifecycle.validate_population_lifecycle_state(candidate)
    old_core = world.__dict__["_core"]
    world.__dict__.clear()
    world.__dict__.update(candidate.__dict__)
    # The copy's core is the world's now (lane 6-5 of Issue #152).
    state_mirror.owned_again(world, old_core)
    return receipt


def _free_person(world: WorldState) -> str:
    return next(iter(world.agents))


def test_a_person_leaves_and_returns_with_the_same_state(world) -> None:
    person = _free_person(world)
    needs_before = world.agents[person].needs.to_dict()
    zone = world.agents[person].zone_id

    receipt = _command(
        world,
        minute=1,
        action="temporary_exit",
        agent_id=person,
        expected_return_minute=9,
        source_ids=("ASM-TEST-A", "ASM-TEST-B"),
    )
    assert receipt["action"] == "temporary_exit"
    assert person not in world.agents
    presence = world.model_state[PRESENCE_NAMESPACE]["people"][person]
    assert presence["status"] == "temporarily_absent"
    assert presence["expected_return_minute"] == 9
    assert person in world.model_state[POPULATION_LIFECYCLE_STATE_KEY]["people"]

    assert _command(world, minute=2, action="return", agent_id=person)["action"] == "returned"
    assert world.agents[person].zone_id == zone
    assert world.agents[person].needs.to_dict() == needs_before
    assert world.model_state[PRESENCE_NAMESPACE]["people"][person]["status"] == "admitted"


def test_a_return_to_a_full_zone_waits_and_the_rule_admits_it_later(world) -> None:
    person = _free_person(world)
    zone = world.agents[person].zone_id
    _command(world, minute=1, action="transfer", agent_id=person, zone_id=_other_zone(world))
    world.zones[zone].capacity = _occupancy(world, zone)

    receipt = _command(world, minute=2, action="return", agent_id=person, zone_id=zone)
    assert receipt["action"] == "waiting"
    state = world.model_state[POPULATION_LIFECYCLE_STATE_KEY]
    assert [item["agent_id"] for item in state["pending_returns"]] == [person]

    # Still full.
    module_rule = "admit_pending_returns"
    assert _same_as_rule("population_lifecycle", module_rule, world, 3) == ()
    assert person not in world.agents

    world.zones[zone].capacity += 1
    admitted = _same_as_rule("population_lifecycle", module_rule, world, 3)
    assert admitted == (person,)
    assert state["pending_returns"] == []
    assert world.model_state[PRESENCE_NAMESPACE]["people"][person]["status"] == "admitted"
    assert world.model_state[PRESENCE_NAMESPACE]["history"][-1]["action"] == "returned"


def _command_suppressed(world: WorldState, text: str, minute: int = 3, **fields: Any) -> None:
    """A deferred command, by the kernel: the rule's text, and the world the rule left."""
    world.minute_index = minute
    typed = PopulationPresenceCommand.model_validate(fields)
    ids = typed.selected_ids()
    event = _event(f"command-{fields['action']}-{minute}", minute=minute)
    candidate = copy.deepcopy(world)
    with pytest.raises(EventSuppressed) as ours:
        native.rules("population_lifecycle").apply_presence_action(
            world, candidate, event, typed, ids
        )
    assert str(ours.value) == f"population exit waits until the person is free: {text}"
    _python_gave(world)


@pytest.mark.parametrize(
    ("how", "text"),
    [
        (Activity.QUEUED, "the person is queued"),
        ("request", "the person is waiting for support"),
        ("task", "the person owns an active task"),
    ],
    ids=["queued", "request", "task"],
)
def test_an_exit_of_someone_busy_is_deferred_as_the_rule_says(world, how: Any, text: str) -> None:
    person = _free_person(world)
    if how == "request":
        world.agents[person].support_request_id = "some-request"
    elif how == "task":
        world.active_tasks["some-task"] = ActiveTask(
            id="some-task",
            kind="travel_return",
            agent_id=person,
            resource_id=None,
            start_minute=0,
            due_minute=5,
        )
    else:
        world.agents[person].activity = how
    _command_suppressed(world, text, action="transfer", agent_id=person)
    assert any(item.event_id.startswith("presence-retry-") for item in world.event_queue.pending())


def test_an_exit_past_its_expected_return_is_dropped(world) -> None:
    person = _free_person(world)
    event = _event("late-exit", minute=9)
    event = ScheduledEvent(
        **{**event.to_dict(), "kind": event.kind, "provenance": {"population_retry": {"count": 1}}}
    )
    world.minute_index = 9
    fields = {"action": "temporary_exit", "agent_id": person, "expected_return_minute": 5}
    with pytest.raises(EventSuppressed) as ours:
        native.rules("population_lifecycle").apply_presence_command(world, event, fields)
    assert str(ours.value) == (
        "temporary exit dropped: the person was not free before the expected return"
    )
    _python_gave(world)


def test_a_whole_command_is_the_rule_s(world) -> None:
    person = _free_person(world)
    world.minute_index = 1
    event = _event("whole-command", minute=1)
    fields = {"action": "temporary_exit", "agent_id": person, "expected_return_minute": 9}
    receipt = native.rules("population_lifecycle").apply_presence_command(world, event, fields)
    _python_gave(receipt, world)
    assert person not in world.agents


def test_a_reunion_and_a_relocation_in_place_are_the_rule_s(world) -> None:
    first, second = [agent.id for agent in world.agents.values() if agent.zone_id == _zone(world)][
        :2
    ]
    receipt = _command(
        world, minute=1, action="reunite", agent_ids=(first, second), household_id="household-new"
    )
    assert receipt["agent_ids"] == sorted((first, second))
    assert native.rules("population_lifecycle").current_household_id(world, first) == (
        "household-new"
    )
    receipt = _command(
        world, minute=2, action="relocate", agent_id=first, zone_id=world.agents[first].zone_id
    )
    assert receipt["action"] == "relocated"


def test_a_relocation_walks_and_its_completion_is_the_rule_s(world) -> None:
    person = _free_person(world)
    origin = world.agents[person].zone_id
    destination = next(
        edge.to_zone_id for edge in world.zone_edges.values() if edge.from_zone_id == origin
    )
    world.zones[destination].capacity = None
    receipt = _command(world, minute=1, action="relocate", agent_id=person, zone_id=destination)
    assert receipt["action"] == "relocation_scheduled"
    assert world.agents[person].activity.value == "moving"
    relocation = world.model_state[POPULATION_LIFECYCLE_STATE_KEY]["pending_relocations"][person]
    event = next(
        item for item in world.event_queue.pending() if item.event_id == relocation["event_id"]
    )

    world.minute_index = event.due_minute
    agent = world.agents[person]
    agent.zone_id = destination
    agent.activity = type(agent.activity).IDLE
    agent.target_id = None
    world.active_tasks.pop(relocation["task_id"])

    _same_as_rule("population_lifecycle", "complete_presence_relocation", world, event)
    assert person not in world.model_state[POPULATION_LIFECYCLE_STATE_KEY]["pending_relocations"]
    assert world.model_state[PRESENCE_NAMESPACE]["history"][-1]["action"] == "relocated"


@pytest.mark.parametrize(
    ("fields", "text"),
    [
        (
            {"action": "temporary_exit", "agent_id": "nobody"},
            "temporary exit requires an admitted person",
        ),
        ({"action": "transfer", "agent_id": "nobody"}, "transfer requires an admitted person"),
        ({"action": "return", "agent_id": "nobody"}, "return requires a retained absent person"),
        (
            {"action": "relocate", "agent_id": "nobody", "zone_id": "nowhere"},
            "relocation requires an admitted person and valid destination",
        ),
        (
            {"action": "reunite", "agent_ids": ("nobody", "nor-them"), "household_id": "h"},
            "reunion requires every person to be admitted",
        ),
    ],
    ids=["temporary_exit", "transfer", "return", "relocate", "reunite"],
)
def test_a_command_the_rule_refuses_says_its_text_with_nothing_changed(
    world, fields, text: str
) -> None:
    typed = PopulationPresenceCommand.model_validate(fields)
    event = _event("refused-command")
    candidate = copy.deepcopy(world)
    before = _document(candidate)
    with pytest.raises(EventApplicationError) as made:
        native.rules("population_lifecycle").apply_presence_action(
            world, candidate, event, typed, typed.selected_ids()
        )
    assert getattr(made.value, NATIVE_OUTCOME)
    assert str(made.value) == text
    assert _document(candidate) == before


def _world_with_a_room(scenario: Any) -> WorldState:
    """Everyone full but one surveyed room in the quiet area."""
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


def _arrive_in_room(world: WorldState, count: int) -> None:
    arrived = _same_as_rule(
        "population",
        "queue_arrivals",
        world,
        event=_event(minute=1),
        zone_id="medical",
        count=count,
        admission_zone_ids=("quiet",),
    )
    assert arrived == count


def test_people_given_a_room_in_one_event_stand_apart(generated_scenario) -> None:
    world = _world_with_a_room(generated_scenario)
    _arrive_in_room(world, 3)
    admissions = world.model_state["geometry_overlay_v1"]["room_admissions"]
    assert len(admissions) == 3


def test_a_room_admission_keeps_half_a_metre_from_who_is_there(generated_scenario) -> None:
    probe = _world_with_a_room(generated_scenario)
    _arrive_in_room(probe, 1)
    (person,) = probe.model_state["geometry_overlay_v1"]["room_admissions"]
    point = probe.model_state["movement_v1"]["positions"][person]["point"]

    world = _world_with_a_room(generated_scenario)
    resident = next(iter(world.agents))
    world.model_state["geometry_overlay_v1"]["room_admissions"][resident] = {
        "agent_id": resident,
        "room_id": "room",
        "admitted_minute": 1,
        "event_id": "earlier",
        "source_ids": ["ASM-TEST-ROOM"],
    }
    # Just past the rule's distance, so the same point is still free.
    world.model_state["movement_v1"]["positions"][resident]["point"] = {
        "x": point["x"] + 0.501,
        "y": point["y"],
    }
    _arrive_in_room(world, 1)
    new = next(
        key
        for key in world.model_state["geometry_overlay_v1"]["room_admissions"]
        if key != resident
    )
    assert world.model_state["movement_v1"]["positions"][new]["point"] == point


def test_a_full_presence_history_drops_its_oldest_as_the_rule_does(world) -> None:
    template = {
        "schema_version": 2,
        "event_id": "old-event",
        "minute": 0,
        "action": "arrived",
        "from_zone_id": None,
        "to_zone_id": None,
        "from_household_id": None,
        "to_household_id": None,
        "causation_id": None,
        "source_ids": [],
        "state_snapshot_hash": None,
        "time_semantics": "recorded_event_minute",
    }
    person = next(iter(world.agents))
    world.model_state[PRESENCE_NAMESPACE]["history"] = [
        {**template, "transition_id": f"old-{index}", "agent_id": person} for index in range(20_000)
    ]
    world.zones[_zone(world)].capacity = None
    assert _arrive(world, count=2) == 2
    history = world.model_state[PRESENCE_NAMESPACE]["history"]
    assert len(history) == 20_000
    assert history[0]["transition_id"] == "old-2"


def test_a_full_lifecycle_history_drops_its_oldest_as_the_rule_does(world) -> None:
    state = world.model_state[POPULATION_LIFECYCLE_STATE_KEY]
    state["history"] = [{"event_id": f"old-{index}"} for index in range(20_000)]
    _command(world, minute=1, action="temporary_exit", agent_id=_free_person(world))
    history = world.model_state[POPULATION_LIFECYCLE_STATE_KEY]["history"]
    assert len(history) == 20_000
    assert history[0]["event_id"] == "old-1"


def _break_dynamics_pending(state: dict[str, Any]) -> None:
    state["pending_arrivals"][0]["zone_id"] = "no-such-zone"


def _break_dynamics_traits(state: dict[str, Any]) -> None:
    state["pending_arrivals"][0]["traits"]["age_years"] = 500


def _break_dynamics_context(state: dict[str, Any]) -> None:
    next(iter(state["profile_context"].values()))["source_ids"] = []


def _break_dynamics_field(state: dict[str, Any]) -> None:
    state["pending_arrivals"][0]["something_else"] = 1


@pytest.mark.parametrize(
    ("breaks", "text"),
    [
        (_break_dynamics_pending, "population_dynamics_v1 pending arrival is invalid"),
        # PersonTraits refused it: a contract's error is not the rule's own text.
        (_break_dynamics_traits, None),
        (
            _break_dynamics_context,
            "dynamic household context disagrees with generated profile provenance",
        ),
        (_break_dynamics_field, "population_dynamics_v1 pending arrival is invalid"),
    ],
    ids=["pending", "traits", "context", "field"],
)
def test_a_broken_population_state_raises_the_rule_s_text(world, breaks, text: str | None) -> None:
    zone = _zone(world)
    world.zones[zone].capacity = _occupancy(world, zone)
    _arrive(world, count=2)
    kernels = native.rules("population")
    assert kernels.validate_population_dynamics_state(world) is None

    breaks(world.model_state[population.POPULATION_DYNAMICS_STATE_KEY])

    if text is None:
        with pytest.raises(native.Unsupported):
            kernels.validate_population_dynamics_state(world)
        with pytest.raises(KernelRefusal):
            population.validate_population_dynamics_state(world)
    else:
        with pytest.raises(ValueError, match=f"^{re.escape(text)}$"):
            population.validate_population_dynamics_state(world)


def _break_lifecycle_field(state: dict[str, Any]) -> None:
    state["extra"] = 1


def _break_lifecycle_person(state: dict[str, Any]) -> None:
    next(iter(state["people"].values()))["agent"]["stress"] = 2.0


def _break_lifecycle_archive(state: dict[str, Any]) -> None:
    state["relationship_archive"].append({"namespace": "other"})


def _break_lifecycle_return(state: dict[str, Any]) -> None:
    state["pending_returns"].append({"agent_id": "nobody"})


@pytest.mark.parametrize(
    ("breaks", "text"),
    [
        (_break_lifecycle_field, "population_lifecycle_v1 has an invalid exact schema"),
        (_break_lifecycle_person, "population lifecycle normalized Agent value is invalid"),
        (_break_lifecycle_archive, "population_lifecycle_v1 relationship archive is invalid"),
        (_break_lifecycle_return, "population_lifecycle_v1 pending return is invalid"),
    ],
    ids=["field", "person", "archive", "return"],
)
def test_a_broken_lifecycle_state_raises_the_rule_s_text(world, breaks, text: str) -> None:
    _command(world, minute=1, action="temporary_exit", agent_id=_free_person(world))
    kernels = native.rules("population_lifecycle")
    assert kernels.validate_population_lifecycle_state(world) is None

    breaks(world.model_state[POPULATION_LIFECYCLE_STATE_KEY])

    with pytest.raises(ValueError, match=f"^{re.escape(text)}$"):
        population_lifecycle.validate_population_lifecycle_state(world)
    # As the registered validator, it says only that Python must check.
    with pytest.raises(native.Unsupported):
        kernels.validate(world)


def test_a_broken_presence_ledger_stops_the_kernels_before_a_write(world) -> None:
    zone = _zone(world)
    world.zones[zone].capacity = _occupancy(world, zone)
    _arrive(world, count=1)
    world.zones[zone].capacity += 1
    world.model_state[PRESENCE_NAMESPACE]["history"].append({"unknown": 1})
    before = _document(world)
    with pytest.raises(native.Unsupported):
        native.rules("population").admit_pending_arrivals(world, 0)
    assert _document(world) == before


# ---- The plan's reads of the world, in any order (lane 6-3) -------------------------------------


def _out_and_back(agent_id: str, exit_minute: int, return_minute: int) -> EffectProgram:
    """One program that takes a person out and brings them back."""
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


def _in_order(world: WorldState, reverse: bool) -> WorldState:
    """A copy whose people and tasks are in the canonical order (by ID), or its reverse."""
    other = copy.deepcopy(world)
    other.agents = dict(sorted(other.agents.items(), reverse=reverse))
    other.active_tasks = dict(sorted(other.active_tasks.items(), reverse=reverse))
    return other


@pytest.mark.parametrize("case", ["busy", "return_due"])
def test_the_plan_reads_the_world_in_any_order(world, case: str) -> None:
    """A presence command's transaction reads the world in the order the copy had, where the rule
    read the world as it was: what the plan reads of it (whether the person is busy, the minutes
    of the program's other commands) must not depend on that order."""
    person, other = sorted(world.agents)[:2]
    schedule_effect_program(world, _out_and_back(person, 1, 3))
    exit_event = next(
        event
        for event in world.event_queue.pending()
        if event.event_type == "effect_program_population"
        and event.payload["command"]["action"] == "temporary_exit"
    )
    for task_id, agent_id in (("task-b", person), ("task-a", other)):
        world.active_tasks[task_id] = ActiveTask(
            id=task_id,
            kind="travel_return",
            agent_id=agent_id,
            resource_id=None,
            start_minute=0,
            due_minute=5,
        )
    if case == "busy":
        text = "population exit waits until the person is free: the person owns an active task"
    else:
        # The exit is retried at the minute its program's return is due: that is read first.
        exit_event = ScheduledEvent(
            **{**exit_event.to_dict(), "kind": exit_event.kind, "due_minute": 3}
        )
        text = (
            "temporary exit dropped: the person was not free before the return in the same "
            "EffectProgram"
        )
    world.minute_index = exit_event.due_minute
    typed = PopulationPresenceCommand.model_validate(exit_event.payload["command"])
    readers = [_in_order(world, reverse=False), _in_order(world, reverse=True)]
    assert list(readers[0].agents) != list(readers[1].agents)
    assert list(readers[0].active_tasks) != list(readers[1].active_tasks)
    outcomes = []
    for reader in readers:
        candidate = copy.deepcopy(world)
        with pytest.raises(EventSuppressed) as raised:
            native.rules("population_lifecycle").apply_presence_action(
                reader, candidate, exit_event, typed, typed.selected_ids()
            )
        outcomes.append((str(raised.value), _document(candidate), _document(reader)))
    assert outcomes[0][0] == text
    assert outcomes[0] == outcomes[1]


# ---- The agent an admission makes (lane c5 of Issue #152) ---------------------------------------
#
# An admission puts the person's row into the world's table, and the entity store makes the Python
# object from it; `build_agent` of the core is what `_admit_profile`'s constructors made of a saved
# profile. Each case runs both: the same fields with the same types, the same ValueError, or a
# refusal where the constructors raise another error or make an object no row holds (the store
# would taint), or the profile has a mobility that is not a str (no saved profile has one).


def _constructed(profile: dict[str, Any], zone_id: str, minute: int, schema_version: int):
    """`_admit_profile`'s constructors, in their order."""
    from sheltersim.domain.models import (
        AgentProfile,
        AgentState,
        InitialNeedProfile,
        Mobility,
        NeedState,
    )

    initial: dict[str, Any] = {}
    initial.update(profile["initial_needs"])
    if schema_version == 5:
        initial["hydration_need"] = 0.0
    provenance: dict[str, Any] = {}
    provenance.update(profile["provenance"])
    options = {
        "id": profile["id"],
        "group_id": profile["group_id"],
        "initial_zone_id": profile["initial_zone_id"],
        "mobility": Mobility(profile["mobility"]),
        "support_need": profile["support_need"],
        "stress_sensitivity": profile["stress_sensitivity"],
        "rng_key": profile["rng_key"],
        "initial_needs": InitialNeedProfile(**initial),
        "provenance": provenance,
    }
    made = AgentProfile(**options)
    return AgentState(
        profile=made, zone_id=zone_id, needs=NeedState(**initial), next_decision_at=minute
    )


def _needs(value: Any) -> dict[str, Any]:
    return {
        name: getattr(value, name)
        for name in ("toilet_urgency", "hunger", "fatigue", "hydration_need")
    }


def _fields(agent: Any) -> dict[str, Any]:
    needs = _needs
    profile = agent.profile
    return {
        "profile": {
            "id": profile.id,
            "group_id": profile.group_id,
            "initial_zone_id": profile.initial_zone_id,
            "mobility": profile.mobility.value,
            "support_need": profile.support_need,
            "stress_sensitivity": profile.stress_sensitivity,
            "rng_key": profile.rng_key,
            "initial_needs": needs(profile.initial_needs),
            "provenance": profile.provenance,
        },
        "zone_id": agent.zone_id,
        "activity": agent.activity.value,
        "target_id": agent.target_id,
        "needs": needs(agent.needs),
        "stress": agent.stress,
        "trust": agent.trust,
        "next_decision_at": agent.next_decision_at,
        "support_request_id": agent.support_request_id,
        "support_cooldown_until": agent.support_cooldown_until,
    }


def _typed(value: Any) -> Any:
    """A value with the type of each atom, so that 1 and 1.0 differ."""
    if isinstance(value, dict):
        return {key: _typed(item) for key, item in value.items()}
    if isinstance(value, (list, tuple)):
        return [_typed(item) for item in value]
    return (type(value).__name__, repr(value))


def _fits_a_row(fields: dict[str, Any]) -> bool:
    profile = fields["profile"]
    floats = [
        *profile["initial_needs"].values(),
        *fields["needs"].values(),
        profile["support_need"],
        profile["stress_sensitivity"],
    ]
    texts = [profile["id"], profile["initial_zone_id"], profile["rng_key"]]
    return (
        all(type(value) is float for value in floats)
        and all(type(value) is str for value in texts)
        and (profile["group_id"] is None or type(profile["group_id"]) is str)
    )


def _profile(**changes: Any) -> dict[str, Any]:
    profile: dict[str, Any] = {
        "id": "arrival-1",
        "group_id": "household-1",
        "initial_zone_id": "z0",
        "mobility": "independent",
        "support_need": 0.25,
        "stress_sensitivity": 0.5,
        "rng_key": "arrival:1",
        "initial_needs": {
            "toilet_urgency": 0.1,
            "hunger": 0.2,
            "fatigue": 0.3,
            "hydration_need": 0.4,
        },
        "provenance": {"source_type": "assumption", "seed": 2026},
    }
    for key, value in changes.items():
        if value is _MISSING:
            profile.pop(key)
        else:
            profile[key] = value
    return profile


_MISSING = object()


def _profiles() -> list[tuple[dict[str, Any], int, int]]:
    import random

    cases: list[tuple[dict[str, Any], int, int]] = [
        (_profile(), 3, 6),
        (_profile(), 0, 5),
        (_profile(), -1, 6),
    ]
    for mobility in (
        "assisted",
        "wheelchair",
        "flying",
        "",
        "It's",
        "INDEPENDENT",
        3,
        None,
        ["independent"],
    ):
        cases.append((_profile(mobility=mobility), 0, 6))
    for value in (
        0.0,
        1.0,
        -0.0,
        -0.1,
        1.0000001,
        float("nan"),
        float("inf"),
        True,
        False,
        0,
        1,
        2,
        "0.5",
        None,
        [0.1],
    ):
        for name in ("toilet_urgency", "hydration_need"):
            needs = dict(_profile()["initial_needs"], **{name: value})
            cases.append((_profile(initial_needs=needs), 1, 6))
            cases.append((_profile(initial_needs=needs), 1, 5))
    cases.append((_profile(initial_needs={}), 0, 6))
    cases.append((_profile(initial_needs={"hunger": 0.5, "thirst": 0.1}), 0, 6))
    cases.append((_profile(initial_needs={"hunger": 2.0, "thirst": 0.1}), 0, 6))
    cases.append((_profile(mobility="x", initial_needs={"hunger": 2.0}), 0, 6))
    cases.append((_profile(mobility="x", rng_key=_MISSING), 0, 6))
    cases.append((_profile(mobility="x", support_need=0), 0, 6))
    for key in (
        "id",
        "group_id",
        "initial_zone_id",
        "support_need",
        "stress_sensitivity",
        "rng_key",
        "initial_needs",
        "provenance",
    ):
        cases.append((_profile(**{key: _MISSING}), 0, 6))
    for key, value in (
        ("group_id", None),
        ("group_id", 3),
        ("support_need", 0),
        ("support_need", 1),
        ("stress_sensitivity", "high"),
        ("id", 5),
        ("rng_key", None),
    ):
        cases.append((_profile(**{key: value}), 0, 6))
    draws = random.Random(2026)
    for _ in range(200):
        needs = {
            name: draws.choice([draws.random(), draws.uniform(-0.5, 1.5), 0.0, 1.0])
            for name in ("toilet_urgency", "hunger", "fatigue", "hydration_need")
            if draws.random() < 0.9
        }
        profile = _profile(
            mobility=draws.choice(["independent", "assisted", "wheelchair", "walking"]),
            support_need=draws.random(),
            stress_sensitivity=draws.random(),
            initial_needs=needs,
            group_id=draws.choice([None, "household-2"]),
        )
        cases.append((profile, draws.randrange(-2, 1000), draws.choice([5, 6])))
    return cases


def test_an_admitted_agent_is_what_the_constructors_make_of_its_profile() -> None:
    kernel = native.rules("population").agent_of_profile
    for profile, minute, version in _profiles():
        said, value = kernel(profile, "z1", minute, version)
        try:
            agent = _constructed(profile, "z1", minute, version)
        except ValueError as error:
            if said == "refused":
                # The enum's ValueError of a mobility that is not a str: no saved profile has one.
                assert not isinstance(profile.get("mobility"), str), (profile, value)
                continue
            assert (said, value) == ("value_error", str(error)), profile
            continue
        except (KeyError, TypeError):
            assert said == "refused", (profile, said, value)
            continue
        fields = _fields(agent)
        if not _fits_a_row(fields):
            assert said == "refused", (profile, said, value)
            continue
        assert said == "row", (profile, value)
        assert _typed(value) == _typed(fields), profile
