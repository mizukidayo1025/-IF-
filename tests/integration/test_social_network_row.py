"""social_network_v1 owned by Rust, with its rules (docs/rust-port/lanes/m3-social-network.md).

Here: what taints the row, each way into it that the per-tick check must see now that it reads
only the written records, what the kernels refuse, and random inputs, whose results are the bits
the Python rules gave (PYTHON_GAVE). The social action of `SocialInteractionHandler` has no Python
rule left (lane L10, `kernel_only`): it is compared with the rule's formula, written down from it
(`_apply_formula`), and its texts with the rule's.
"""

from __future__ import annotations

import copy
import hashlib
import json
import random
from types import SimpleNamespace
from typing import Any

import pytest
from support.world import kernel_says_python_s_text

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
    EventApplicationError,
    EventKind,
    EventSuppressed,
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


# ---- What the Python rules gave ---------------------------------------------------------------
# observe_event and the two of the social dialogue have no Python rule left (lane L10,
# `kernel_only`). Each case was run with them once (2d8fb46): what they returned and wrote is pinned
# here, per test and in call order, as a digest compared on every OS, as the gate's hashes are.

PYTHON_GAVE: dict[str, list[str]] = {
    "test_capture_gives_what_the_python_rule_gives": [
        "c266c955239cf005",
        "c266c955239cf005",
        "5a9c239b8a353685",
        "5a9c239b8a353685",
    ],
    "test_observe_event_computes_trust_when_the_body_has_no_record": [
        "5223d144e68eb137",
        "6e056b1a622fdf84",
        "4848febde7b9557d",
        "6e4490f4644eeb43",
        "38953c71605d34fa",
    ],
    "test_observe_event_gives_the_bits_of_the_python_rule": ["4b665bcf9f3db776"],
    "test_observe_event_skips_odd_payloads_as_the_python_rule_did[0]": ["cac447cad753925e"],
    "test_observe_event_skips_odd_payloads_as_the_python_rule_did[1]": ["cac447cad753925e"],
    "test_observe_event_skips_odd_payloads_as_the_python_rule_did[2]": ["cac447cad753925e"],
    "test_observe_event_skips_odd_payloads_as_the_python_rule_did[3]": ["be917f018c347bb8"],
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
    text = "\n".join(json.dumps(value, sort_keys=True, default=repr) for value in values)
    return hashlib.sha256(text.encode()).hexdigest()[:16]


def _python_gave(*values: Any) -> None:
    """These values are what the Python rule gave at this call of the test."""
    name, index = _SCOPE
    _SCOPE[1] += 1
    assert _digest(*values) == PYTHON_GAVE[name][index]


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


# ---- The rules against Python ------------------------------------------------------------------


def _random_body(rng: random.Random, worlds: tuple[Any, Any], ids: list[str]) -> None:
    affect = {name: rng.random() for name in ("positive_affect", "negative_affect")}
    for id_ in ids:
        for name, value in affect.items():
            for each in worlds:
                each.model_state["body_state_v1"]["agents"][id_][name] = value


def _random_event(rng: random.Random, world: Any, ids: list[str], number: int) -> ScheduledEvent:
    first, second = rng.sample(ids, 2)
    kind = rng.randrange(7)
    minute = world.minute_index
    if kind == 0:
        message = rng.choice([None, f"m{rng.randrange(80)}", f"m{number}"])
        payload = {"recipient_agent_id": first, "sender_agent_id": second}
        if message is not None:
            payload["message_id"] = message
        return _event("information_contact_update", payload, minute, number)
    if kind == 1:
        return _event(
            "proximity_contact_update",
            {"first_agent_id": first, "second_agent_id": second, "observed_minute": minute},
            minute,
            number,
        )
    if kind == 2:
        return _event("information_correction", {"message_id": "m1"}, minute, number)
    interaction = (
        "reassurance_exchange",
        "family_care",
        "voluntary_support",
        "cooperation",
    )[kind - 3]
    return _event(
        "social_interaction",
        {"interaction_type": interaction, "agent_id": first, "participant_agent_id": second},
        minute,
        number,
    )


def test_observe_event_gives_the_bits_of_the_python_rule(world: Any) -> None:
    rng = random.Random(11)
    ids = _people(world, 6)
    ran = 0
    for number in range(1500):
        _random_body(rng, (world,), ids)
        event = _random_event(rng, world, ids, number + 1)
        minute = world.minute_index
        before = _rules().observe_event(world, event, minute)
        assert before is None
        ran += 1
    assert ran == 1500
    ours = world.model_state[KEY]
    _python_gave(ours, list(ours["relationships"]))
    assert _mirror(world).verify()
    assert _accepted(world)
    # The cap of the message IDs was reached, or the test proves little.
    assert any(len(record["shared_message_ids"]) >= 10 for record in ours["relationships"].values())


def test_observe_event_computes_trust_when_the_body_has_no_record(world: Any) -> None:
    first, second = _people(world)
    for key in (ordered_pair_key(first, second), ordered_pair_key(second, first)):
        world.model_state["body_state_v1"]["relationships"].pop(key, None)
        world.model_state[KEY]["relationships"].pop(key, None)
    for number, kind in enumerate(
        [
            "cooperation",
            "voluntary_support",
            "family_care",
            "reassurance_exchange",
            "information_reaction",
        ]
    ):
        event = _event(
            "social_interaction",
            {"interaction_type": kind, "agent_id": first, "participant_agent_id": second},
            world.minute_index,
            number + 1,
        )
        _rules().observe_event(world, event, world.minute_index)
        _python_gave(world.model_state[KEY])
    record = world.model_state[KEY]["relationships"][ordered_pair_key(first, second)]
    assert record["cooperation_count"] == 1 and record["last_interaction"] == "information_reaction"
    assert record["trust"] is not None
    # A 1% change of the transfer shows.
    other = copy.deepcopy(world)
    third = copy.deepcopy(world)
    config = third.model_state["body_state_v1"]["config"]
    config["social_action_affect_transfer"] *= 1.01
    event = _event(
        "social_interaction",
        {"interaction_type": "family_care", "agent_id": first, "participant_agent_id": second},
        third.minute_index,
    )
    for each in (third, other):
        for key in list(each.model_state["body_state_v1"]["relationships"]):
            del each.model_state["body_state_v1"]["relationships"][key]
    _rules().observe_event(other, event, other.minute_index)
    _rules().observe_event(third, event, third.minute_index)
    assert json.dumps(third.model_state[KEY], sort_keys=True) != json.dumps(
        other.model_state[KEY], sort_keys=True
    )


def test_the_message_list_keeps_its_last_64_in_row_and_tree(world: Any) -> None:
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


@pytest.mark.parametrize(
    "payload",
    [
        # One person with themselves, a missing ID and an ID of another type: nothing recorded.
        {"recipient_agent_id": "a", "sender_agent_id": "a"},
        {"recipient_agent_id": "a"},
        {"recipient_agent_id": 3, "sender_agent_id": "a"},
        # A message ID of another type: the contact, without a message.
        {"recipient_agent_id": "a", "sender_agent_id": "b", "message_id": 4},
    ],
)
def test_observe_event_skips_odd_payloads_as_the_python_rule_did(world: Any, payload: dict) -> None:
    event = SimpleNamespace(event_type="information_contact_update", payload=payload)
    before = _mirror(world).row_json()
    _rules().observe_event(world, event, world.minute_index)
    _python_gave(world.model_state[KEY])
    if "message_id" in payload:
        record = world.model_state[KEY]["relationships"]['["a","b"]']
        assert (record["contact_count"], record["shared_message_ids"]) == (1, [])
    else:
        assert _mirror(world).row_json() == before
    assert _mirror(world).verify()


def test_observe_event_refuses_an_id_json_escapes_with_nothing_written(world: Any) -> None:
    """No Agent ID holds one (Scenario IDs are ASCII), and a pair key of it is not this kernel's."""
    payload = {"recipient_agent_id": "あ", "sender_agent_id": "b"}
    event = SimpleNamespace(event_type="information_contact_update", payload=payload)
    before = _mirror(world).row_json()
    with pytest.raises(KernelRefusal):
        social_network.observe_event(world, event, world.minute_index)
    assert _mirror(world).row_json() == before


def test_at_the_bound_the_python_rule_says_so(world: Any, monkeypatch: pytest.MonkeyPatch) -> None:
    first, second = _people(world)
    event = _event(
        "information_contact_update",
        {"recipient_agent_id": first, "sender_agent_id": second},
        world.minute_index,
    )
    relationships = world.model_state[KEY]["relationships"]
    relationships.clear()
    relationships.update({f'["p{index}","q"]': _record() for index in range(100_000)})
    before = _mirror(world).row_json()
    with pytest.raises(
        ValueError, match="^social_network_v1 reached the configured relationship bound$"
    ):
        _rules().observe_event(world, event, world.minute_index)
    with pytest.raises(
        ValueError, match="^social_network_v1 reached the configured relationship bound$"
    ):
        social_network.observe_event(world, event, world.minute_index)
    assert _mirror(world).row_json() == before


def test_record_observed_contacts_comes_through_the_social_step_without_python(
    finished: Any,
) -> None:
    """The step writes the row itself: forwards are the population's and the first tick's."""
    assert _mirror(finished).stats()["forwards"] < 50


# ---- The state context of a social event -------------------------------------------------------


def test_capture_gives_what_the_python_rule_gives(world: Any) -> None:
    first, second = _people(world)
    kernel = native.rules("social_dialogue").capture_context
    for ids in ((first, second), [second, first], (first,), (first, first)):
        _python_gave(json.dumps(kernel(world, ids)))


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


@pytest.mark.parametrize(
    ("ids", "text"),
    [
        ((), "one or two"),
        (("a", "b", "c"), "one or two"),
        (("nobody",), "one or two"),
    ],
)
def test_capture_says_the_rule_s_text(world: Any, ids: tuple, text: str) -> None:
    with pytest.raises(ValueError, match=text):
        social_dialogue.capture_social_state_context(world, ids)


def test_a_body_value_out_of_bounds_says_the_rule_s_text(world: Any) -> None:
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
        (lambda c, i: c.__setitem__("captured_minute", -1), _NOT_ITS_EVENT),
        (lambda c, i: c.__setitem__("captured_minute", 10**6), _NOT_ITS_EVENT),
        (lambda c, i: c.__setitem__("captured_minute", True), _NOT_ITS_EVENT),
        (lambda c, i: c.__setitem__("more", 1), _NOT_ITS_EVENT),
        (lambda c, i: c["participants"].pop(i), _NOT_ITS_EVENT),
        (
            lambda c, i: c["participants"].__setitem__("other", c["participants"].pop(i)),
            _NOT_ITS_EVENT,
        ),
        (lambda c, i: c["participants"][i].__setitem__("stress", 1.5), _NOT_UNIT),
        (lambda c, i: c["participants"][i].__setitem__("trust", float("nan")), _NOT_UNIT),
        (lambda c, i: c["participants"][i].__setitem__("stress", True), _NOT_UNIT),
        (lambda c, i: c["participants"][i]["needs"].__setitem__("hunger", -0.5), _NOT_UNIT),
        (
            lambda c, i: c["participants"][i]["needs"].pop("hunger"),
            "social dialogue needs context is invalid",
        ),
        (
            lambda c, i: c["participants"][i].__setitem__("affect", {"positive_affect": 0.5}),
            "social dialogue affect context is invalid",
        ),
        (
            lambda c, i: c["participants"][i].pop("affect"),
            "social dialogue participant state has unsupported fields",
        ),
    ],
)
def test_validation_of_a_context_says_the_rule_s_text(world: Any, spoil: Any, text: str) -> None:
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


@pytest.mark.parametrize("kind", ["reassurance_exchange", "voluntary_support", "family_care"])
def test_a_social_action_gives_the_bits_of_the_rule_s_formula(world: Any, kind: str) -> None:
    ids = _people(world)
    _colocate(world, ids)
    actor, participant = ids
    presence = world.model_state.get("population_presence_v1")
    if presence is not None:
        for id_ in ids:
            presence["people"].setdefault(id_, {})["household_id"] = "h1"
    else:
        for id_ in ids:
            world.agents[id_].profile.group_id = "h1"
    other = copy.deepcopy(world)
    rng = random.Random(3)
    ran = 0
    for number in range(400):
        stress, trust, fatigue = rng.random(), rng.random(), rng.random() * 0.2
        positive, negative = rng.random(), rng.random()
        for each in (world, other):
            each.agents[actor].stress = fatigue
            each.agents[actor].trust = trust
            each.agents[actor].needs.fatigue = fatigue
            each.agents[actor].needs.hunger = 0.0
            each.agents[actor].needs.toilet_urgency = 0.0
            each.agents[actor].needs.hydration_need = 0.0
            each.agents[participant].stress = max(stress, 0.6)
            each.agents[participant].trust = trust
            record = each.model_state["body_state_v1"]["agents"][actor]
            record["positive_affect"], record["negative_affect"] = positive, negative
        event = _action(world, kind, actor, participant, number + 1)
        _apply_kernel(world, event)
        _apply_formula(other, event)
        ran += 1
        assert _values(world, ids) == _values(other, ids)
    assert ran == 400
    assert world._core.entities().verify(world) is None


def test_a_1_percent_change_of_the_transfer_shows_in_the_action(world: Any) -> None:
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


def _suppresses(world: Any, event: Any, text: str) -> None:
    """The kernel suppresses the action with the Python rule's text, written down from it."""
    with pytest.raises(EventSuppressed) as by_kernel:
        _apply_kernel(world, event)
    assert str(by_kernel.value) == text


def test_a_suppressed_action_says_python_s_text_with_nothing_written(world: Any) -> None:
    ids = _people(world)
    zone = _colocate(world, ids)
    event = _action(world, "reassurance_exchange", *ids)
    others = [name for name in world.zones if name != zone]
    world.agents[ids[1]].zone_id = others[0]
    before = _values(world, ids)
    _suppresses(world, event, "social action participants are no longer co-located")
    assert _values(world, ids) == before


def test_a_helper_who_needs_more_than_the_target_is_suppressed(world: Any) -> None:
    ids = _people(world)
    _colocate(world, ids)
    world.agents[ids[0]].stress = 0.9
    world.agents[ids[1]].stress = 0.1
    for name in ("toilet_urgency", "hunger", "fatigue", "hydration_need"):
        setattr(world.agents[ids[1]].needs, name, 0.1)
    event = _action(world, "voluntary_support", *ids)
    before = _values(world, ids)
    _suppresses(world, event, "the selected person no longer has a higher immediate need")
    assert _values(world, ids) == before


def test_a_different_household_is_suppressed(world: Any) -> None:
    ids = _people(world)
    _colocate(world, ids)
    presence = world.model_state.get("population_presence_v1")
    if presence is not None:
        presence["people"].setdefault(ids[0], {})["household_id"] = "h1"
        presence["people"].setdefault(ids[1], {})["household_id"] = "h2"
    else:
        world.agents[ids[0]].profile.group_id = "h1"
        world.agents[ids[1]].profile.group_id = "h2"
    event = _action(world, "family_care", *ids)
    _suppresses(world, event, "family support target is no longer in the same household")


DURATION = "social action duration must be within 1..1440 minutes"


@pytest.mark.parametrize(
    ("more", "text"),
    [
        ({"interaction_duration_minutes": 0}, DURATION),
        ({"interaction_duration_minutes": True}, DURATION),
        ({"reason_short": "other"}, "social action reason code is not allowed"),
        ({"action": "other"}, "social action does not match its typed topic"),
        ({"extra": 1}, "autonomous social action payload fields do not match"),
        (
            {"conversation_state_context": {"schema_version": 2}},
            "social dialogue state context is invalid",
        ),
    ],
)
def test_a_bad_payload_is_failed_by_the_kernel_in_python_s_text(
    world: Any, more: dict, text: str
) -> None:
    ids = _people(world)
    _colocate(world, ids)
    event = _action(world, "reassurance_exchange", *ids, **more)
    before = _values(world, ids)
    expected = (EventApplicationError, text)
    kernel_says_python_s_text(SocialInteractionHandler(), world, event, expected)
    assert _values(world, ids) == before
