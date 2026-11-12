"""social_network_v1 owned by Rust, with its rules (docs/rust-port/lanes/m3-social-network.md).

Here: what taints the row, each way into it that the per-tick check must see now that it reads
only the written records, what the entries refuse themselves, and an entry test for each PyO3
entry. The rules have no Python body left (lane L10, `kernel_only`): the core's tests
(native/core/tests/social_network_rows.rs, on the worlds and the random inputs of this file, #225)
check `observe_event`, the social dialogue's context and the social action of
`SocialInteractionHandler` against the bits and the texts the Python rules gave, and the action
against the rule's formula, written down from it (`_apply_formula` here).
"""

from __future__ import annotations

import copy
import random
from typing import Any

import pytest

from sheltersim import native
from sheltersim.core.engine import TraceLevel
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.state_mirror import owned_row_bytes, rust_validated
from sheltersim.domain import social_dialogue, social_network
from sheltersim.domain.models import ordered_pair_key
from sheltersim.domain.simulator_state import validate_simulator_extensions
from sheltersim.domain.social_network import SOCIAL_NETWORK_STATE_KEY as KEY
from sheltersim.events.handlers import SocialInteractionHandler
from sheltersim.events.models import (
    EventKind,
    ScheduledEvent,
)
from sheltersim.experiments.runner import run_scenario
from sheltersim.experiments.scenario import ScenarioSpec

SOURCE_ID = "ASM-EVENT-TRIGGERED-SOCIAL-INTERACTIONS-V1"
ACTIONS = {
    "reassurance_exchange": ("offer_reassurance", "emotional_check_in"),
    "family_care": ("provide_family_support", "family_care"),
    "voluntary_support": ("offer_practical_help", "mutual_aid"),
}


@pytest.fixture(scope="module")
def finished(generated_scenario_preview: Any, tmp_path_factory: pytest.TempPathFactory) -> Any:
    document = copy.deepcopy(generated_scenario_preview["scenario"])
    document["duration_minutes"] = 40
    return run_scenario(
        ScenarioSpec.model_validate(document),
        tmp_path_factory.mktemp("run"),
        trace_level=TraceLevel.OFF,
    )


@pytest.fixture
def world(finished: Any) -> Any:
    return copy.deepcopy(finished)


def _mirror(world: Any) -> Any:
    return world._core.mirror(KEY)


def _rules() -> Any:
    return native.rules("social_network")


def _record(**changes: Any) -> dict[str, Any]:
    return {
        "contact_count": 1,
        "conversation_count": 0,
        "cooperation_count": 0,
        "trust": 0.5,
        "shared_message_ids": [],
        "last_contact_minute": 3,
        "last_interaction": None,
        **changes,
    }


def _accepted(world: Any) -> bool:
    return native.state("social_network").validate(world) is True


def _people(world: Any, count: int = 2) -> list[str]:
    return sorted(world.agents)[:count]


def _colocate(world: Any, ids: list[str]) -> str:
    zone = world.agents[ids[0]].zone_id
    for id_ in ids:
        world.agents[id_].zone_id = zone
    return zone


def _event(
    event_type: str, payload: dict[str, Any], minute: int, number: int = 1
) -> ScheduledEvent:
    return ScheduledEvent(
        event_id=f"e{number}",
        kind=EventKind.ENDOGENOUS,
        due_minute=minute,
        priority=0,
        sequence=number,
        event_type=event_type,
        payload=payload,
        source_id="policy-v1",
        provenance={
            "source_id": SOURCE_ID,
            "source_type": "assumption",
            "policy_version": "policy-v1",
        },
    )


def _action(
    world: Any, kind: str, actor: str, participant: str, number: int = 1, **more: Any
) -> Any:
    action, topic = ACTIONS[kind]
    return _event(
        "social_interaction",
        {
            "interaction_type": kind,
            "agent_id": actor,
            "participant_agent_id": participant,
            "zone_id": world.agents[actor].zone_id,
            "topic_id": topic,
            "action": action,
            "reason_short": "autonomous_need_choice",
            "interaction_duration_minutes": 5,
            **more,
        },
        world.minute_index,
        number,
    )


# ---- The row ---------------------------------------------------------------------------------


def test_a_real_run_fills_the_row_and_the_save_uses_its_bytes(finished: Any) -> None:
    mirror = _mirror(finished)
    assert finished.model_state[KEY]["relationships"]
    assert mirror.verify()
    assert mirror.row_json() == native.canonical_json(finished.model_state[KEY])
    assert owned_row_bytes(finished)[1][KEY] == native.canonical_json(finished.model_state[KEY])
    assert _accepted(finished)


def test_a_run_writes_the_namespace_whole_only_when_it_starts(finished: Any) -> None:
    assert _mirror(finished).stats()["whole_writes"] <= 2


@pytest.mark.parametrize(
    "change",
    [
        {"trust": 1},
        {"contact_count": 1.0},
        {"contact_count": True},
        {"shared_message_ids": [3]},
        {"last_interaction": 7},
        {"last_contact_minute": 2.0},
        {"more": 1},
    ],
)
def test_a_value_of_another_type_taints_and_the_kernel_refuses(world: Any, change: dict) -> None:
    relationships = world.model_state[KEY]["relationships"]
    relationships['["x1","x2"]'] = _record(**change)
    assert _mirror(world).row_json() is None
    with pytest.raises(native.Unsupported):
        native.state("social_network").validate(world)
    assert not rust_validated(world, KEY)
    try:
        social_network.validate_social_network_state(world)
    except ValueError as error:
        assert "social_network_v1" in str(error)
    # No rule writes a tainted row: the kernel refuses it, and nothing is written.
    first, second = _people(world)
    event = _event(
        "information_contact_update",
        {"recipient_agent_id": first, "sender_agent_id": second},
        world.minute_index,
    )
    before = world.model_state[KEY]["interaction_count"]
    with pytest.raises(native.Unsupported):
        _rules().observe_event(world, event, world.minute_index)
    with pytest.raises(KernelRefusal):
        social_network.observe_event(world, event, world.minute_index)
    assert world.model_state[KEY]["interaction_count"] == before


@pytest.mark.parametrize(
    "write",
    [
        lambda state, record: record.__setitem__("trust", 2.0),
        lambda state, record: record.__setitem__("trust", float("nan")),
        lambda state, record: record.__setitem__("contact_count", -1),
        lambda state, record: record.__setitem__("last_contact_minute", -1),
        lambda state, record: record.update(conversation_count=-3),
        lambda state, record: state["relationships"].__setitem__('["x1","x1"]', _record()),
        lambda state, record: state["relationships"].__setitem__('["x1"]', _record()),
        lambda state, record: state["relationships"].update({"[": _record()}),
        lambda state, record: state["relationships"].setdefault('["a", "b"]', _record()),
        lambda state, record: state.__setitem__("relationships", {"[": _record()}),
        lambda state, record: state.__setitem__("schema_version", 2),
        lambda state, record: state.__setitem__("policy_version", "other"),
    ],
)
def test_every_write_the_check_of_a_tick_must_see_after_it_has_passed(
    world: Any, write: Any
) -> None:
    """The check reads only written records, so each way of writing has to reach it. What stops
    the world is the Python rule's own text, the same as with no kernel."""
    state = world.model_state[KEY]
    assert _accepted(world)
    assert _accepted(world)
    write(state, next(iter(state["relationships"].values())))
    assert not _accepted(world)
    # It stays refused: the bad record is not forgotten by having been read once.
    assert not _accepted(world)
    with pytest.raises(ValueError, match="social_network_v1") as rust_way:
        validate_simulator_extensions(world)
    with pytest.raises(ValueError, match="social_network_v1") as python_way:
        social_network.validate_social_network_state(world)
    assert str(rust_way.value) == str(python_way.value)


def test_a_bad_record_read_again_passes_once_it_is_fixed_or_gone(world: Any) -> None:
    relationships = world.model_state[KEY]["relationships"]
    key = next(iter(relationships))
    assert _accepted(world)
    relationships[key]["trust"] = 2.0
    assert not _accepted(world)
    relationships[key]["trust"] = 0.25
    assert _accepted(world)
    relationships['["x1","x1"]'] = _record()
    assert not _accepted(world)
    del relationships['["x1","x1"]']
    assert _accepted(world)


def test_more_writes_than_it_lists_still_reach_the_check(world: Any) -> None:
    record = next(iter(world.model_state[KEY]["relationships"].values()))
    assert _accepted(world)
    record["trust"] = 2.0
    for _ in range(5000):
        record["contact_count"] += 1
    assert not _accepted(world)
    record["trust"] = 0.5
    assert _accepted(world)


def test_a_restored_copy_is_checked_from_the_start(world: Any) -> None:
    world.model_state[KEY]["relationships"]['["x1","x1"]'] = _record()
    assert not _accepted(world)
    assert not _accepted(copy.deepcopy(world))


def test_the_kernel_takes_no_key_python_refuses(world: Any) -> None:
    good = native.state("social_network").key_is_good
    keys = [
        '["a","b"]',
        '["a","a"]',
        '["a"]',
        '["a","b","c"]',
        '["a", "b"]',
        '[ "a","b"]',
        '["a","b"] ',
        '["a\\"","b"]',
        '["\\u3042","b"]',
        '["あ","b"]',
        '["a\\n","b"]',
        '["a\x7f","b"]',
        '["","b"]',
        '["",""]',
        '["a",1]',
        '{"a":"b"}',
        "[",
        "",
    ]
    rng = random.Random(7)
    alphabet = '[]",\\ab \x7fあ'
    keys += ["".join(rng.choice(alphabet) for _ in range(rng.randint(0, 12))) for _ in range(5000)]
    keys += [
        '["' + "".join(rng.choice('ab",\\') for _ in range(rng.randint(0, 8))) + '"]'
        for _ in range(5000)
    ]
    accepted = 0
    for key in keys:
        if good(key):
            accepted += 1
            assert social_network._pair_key_problem(key) is None, key
    assert accepted >= 10


def test_python_writes_into_the_row(world: Any) -> None:
    state = world.model_state[KEY]
    key = next(iter(state["relationships"]))
    state["relationships"][key]["shared_message_ids"].append("m1")
    state["interaction_count"] += 1
    assert _mirror(world).row_json() == native.canonical_json(state)
    popped = state["relationships"].pop(key)
    assert popped["shared_message_ids"][-1] == "m1"
    assert _mirror(world).row_json() == native.canonical_json(state)


# ---- The entry of the rules --------------------------------------------------------------------


def test_the_message_list_keeps_its_last_64_in_row_and_tree(world: Any) -> None:
    """The entry of `observe_event` (the core's tests check the rule against the Python rule's
    bits)."""
    first, second = _people(world)
    key = ordered_pair_key(first, second)
    for number in range(70):
        event = _event(
            "information_contact_update",
            {"recipient_agent_id": first, "sender_agent_id": second, "message_id": f"n{number}"},
            world.minute_index,
            number + 1,
        )
        _rules().observe_event(world, event, world.minute_index)
    ids = world.model_state[KEY]["relationships"][key]["shared_message_ids"]
    assert ids[0] == "n6" and ids[-1] == "n69" and len(ids) == 64
    assert _mirror(world).verify()


def test_record_observed_contacts_comes_through_the_social_step_without_python(
    finished: Any,
) -> None:
    """The step writes the row itself: forwards are the population's and the first tick's."""
    assert _mirror(finished).stats()["forwards"] < 50


# ---- The state context of a social event -------------------------------------------------------


@pytest.mark.parametrize(
    "edit",
    [
        lambda record: record.pop("hygiene_need"),
        lambda record: record.update(reassurance=1, discontent=False),
    ],
)
def test_capture_refuses_a_body_state_without_its_row(world: Any, edit: Any) -> None:
    """The kernel reads the body state's row alone (lane P8 a2): a record without a field, or with
    an int or a bool, leaves the namespace without a row, and the capture refuses."""
    first, second = _people(world)
    edit(world.model_state["body_state_v1"]["agents"][first])
    with pytest.raises(native.Unsupported, match="the namespace has no row"):
        native.rules("social_dialogue").capture_context(world, (first, second))
    with pytest.raises(KernelRefusal):
        social_dialogue.capture_social_state_context(world, (first, second))


def test_a_body_value_out_of_bounds_says_the_rule_s_text(world: Any) -> None:
    """The entry of `capture_context` (the core's tests check the captures and the other texts)."""
    first, second = _people(world)
    world.model_state["body_state_v1"]["agents"][first]["discontent"] = 1.5
    with pytest.raises(ValueError, match="^social dialogue state values must be finite and within"):
        social_dialogue.capture_social_state_context(world, (first, second))


def _context(world: Any, ids: tuple[str, str]) -> dict[str, Any]:
    return social_dialogue.capture_social_state_context(world, ids)


_NOT_ITS_EVENT = "social dialogue state context does not match its event"
_NOT_UNIT = "social dialogue state values must be finite and within 0..1"


@pytest.mark.parametrize(
    ("spoil", "text"),
    [
        (lambda c, i: c.__setitem__("schema_version", 2), _NOT_ITS_EVENT),
        (lambda c, i: c["participants"][i].__setitem__("stress", 1.5), _NOT_UNIT),
    ],
)
def test_validation_of_a_context_says_the_rule_s_text(world: Any, spoil: Any, text: str) -> None:
    """The entry of `validate_context`, of a mapping and of an event's frozen payload (the core's
    tests check each text in the rule's order of checks)."""
    ids = tuple(_people(world))
    context = _context(world, ids)
    kernel = native.rules("social_dialogue").validate_context
    assert kernel(context, ids, world.minute_index) is None
    assert kernel(_event("x", {"c": context}, 0).payload["c"], ids, world.minute_index) is None
    spoil(context, ids[0])
    with pytest.raises(ValueError) as raised:
        social_dialogue.validate_social_state_context(context, ids, world.minute_index)
    assert str(raised.value) == text
    assert type(raised.value) is ValueError


# ---- The social action of the handler ----------------------------------------------------------


def _values(world: Any, ids: list[str]) -> list[str]:
    return [
        value.hex()
        for id_ in ids
        for value in (
            world.agents[id_].stress,
            world.agents[id_].trust,
            world.agents[id_].needs.fatigue,
        )
    ]


def _apply_kernel(world: Any, event: Any) -> None:
    native.rules("social_interaction").apply(SocialInteractionHandler(), world, event)


def _apply_formula(world: Any, event: Any) -> None:
    """The deleted Python rule's writes of a social action whose checks all pass.

    The participant is relieved by the transfer times the actor's affect signal, clipped to -1..1:
    stress down by it, trust up by half of it, both within 0..1. Except in a reassurance exchange,
    the actor gains 0.002 fatigue and a quarter of a positive relief as trust.
    """
    payload = event.payload
    actor = world.agents[payload["agent_id"]]
    participant = world.agents[payload["participant_agent_id"]]
    state = world.model_state.get("body_state_v1", {})
    body = state.get("agents", {}).get(actor.id, {})
    signal = min(
        1.0, max(-1.0, body.get("positive_affect", 0.5) - body.get("negative_affect", 0.0))
    )
    relief = state.get("config", {}).get("social_action_affect_transfer", 0.02) * signal
    participant.stress = min(1.0, max(0.0, participant.stress - relief))
    participant.trust = min(1.0, max(0.0, participant.trust + relief * 0.5))
    if payload["interaction_type"] != "reassurance_exchange":
        actor.needs.fatigue = min(1.0, actor.needs.fatigue + 0.002)
        actor.trust = min(1.0, actor.trust + max(0.0, relief * 0.25))


def test_a_1_percent_change_of_the_transfer_shows_in_the_action(world: Any) -> None:
    """The entry of the social action (the core's tests check it against the formula on random
    states, and the texts of the actions it turns down)."""
    ids = _people(world)
    _colocate(world, ids)
    other = copy.deepcopy(world)
    other.model_state["body_state_v1"]["config"]["social_action_affect_transfer"] *= 1.01
    record = world.model_state["body_state_v1"]["agents"][ids[0]]
    record["positive_affect"], record["negative_affect"] = 0.9, 0.1
    record = other.model_state["body_state_v1"]["agents"][ids[0]]
    record["positive_affect"], record["negative_affect"] = 0.9, 0.1
    for each in (world, other):
        each.agents[ids[1]].stress = 0.5
    event = _action(world, "reassurance_exchange", *ids)
    # Each is the formula's with its own transfer, and the two differ.
    for each in (world, other):
        expected = copy.deepcopy(each)
        _apply_kernel(each, event)
        _apply_formula(expected, event)
        assert _values(each, ids) == _values(expected, ids)
    assert _values(world, ids) != _values(other, ids)
