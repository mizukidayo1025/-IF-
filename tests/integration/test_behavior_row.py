"""The entries of rules/behavior.rs: the proposals and the observed state of a person.

The Python rules of `UtilityBehaviorPolicy.propose`, `PriorityRuleBehaviorPolicy.propose` and
`_agent_view_snapshot` are gone (lane L10; docs/rust-port/lanes/h-behavior.md). What the kernel
makes, every field the bits of what the Python rules made from the same inputs (drawn worlds, seeds
past 64 bits, requests and queued tasks, a Run), is checked without Python in
`native/core/tests/behavior_rows.rs`, on the same worlds (`support/core_fixtures_reads.py`, #225).
Here are the entries, what the kernel refuses of what Python holds, and the ticks.
"""

from __future__ import annotations

from dataclasses import replace
from types import SimpleNamespace
from typing import Any

import pytest
from support.core_fixtures_reads import requests_world
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
from sheltersim.experiments.timeline import _MetricSink
from sheltersim.policies.behavior import (
    BehaviorDecisionRunner,
    PriorityRuleBehaviorPolicy,
    UtilityBehaviorPolicy,
    WorldView,
)


def _rules() -> Any:
    return native.rules("behavior")


def _controller(world: WorldState) -> OfflineSimulationController:
    return OfflineSimulationController(world, _MetricSink(MetricsRecorder()))


def _view(world: WorldState, controller: Any = None) -> WorldView:
    controller = controller or _controller(world)
    return native.rules("world_view").view_for_proposals(controller, world, world.minute_index)


def test_requests_cooldowns_and_queued_tasks_are_read_as_python_reads_them(
    generated_scenario: Any,
) -> None:
    """The entries of the proposals and of the observed state (the core's test of the same name has
    the bits of all they made)."""
    world, controller = requests_world(generated_scenario)
    view = _view(world, controller)
    people = sorted(world.agents)
    for threshold in (1.1, 0.0):
        runner = BehaviorDecisionRunner(
            UtilityBehaviorPolicy(information_query_threshold=threshold)
        )
        made = _rules().propose_due(SimpleNamespace(decisions=runner), view, CounterRNG(11))
        assert {proposal.agent_id for proposal in made} == set(people)
    snapshots = {
        agent_id: _rules().observed_state(SimpleNamespace(), view, agent_id) for agent_id in people
    }
    assert snapshots[people[0]]["support"]["status"] == "pending"
    assert snapshots[people[0]]["support"]["wait_minutes"] == 35
    assert snapshots[people[5]]["support"]["cooldown_remaining_minutes"] == 15
    assert snapshots[people[8]]["queue"] == {
        "task_id": "T2",
        "kind": "resource_service_queue",
        "target_id": None,
        "wait_minutes": 0,
    }


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
