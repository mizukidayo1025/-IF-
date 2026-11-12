"""One test of each entry of the kernels of population.py and population_lifecycle.py, and the tests
of their Python boundary.

The rules are tested in the core without Python (native/core/tests/population_rules.rs, Issue
#225), case by case, against what their Python rules gave. These tests keep each entry of the PyO3
crate reaching the kernel and giving the rule's answer, and what only Python has: a refusal that is
a KernelRefusal, events scheduled as Python objects, the constructors of an admitted agent, and the
registered validator. The Python rules are gone (lane L10, `kernel_only`): each case was run with
them once (2d8fb46), and what they returned and the world they left is pinned in PYTHON_GAVE, per
test and in call order, as a digest compared on every OS, as the gate's hashes are. A kernel raises
the rule's own saved text where the rule raised one (tests/unit/domain/test_population_texts.py);
an input outside the rule's contract is a KernelRefusal, with nothing changed.
"""

from __future__ import annotations

import copy
import hashlib
import json
import os
import re
from typing import Any

import pytest

from sheltersim import native
from sheltersim.contracts.presence import PRESENCE_NAMESPACE, PresenceState
from sheltersim.core import state_mirror
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.world import WorldState
from sheltersim.domain import population, population_lifecycle
from sheltersim.domain.population_lifecycle import (
    POPULATION_LIFECYCLE_STATE_KEY,
    PopulationPresenceCommand,
    initialize_population_lifecycle_state,
)
from sheltersim.events.models import EventKind, EventSuppressed, ScheduledEvent


def _document(world: WorldState) -> str:
    # A digest: pytest's report of two long documents that differ takes minutes.
    text = world._canonical_text(world._canonical_data())
    return hashlib.sha256(text.encode()).hexdigest()


# ---- What the Python rules gave ---------------------------------------------------------------

PYTHON_GAVE: dict[str, list[str]] = {
    "test_a_broken_lifecycle_state_raises_the_rule_s_text": ["4e3e1a5cf967fc03"],
    "test_a_relocation_walks_and_its_completion_is_the_rule_s": [
        "36631542c997036d",
        "4e17d33e56d39170",
    ],
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
    "test_a_whole_command_is_the_rule_s": ["7ed9330305fa2915"],
    "test_arrivals_past_capacity_wait_and_are_admitted_when_room_opens": [
        "8c6f0730f5e57e7d",
        "819214104b4d924c",
        "240aab53b0c2be84",
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


def _same_suppression(world: WorldState, text: str, *arguments: Any, **options: Any) -> None:
    """The kernel stops with the rule's text, and the world keeps what the rule scheduled."""
    kernel = native.rules("population").schedule_population_retry
    with pytest.raises(EventSuppressed) as ours:
        kernel(world, *arguments, **options)
    assert str(ours.value) == text
    _python_gave(world)


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


def test_a_broken_lifecycle_state_raises_the_rule_s_text(world) -> None:
    _command(world, minute=1, action="temporary_exit", agent_id=_free_person(world))
    kernels = native.rules("population_lifecycle")
    assert kernels.validate_population_lifecycle_state(world) is None

    state = world.model_state[POPULATION_LIFECYCLE_STATE_KEY]
    next(iter(state["people"].values()))["agent"]["stress"] = 2.0

    text = "population lifecycle normalized Agent value is invalid"
    with pytest.raises(ValueError, match=f"^{re.escape(text)}$"):
        population_lifecycle.validate_population_lifecycle_state(world)
    # As the registered validator, it says only that Python must check.
    with pytest.raises(native.Unsupported):
        kernels.validate(world)


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
