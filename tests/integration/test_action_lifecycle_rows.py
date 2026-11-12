"""agent_dynamics_v1 owned by Rust, and the open actions and receipts of the action lifecycle.

native/src/rules/{agent_dynamics,action_lifecycle}.rs are the PyO3 entries of the core's rules
(docs/rust-port/lanes/f-action-lifecycle.md). Their Python bodies are gone (lane L10,
`kernel_only`). What a rule writes, against what the Python rule wrote on the same inputs (its
values, or the digests of its receipts, open actions and namespaces), and what the kernels refuse
of their inputs, are checked by the core's tests (native/core/tests/action_lifecycle_rows.rs, on
the worlds and the random inputs of this file, #225). Here: an entry test for each PyO3 entry, the
rows and their trees, what taints them and what the entries refuse themselves (KernelRefusal), the
store's Python object, and the validation of a saved namespace, which stays in Python and is
compared with the Rust one.
"""

from __future__ import annotations

import copy
import json
import random
from types import SimpleNamespace
from typing import Any

import pytest

from sheltersim import native
from sheltersim.core import state_mirror
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.state_mirror import rust_validated
from sheltersim.core.world import StateInvariantError, WorldState
from sheltersim.domain.action_lifecycle import (
    active_action_index,
    active_action_store,
    finalize_active_actions,
    record_action_proposal,
    record_applied_action_completion,
    record_applied_population_exit,
)
from sheltersim.domain.agent_dynamics import (
    ANNOUNCED_FACILITY_AGENT_DYNAMICS_POLICY_VERSION,
    ORIENTATION_FACILITY_SOURCE_ID,
    add_agent,
    agent_knows_facility,
    announce_installed_facility,
    canonical_agent_dynamics_config,
    create_agent_dynamics_state,
    learn_facility,
    record_proposal,
    validate_agent_dynamics_state,
)
from sheltersim.domain.simulator_state import initialize_simulator_extensions
from sheltersim.events.models import EventKind, EventLogRecord, EventStatus, ScheduledEvent
from sheltersim.events.queue import EventLog
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.policies.behavior import ActionKind

KEY = "agent_dynamics_v1"


def _text(value: Any) -> str:
    """Float bits, 1 against 1.0, and key order all show."""
    return json.dumps(value)


@pytest.fixture
def world(generated_scenario) -> WorldState:
    scenario = ScenarioSpec.model_validate(
        copy.deepcopy(generated_scenario.model_dump(mode="json"))
    )
    made = WorldState.from_scenario(scenario)
    initialize_simulator_extensions(made)
    return made


def _proposal(
    agent_id: str, action: ActionKind, minute: int, trigger: str = "t"
) -> SimpleNamespace:
    return SimpleNamespace(
        agent_id=agent_id,
        action=action,
        minute_index=minute,
        trigger=trigger,
        next_decision_at=minute + 5,
    )


def _agent_id(world: WorldState) -> str:
    return sorted(world.agents)[0]


def _event(
    event_id: str, event_type: str, sequence: int, due: int, **payload: Any
) -> ScheduledEvent:
    return ScheduledEvent(event_id, EventKind.COMPLETION, due, 0, sequence, event_type, payload)


def _applied(world: WorldState, event: ScheduledEvent, minute: int) -> ScheduledEvent:
    world.id_counters["event"] = max(world.id_counters.get("event", 0), event.sequence + 1)
    world.event_log.append(EventLogRecord(event, minute, EventStatus.APPLIED))
    return event


def _lifecycle_documents(world: WorldState) -> list[Any]:
    return [record.to_dict() for record in world.event_log.of_types({"agent_action_lifecycle"})]


def _receipts(
    world: WorldState, rule: Any, store: Any, *arguments: Any, **options: Any
) -> list[Any]:
    """Run a rule: its receipts, as documents."""
    before = len(world.event_log)
    rule(world, *arguments, active_actions=store, **options)
    raws = world.event_log.items_bytes(before, len(world.event_log))
    return [json.loads(raw) for raw in raws]


# ---- agent_dynamics_v1: the row ----


def test_a_row_writes_the_bytes_of_the_tree_and_a_write_reaches_it(world) -> None:
    mirror = world._core.mirror(KEY)
    assert mirror.row_json() == native.canonical_json(world.model_state[KEY])
    record = world.model_state[KEY]["agents"][_agent_id(world)]
    record["last_action"] = "wait"
    record["known_facility_ids"] = sorted(world.facilities)
    assert mirror.verify()
    assert mirror.row_json() == native.canonical_json(world.model_state[KEY])


@pytest.mark.parametrize(
    "key,value",
    [
        ("last_action", 3),
        ("last_target_id", 1.5),
        ("decision_reason", ["a"]),
        ("decision_due_minute", True),
        ("decision_due_minute", 1.0),
        ("decision_due_minute", 10**30),
        ("last_decision_minute", False),
        ("known_facility_ids", ("x",)),
        ("known_facility_ids", [1]),
    ],
)
def test_a_value_the_row_cannot_hold_taints_it_and_the_python_rule_decides(
    world, key: str, value: Any
) -> None:
    agent_id = _agent_id(world)
    world.model_state[KEY]["agents"][agent_id][key] = value
    assert world._core.mirror(KEY).tainted is not None
    assert not rust_validated(world, KEY)
    with pytest.raises(native.Unsupported):
        native.rules("agent_dynamics").record_proposal(
            world, _proposal(agent_id, ActionKind.WAIT, 1), target_id=None
        )


def test_the_kernels_write_no_whole_namespace(world) -> None:
    mirror = world._core.mirror(KEY)
    before = mirror.stats()["whole_writes"]
    agent_id = _agent_id(world)
    record_proposal(world, _proposal(agent_id, ActionKind.WAIT, 3), target_id=None)
    learn_facility(world, agent_id, sorted(world.facilities)[0])
    add_agent(world, agent_id)
    assert mirror.stats()["whole_writes"] == before


# ---- agent_dynamics_v1: the rules against the Python rules ----


V1 = {"schema_version": 1, "policy_version": "need-led-autonomy-memory-v1"}
ORIENTED = {"schema_version": 1, "source_id": ORIENTATION_FACILITY_SOURCE_ID}


@pytest.mark.parametrize(
    ("raw", "expected"),
    [
        (None, V1),
        (
            {"policy_version": "need-led-autonomy-memory-v2"},
            {**ORIENTED, "policy_version": "need-led-autonomy-memory-v2"},
        ),
    ],
)
def test_the_canonical_config_is_the_python_rules(raw: Any, expected: Any) -> None:
    """The entry builds the dict of the policy the core reads (the core's tests check each
    config's policy and text)."""
    config = canonical_agent_dynamics_config(raw)
    assert _text(config) == _text(
        {"schema_version": 1, "policy_version": expected["policy_version"], **expected}
    )


FIELDS = "agent_dynamics config has unsupported fields"
VERSION = "agent_dynamics config has an unsupported version"


@pytest.mark.parametrize(
    ("raw", "text"),
    [
        # A key that is not a str, which JSON cannot hold (the core's tests check the others).
        ({1: 1}, FIELDS),
        ({"schema_version": 2}, VERSION),
    ],
)
def test_a_config_the_python_rule_refuses_gets_its_text(raw: Any, text: str) -> None:
    with pytest.raises(ValueError) as raised:
        canonical_agent_dynamics_config(raw)
    assert type(raised.value) is ValueError
    assert str(raised.value) == text


def test_an_unhashable_policy_is_refused() -> None:
    """Python's `in` of a set raised TypeError for it."""
    with pytest.raises(KernelRefusal):
        canonical_agent_dynamics_config({"policy_version": []})


@pytest.mark.parametrize(
    "awareness",
    [
        {"initial_known_facility_ids": ["a", "a"]},
        # A tuple, which JSON cannot hold (the core's tests check the others).
        {"initial_known_facility_ids": ("a",)},
    ],
)
def test_an_awareness_config_the_model_decides_is_left_to_it(world, awareness: Any) -> None:
    world.model_configs["facility_awareness"] = awareness
    with pytest.raises(native.Unsupported):
        native.rules("agent_dynamics").create_state(world)


def test_an_announced_facility_is_known_to_every_agent(world) -> None:
    """The entry of `announce_installed_facility`: with the announced policy, every agent learns a
    facility of an orientation kind (the core's tests check the writers on random calls)."""
    world.model_configs["agent_dynamics"] = {
        "policy_version": ANNOUNCED_FACILITY_AGENT_DYNAMICS_POLICY_VERSION
    }
    facility = next(
        item.id
        for item in sorted(world.facilities.values(), key=lambda item: item.id)
        if item.kind == "toilet"
    )
    world.model_state[KEY]["agents"][_agent_id(world)]["known_facility_ids"] = []
    announce_installed_facility(world, facility)
    assert all(
        facility in record["known_facility_ids"]
        for record in world.model_state[KEY]["agents"].values()
    )
    assert agent_knows_facility(world, _agent_id(world), facility) is True


def test_a_learned_list_that_comes_out_the_same_is_not_written(world) -> None:
    agent_id = _agent_id(world)
    facility_id = sorted(world.facilities)[0]
    learn_facility(world, agent_id, facility_id)
    known = world.model_state[KEY]["agents"][agent_id]["known_facility_ids"]
    learn_facility(world, agent_id, facility_id)
    assert world.model_state[KEY]["agents"][agent_id]["known_facility_ids"] is known


def test_adding_an_agent_is_the_python_rule(world) -> None:
    agent_id = _agent_id(world)
    world.model_state[KEY]["agents"][agent_id]["last_action"] = "wait"
    expected = json.loads(_text(world.model_state[KEY]))
    # The record `create_agent_dynamics_state` gives the agent, in its place.
    expected["agents"][agent_id] = create_agent_dynamics_state(world)["agents"][agent_id]
    assert expected["agents"][agent_id]["last_action"] is None
    add_agent(world, agent_id)
    assert _text(world.model_state[KEY]) == _text(expected)
    native.rules("agent_dynamics").add_agent(world, "nobody")
    assert world._core.mirror(KEY).verify()


def test_without_a_config_every_facility_is_known_to_older_scenarios(world) -> None:
    world.model_configs.pop("agent_dynamics", None)
    world.model_configs.pop("facility_awareness", None)
    assert agent_knows_facility(world, "nobody", "nowhere") is True


# ---- agent_dynamics_v1: what stands behind retiring agent_dynamics_valid ----


MEMORY_SCHEMA = "agent_dynamics_v1 has an invalid exact schema"


def _record_error(agent_id: str) -> str:
    return f"agent_dynamics_v1 record for {agent_id!r} is invalid"


def _minute_error(agent_id: str) -> str:
    return f"agent_dynamics_v1 decision minute for {agent_id!r} is invalid"


def _error(world: WorldState) -> str | None:
    try:
        world.validate(validate_event_history=False)
    except StateInvariantError as error:
        return str(error.__cause__ or error)
    return None


def _python_error(world: WorldState, monkeypatch) -> str | None:
    with monkeypatch.context() as patch:
        patch.setattr(state_mirror, "_RUST_VALIDATORS", {})
        return _error(world)


@pytest.mark.parametrize(
    "write,message",
    [
        ({"known_facility_ids": ["nowhere"]}, _record_error),
        ({"known_facility_ids": "dup"}, _record_error),
        ({"known_facility_ids": ("x",)}, _record_error),
        ({"decision_due_minute": -1}, _record_error),
        ({"decision_due_minute": 1.0}, _record_error),
        ({"last_decision_minute": -1}, _minute_error),
        ({"last_decision_minute": True}, _minute_error),
        ({"last_decision_minute": 1.0}, _minute_error),
    ],
)
def test_a_bad_record_through_a_tracked_dict_stops_with_the_python_text(
    world, monkeypatch, write: dict[str, Any], message: Any
) -> None:
    agent_id = _agent_id(world)
    record = world.model_state[KEY]["agents"][agent_id]
    if write.get("known_facility_ids") == "dup":
        facility = sorted(world.facilities)[0]
        write = {"known_facility_ids": [facility, facility]}
    record.update(write)
    assert not rust_validated(world, KEY)
    found = _error(world)
    assert found is not None and message(agent_id) in found
    assert found == _python_error(world, monkeypatch)


@pytest.mark.parametrize(
    "route",
    ["whole namespace", "whole table", "missing agent", "extra agent", "schema", "policy", "key"],
)
def test_each_way_of_breaking_the_namespace_stops_with_the_python_text(
    world, monkeypatch, route: str
) -> None:
    agent_id = _agent_id(world)
    state = world.model_state[KEY]
    if route == "whole namespace":
        world.model_state[KEY] = {**json.loads(json.dumps(state)), "schema_version": 2}
    elif route == "whole table":
        table = json.loads(json.dumps(state))
        world.model_state = {**world.model_state, KEY: {**table, "policy_version": "other"}}
    elif route == "missing agent":
        del state["agents"][agent_id]
    elif route == "extra agent":
        state["agents"]["nobody"] = json.loads(json.dumps(state["agents"][agent_id]))
    elif route == "schema":
        state["schema_version"] = 2
    elif route == "policy":
        state["policy_version"] = "need-led-autonomy-memory-v2"
    else:
        state["other"] = 1
    assert not rust_validated(world, KEY)
    found = _error(world)
    assert found is not None and MEMORY_SCHEMA in found
    assert found == _python_error(world, monkeypatch)


def test_an_agent_who_leaves_the_world_is_seen_by_the_row_kernel(world) -> None:
    assert rust_validated(world, KEY)
    world.agents.pop(_agent_id(world))
    assert not rust_validated(world, KEY)


def test_a_good_namespace_is_accepted_and_python_agrees(world) -> None:
    assert rust_validated(world, KEY)
    validate_agent_dynamics_state(world)
    rng = random.Random(3)
    facility = sorted(world.facilities)[0]
    for _ in range(50):
        record = world.model_state[KEY]["agents"][rng.choice(sorted(world.agents))]
        record["decision_due_minute"] = rng.randrange(0, 100)
        record["last_decision_minute"] = rng.choice([None, 0, 5])
        record["known_facility_ids"] = rng.choice([[], [facility]])
        assert rust_validated(world, KEY)
        validate_agent_dynamics_state(world)


# ---- The open actions ----


def _store(world: WorldState) -> Any:
    return active_action_store(world)


def test_the_store_holds_rows_and_takes_its_plain_dict_back(world) -> None:
    store = _store(world)
    assert store.in_rows()
    agent_id = _agent_id(world)
    record_action_proposal(
        world,
        _proposal(agent_id, ActionKind.WAIT, 0),
        target_id=None,
        committed=True,
        active_actions=store,
    )
    assert store.in_rows() and agent_id in store and len(store) == 1
    assert "nobody" not in store and 3 not in store
    expected = store.snapshot()
    plain = store.as_plain()
    assert plain == expected and not store.in_rows()
    plain[agent_id]["continued"] = True
    world.minute_index = 1
    record_action_proposal(
        world,
        _proposal(agent_id, ActionKind.WAIT, 1),
        target_id=None,
        committed=True,
        active_actions=store,
    )
    assert store.in_rows() and store.snapshot()[agent_id]["continued"] is True


def test_a_store_that_does_not_fit_its_rows_is_refused(world) -> None:
    """No rule writes an int need: the store can't take it back into its rows."""
    store = _store(world)
    agent_id = _agent_id(world)
    record_action_proposal(
        world,
        _proposal(agent_id, ActionKind.WAIT, 0),
        target_id=None,
        committed=True,
        active_actions=store,
    )
    store.as_plain()[agent_id]["start_needs"]["fatigue"] = 1
    before = len(world.event_log), dict(world.id_counters), _text(store.snapshot())
    with pytest.raises(KernelRefusal):
        record_action_proposal(
            world,
            _proposal(agent_id, ActionKind.REST, 1),
            target_id=None,
            committed=True,
            active_actions=store,
        )
    assert not store.in_rows()
    assert (len(world.event_log), dict(world.id_counters), _text(store.snapshot())) == before


def test_a_restored_log_gives_a_store_in_rows(world) -> None:
    store = _store(world)
    agent_id = _agent_id(world)
    record_action_proposal(
        world,
        _proposal(agent_id, ActionKind.WAIT, 0),
        target_id=None,
        committed=True,
        active_actions=store,
    )
    raws = world.event_log.items_bytes(0, len(world.event_log))
    restored = EventLog.from_saved(raws)
    twin = SimpleNamespace(event_log=restored)
    again = active_action_store(twin)
    assert again.in_rows()
    assert again.snapshot()[agent_id]["start_needs"] == dict(
        active_action_index(twin)[agent_id]["start_needs"]
    )
    assert again.snapshot()[agent_id] == store.snapshot()[agent_id]


# ---- The entries of the rules (the core's tests check their receipts) ----


def test_a_sequence_of_actions_gives_each_transition_s_receipts(world) -> None:
    """The entries of `record_action_proposal` and `finalize_active_actions` on random proposals,
    whose receipts the core's tests check against the Python rules'."""
    rng = random.Random(11)
    store = _store(world)
    agents = sorted(world.agents)[:6]
    actions = [ActionKind.WAIT, ActionKind.REST, ActionKind.SLEEP, ActionKind.FETCH_WATER]
    seen = set()
    for minute in range(300):
        world.minute_index = minute
        agent_id = rng.choice(agents)
        if rng.random() < 0.3:
            world.agents[agent_id].needs.fatigue = rng.random()
        target = rng.choice([None, None, "T1"])
        receipts = _receipts(
            world,
            record_action_proposal,
            store,
            _proposal(agent_id, rng.choice(actions), minute),
            target_id=target,
            committed=rng.random() < 0.9,
        )
        seen.update(record["event"]["payload"]["transition"] for record in receipts)
    world.minute_index = 300
    receipts = _receipts(world, finalize_active_actions, store, 300)
    seen.update(record["event"]["payload"]["transition"] for record in receipts)
    assert len(store) == 0
    assert {"started", "continued", "completed", "interrupted", "censored"} <= seen
    for document in _lifecycle_documents(world):
        assert document["status"] == "applied"
        assert (
            document["detail"] == f"autonomous action {document['event']['payload']['transition']}"
        )
        assert document["event"]["event_id"] == f"action-lifecycle-{document['event']['sequence']}"


def test_events_the_entry_turns_away_change_nothing(world) -> None:
    """A failed event and one the log lacks are turned away by the PyO3 entry (the core's tests
    check the events the kernel finds end nothing)."""
    store = _store(world)
    agent_id = _agent_id(world)
    record_action_proposal(
        world,
        _proposal(agent_id, ActionKind.FETCH_WATER, 5),
        target_id=None,
        committed=True,
        active_actions=store,
    )
    open_actions = _text(store.snapshot())
    failed = _event("e-failed", "resource_service_complete", 901, 6, agent_id=agent_id)
    world.event_log.append(EventLogRecord(failed, 6, EventStatus.FAILED))
    assert not _receipts(world, record_applied_action_completion, store, failed, 6)
    unknown = _event("e-absent", "resource_service_complete", 902, 6, agent_id=agent_id)
    assert not _receipts(world, record_applied_action_completion, store, unknown, 6)
    assert _text(store.snapshot()) == open_actions


def test_a_population_exit_interrupts_the_actions_of_those_it_took(world) -> None:
    """The entry of `record_applied_population_exit` (the core's tests check its receipts against
    the Python rule's); an event of another type ends nothing."""
    store = _store(world)
    agents = sorted(world.agents)
    for agent_id in agents[:4]:
        record_action_proposal(
            world,
            _proposal(agent_id, ActionKind.WAIT, 1),
            target_id=None,
            committed=True,
            active_actions=store,
        )
    event = _applied(world, _event("pop-1", "effect_program_population", 900, 3), 3)
    world.model_state["population_lifecycle_v1"] = {
        "history": [
            {"event_id": "pop-1", "action": "temporary_exit", "agent_id": agents[3]},
            {"event_id": "pop-1", "action": "transferred", "agent_id": agents[1]},
            {"event_id": "pop-1", "action": "returned", "agent_id": agents[0]},
            {"event_id": "other", "action": "transferred", "agent_id": agents[0]},
            "not a dict",
        ]
    }
    world.model_state["population_dynamics_v1"] = {
        "departures": [{"event_id": "pop-1", "agent_id": agents[1]}, {"event_id": "pop-2"}]
    }
    world.agents.pop(agents[1])
    receipts = _receipts(world, record_applied_population_exit, store, event, 3)
    assert [record["event"]["payload"]["reason_code"] for record in receipts] == [
        "population_departure",
        "population_temporary_exit",
    ]
    other = _event("o-1", "toilet_service_complete", 901, 3, agent_id=agents[0])
    assert not _receipts(world, record_applied_population_exit, store, other, 3)


@pytest.mark.parametrize(
    "body",
    [{"sleep_debt_minutes": 3, "hygiene_need": 0.25}, {}, {"sleep_debt_minutes": 1.5}],
)
def test_a_body_record_that_does_not_fit_its_row_is_refused(world, body: dict[str, Any]) -> None:
    """The kernels read the body state's row alone (lane P8 a2): a record of other types or fields
    leaves the namespace without a row, and the rule refuses with nothing changed."""
    agent_id = _agent_id(world)
    world.model_state["body_state_v1"]["agents"][agent_id] = body
    _refused(world, _store(world), _proposal(agent_id, ActionKind.SLEEP, 0))


# ---- What the kernels refuse, with nothing changed ----


def _refused(world: WorldState, store: Any, proposal: Any, target_id: Any = None) -> None:
    """Where the Python rule's receipts' model refused or converted a value, or its log raised."""
    before = len(world.event_log), dict(world.id_counters), _text(store.snapshot())
    with pytest.raises(native.Unsupported):
        native.rules("action_lifecycle").record_action_proposal(
            world, proposal, target_id=target_id, committed=True, active_actions=store
        )
    with pytest.raises(KernelRefusal):
        record_action_proposal(
            world, proposal, target_id=target_id, committed=True, active_actions=store
        )
    assert (len(world.event_log), dict(world.id_counters), _text(store.snapshot())) == before


@pytest.mark.parametrize("reason", [5, None])
def test_a_reason_the_entry_cannot_read_is_refused(world, reason: Any) -> None:
    """The core's tests check the reasons the receipts' model decides."""
    _refused(world, _store(world), _proposal(_agent_id(world), ActionKind.WAIT, 0, reason))


def test_a_target_the_entry_cannot_read_is_refused(world) -> None:
    """The core's tests check the targets the receipts' model decides."""
    _refused(world, _store(world), _proposal(_agent_id(world), ActionKind.WAIT, 0), 5)


@pytest.mark.parametrize("value", [float("nan"), float("inf"), "x", None, True, 2**60])
def test_a_body_value_the_rule_decides_is_refused(world, value: Any) -> None:
    agent_id = _agent_id(world)
    world.model_state["body_state_v1"]["agents"][agent_id] = {"hygiene_need": value}
    _refused(world, _store(world), _proposal(agent_id, ActionKind.WAIT, 0))


def test_a_tainted_namespace_or_entity_refuses_a_proposal(world) -> None:
    agent_id = _agent_id(world)
    world.agents[agent_id].needs.fatigue = 1
    store = _store(world)
    with pytest.raises(native.Unsupported):
        native.rules("action_lifecycle").record_action_proposal(
            world,
            _proposal(agent_id, ActionKind.WAIT, 0),
            target_id=None,
            committed=True,
            active_actions=store,
        )


def test_a_committed_flag_that_is_not_a_bool_is_refused(world) -> None:
    with pytest.raises(native.Unsupported):
        native.rules("action_lifecycle").record_action_proposal(
            world,
            _proposal(_agent_id(world), ActionKind.WAIT, 0),
            target_id=None,
            committed=1,
            active_actions=_store(world),
        )


def _open_fetch(world, store) -> str:
    agent_id = _agent_id(world)
    record_action_proposal(
        world,
        _proposal(agent_id, ActionKind.FETCH_WATER, 1),
        target_id=None,
        committed=True,
        active_actions=store,
    )
    return agent_id


def test_another_object_of_the_same_row_is_the_event_the_log_holds(world) -> None:
    store = _store(world)
    agent_id = _open_fetch(world, store)
    _applied(world, _event("e-1", "resource_service_complete", 900, 2, agent_id=agent_id), 2)
    copied = _event("e-1", "resource_service_complete", 900, 2, agent_id=agent_id)
    native.rules("action_lifecycle").record_applied_action_completion(
        world, copied, 2, active_actions=store
    )
    assert agent_id not in store


def test_a_record_a_row_cannot_tell_from_the_event_is_refused(world) -> None:
    store = _store(world)
    agent_id = _open_fetch(world, store)
    _applied(world, _event("e-1", "resource_service_complete", 900, 2, agent_id=agent_id, n=1), 2)
    # Python's `==` takes 1 and 1.0 as equal, which a row does not.
    copied = _event("e-1", "resource_service_complete", 900, 2, agent_id=agent_id, n=1.0)
    with pytest.raises(native.Unsupported):
        native.rules("action_lifecycle").record_applied_action_completion(
            world, copied, 2, active_actions=store
        )
    with pytest.raises(KernelRefusal):
        record_applied_action_completion(world, copied, 2, active_actions=store)
    assert agent_id in store


def test_the_writers_write_the_row_and_a_tree_taken_past_it_follows(world) -> None:
    """The row is the truth (lane P8-0c): a record taken from the tree alone is written in the row,
    and the tree has it again, with the row's values, when the kernel gives the world back."""
    agent_id = _agent_id(world)
    facility = sorted(world.facilities)[0]
    mirror = world._core.mirror(KEY)
    agents = world.model_state[KEY]["agents"]
    agents[agent_id]["known_facility_ids"] = []
    dict.pop(agents, agent_id)
    rules = native.rules("agent_dynamics")
    rules.record_proposal(world, _proposal(agent_id, ActionKind.WAIT, 1), target_id=None)
    rules.learn_facility(world, agent_id, facility)
    record = world.model_state[KEY]["agents"][agent_id]
    assert record["last_action"] == "wait" and facility in record["known_facility_ids"]
    assert json.loads(mirror.row_json())["agents"][agent_id] == record
    assert mirror.verify()


# ---- A Run: the kernels really run, and saved identities are kept ----


def test_a_tick_makes_no_lifecycle_object_and_writes_no_whole_namespace(generated_scenario) -> None:
    from sheltersim.core.engine import TickEngine
    from sheltersim.experiments.metrics import MetricsRecorder
    from sheltersim.experiments.timeline import _MetricSink

    scenario = ScenarioSpec.model_validate(generated_scenario.model_dump(mode="json"))
    world = WorldState.from_scenario(scenario)
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    engine = TickEngine(world, phase_handler=controller)
    whole_writes = None
    for minute in range(30):
        engine.step()
        if minute == 0:
            whole_writes = world._core.mirror(KEY).stats()["whole_writes"]
    assert controller.active_actions.in_rows() and controller.active_actions.in_run()
    assert world._core.mirror(KEY).stats()["whole_writes"] == whole_writes
    # Records of the type have no object until something reads them.
    rows = native.state("events").saved_rows(world, "agent_action_lifecycle")
    assert rows
    assert world.event_log.unread() >= len(rows)


def test_two_identical_runs_with_a_restore_between_keep_the_open_actions(
    generated_scenario, tmp_path
) -> None:
    from sheltersim.experiments.runner import run_scenario

    document = generated_scenario.model_dump(mode="json")
    document["duration_minutes"] = 45
    world = run_scenario(ScenarioSpec.model_validate(document), tmp_path / "run")
    restored = WorldState.from_dict(world.to_dict())
    store = active_action_store(restored)
    index = active_action_index(world)
    keys = store.snapshot()
    assert set(keys) == set(index)
