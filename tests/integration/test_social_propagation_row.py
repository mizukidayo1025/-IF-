"""social_propagation_v1 owned by Rust, and the social model's step in native/src/rules.

The step is rules/social_propagation.rs alone (docs/rust-port/lanes/b-social-propagation.md): the
Python rules of policies/social.py and policies/alternatives.py are gone. What they answered, on
small worlds and on values no scenario reaches, was recorded once as digests (ANSWERS), which the
kernel's answers must equal on every OS; a float's bits are compared through its hex.

The tests of the step's answers on the small worlds (each model, the trace's documents, a restored
namespace, the view's households, contacts without weight, unsorted deltas, int coefficients) are
the core's, `native/core/tests/social_steps.rs` (#225), with the same digests. This file keeps the
entry test of one model, the row, what the entry turns down, the bits of `plan_of_values` on random
values, and the Runs.
"""

from __future__ import annotations

import hashlib
import json
import os
import random
from dataclasses import asdict, replace
from types import SimpleNamespace
from typing import Any

import pytest
from support.world import driven_ticks, view_of, watching_phases

from sheltersim import native
from sheltersim.core.engine import TickPhase, TraceLevel
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.rng import CounterRNG
from sheltersim.core.world import WorldState
from sheltersim.experiments.runner import OfflineSimulationController, run_scenario
from sheltersim.policies.alternatives import DeGrootContactPolicy
from sheltersim.policies.behavior import WorldView
from sheltersim.policies.psychology import AgentStateDelta
from sheltersim.policies.social import (
    SOCIAL_RUNTIME_STATE_KEY,
    ExpressivenessChannelOpennessPolicy,
    SocialPropagationParameters,
    StressPropagationPolicy,
)

KEY = SOCIAL_RUNTIME_STATE_KEY
BOSSE = "ASM-BOSSE-AFFECT-CONTAGION-V1"
DEGROOT = "ASM-DEGROOT-CONTACT-V1"
POLICIES = {
    "sparse": (StressPropagationPolicy, "ASM-SOCIAL-PROPAGATION-V1"),
    "channel": (ExpressivenessChannelOpennessPolicy, BOSSE),
    "degroot": (DeGrootContactPolicy, DEGROOT),
}


def _rules() -> Any:
    return native.rules("social_propagation")


def _state() -> dict[str, Any]:
    return {
        "schema_version": 1,
        "contacts": {
            '["a","b"]': {"last_seen_minute": 4, "consecutive_minutes": 2},
            '["b","a"]': {"last_seen_minute": 3, "consecutive_minutes": 1},
        },
        "agent_cooldown_until": {"b": 9, "a": 7},
    }


def _world(state: dict[str, Any] | None = None) -> WorldState:
    return WorldState("s", 1, 100, model_state={KEY: state if state is not None else _state()})


def _mirror(world: WorldState) -> Any:
    return world._core.mirror(KEY)


# ---- The row -----------------------------------------------------------------------------------


def test_a_row_writes_the_bytes_of_the_tree_with_its_keys_sorted() -> None:
    world = _world()
    assert _mirror(world).row_json() == native.canonical_json(_state())
    assert _mirror(world).verify()
    assert list(world.model_state[KEY]["agent_cooldown_until"]) == ["b", "a"]


@pytest.mark.parametrize(
    "write",
    [
        lambda state: state["contacts"]['["a","b"]'].__setitem__("last_seen_minute", 1.0),
        lambda state: state["contacts"]['["a","b"]'].__setitem__("consecutive_minutes", True),
        lambda state: state["contacts"]['["a","b"]'].__setitem__("other", 1),
        lambda state: state["contacts"]['["a","b"]'].pop("consecutive_minutes"),
        lambda state: state["contacts"].__setitem__("x", {"last_seen_minute": 1}),
        lambda state: state["agent_cooldown_until"].__setitem__("x", 1.5),
        lambda state: state.__setitem__("other", 1),
        lambda state: state.__setitem__("schema_version", "1"),
    ],
)
def test_a_value_the_row_cant_hold_taints_the_namespace(write: Any) -> None:
    world = _world()
    write(world.model_state[KEY])
    assert _mirror(world).tainted is not None
    assert not _mirror(world).has_row


def test_forwarded_writes_reach_the_row() -> None:
    world = _world()
    state = world.model_state[KEY]
    state["contacts"].pop('["a","b"]')
    state["agent_cooldown_until"].pop("a", None)
    state["contacts"]['["c","d"]'] = {"last_seen_minute": 5, "consecutive_minutes": 1}
    assert _mirror(world).verify()
    state["contacts"] = {k: v for k, v in state["contacts"].items() if "c" not in k}
    assert _mirror(world).verify()
    assert json.loads(_mirror(world).row_json())["contacts"].keys() == {'["b","a"]'}


# ---- The step on small worlds ------------------------------------------------------------------

# What the deleted Python rules answered (policies/social.py and policies/alternatives.py at
# 2d8fb46), as sha256 of the answers' texts, recorded once on Linux. With SHELTERSIM_RECORD_SOCIAL
# set, the tests wrote these digests of the Python rules' answers to that file instead.
ANSWERS: dict[str, str] = {
    "random-channel": ("b3c68b6b02dbe536a83670cb8b5ffa01d7ad8c91250ef92c48b6b3a034f6f037"),
    "random-degroot": ("f5bebc51bbbf2f949480df578091c55beea7c4bd1024b3f99e8cce020241235b"),
    "random-sparse": ("30443fb28baea6f03cec9351bdc890ed0e2648a9097c3816ffdd92f60c7e6681"),
    "run-degroot-contact-v1": ("2519d1789c69d1d618b264953f27802b26d087cf533d7a29a7f47d4c67acde52"),
    "run-expressiveness-channel-openness-v1": (
        "d699ce16a6900720a790d2a35001feadff0f12ff091db070b6305ee9a343e439"
    ),
    "run-sparse-family-zone-contact-v1": (
        "23eb9a3c7f479b3085bf0ec4a723ea97b4aa84f739aab9a663db519a857a51f1"
    ),
    "step-channel": ("7f8ea0412eff688936fc0644a8f464773230ac675108c0eb0b37e7f8c349f289"),
}
_RECORD = os.environ.get("SHELTERSIM_RECORD_SOCIAL")


def _answers(name: str, texts: list[str]) -> None:
    digest = hashlib.sha256()
    for text in texts:
        digest.update(text.encode() + b"\n")
    if _RECORD:
        with open(_RECORD, "a", encoding="utf-8") as file:
            file.write(json.dumps({name: digest.hexdigest()}) + "\n")
        return
    assert digest.hexdigest() == ANSWERS[name], name


def _small(
    generated_scenario: Any, *, policy: str = "channel", **parameters: Any
) -> tuple[Any, ...]:
    """A world with a row, a controller, and the arguments of the step at the next minute."""
    world = WorldState.from_scenario(generated_scenario)
    world.model_state[KEY] = {"schema_version": 1, "contacts": {}, "agent_cooldown_until": {}}
    for index, agent in enumerate(world.agents.values()):
        agent.stress = (index % 10) / 10.0
    view = view_of(world, include_state_hash=False)
    kind, source = POLICIES[policy]
    settings = SocialPropagationParameters(
        enabled=True, stress_spread_per_minute=0.2, minimum_contact_minutes=1, source_id=source
    )
    controller = SimpleNamespace(
        stress_policy=kind(replace(settings, **parameters)),
        artifacts=SimpleNamespace(trace_level=TraceLevel.OFF),
        zone_neighbors={zone: () for zone in world.zones},
    )
    deltas = tuple(
        AgentStateDelta(
            agent_id=agent_id,
            minute_index=view.minute_index,
            toilet_urgency=0.0,
            hunger=0.0,
            fatigue=0.0,
            stress=0.5,
            trust=0.5,
        )
        for agent_id in sorted(view.agents)
    )
    return world, controller, view, deltas, CounterRNG(1)


def _python_result(world: Any, controller: Any, view: Any, deltas: Any, rng: Any) -> Any:
    """Only while recording: the deleted Python rule."""
    state = json.loads(json.dumps(world.model_state.get(KEY)))
    return controller.stress_policy.update(
        view, deltas, rng, zone_neighbors=controller.zone_neighbors, runtime_state=state
    )


def _run_kernel(world: Any, controller: Any, view: Any, deltas: Any, rng: Any) -> Any:
    return _rules().update_social_propagation(
        controller, world, view, deltas, view.minute_index, rng
    )


def _deltas_text(deltas: Any) -> str:
    return json.dumps([{**asdict(delta), "stress": delta.stress.hex()} for delta in deltas])


def test_the_step_writes_what_the_python_rule_wrote(generated_scenario: Any) -> None:
    """The entry with one model; each model's answer is the core's test
    (`the_step_writes_what_the_python_rule_wrote` of `social_steps.rs`)."""
    policy = "channel"
    arguments = _small(generated_scenario, policy=policy, cooldown_minutes=2)
    world = arguments[0]
    if _RECORD:
        python = _python_result(*arguments)
        _answers(f"step-{policy}", [json.dumps(python.runtime_state), _deltas_text(python.deltas)])
        return
    final = _run_kernel(*arguments)
    state = world.model_state[KEY]
    assert _mirror(world).row_json() == native.canonical_json(state)
    assert any(delta.stress != 0.5 for delta in final)
    _answers(f"step-{policy}", [json.dumps(state), _deltas_text(final)])


def test_records_change_keys_and_later_writes_reach_their_new_place(
    generated_scenario: Any,
) -> None:
    world, controller, view, deltas, rng = _small(generated_scenario)
    for minute in range(3):
        moved = replace(view, minute_index=minute)
        shifted = tuple(replace(delta, minute_index=minute) for delta in deltas)
        _run_kernel(world, controller, moved, shifted, rng)
        assert _mirror(world).verify()
    state = world.model_state[KEY]
    assert _mirror(world).stats()["whole_writes"] == 1
    # The root, two maps and the records, so nothing is left in the table.
    assert _mirror(world).stats()["live_nodes"] == 3 + len(state["contacts"])
    key = next(iter(state["contacts"]))
    state["contacts"][key]["consecutive_minutes"] = 99
    assert _mirror(world).verify()
    assert json.loads(_mirror(world).row_json())["contacts"][key]["consecutive_minutes"] == 99
    popped = state["contacts"].pop(key)
    popped["consecutive_minutes"] = 100
    assert key not in json.loads(_mirror(world).row_json())["contacts"]
    assert _mirror(world).verify()


def test_a_disabled_model_returns_the_deltas_and_writes_nothing(generated_scenario: Any) -> None:
    world, controller, view, deltas, rng = _small(generated_scenario, enabled=False)
    before = _mirror(world).row_json()
    assert _run_kernel(world, controller, view, deltas, rng) is deltas
    assert _mirror(world).row_json() == before


class _OtherRNG(CounterRNG):
    pass


class _OtherPolicy(ExpressivenessChannelOpennessPolicy):
    pass


@pytest.mark.parametrize(
    ("change", "reason"),
    [
        (lambda w, c, v, d, r: {"rng": _OtherRNG(1)}, "RNG"),
        (
            lambda w, c, v, d, r: {
                "controller": SimpleNamespace(
                    **{**vars(c), "stress_policy": _OtherPolicy(c.stress_policy.parameters)}
                )
            },
            "policy",
        ),
        (lambda w, c, v, d, r: {"deltas": d[:-1]}, "different people"),
        (lambda w, c, v, d, r: {"deltas": list(d)}, "tuple"),
        (
            lambda w, c, v, d, r: {"deltas": d[:-1] + (replace(d[-1], minute_index=7),)},
            "another minute",
        ),
        (lambda w, c, v, d, r: {"deltas": d[:-1] + (SimpleNamespace(**asdict(d[-1])),)}, "class"),
        (lambda w, c, v, d, r: {"view": replace(v, minute_index=7)}, "another minute"),
        (
            lambda w, c, v, d, r: {
                "controller": SimpleNamespace(
                    **{**vars(c), "zone_neighbors": {z: "ab" for z in c.zone_neighbors}}
                )
            },
            "tuple or a list",
        ),
        (
            lambda w, c, v, d, r: {
                "controller": SimpleNamespace(
                    **{**vars(c), "zone_neighbors": {z: (1,) for z in c.zone_neighbors}}
                )
            },
            "not a str",
        ),
    ],
)
def test_what_the_kernel_does_not_handle_is_unsupported_before_any_change(
    generated_scenario: Any, change: Any, reason: str
) -> None:
    """Inputs no tick gives the step: a test's RNG or policy class, deltas or a view of another
    tick, neighbors of another type. The step of a controller raises KernelRefusal for them."""
    world, controller, view, deltas, rng = _small(generated_scenario)
    arguments = {"controller": controller, "view": view, "deltas": deltas, "rng": rng}
    arguments |= change(world, controller, view, deltas, rng)
    before = _mirror(world).row_json()
    with pytest.raises(native.Unsupported, match=reason):
        _rules().update_social_propagation(
            arguments["controller"],
            world,
            arguments["view"],
            arguments["deltas"],
            view.minute_index,
            arguments["rng"],
        )
    assert _mirror(world).row_json() == before


@pytest.mark.parametrize(
    "corrupt",
    [
        lambda state: state.__setitem__("schema_version", 2),
        lambda state: state["contacts"].__setitem__(
            "x", {"last_seen_minute": -1, "consecutive_minutes": 1}
        ),
        lambda state: state["agent_cooldown_until"].__setitem__("x", -1),
        lambda state: state["agent_cooldown_until"].__setitem__("x", 1.5),
    ],
)
def test_a_state_the_rule_turns_down_is_a_refusal_of_the_step(
    generated_scenario: Any, corrupt: Any
) -> None:
    """A namespace no Run writes (the Python rule raised a ValueError on these): the step of a
    controller raises KernelRefusal and changes nothing."""
    world, controller, view, deltas, rng = _small(generated_scenario)
    corrupt(world.model_state[KEY])
    before = json.dumps(world.model_state[KEY])
    step = OfflineSimulationController._social_propagation
    controller.world = world
    with pytest.raises(KernelRefusal, match="update_social_propagation refused"):
        step(controller, world, view, deltas, view.minute_index, rng)
    assert json.dumps(world.model_state[KEY]) == before


def test_a_tainted_entity_store_is_a_refusal(generated_scenario: Any) -> None:
    world, controller, view, deltas, rng = _small(generated_scenario)
    next(iter(world.agents.values())).stress = 1
    with pytest.raises(native.Unsupported, match="tainted"):
        _run_kernel(world, controller, view, deltas, rng)


# ---- The step on a whole Run -------------------------------------------------------------------


@pytest.mark.parametrize(
    "policy",
    ["sparse-family-zone-contact-v1", "degroot-contact-v1", "expressiveness-channel-openness-v1"],
)
def test_every_tick_of_a_run_matches_the_python_rule_and_writes_no_whole_namespace(
    generated_scenario_with: Any, tmp_path: Any, policy: str
) -> None:
    """After each update phase, the namespace and the final deltas, which `_apply_state_deltas`
    wrote to the people and their body records. Their answers were recorded from the Python rule
    through the phases' Python steps, which went with #221; they are not recorded again."""
    if _RECORD:
        pytest.skip("the Python rule's answers of a Run were recorded through the phases")
    scenario = generated_scenario_with(social_policy_id=policy)
    document = scenario.model_dump(mode="json")
    document["duration_minutes"] = 90
    scenario = type(scenario).model_validate(document)
    texts: list[str] = []
    ran = []

    def counted(engine: Any, phase: TickPhase, before: bool) -> None:
        if phase is not TickPhase.UPDATE_AGENT_STATE or before:
            return
        world = engine.world
        assert engine.phase_handler.stress_policy.policy_version == policy
        ran.append(len(world.model_state[KEY]["contacts"]))
        texts.extend(
            [json.dumps(world.model_state[KEY]), _deltas_text(_written_deltas(world, len(ran) - 1))]
        )
        assert _mirror(world).verify()

    ticks = driven_ticks()
    with watching_phases(counted):
        world = run_scenario(scenario, tmp_path, trace_level=TraceLevel.OFF)
    assert driven_ticks() - ticks == 90
    assert len(ran) == 90 and sum(ran) > 0
    # The first tick makes the namespace, as the Python rule did.
    assert _mirror(world).stats()["whole_writes"] == 1
    _answers(f"run-{policy}", texts)


def _written_deltas(world: Any, minute: int) -> list[AgentStateDelta]:
    """The final deltas of the minute as `_apply_state_deltas` wrote them: each person's needs,
    stress and trust, and the body values of their record (None without one), in ID order."""
    records = world.model_state.get("body_state_v1", {}).get("agents", {})
    deltas = []
    for agent_id in sorted(world.agents):
        agent = world.agents[agent_id]
        record = records.get(agent_id)
        fields = {
            "agent_id": agent_id,
            "minute_index": minute,
            "toilet_urgency": agent.needs.toilet_urgency,
            "hunger": agent.needs.hunger,
            "fatigue": agent.needs.fatigue,
            "stress": agent.stress,
            "trust": agent.trust,
            "hydration_need": agent.needs.hydration_need,
        }
        for name in _BODY_VALUES:
            fields[name] = None if record is None else record[name]
        deltas.append(AgentStateDelta.within_bounds(fields))
    return deltas


_BODY_VALUES = (
    "sleep_debt_minutes",
    "uncertainty",
    "reassurance",
    "discontent",
    "positive_affect",
    "negative_affect",
    "hygiene_need",
)


def test_a_run_with_a_trace_is_the_same_world_as_one_without(
    generated_scenario: Any, tmp_path: Any
) -> None:
    document = generated_scenario.model_dump(mode="json")
    document["duration_minutes"] = 30
    scenario = type(generated_scenario).model_validate(document)
    traced = run_scenario(scenario, tmp_path / "traced", trace_level=TraceLevel.SUMMARY)
    plain = run_scenario(scenario, tmp_path / "plain", trace_level=TraceLevel.OFF)
    assert traced.canonical_json() == plain.canonical_json()
    # The kernel runs with a trace too, and rules/trace.rs writes its document.
    assert _mirror(traced).stats()["whole_writes"] == 1
    assert _mirror(plain).stats()["whole_writes"] == 1


# ---- The bits, on values no scenario reaches -----------------------------------------------------


class _KeepingRNG(CounterRNG):
    def __init__(self, seed: int) -> None:
        super().__init__(seed)
        self.draws: list[tuple[str, str]] = []

    def random(self, *key_parts: Any) -> float:
        value = super().random(*key_parts)
        key = json.dumps(list(key_parts), ensure_ascii=False, separators=(",", ":"))
        self.draws.append((key, value.hex()))
        return value


def _plan(
    policy: str, seed: int, view: Any, deltas: Any, settings: Any, neighbors: Any, state: Any
) -> Any:
    ids = sorted(view.agents)
    base = {delta.agent_id: delta.stress for delta in deltas}
    return _rules().plan_of_values(
        policy,
        str(seed),
        view.minute_index,
        [
            settings.max_contacts_per_agent,
            settings.minimum_contact_minutes,
            settings.cooldown_minutes,
        ],
        [
            settings.stress_spread_per_minute,
            settings.natural_recovery_per_minute,
            settings.max_pair_stress_increase,
            settings.max_agent_stress_increase_per_minute,
            settings.family_channel_strength,
            settings.same_zone_channel_strength,
            settings.nearby_zone_channel_strength,
        ],
        ids,
        [
            (
                view.agents[agent_id].group_id,
                view.agents[agent_id].zone_id,
                view.agents[agent_id].stress,
                view.agents[agent_id].stress_sensitivity,
                settings.source_expressiveness_by_agent.get(agent_id, 1.0),
                base[agent_id],
            )
            for agent_id in ids
        ],
        {zone: list(names) for zone, names in neighbors.items()},
        state["contacts"],
        state["agent_cooldown_until"],
    )


def _random_case(draw: random.Random, template: Any, policy: str) -> tuple[Any, ...]:
    names = ["A1", "A2", 'A"3', "A\\4", "é5", "ß6", "A\n7", "B8", "B9", "C10", "C11", "日12"]
    zones = ["z1", "z2", "zé", 'z"4']
    stresses = [0.0, 1.0, 0.5, 0.25, 1e-300, 1.0 - 2**-53]

    def unit() -> float:
        return draw.choice(stresses) if draw.random() < 0.3 else draw.random()

    minute = draw.randrange(0, 50)
    ids = draw.sample(names, draw.randrange(1, len(names) + 1))
    agents = {
        agent_id: replace(
            template,
            id=agent_id,
            zone_id=draw.choice(zones),
            group_id=draw.choice((None, "g1", "g2", 'g"3')),
            stress=unit(),
            stress_sensitivity=draw.choice((0.0, 1.0, draw.random())),
        )
        for agent_id in ids
    }
    view = WorldView(minute_index=minute, agents=agents, facilities={}, state_hash=None)
    neighbors = {
        zone: tuple(draw.sample(zones, draw.randrange(0, len(zones) + 1)))
        for zone in zones
        if draw.random() < 0.8
    }
    contacts = {
        json.dumps([a, b], ensure_ascii=False, separators=(",", ":")): {
            "last_seen_minute": draw.choice((minute - 1 if minute else 0, minute, 0)),
            "consecutive_minutes": draw.randrange(1, 12),
        }
        for a in names
        for b in names
        if a != b and draw.random() < 0.3
    }
    state = {
        "schema_version": 1,
        "contacts": contacts,
        "agent_cooldown_until": {
            name: draw.randrange(0, 60) for name in names if draw.random() < 0.4
        },
    }
    deltas = tuple(
        AgentStateDelta(
            agent_id=agent_id,
            minute_index=minute,
            toilet_urgency=0.0,
            hunger=0.0,
            fatigue=0.0,
            stress=unit(),
            trust=0.5,
        )
        for agent_id in sorted(ids)
    )
    settings = SocialPropagationParameters(
        enabled=True,
        max_contacts_per_agent=draw.randrange(0, 5),
        minimum_contact_minutes=draw.randrange(1, 6),
        stress_spread_per_minute=draw.choice((0.0, 0.25, draw.random() / 4)),
        natural_recovery_per_minute=draw.choice((0.0, 0.25, draw.random() / 4)),
        max_pair_stress_increase=draw.choice((0.0, 0.05, 1e-6, draw.random() / 4)),
        max_agent_stress_increase_per_minute=draw.choice((0.0, 0.05, draw.random() / 4)),
        cooldown_minutes=draw.randrange(1, 40),
        source_expressiveness_by_agent={
            name: draw.choice((0.0, 1.0, draw.random())) for name in names if draw.random() < 0.5
        },
        family_channel_strength=draw.choice((0.0, 1.0, draw.random())),
        same_zone_channel_strength=draw.choice((0.0, 1.0, draw.random())),
        nearby_zone_channel_strength=draw.choice((0.0, 1.0, draw.random())),
        source_id=POLICIES[policy][1],
    )
    return view, deltas, settings, neighbors, state


def _template(generated_scenario: Any) -> Any:
    world = WorldState.from_scenario(generated_scenario)
    return next(iter(view_of(world, include_state_hash=False).agents.values()))


@pytest.mark.parametrize("policy", POLICIES)
def test_the_rule_gives_the_python_rule_s_answers_on_random_inputs(
    generated_scenario: Any, policy: str
) -> None:
    """IDs that JSON escapes, a zone next to itself, empty channels, zero sensitivity, exact caps,
    and every cooldown and contact history a Run may not reach by itself. Each draw too."""
    draw = random.Random(2615)
    template = _template(generated_scenario)
    texts: list[str] = []
    stress_moved = draws = 0
    for _trial in range(400):
        view, deltas, settings, neighbors, state = _random_case(draw, template, policy)
        seed = draw.randrange(0, 2**70)
        ids = sorted(view.agents)
        if _RECORD:
            rng = _KeepingRNG(seed)
            python = POLICIES[policy][0](settings).update(
                view, deltas, rng, zone_neighbors=neighbors, runtime_state=state
            )
            by_id = {delta.agent_id: delta.stress for delta in python.deltas}
            answer = [
                rng.draws,
                [by_id[agent_id].hex() for agent_id in ids],
                [
                    [key, record["last_seen_minute"], record["consecutive_minutes"]]
                    for key, record in python.runtime_state["contacts"].items()
                ],
                sorted(python.runtime_state["agent_cooldown_until"].items()),
                [[item.recipient_id, item.source_id] for item in python.contacts],
            ]
            texts.append(json.dumps(answer, ensure_ascii=False))
            continue
        _rules().start_draw_log()
        try:
            stresses, contacts, cooldowns, pairs = _plan(
                policy, seed, view, deltas, settings, neighbors, state
            )
        finally:
            rust_draws = _rules().take_draw_log()
        draws += len(rust_draws)
        answer = [
            [[key, value.hex()] for key, value in rust_draws],
            [value.hex() for value in stresses],
            [[key, last, count] for key, last, count in contacts],
            sorted(dict(cooldowns).items()),
            [[ids[a], ids[b]] for a, b in pairs],
        ]
        texts.append(json.dumps(answer, ensure_ascii=False))
        by_id = dict(zip(ids, stresses, strict=True))
        stress_moved += any(delta.stress != by_id[delta.agent_id] for delta in deltas)
    if not _RECORD:
        assert stress_moved > 5 and draws > 1000
    _answers(f"random-{policy}", texts)


@pytest.mark.parametrize("policy", POLICIES)
def test_the_comparison_sees_a_change_of_one_percent(generated_scenario: Any, policy: str) -> None:
    draw = random.Random(2615)
    template = _template(generated_scenario)
    seen = 0
    for _ in range(400):
        view, deltas, settings, neighbors, state = _random_case(draw, template, policy)
        try:
            right = _plan(policy, 1, view, deltas, settings, neighbors, state)
        except native.Unsupported:
            continue
        spread = settings.stress_spread_per_minute * 0.99
        off = replace(settings, stress_spread_per_minute=spread)
        wrong = _plan(policy, 1, view, deltas, off, neighbors, state)
        seen += right[0] != wrong[0]
    assert seen > 0
