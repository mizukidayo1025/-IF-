"""The entries of rules/behavior.rs: the proposals and the observed state of a person.

The Python rules of `UtilityBehaviorPolicy.propose`, `PriorityRuleBehaviorPolicy.propose` and
`_agent_view_snapshot` are gone (lane L10; docs/rust-port/lanes/h-behavior.md). Each case hashes the
bits (`_exact`) of everything the kernel made, in order, and compares the digest with the one of
what the Python rules made from the same inputs, recorded from them on 45a9bc9 before they went
(`_Bits`, pinned on every OS, as the gate's hashes are). Model version 2 (step 4, L11: the pinned
libm crate and the information events' draw keys) changed some of these results: those digests
were re-recorded from the kernels of that model and are no longer the deleted Python rules'.

The drawn worlds stay here (#225, decision 5: their inputs, drawn with `random.Random`, are too
large to export), and are the entries of the proposals and the observed state. A seed past 64
bits, requests and queued tasks, and a Run's proposals are checked without Python in
`native/core/tests/behavior_rows.rs` (`support/core_fixtures_reads.py`). Here are also what the
kernel refuses of what Python holds, and the ticks.
"""

from __future__ import annotations

import hashlib
import json
import random
from dataclasses import replace
from enum import Enum
from types import SimpleNamespace
from typing import Any

import pytest
from support.core_fixtures_reads import draw_policy, draw_world
from support.paths import SMALL_REPLAY
from support.world import driven_ticks, view_of

from sheltersim import native
from sheltersim.core.engine import TickEngine, TickPhase
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.rng import CounterRNG
from sheltersim.core.world import WorldState
from sheltersim.domain.body_state import create_body_state
from sheltersim.domain.models import (
    Activity,
)
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.experiments.timeline import _MetricSink
from sheltersim.policies.behavior import (
    BehaviorDecisionRunner,
    PriorityRuleBehaviorPolicy,
    UtilityBehaviorPolicy,
    WorldView,
)


def _exact(value: Any) -> Any:
    """Equal only with the same types, the same bits, and the same order of each dict's keys."""
    if value is None or type(value) is bool:
        return value
    if type(value) is float:
        return value.hex()
    if isinstance(value, Enum):
        return [type(value).__name__, value.value]
    if type(value) is int or type(value) is str:
        return [type(value).__name__, value]
    if isinstance(value, (tuple, list)):
        return [type(value).__name__, *(_exact(item) for item in value)]
    if isinstance(value, dict):
        return ["dict", *([key, _exact(item)] for key, item in value.items())]
    if hasattr(value, "__dataclass_fields__"):
        return [type(value).__name__, *([k, _exact(v)] for k, v in value.__dict__.items())]
    raise TypeError(type(value))


def _rules() -> Any:
    return native.rules("behavior")


def _controller(world: WorldState) -> OfflineSimulationController:
    return OfflineSimulationController(world, _MetricSink(MetricsRecorder()))


def _view(world: WorldState, controller: Any = None) -> WorldView:
    controller = controller or _controller(world)
    return native.rules("world_view").view_for_proposals(controller, world, world.minute_index)


class _Bits:
    """The bits of each value the kernel made, in order, as one digest; `check` compares it with
    the digest of the Python rules' values, recorded from them on 45a9bc9 (on Linux)."""

    def __init__(self) -> None:
        self._hash = hashlib.sha256()

    def add(self, value: Any) -> Any:
        self._hash.update(json.dumps(_exact(value)).encode("utf-8"))
        return value

    def check(self, digest: str) -> None:
        assert self._hash.hexdigest()[:16] == digest


def _proposals(bits: _Bits, policy: Any, view: WorldView, seed: int = 11) -> tuple[Any, ...]:
    """The kernel's proposals (`ActionProposal`s), added to `bits`."""
    runner = BehaviorDecisionRunner(policy)
    return bits.add(_rules().propose_due(SimpleNamespace(decisions=runner), view, CounterRNG(seed)))


def _observed(bits: _Bits, view: WorldView, agent_id: str) -> Any:
    """The kernel's observed state of a person, added to `bits`."""
    return bits.add(_rules().observed_state(SimpleNamespace(), view, agent_id))


def _drawn(draws: random.Random, scenario: Any) -> tuple[WorldState, WorldView]:
    """A drawn world (`support/core_fixtures_reads.py`'s `draw_world`) and its view."""
    world, controller, _ = draw_world(draws, scenario)
    return world, _view(world, controller)


# What the Python rules made of each drawn world below, by its seed.
DRAWN_WORLDS = {
    0: "2fae7a31edbc3c16",
    1: "fd8741e8bc7a3693",
    2: "4156e098c70b0e82",
    3: "b84771aeec0cadd1",
    4: "31140fd72ef0f3a3",
    5: "69f5cb1c3f6cd3f6",
    6: "758fadbbe877fb40",
    7: "f365645fc57ad062",
    8: "45a72a5cec4be3f5",
    9: "015eba747f860aa6",
    10: "295a09cee324245d",
    11: "98b947d8e7144135",
    12: "287fd8ba0f8f7cd0",
    13: "34e963c729fc7d55",
    14: "cfdcc5e644625650",
    15: "57c04f8a579696f7",
    16: "681c23730e464b81",
    17: "8356521e966b2ce0",
    18: "ed29383f6605becb",
    19: "499c3c2d73cf4264",
    20: "09f53e4bfb031ffb",
    21: "89dc8cbe38936da4",
    22: "96db2d073a3665a0",
    23: "edcc9b9e0b1303c1",
    24: "2e11073734139790",
    25: "8339df839444504d",
    26: "78447961a6b236d2",
    27: "530c552f9d011ce4",
    28: "e535c9405f0e2425",
    29: "da5ecfb298102457",
}


@pytest.mark.parametrize("seed", range(30))
def test_each_proposal_of_a_drawn_world_has_the_bits_of_python(
    seed: int, generated_scenario: Any
) -> None:
    draws = random.Random(seed)
    # The generated scenario has the water and meal actions, the small one doesn't.
    scenario = ScenarioSpec.load(SMALL_REPLAY) if seed % 3 == 0 else generated_scenario
    world, view = _drawn(draws, scenario)
    assert view.resource_service_enabled is (seed % 3 != 0)
    bits = _Bits()
    for _ in range(4):
        policy = draw_policy(draws, sorted(world.agents))
        _proposals(bits, policy, view, seed=draws.randrange(0, 2**40))
    for agent_id in world.agents:
        _observed(bits, view, agent_id)
    bits.check(DRAWN_WORLDS[seed])


def test_drawn_worlds_reach_what_they_are_for(generated_scenario: Any) -> None:
    """The comparison above means little if the branches stay unreached."""
    actions: set[str] = set()
    with_target: set[str] = set()
    demand = ties = 0
    bits = _Bits()
    for seed in range(60):
        draws = random.Random(seed)
        world, view = _drawn(draws, generated_scenario)
        policy = draw_policy(draws, sorted(world.agents))
        for proposal in _proposals(bits, policy, view):
            actions.add(proposal.action.value)
            if proposal.target_id is not None:
                with_target.add(proposal.action.value)
            ties += proposal.rng_key[-1] == "priority-tie-break"
            demand += any("experimental zone" in item.trigger for item in proposal.candidates)
    assert actions >= {
        "wait",
        "request_toilet",
        "rest",
        "sleep",
        "seek_help",
        "seek_information",
        "socialize",
        "care_family",
        "help_others",
        "fetch_water",
        "collect_meal",
    }
    assert with_target == {"socialize", "care_family", "help_others"}
    assert ties > 0 and demand > 0
    bits.check("6057d790ccb496c7")


def test_a_tick_makes_no_python_view_of_the_people(
    generated_scenario: Any, monkeypatch: pytest.MonkeyPatch
) -> None:
    """No view of a tick makes its people in Python: no Python rule reads them."""
    world = WorldState.from_scenario(generated_scenario)
    controller = _controller(world)
    engine = TickEngine(world, phase_handler=controller)
    made: list[int] = []
    read = WorldView.__getattr__

    def counted(self: WorldView, name: str) -> Any:
        if "agents" not in self.__dict__ and "_rust_view" in self.__dict__:
            made.append(world.minute_index)
        return read(self, name)

    decided: list[int] = []
    due: dict[str, int] = {}

    def watch(phase: TickPhase, before: bool) -> None:
        # A decision moves its person's next decision on.
        if phase is TickPhase.PROPOSE_ACTIONS:
            now = {agent.id: agent.next_decision_at for agent in world.agents.values()}
            if before:
                due.update(now)
            else:
                decided.append(sum(now[agent_id] != at for agent_id, at in due.items()))

    monkeypatch.setattr(WorldView, "__getattr__", counted)
    engine.phase_observer = watch
    ticks = driven_ticks()
    for _ in range(20):
        engine.step()
    assert driven_ticks() - ticks == 20
    assert len(decided) == 20 and sum(decided) > 0
    # The first tick's social step is Python's: its namespace doesn't exist yet.
    assert all(minute == 0 for minute in made)


def test_a_view_python_made_is_refused(generated_scenario: Any) -> None:
    world = WorldState.from_scenario(generated_scenario)
    world.model_state.setdefault("body_state_v1", create_body_state(world))
    # A copy of the kernel's view, without its snapshot.
    view = replace(view_of(world))
    assert "_rust_view" not in view.__dict__
    controller = SimpleNamespace(decisions=BehaviorDecisionRunner())
    with pytest.raises(native.Unsupported, match="Python's"):
        _rules().propose_due(controller, view, CounterRNG(1))
    with pytest.raises(native.Unsupported, match="Python's"):
        _rules().observed_state(controller, view, "A001")


def test_a_view_without_its_hash_is_refused(generated_scenario: Any) -> None:
    world = WorldState.from_scenario(generated_scenario)
    controller = _controller(world)
    view = native.rules("world_view").view_after_movement(controller, world)
    assert view.state_hash is None
    with pytest.raises(native.Unsupported, match="hash"):
        _rules().propose_due(
            SimpleNamespace(decisions=BehaviorDecisionRunner()), view, CounterRNG(1)
        )


def test_what_the_kernel_does_not_know_is_refused(generated_scenario: Any) -> None:
    class OtherPolicy(UtilityBehaviorPolicy):
        pass

    class OtherRunner(BehaviorDecisionRunner):
        pass

    class OtherRandom(CounterRNG):
        pass

    world = WorldState.from_scenario(generated_scenario)
    view = _view(world)
    rng = CounterRNG(1)
    cases = (
        (SimpleNamespace(decisions=OtherRunner()), rng, "decisions"),
        (SimpleNamespace(decisions=BehaviorDecisionRunner(OtherPolicy())), rng, "policy"),
        (
            SimpleNamespace(
                decisions=BehaviorDecisionRunner(PriorityRuleBehaviorPolicy(OtherPolicy()))
            ),
            rng,
            "scorer",
        ),
        (SimpleNamespace(decisions=BehaviorDecisionRunner()), OtherRandom(1), "RNG"),
    )
    for controller, source, reason in cases:
        with pytest.raises(native.Unsupported, match=reason):
            _rules().propose_due(controller, view, source)
    with pytest.raises(native.Unsupported, match="no such person"):
        _rules().observed_state(SimpleNamespace(), view, "nobody")


def _everyone_due(world: WorldState) -> None:
    for agent in world.agents.values():
        agent.activity = Activity.IDLE
        agent.next_decision_at = 0


@pytest.mark.parametrize(
    ("field", "value", "message"),
    [
        # The difference from the best utility, divided by this, leaves the range of exp: the
        # Python rule raised OverflowError.
        ("temperature", -1e-300, "exponent is out of range"),
        # The Python rule raised "proposal has invalid decision timing".
        ("decision_interval_minutes", 0, "invalid decision timing"),
    ],
)
def test_what_python_raised_on_is_a_kernel_refusal(
    generated_scenario: Any, field: str, value: Any, message: str
) -> None:
    """Values the policy's own checks turn down when it is made, set past them."""
    world = WorldState.from_scenario(generated_scenario)
    _everyone_due(world)
    world.agents["A001"].needs.fatigue = 0.9
    controller = _controller(world)
    view = _view(world, controller)
    policy = UtilityBehaviorPolicy()
    object.__setattr__(policy, field, value)
    controller.decisions = BehaviorDecisionRunner(policy)
    controller._run_core.refresh_policies(controller)
    with pytest.raises(KernelRefusal, match=message):
        controller._propose_due(view, CounterRNG(1))


def test_the_step_refuses_a_policy_of_another_class(generated_scenario: Any) -> None:
    """A subclass may score otherwise: the step has no Python rule to run it with."""
    world = WorldState.from_scenario(generated_scenario)
    _everyone_due(world)
    controller = _controller(world)
    view = _view(world, controller)

    class Counting(UtilityBehaviorPolicy):
        pass

    controller.decisions = BehaviorDecisionRunner(Counting())
    controller._run_core.refresh_policies(controller)
    with pytest.raises(KernelRefusal, match="the policy is another class"):
        controller._propose_due(view, CounterRNG(1))
    assert "agents" not in view.__dict__
