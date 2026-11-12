"""Each person's minute in Rust: native/core/src/rules/psychology.rs, the only rules of the models.

The Python rules of policies/psychology.py and policies/alternatives.py are gone (lane L10). What
they answered, on worlds whose values no scenario reaches and on whole ticks, was recorded once as
digests (ANSWERS), which the kernel's answers must equal on every OS; a float's bits all show in the
JSON text. What the deleted rules' checks turned down, the kernels turn down, and the steps raise
KernelRefusal before anything changes.
"""

from __future__ import annotations

import copy
import hashlib
import json
import random
from dataclasses import fields, replace
from types import SimpleNamespace
from typing import Any

import pytest
from support.world import driven_ticks, view_of

from sheltersim import native
from sheltersim.core.engine import TickEngine, TraceLevel
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.world import WorldState
from sheltersim.domain.body_state import create_body_state
from sheltersim.domain.models import Activity, SupportRequestState, SupportRequestStatus
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.timeline import _MetricSink
from sheltersim.policies.alternatives import BoundedStressPolicy
from sheltersim.policies.psychology import (
    AgentStateDelta,
    IntegratedDistressLifeStatePolicy,
    LifeStatePsychologyPolicy,
    MeanRevertingDistressPolicy,
    PsychologyParameters,
    SimplePsychologyPolicy,
)
from sheltersim.policies.registry import BodyDynamicsFormula

FULL = BodyDynamicsFormula("body-feedback-complete-v1", "S")
SLEEP_ONLY = BodyDynamicsFormula("body-feedback-sleep-debt-v1", "S")

# What the deleted Python rules answered (policies/psychology.py, policies/alternatives.py and the
# update phase's Python steps at 45a9bc9), as sha256 of the answers' texts, recorded once on Linux.
ANSWERS: dict[str, str] = {
    "group": ("8208a4d334ad6307d11d306de2f41d0e887637e1df88c54fba09b2942a1a427e"),
    "ints-0": ("fb6093685a0aa159c4b2e34c33d8ef488d2d6cfab9824db8d154f4ae4bf4e266"),
    "ints-1": ("b5123a4fdd6fda5f6037c332e421c683a21100f62637c045a7a92860660154a4"),
    "ints-2": ("81330efb55356eb79d75658b9f7100999f40d621f77a20106bef2345e76c553f"),
    "models-0": ("b3a010870ea3f7b624b0dd09629fd0ff9407012d28754b7f5572fcdc3178c7d6"),
    "models-1": ("de4291d76b22c58d22c97b451851622bc4b0dfbe4764f87d5151450a5965913b"),
    "models-2": ("bbc661e8d439b21b4b2ab67dbd9b43a5cefb1764dad97666fe641945199f06ba"),
    "models-3": ("8776482f8b3df765a9964ef1508045b147c8b3c8e84ae4f41a5cbf3b8605f08b"),
    "models-4": ("d23a08311e64f92397a925e05d3f00cf6a285924c3ff994c19cdf99167d7ff44"),
    "models-5": ("640b058ed3f594e8f55cbc9f691d1ea2864850be70a7adb54a45a29fecf08421"),
    "models-6": ("68f5b75c362c695af2a75ef8012d9fd3b977b26bedc4adac54efa2844409e6d9"),
    "models-7": ("1fe904d07fdb20f7f7b50ea6b9d4f49cc566b65e0d3d48099ee4c9b3cd2ce57e"),
    "one-percent": ("747ac51334eb2003d4a5e5f7cd53df787172656c655d9ca0ea3f0299829fc413"),
    "ticks": ("44646ce1fe7fd7bc0bdc7c90f2941716a9434c143f0cfac618c89118819b6d19"),
    "writing-False": ("bf2dd829482d2419d163e4dce6974edf2d1de8ac0f403fef9a18753109db07b1"),
    "writing-True": ("9d710b4c06f9ad84cf9edb3bbd773aa90a0ec8b92100d5a7cf540739d512597f"),
}


def _answers(name: str, texts: list[str]) -> None:
    digest = hashlib.sha256()
    for text in texts:
        digest.update(text.encode() + b"\n")
    assert digest.hexdigest() == ANSWERS[name], name


def _rules() -> Any:
    return native.rules("psychology")


def _controller(world: WorldState, policy: Any, trace: TraceLevel = TraceLevel.OFF) -> Any:
    return SimpleNamespace(
        world=world, psychology=policy, artifacts=SimpleNamespace(trace_level=trace)
    )


def _text(delta: AgentStateDelta) -> str:
    """Float bits and 1 against 1.0 all show."""
    return json.dumps([getattr(delta, item.name) for item in fields(delta)])


def _people_text(world: WorldState) -> str:
    return json.dumps(
        [[agent.stress, agent.trust, agent.needs.to_dict()] for agent in world.agents.values()]
    )


def _draw_parameters(draws: random.Random, ids: list[str]) -> PsychologyParameters:
    def rate() -> float:
        return draws.choice((0.0, draws.random() * 0.25))

    return PsychologyParameters(
        toilet_growth=rate(),
        hunger_growth=rate(),
        awake_fatigue_growth=rate(),
        resting_fatigue_recovery=rate(),
        sleeping_fatigue_recovery=rate(),
        stress_pressure=rate(),
        queued_stress=rate(),
        resting_stress_recovery=rate(),
        sleeping_stress_recovery=rate(),
        need_pressure_threshold=draws.random(),
        hydration_growth=rate(),
        support_wait_stress=rate(),
        natural_recovery_rate_per_minute=rate(),
        toilet_growth_by_agent={ids[0]: rate()},
        hunger_growth_by_agent={ids[0]: rate()},
        awake_fatigue_growth_by_agent={ids[0]: rate()},
        natural_recovery_rate_by_agent={ids[0]: rate()},
        baseline_distress_by_agent={ids[0]: draws.random()},
    )


def _draw_policies(draws: random.Random, ids: list[str]) -> list[Any]:
    body = {
        "uncertainty_drift_per_minute": draws.random() * 0.01,
        "discontent_stress_rate": draws.random() * 0.01,
        "sleep_debt_stress_rate_by_agent": {ids[0]: draws.random() * 0.01},
        "negative_affect_stress_rate_by_agent": {ids[0]: draws.random() * 0.01},
        "discontent_stress_rate_by_agent": {ids[0]: draws.random() * 0.01},
    }
    parameters = _draw_parameters(draws, ids)
    return [
        SimplePsychologyPolicy(parameters),
        SimplePsychologyPolicy(parameters, include_direct_rest_stress_recovery=False),
        BoundedStressPolicy(parameters),
        MeanRevertingDistressPolicy(parameters),
        LifeStatePsychologyPolicy(parameters, body_config=body),
        LifeStatePsychologyPolicy(parameters, body_config=body, body_formula=FULL),
        LifeStatePsychologyPolicy(parameters, body_config=body, body_formula=SLEEP_ONLY),
        LifeStatePsychologyPolicy(
            parameters,
            body_config=body,
            body_formula=FULL,
            include_direct_rest_stress_recovery=False,
            use_tick_start_stress_for_feedback=True,
        ),
        IntegratedDistressLifeStatePolicy(parameters, body_config=body),
        IntegratedDistressLifeStatePolicy(parameters, body_config=body, body_formula=FULL),
        IntegratedDistressLifeStatePolicy(parameters, body_config=body, body_formula=SLEEP_ONLY),
    ]


def _shuffled_world(scenario: Any, draws: random.Random) -> WorldState:
    world = WorldState.from_scenario(scenario)
    world.model_state.setdefault("body_state_v1", create_body_state(world))
    zones = sorted(world.zones)
    extremes = (0.0, 1.0, 0.5, 0.35, 0.7)
    for number, agent in enumerate(world.agents.values()):
        agent.activity = draws.choice(list(Activity))
        agent.zone_id = draws.choice(zones)
        needs = agent.needs
        for name in ("toilet_urgency", "hunger", "fatigue", "hydration_need"):
            setattr(needs, name, draws.random() if number % 3 else draws.choice(extremes))
        agent.stress = draws.random() if number % 3 else draws.choice(extremes)
        record = world.model_state["body_state_v1"]["agents"][agent.id]
        for name in record:
            record[name] = draws.random() if number % 3 else draws.choice(extremes)
        record["sleep_debt_minutes"] = draws.choice((0.0, 1440.0, draws.random() * 1440.0))
    for number, agent in enumerate(list(world.agents.values())[:6]):
        status = list(SupportRequestStatus)[number % 4]
        request = SupportRequestState(f"R{number}", agent.id, 0, 10, status=status)
        world.support_requests[request.id] = request
        agent.support_request_id = request.id
    # An id with no request is no open request.
    list(world.agents.values())[7].support_request_id = "nothing"
    return world


def _deltas(world: WorldState, policy: Any, view: Any) -> tuple[Any, ...]:
    result = _rules().update_psychology(_controller(world, policy), view, world.minute_index)
    assert result.traces == ()
    for delta in result.deltas:
        assert type(delta) is AgentStateDelta
        # What the social step does to a delta.
        assert delta.with_stress(0.25).stress == 0.25
    return result.deltas


def _texts_of_models(world: WorldState, policies: list[Any], view: Any) -> list[str]:
    return [_text(delta) for policy in policies for delta in _deltas(world, policy, view)]


@pytest.mark.parametrize("seed", range(8))
def test_every_model_gives_the_bits_of_the_deleted_rules(
    generated_scenario: Any, seed: int
) -> None:
    draws = random.Random(seed)
    world = _shuffled_world(generated_scenario, draws)
    view = view_of(world, include_state_hash=False)
    for agent in world.agents.values():
        assert view.agents[agent.id].needs.hunger == agent.needs.hunger
    policies = _draw_policies(draws, sorted(world.agents))
    _answers(f"models-{seed}", _texts_of_models(world, policies, view))


def test_people_of_a_group_are_separated_as_the_deleted_rule_said(
    generated_scenario: Any,
) -> None:
    draws = random.Random(5)
    world = _shuffled_world(generated_scenario, draws)
    view = view_of(world, include_state_hash=False)
    separated = {
        agent.id
        for agent in view.agents.values()
        if any(
            peer != agent.id and view.agents[peer].zone_id != agent.zone_id
            for peer in view.group_members.get(agent.group_id or "", ())
        )
    }
    assert separated and separated != set(view.agents)
    policy = LifeStatePsychologyPolicy(PsychologyParameters(), body_formula=FULL)
    _answers("group", _texts_of_models(world, [policy], view))


def test_a_one_percent_change_shows(generated_scenario: Any) -> None:
    draws = random.Random(1)
    world = _shuffled_world(generated_scenario, draws)
    view = view_of(world, include_state_hash=False)
    parameters = PsychologyParameters()
    changed = replace(parameters, stress_pressure=parameters.stress_pressure * 1.01)
    same = _texts_of_models(
        world, [IntegratedDistressLifeStatePolicy(parameters, body_formula=FULL)], view
    )
    _answers("one-percent", same)
    other = _texts_of_models(
        world, [IntegratedDistressLifeStatePolicy(changed, body_formula=FULL)], view
    )
    assert other != same


def _view_with(view: Any, agent_id: str, **changes: Any) -> Any:
    return replace(
        view, agents={**view.agents, agent_id: replace(view.agents[agent_id], **changes)}
    )


class _OwnRules(SimplePsychologyPolicy):
    policy_version = "causal-stress-v1"


@pytest.mark.parametrize(
    ("change", "reason"),
    [
        ({"sleep_debt_minutes": -1.0}, "out of bounds"),
        ({"sleep_debt_minutes": 1441.0}, "out of bounds"),
        ({"sleep_debt_minutes": float("nan")}, "out of bounds"),
        ({"sleep_debt_minutes": 0}, "not a float"),
        ({"rest_efficiency_multiplier": 1.5}, "out of bounds"),
        ({"required_sleep_minutes": 1441}, "out of bounds"),
        ({"required_sleep_minutes": 8.0}, "not an int"),
        ({"positive_affect": 3.0}, "out of bounds"),
        ({"reassurance": 2.0}, "out of bounds"),
        ({"group_id": ""}, "empty"),
    ],
)
def test_what_the_deleted_checks_turned_down_is_refused(
    generated_scenario: Any, change: dict[str, Any], reason: str
) -> None:
    """The deleted rules raised on these (sleep_debt_step's and AgentStateDelta's checks); no
    view of a Run holds them."""
    world = _shuffled_world(generated_scenario, random.Random(2))
    before = _people_text(world)
    view = view_of(world, include_state_hash=False)
    first = next(iter(view.agents))
    policy = IntegratedDistressLifeStatePolicy(PsychologyParameters(), body_formula=FULL)
    with pytest.raises(native.Unsupported, match=reason):
        _rules().update_psychology(_controller(world, policy), _view_with(view, first, **change), 0)
    assert _people_text(world) == before


@pytest.mark.parametrize(
    ("policy", "trace", "minute", "reason"),
    [
        (SimplePsychologyPolicy(), TraceLevel.SUMMARY, 0, "a view Python made"),
        (SimplePsychologyPolicy(), TraceLevel.FULL, 0, "a view Python made"),
        (_OwnRules(), TraceLevel.OFF, 0, "not one this kernel knows"),
        (object(), TraceLevel.OFF, 0, "not one this kernel knows"),
        (SimplePsychologyPolicy(), TraceLevel.OFF, 1, "not the view's"),
        (
            SimplePsychologyPolicy(PsychologyParameters(toilet_growth=0)),
            TraceLevel.OFF,
            0,
            "not a float",
        ),
    ],
)
def test_what_the_kernel_does_not_handle_is_refused(
    generated_scenario: Any, policy: Any, trace: TraceLevel, minute: int, reason: str
) -> None:
    world = WorldState.from_scenario(generated_scenario)
    view = view_of(world, include_state_hash=False)
    if reason == "a view Python made":
        # A copy of the kernel's view, without its snapshot.
        view = replace(view)
    with pytest.raises(native.Unsupported, match=reason):
        _rules().update_psychology(_controller(world, policy, trace), view, minute)
    # The step of a tick raises it as KernelRefusal.
    with pytest.raises(KernelRefusal, match=reason):
        OfflineSimulationController._psychology_update(
            _controller(world, policy, trace), view, minute
        )


def _int_policies(ids: list[str]) -> list[Any]:
    """Each model with the ints a Scenario may write: per-person rates and baselines of `0` or `1`
    (StrictInt in its psychology config), a resource service's hydration growth of `0`, and body
    rates of `0` or `1` (canonical_body_config allows ints)."""
    first, second, third = ids[:3]
    parameters = PsychologyParameters(
        toilet_growth_by_agent={first: 0, second: 0.01},
        hunger_growth_by_agent={first: 0},
        awake_fatigue_growth_by_agent={third: 0},
        natural_recovery_rate_by_agent={first: 0, second: 0},
        baseline_distress_by_agent={first: 1, second: 0, third: 0.3},
        hydration_growth=0,
    )
    body = {
        "discontent_stress_rate": 1,
        "awake_sleep_debt_minutes_per_minute": 1,
        "sleep_debt_stress_rate": 0,
        "uncertainty_decay_per_minute": 0,
        "discontent_queue_rate": 1,
        "reassurance_decay_per_minute": 0,
        "positive_affect_decay_per_minute": 1,
        "sleep_debt_stress_rate_by_agent": {first: 1},
        "negative_affect_stress_rate_by_agent": {second: 0},
        "discontent_stress_rate_by_agent": {third: 1},
    }
    return [
        SimplePsychologyPolicy(parameters),
        BoundedStressPolicy(parameters),
        MeanRevertingDistressPolicy(parameters),
        LifeStatePsychologyPolicy(parameters, body_config=body),
        LifeStatePsychologyPolicy(parameters, body_config=body, body_formula=FULL),
        LifeStatePsychologyPolicy(parameters, body_config=body, body_formula=SLEEP_ONLY),
        IntegratedDistressLifeStatePolicy(parameters, body_config=body),
        IntegratedDistressLifeStatePolicy(parameters, body_config=body, body_formula=FULL),
    ]


@pytest.mark.parametrize("seed", range(3))
def test_int_values_of_a_scenario_give_the_bits_of_the_deleted_rules(
    generated_scenario: Any, seed: int
) -> None:
    """They meet floats in every expression, where they are the same f64. With a trace, a
    separated household's int pressure stays an int (tests/integration/test_trace_documents.py)."""
    draws = random.Random(seed)
    world = _shuffled_world(generated_scenario, draws)
    view = view_of(world, include_state_hash=False)
    _answers(f"ints-{seed}", _texts_of_models(world, _int_policies(sorted(world.agents)), view))


def test_a_tainted_store_is_refused(generated_scenario: Any) -> None:
    world = WorldState.from_scenario(generated_scenario)
    view = view_of(world, include_state_hash=False)
    deltas = (
        _rules().update_psychology(_controller(world, SimplePsychologyPolicy()), view, 0).deltas
    )
    next(iter(world.agents.values())).stress = 0
    assert world._core.entities().tainted is not None
    with pytest.raises(native.Unsupported, match="tainted"):
        _rules().update_psychology(_controller(world, SimplePsychologyPolicy()), view, 0)
    before = _people_text(world)
    with pytest.raises(KernelRefusal, match="tainted"):
        OfflineSimulationController._apply_state_deltas(None, world, deltas)
    assert _people_text(world) == before


def test_a_trust_out_of_bounds_is_refused(generated_scenario: Any) -> None:
    """The deleted rule raised AgentStateDelta's error; no Run writes such a trust."""
    world = WorldState.from_scenario(generated_scenario)
    next(iter(world.agents.values())).trust = 1.5
    view = view_of(world, include_state_hash=False)
    with pytest.raises(KernelRefusal, match="trust"):
        OfflineSimulationController._psychology_update(
            _controller(world, SimplePsychologyPolicy()), view, 0
        )


# ---- Writing the values ----------------------------------------------------------------------


def _deltas_of(world: WorldState, draws: random.Random, *, body: bool) -> tuple[Any, ...]:
    def value() -> float:
        return draws.random()

    return tuple(
        AgentStateDelta(
            agent_id=agent_id,
            minute_index=world.minute_index,
            toilet_urgency=value(),
            hunger=agent.needs.hunger,
            fatigue=value(),
            stress=value(),
            trust=agent.trust,
            hydration_need=value(),
            **(
                {
                    "sleep_debt_minutes": value() * 1440.0,
                    "uncertainty": value(),
                    "reassurance": value(),
                    "discontent": value(),
                    "positive_affect": value(),
                    "negative_affect": value(),
                    "hygiene_need": value(),
                }
                if body
                else {}
            ),
        )
        for agent_id, agent in world.agents.items()
    )


@pytest.mark.parametrize("body", [False, True])
def test_writing_gives_what_the_deleted_rule_gave(generated_scenario: Any, body: bool) -> None:
    world = WorldState.from_scenario(generated_scenario)
    world.model_state.setdefault("body_state_v1", create_body_state(world))
    deltas = _deltas_of(world, random.Random(3), body=body)
    store = world._core.entities()
    forwards = store.stats()["forwards"]
    _rules().apply_deltas(None, world, deltas)
    _answers(
        f"writing-{body}",
        [
            _people_text(world),
            json.dumps(world.model_state["body_state_v1"]),
            world.canonical_json(),
        ],
    )
    # Nothing came back through __setattr__, and nothing but the objects and rows moved.
    assert store.stats()["forwards"] == forwards
    assert store.verify(world) is None
    # The objects still forward what comes next, to the same rows.
    first = next(iter(world.agents.values()))
    first.needs.hunger = 0.125
    first.stress = 0.25
    assert store.verify(world) is None


def test_an_unchanged_world_is_not_written_again(generated_scenario: Any) -> None:
    world = WorldState.from_scenario(generated_scenario)
    world.model_state.setdefault("body_state_v1", create_body_state(world))
    deltas = _deltas_of(world, random.Random(3), body=False)
    _rules().apply_deltas(None, world, deltas)
    agent = next(iter(world.agents.values()))
    stress = agent.stress
    again = tuple(replace(delta, stress=float(delta.stress)) for delta in deltas)
    _rules().apply_deltas(None, world, again)
    assert agent.stress is stress


def _broken(delta: AgentStateDelta, **changes: Any) -> AgentStateDelta:
    copied = copy.copy(delta)
    for name, value in changes.items():
        object.__setattr__(copied, name, value)
    return copied


def _refused_without_a_change(world: WorldState, deltas: Any, reason: str = "") -> None:
    def state() -> str:
        return _people_text(world) + json.dumps(world.model_state.get("body_state_v1"))

    before = state()
    with pytest.raises(KernelRefusal, match=reason):
        OfflineSimulationController._apply_state_deltas(None, world, tuple(deltas))
    assert state() == before
    assert world._core.entities().verify(world) is None


@pytest.mark.parametrize(
    "change",
    [
        {"agent_id": "nobody"},
        {"minute_index": 9},
        {"hunger": float("nan")},
        {"hunger": 1.5},
        {"stress": -0.5},
        {"trust": True},
    ],
)
def test_a_bad_delta_is_refused_and_changes_nothing(
    generated_scenario: Any, change: dict[str, Any]
) -> None:
    """The deleted rule raised StateInvariantError on these; the kernels make no such delta."""
    world = WorldState.from_scenario(generated_scenario)
    world.model_state.setdefault("body_state_v1", create_body_state(world))
    deltas = list(_deltas_of(world, random.Random(4), body=True))
    deltas[-1] = _broken(deltas[-1], **change)
    _refused_without_a_change(world, deltas)


def test_two_deltas_for_one_person_are_refused(generated_scenario: Any) -> None:
    world = WorldState.from_scenario(generated_scenario)
    delta = _deltas_of(world, random.Random(4), body=False)[0]
    _refused_without_a_change(world, (delta, delta), "two deltas")


@pytest.mark.parametrize("drop", ["namespace", "record"])
def test_body_values_without_a_record_are_refused(generated_scenario: Any, drop: str) -> None:
    world = WorldState.from_scenario(generated_scenario)
    world.model_state.setdefault("body_state_v1", create_body_state(world))
    deltas = _deltas_of(world, random.Random(4), body=True)
    if drop == "namespace":
        del world.model_state["body_state_v1"]
    else:
        del world.model_state["body_state_v1"]["agents"][deltas[-1].agent_id]
    _refused_without_a_change(world, deltas, "no body record")


def test_a_delta_of_ints_is_refused(generated_scenario: Any) -> None:
    """The deleted rule accepted an int within bounds; the kernels' deltas hold floats."""
    world = WorldState.from_scenario(generated_scenario)
    delta = _broken(_deltas_of(world, random.Random(4), body=False)[0], hunger=1)
    _refused_without_a_change(world, (delta,))


def test_a_body_namespace_without_a_row_is_refused(generated_scenario: Any) -> None:
    world = WorldState.from_scenario(generated_scenario)
    world.model_state["body_state_v1"] = type("OtherDict", (dict,), {})(create_body_state(world))
    deltas = _deltas_of(world, random.Random(4), body=True)
    with pytest.raises(native.Unsupported, match="no row"):
        _rules().apply_deltas(None, world, deltas)


# ---- Whole ticks ---------------------------------------------------------------------------------


def test_ticks_run_in_the_kernels_and_make_the_world_of_the_deleted_rules(
    generated_scenario: Any,
) -> None:
    world = WorldState.from_scenario(generated_scenario)
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    engine = TickEngine(world, phase_handler=controller)
    ticks = driven_ticks()
    engine.run(steps=40)
    # The driver ran each tick, and its update phase the steps' kernels.
    assert driven_ticks() - ticks == 40
    assert world._core.entities().verify(world) is None
    _answers("ticks", [world.canonical_json()])
