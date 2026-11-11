"""Immutable policy views and reproducible behavior proposals.

The proposals are native/src/rules/behavior.rs's, as the propose phase makes them
(`support.world.decisions_of`). Checkpoint replay of a behavior policy variant is checked in
tests/integration/test_behavior_policy.py.
"""

from __future__ import annotations

from dataclasses import FrozenInstanceError

import pytest
from support.paths import BASELINE_100_7D, SCENARIO_100_AGENTS
from support.world import decisions_of, view_of

from sheltersim import native
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.rng import CounterRNG
from sheltersim.core.world import WorldState
from sheltersim.domain.body_state import create_body_state
from sheltersim.domain.models import Activity
from sheltersim.domain.staff import StaffService
from sheltersim.experiments.metrics import MetricsRecorder
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.experiments.timeline import _MetricSink
from sheltersim.policies.behavior import (
    ActionCandidate,
    ActionKind,
    ActionProposal,
    AgentView,
    BehaviorDecisionRunner,
    PriorityRuleBehaviorPolicy,
    UtilityBehaviorPolicy,
    behavior_policy_catalog,
    create_behavior_policy,
)


def make_world() -> WorldState:
    return WorldState.from_scenario(ScenarioSpec.load(SCENARIO_100_AGENTS))


def test_world_view_gives_every_person_each_field_of_the_view(generated_scenario) -> None:
    """The view kernel fills a view's fields directly; none may fall back to the class default."""
    view = view_of(WorldState.from_scenario(generated_scenario))
    names = set(AgentView.__dataclass_fields__)
    for agent in view.agents.values():
        assert set(agent.__dict__) == names
        assert AgentView(**agent.__dict__) == agent
        with pytest.raises(FrozenInstanceError):
            agent.stress = 1.0


def test_world_view_is_detached_read_only_and_canonical() -> None:
    world = make_world()
    view = view_of(world)
    original_hash = view.state_hash

    with pytest.raises(TypeError):
        view.agents["other"] = view.agents["A001"]  # type: ignore[index]
    with pytest.raises(FrozenInstanceError):
        view.agents["A001"].stress = 0.7  # type: ignore[misc]

    world.agents["A001"].stress = 0.7
    assert view.agents["A001"].stress == 0.0
    assert view.state_hash == original_hash
    assert view_of(make_world()).state_hash == original_hash


def test_behavior_proposal_is_reproducible_and_exposes_candidate_trace() -> None:
    world = make_world()
    agent = world.agents["A001"]
    agent.activity = Activity.IDLE
    agent.needs.toilet_urgency = 0.95
    view = view_of(world)
    policy = UtilityBehaviorPolicy()

    first = decisions_of(world, policy)["A001"]
    second = decisions_of(world, policy)["A001"]
    candidates = {candidate.action: candidate for candidate in first.candidates}

    assert first == second
    assert first.observed_state_hash == view.state_hash
    assert first.rng_key == (
        "behavior",
        agent.profile.rng_key,
        "A001",
        "minute",
        0,
        "policy",
        policy.policy_version,
        "choice",
    )
    assert first.trigger == "scheduled_decision"
    assert candidates[ActionKind.REQUEST_TOILET].eligible
    assert candidates[ActionKind.REQUEST_TOILET].urgency_priority == 1
    assert (
        candidates[ActionKind.REQUEST_TOILET].trigger == "toilet_urgency>=0.25 or hygiene_need>=0.5"
    )
    assert sum(candidate.probability for candidate in first.candidates) == pytest.approx(1.0)
    assert first.selection_probability == candidates[first.action].probability


def test_a_person_not_due_or_busy_gets_no_proposal() -> None:
    world = make_world()
    agent = world.agents["A001"]
    assert "A001" in decisions_of(world)

    agent.next_decision_at = 1
    assert "A001" not in decisions_of(world)

    agent.next_decision_at = 0
    agent.activity = Activity.QUEUED
    assert "A001" not in decisions_of(world)


def test_a_policy_of_another_class_is_a_kernel_refusal() -> None:
    """The step decides with the installed policies only: there is no Python rule to call."""
    world = make_world()
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    view = controller._view_for_proposals(world, world.minute_index)

    class FixedActionPolicy:
        policy_version = "fixed-wait-v1"

    controller.decisions = BehaviorDecisionRunner(FixedActionPolicy())  # type: ignore[arg-type]
    with pytest.raises(KernelRefusal, match="the policy is another class"):
        controller._propose_due(view, CounterRNG(world.seed))


def test_registered_priority_policy_reuses_candidates_and_is_keyed_deterministic() -> None:
    world = make_world()
    world.agents["A001"].activity = Activity.IDLE
    world.agents["A001"].needs.toilet_urgency = 0.95
    world.agents["A001"].needs.hunger = 0.85
    before = world.canonical_json()
    policy = create_behavior_policy("priority-rule-v1", temperature=0.8)

    first = decisions_of(world, policy)["A001"]
    second = decisions_of(world, policy)["A001"]
    candidates = [item for item in first.candidates if item.eligible]
    best_rank = max((item.urgency_priority, item.utility) for item in candidates)
    selected = next(item for item in candidates if item.action is first.action)

    assert isinstance(policy, PriorityRuleBehaviorPolicy)
    assert first == second
    assert world.canonical_json() == before
    assert first.policy_version == "priority-rule-v1"
    assert (selected.urgency_priority, selected.utility) == best_rank
    assert sum(item.probability for item in first.candidates) == pytest.approx(1.0)
    assert {item["variant"] for item in behavior_policy_catalog()} == {
        "utility-softmax-v1",
        "sleep-aware-utility-softmax-v2",
        "priority-rule-v1",
    }


def test_sleep_aware_behavior_normalizes_debt_by_daily_sleep_need() -> None:
    world = make_world()
    agent = world.agents["A001"]
    agent.activity = Activity.IDLE
    agent.needs.fatigue = 0.8
    body = world.model_state.setdefault("body_state_v1", create_body_state(world))
    body["agents"][agent.id]["sleep_debt_minutes"] = 0.0
    policy = create_behavior_policy(
        "sleep-aware-utility-softmax-v2",
    )
    low_debt = decisions_of(world, policy)[agent.id]
    body["agents"][agent.id]["sleep_debt_minutes"] = 480.0
    high_debt = decisions_of(world, policy)[agent.id]

    low_sleep = next(item for item in low_debt.candidates if item.action is ActionKind.SLEEP)
    high_sleep = next(item for item in high_debt.candidates if item.action is ActionKind.SLEEP)
    assert high_sleep.utility - low_sleep.utility == pytest.approx(1.0)
    assert high_sleep.probability > low_sleep.probability


def test_behavior_runner_only_proposes_due_agents_in_stable_id_order() -> None:
    world = make_world()
    for agent in world.agents.values():
        agent.activity = Activity.MOVING
    world.agents["A002"].activity = Activity.IDLE
    world.agents["A010"].activity = Activity.MOVING
    world.agents["A001"].activity = Activity.IDLE
    world.agents["A003"].activity = Activity.IDLE
    world.agents["A003"].next_decision_at = 5

    assert list(decisions_of(world)) == ["A001", "A002"]


def test_support_candidates_are_suppressed_for_open_request_and_cooldown(
    causal_stress_scenario,
) -> None:
    scenario = causal_stress_scenario(agent_count=1)
    world = WorldState.from_scenario(scenario)
    agent = world.agents["A001"]
    agent.activity = Activity.IDLE
    receipt = StaffService(world).request(agent.id)
    agent.activity = Activity.IDLE  # Keep the proposal gate under test while request is open.
    policy = UtilityBehaviorPolicy()

    proposal = decisions_of(world, policy)[agent.id]
    help_candidate = next(
        item for item in proposal.candidates if item.action is ActionKind.SEEK_HELP
    )
    assert not help_candidate.eligible
    assert world.support_requests[receipt.request_id].status.value == "pending"

    agent.support_request_id = None
    agent.support_cooldown_until = 10
    cooldown_proposal = decisions_of(world, policy)[agent.id]
    assert not next(
        item for item in cooldown_proposal.candidates if item.action is ActionKind.SEEK_HELP
    ).eligible

    world.minute_index = 10
    eligible_proposal = decisions_of(world, policy)[agent.id]
    assert next(
        item for item in eligible_proposal.candidates if item.action is ActionKind.SEEK_HELP
    ).eligible


def test_behavior_refuses_a_view_without_its_required_audit_hash() -> None:
    world = WorldState.from_scenario(ScenarioSpec.load(BASELINE_100_7D))
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    # The update phase's view, which has no audit hash.
    view = controller._view_after_movement(world)
    assert view.state_hash is None

    with pytest.raises(KernelRefusal, match="no hash of a state"):
        controller._propose_due(view, CounterRNG(world.seed))


def test_a_candidate_keeps_its_errors() -> None:
    assert ActionCandidate("rest", True, "t", 0.25, 1, 0.5).action is ActionKind.REST
    for arguments in (
        (ActionKind.REST, True, "", 0.25, 1, 0.5),
        (ActionKind.REST, True, "t", float("nan"), 1, 0.5),
        (ActionKind.REST, True, "t", float("inf"), 1, 0.5),
        (ActionKind.REST, True, "t", 0.25, -1, 0.5),
        (ActionKind.REST, True, "t", 0.25, 1, float("nan")),
        (ActionKind.REST, True, "t", 0.25, 1, 1.5),
        (ActionKind.REST, False, "t", 0.25, 1, 0.5),
    ):
        with pytest.raises(ValueError):
            ActionCandidate(*arguments)


def test_a_recorded_outcome_equals_replace_and_keeps_the_timing_check() -> None:
    from dataclasses import replace

    candidate = ActionCandidate(ActionKind.REST, True, "t", 0.25, 0, 1.0)
    proposal = ActionProposal(
        agent_id="A001",
        minute_index=5,
        observed_state_hash="0" * 64,
        action=ActionKind.REST,
        candidates=(candidate,),
        trigger="scheduled_decision",
        choice_value=0.5,
        rng_key=("behavior",),
        selection_probability=1.0,
        policy_version="v",
        next_decision_at=6,
    )
    assert proposal.with_outcome("X", 9) == replace(proposal, target_id="X", next_decision_at=9)
    with pytest.raises(ValueError):
        proposal.with_outcome("X", 5)


def test_a_kept_zone_environment_follows_the_configs_and_modifiers(generated_scenario) -> None:
    """The view kernel keeps a zone's conditions in the controller's Run while the stamps of the
    configs and of the modifiers stay the same (rules/world_view.rs)."""
    from sheltersim.domain.environment import ZONE_ENVIRONMENT_STATE_KEY, zone_environment_view

    world = WorldState.from_scenario(generated_scenario)
    world.model_state.setdefault(ZONE_ENVIRONMENT_STATE_KEY, {"schema_version": 1, "modifiers": {}})
    agent = next(iter(world.agents.values()))
    zone_id = agent.zone_id
    controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
    rules = native.rules("world_view")

    def lighting() -> float:
        return rules.view_after_movement(controller, world).agents[agent.id].lighting_level

    first = lighting()
    assert first == zone_environment_view(world, zone_id).lighting_level
    world.model_state[ZONE_ENVIRONMENT_STATE_KEY]["modifiers"]["m-1"] = {
        "zone_id": zone_id,
        "op": "set_lighting",
        "parameters": {"level": 0.125},
        "start_minute": 0,
        "start_event_id": "event-1",
        "source_id": "ASM-TEST",
        "assumption_ids": ["ASM-TEST"],
    }
    changed = lighting()
    assert changed == zone_environment_view(world, zone_id).lighting_level
    if "zone_environment" in world.model_configs:
        assert changed == 0.125
    del world.model_state[ZONE_ENVIRONMENT_STATE_KEY]["modifiers"]["m-1"]
    assert lighting() == first
    world.model_configs = dict(world.model_configs)
    assert lighting() == first
    del world.model_state[ZONE_ENVIRONMENT_STATE_KEY]
    assert lighting() == zone_environment_view(world, zone_id).lighting_level
