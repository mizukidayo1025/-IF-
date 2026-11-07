"""Deterministic and bounded contact-based stress propagation tests.

The step is native/src/rules/social_propagation.rs: each test runs it on a world as a tick does,
and reads what it wrote and the contact trace it gave the Run's sink. A full Run with the policy
enabled is checked in tests/integration/test_stress_propagation.py.
"""

from __future__ import annotations

import copy
import json
import math
from dataclasses import replace
from types import SimpleNamespace
from typing import Any

import pytest
from support.paths import SCENARIO_100_AGENTS

from sheltersim import native
from sheltersim.core.engine import TraceLevel
from sheltersim.core.rng import CounterRNG
from sheltersim.core.world import WorldState
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.policies.behavior import WorldView
from sheltersim.policies.psychology import AgentStateDelta, SimplePsychologyPolicy
from sheltersim.policies.registry import create_social
from sheltersim.policies.social import (
    SOCIAL_RUNTIME_STATE_KEY,
    SocialPropagationParameters,
    StressPropagationPolicy,
)


def _two_agent_world(count: int = 2) -> WorldState:
    document = json.loads(SCENARIO_100_AGENTS.read_text(encoding="utf-8"))
    document["duration_minutes"] = 20
    document["scheduled_events"] = []
    document["profiles"] = document["profiles"][:count]
    document["model_configs"] = {}
    world = WorldState.from_scenario(ScenarioSpec.model_validate(document))
    ids = sorted(world.agents)
    world.agents[ids[0]].stress = 0.1
    world.agents[ids[1]].stress = 0.9
    return world


def _base_deltas(world: WorldState, view: WorldView, minute: int) -> tuple[AgentStateDelta, ...]:
    """The deltas of `_psychology_update` (native/src/rules/psychology.rs), before this step."""
    if view.minute_index != minute:
        view = replace(view, minute_index=minute)
    controller = SimpleNamespace(
        world=world,
        psychology=SimplePsychologyPolicy(),
        artifacts=SimpleNamespace(trace_level=TraceLevel.OFF),
    )
    return native.rules("psychology").update_psychology(controller, view, minute).deltas


class _Sink:
    """A Run's sink with a full trace: the documents the step gives it."""

    trace_level = TraceLevel.FULL

    def __init__(self) -> None:
        self.documents: list[dict[str, Any]] = []

    def record_social_contact_trace(self, document: dict[str, Any]) -> None:
        self.documents.append(copy.deepcopy(document))


def _propagate(
    world: WorldState,
    policy: Any,
    view: WorldView,
    deltas: tuple[AgentStateDelta, ...],
    rng: Any,
    *,
    zone_neighbors: dict[str, tuple[str, ...]],
) -> SimpleNamespace:
    """`_social_propagation` of a tick: the new deltas, the namespace and the trace document."""
    sink = _Sink()
    controller = SimpleNamespace(
        stress_policy=policy, artifacts=sink, zone_neighbors=dict(zone_neighbors)
    )
    final = native.rules("social_propagation").update_social_propagation(
        controller, world, view, deltas, view.minute_index, rng
    )
    document = sink.documents[-1] if sink.documents else None
    return SimpleNamespace(
        deltas=final,
        runtime_state=json.loads(json.dumps(world.model_state.get(SOCIAL_RUNTIME_STATE_KEY, {}))),
        document=document,
        contacts=[SimpleNamespace(**item) for item in document["contacts"]] if document else [],
    )


def test_disabled_model_preserves_the_rule_baseline_exactly() -> None:
    world = _two_agent_world()
    view = WorldView.from_world(world)
    baseline = _base_deltas(world, view, 0)

    result = _propagate(
        world,
        StressPropagationPolicy(SocialPropagationParameters(enabled=False)),
        view,
        baseline,
        CounterRNG(world.seed),
        zone_neighbors={},
    )

    assert result.deltas is baseline
    assert result.document is None
    assert result.runtime_state == {}


def test_family_contact_propagates_stress_and_records_contribution() -> None:
    world = _two_agent_world()
    view = WorldView.from_world(world)
    policy = StressPropagationPolicy(
        SocialPropagationParameters(
            enabled=True,
            max_contacts_per_agent=1,
            minimum_contact_minutes=1,
            stress_spread_per_minute=0.1,
            natural_recovery_per_minute=0,
            cooldown_minutes=5,
        )
    )
    baseline = _base_deltas(world, view, 0)

    result = _propagate(world, policy, view, baseline, CounterRNG(world.seed), zone_neighbors={})

    trace = result.document
    assert trace["contributing_contact_count"] == 1
    assert trace["total_stress_added"] > 0
    assert any(item.contact_basis == "family" for item in result.contacts)
    assert trace["selected_contact_count"] == len(result.contacts)
    assert trace["contacts"][0]["stress_contribution"] > 0
    assert len(trace["contributions"]) == trace["contributing_contact_count"]
    assert result.runtime_state["schema_version"] == 1


def test_agent_cooldown_suppresses_repeated_influence_until_it_expires() -> None:
    world = _two_agent_world()
    policy = StressPropagationPolicy(
        SocialPropagationParameters(
            enabled=True,
            max_contacts_per_agent=1,
            minimum_contact_minutes=1,
            stress_spread_per_minute=0.1,
            natural_recovery_per_minute=0,
            cooldown_minutes=5,
        )
    )
    first_result = None
    for minute in range(6):
        world.minute_index = minute
        view = WorldView.from_world(world)
        deltas = _base_deltas(world, view, minute)
        result = _propagate(world, policy, view, deltas, CounterRNG(world.seed), zone_neighbors={})
        if minute == 0:
            first_result = result
        elif minute < 5:
            assert result.document["contributing_contact_count"] == 0
        elif minute == 5:
            assert result.document["contributing_contact_count"] == 1
    assert first_result is not None and first_result.document["total_stress_added"] > 0


def test_nearby_zone_contacts_are_selected_without_family_membership() -> None:
    world = _two_agent_world()
    for index, agent_id in enumerate(sorted(world.agents)):
        world.agents[agent_id].zone_id = "sleeping" if index == 0 else "corridor"
    original = WorldView.from_world(world)
    agents = {
        agent_id: replace(agent, group_id=None) for agent_id, agent in original.agents.items()
    }
    view = WorldView(0, agents, original.facilities, original.state_hash)
    base = _base_deltas(world, view, 0)
    policy = StressPropagationPolicy(
        SocialPropagationParameters(enabled=True, max_contacts_per_agent=1)
    )

    result = _propagate(
        world,
        policy,
        view,
        base,
        CounterRNG(world.seed),
        zone_neighbors={"sleeping": ("corridor",), "corridor": ("sleeping",)},
    )

    assert result.contacts
    assert all(item.contact_basis == "nearby_zone" for item in result.contacts)


def _crowd(count: int) -> WorldState:
    """`count` people of one household in one zone, with stresses in steps of 0.01."""
    document = json.loads(SCENARIO_100_AGENTS.read_text(encoding="utf-8"))
    document["duration_minutes"] = 20
    document["scheduled_events"] = []
    template = document["profiles"][0]
    document["profiles"] = [
        {
            **template,
            "id": f"A{index:04d}",
            "rng_key": f"profile:A{index:04d}",
            "group_id": "one-large-group",
        }
        for index in range(count)
    ]
    document["model_configs"] = {}
    for zone in document["zones"]:
        if zone["capacity"] is not None:
            zone["capacity"] = max(zone["capacity"], count)
    world = WorldState.from_scenario(ScenarioSpec.model_validate(document))
    for index, agent_id in enumerate(sorted(world.agents)):
        world.agents[agent_id].stress = (index % 100) / 100
    return world


def test_contact_graph_is_seed_reproducible_and_bounded_for_1000_agents() -> None:
    world = _crowd(1000)
    zone = next(iter(world.agents.values())).zone_id
    assert all(agent.zone_id == zone for agent in world.agents.values())
    view = WorldView.from_world(world)
    deltas = tuple(
        AgentStateDelta(
            agent_id=agent_id,
            minute_index=0,
            toilet_urgency=0.0,
            hunger=0.0,
            fatigue=0.0,
            stress=agent.stress,
            trust=agent.trust,
        )
        for agent_id, agent in sorted(view.agents.items())
    )
    policy = StressPropagationPolicy(
        SocialPropagationParameters(enabled=True, max_contacts_per_agent=3)
    )
    again = copy.deepcopy(world)

    first = _propagate(world, policy, view, deltas, CounterRNG(2026), zone_neighbors={})
    second = _propagate(
        again, policy, WorldView.from_world(again), deltas, CounterRNG(2026), zone_neighbors={}
    )

    assert first.deltas == second.deltas
    assert first.runtime_state == second.runtime_state
    assert first.document == second.document
    assert first.document["selected_contact_count"] <= len(view.agents) * 3
    assert len(first.runtime_state["contacts"]) <= len(view.agents) * 3
    assert len(first.runtime_state["agent_cooldown_until"]) <= len(view.agents)


def test_coefficients_are_bounded_and_provenance_is_explicit() -> None:
    policy = StressPropagationPolicy(
        SocialPropagationParameters(enabled=True, stress_spread_per_minute=0.01)
    )

    provenance = policy.parameters.provenance()

    assert provenance["source_type"] == "assumption"
    assert provenance["source_id"] == "ASM-SOCIAL-PROPAGATION-V1"
    assert provenance["coefficients"]["stress_spread_per_minute"] == 0.01


def test_bosse_operator_is_bidirectional_and_bounded_and_reads_people_in_id_order(
    research_psychology_scenario, stress_delta
) -> None:
    world = WorldState.from_scenario(research_psychology_scenario(social=True))
    ordered = sorted(world.agents)
    world.agents[ordered[0]].stress = 0.1
    world.agents[ordered[1]].stress = 0.9
    other = copy.deepcopy(world)
    view = WorldView.from_world(world)
    policy = create_social(world.model_configs)
    assert policy.parameters.natural_recovery_per_minute == 0.0
    deltas = tuple(stress_delta(agent_id, 0, view.agents[agent_id].stress) for agent_id in ordered)

    # A tick's view has its people in ID order, which the step reads them in. Another order is no
    # view of a Run's: the kernel changes nothing and says so.
    reversed_view = replace(view, agents=dict(reversed(list(view.agents.items()))))
    before = other.canonical_json()
    with pytest.raises(native.Unsupported, match="ID order"):
        _propagate(other, policy, reversed_view, deltas, CounterRNG(world.seed), zone_neighbors={})
    assert other.canonical_json() == before

    first = _propagate(world, policy, view, deltas, CounterRNG(world.seed), zone_neighbors={})
    after = {delta.agent_id: delta.stress for delta in first.deltas}
    assert after[ordered[0]] > view.agents[ordered[0]].stress
    assert after[ordered[1]] < view.agents[ordered[1]].stress
    assert all(0.0 <= value <= 1.0 for value in after.values())
    assert all(abs(item.stress_contribution) <= 0.25 for item in first.contacts)


def test_bosse_operator_has_no_effect_without_a_local_contact_and_caps_each_tick(
    research_psychology_scenario, stress_delta
) -> None:
    world = WorldState.from_scenario(research_psychology_scenario(social=True))
    ordered = sorted(world.agents)
    for index, agent_id in enumerate(ordered):
        world.agents[agent_id].zone_id = sorted(world.zones)[index]
        world.agents[agent_id].stress = 0.2 + 0.6 * index
    original = WorldView.from_world(world)
    view = replace(
        original,
        agents={
            agent_id: replace(agent, group_id=None) for agent_id, agent in original.agents.items()
        },
    )
    policy = create_social(world.model_configs)
    deltas = tuple(stress_delta(agent_id, 0, view.agents[agent_id].stress) for agent_id in ordered)
    no_contact = _propagate(world, policy, view, deltas, CounterRNG(5), zone_neighbors={})
    assert no_contact.contacts == []
    assert no_contact.deltas == deltas

    equal = WorldState.from_scenario(research_psychology_scenario(social=True))
    for agent_id in ordered:
        equal.agents[agent_id].stress = 0.6
    equal_stress_view = WorldView.from_world(equal)
    equal_stress_deltas = tuple(stress_delta(agent_id, 0, 0.6) for agent_id in ordered)
    equal_state = _propagate(
        equal, policy, equal_stress_view, equal_stress_deltas, CounterRNG(5), zone_neighbors={}
    )
    assert all(delta.stress == pytest.approx(0.6) for delta in equal_state.deltas)

    together = WorldState.from_scenario(research_psychology_scenario(social=True))
    ids = sorted(together.agents)
    together.agents[ids[0]].stress = 0.0
    together.agents[ids[1]].stress = 1.0
    contact_view = WorldView.from_world(together)
    contact_deltas = tuple(
        stress_delta(agent_id, 0, contact_view.agents[agent_id].stress) for agent_id in ids
    )
    bounded = _propagate(
        together, policy, contact_view, contact_deltas, CounterRNG(6), zone_neighbors={}
    )
    assert all(
        abs(row.stress - contact_view.agents[row.agent_id].stress) <= 0.05 for row in bounded.deltas
    )


def test_bosse_trace_matches_normalized_weighted_synchrony_formula(
    research_psychology_scenario, stress_delta
) -> None:
    world = WorldState.from_scenario(research_psychology_scenario(social=True))
    ids = sorted(world.agents)
    world.agents[ids[0]].stress = 0.15
    world.agents[ids[1]].stress = 0.85
    view = WorldView.from_world(world)
    policy = create_social(world.model_configs)
    deltas = tuple(stress_delta(agent_id, 0, view.agents[agent_id].stress) for agent_id in ids)
    result = _propagate(world, policy, view, deltas, CounterRNG(19), zone_neighbors={})
    assert result.contacts
    by_recipient: dict[str, list[Any]] = {}
    for item in result.contacts:
        by_recipient.setdefault(item.recipient_id, []).append(item)
    for contacts in by_recipient.values():
        total_weight = math.fsum(item.influence_weight for item in contacts)
        if total_weight == 0.0:
            assert all(item.stress_contribution == 0.0 for item in contacts)
            continue
        gamma = min(1.0, policy.parameters.stress_spread_per_minute * total_weight)
        assert all(item.aggregate_rate == pytest.approx(gamma) for item in contacts)
        for item in contacts:
            raw = gamma * item.influence_weight / total_weight * item.stress_difference
            assert item.pre_pair_cap_contribution == pytest.approx(raw)
            assert item.pair_cap_contribution == pytest.approx(
                max(
                    -policy.parameters.max_pair_stress_increase,
                    min(policy.parameters.max_pair_stress_increase, raw),
                )
            )


def _without_weight(scenario: ScenarioSpec, case: str) -> ScenarioSpec:
    """The scenario with one factor of every contact weight set to 0, each a valid input."""
    document = scenario.model_dump(mode="json")
    social = document["model_configs"]["social_propagation"]
    if case == "stress_sensitivity":
        for profile in document["profiles"]:
            profile["stress_sensitivity"] = 0.0
    elif case == "source_expressiveness":
        social["source_expressiveness_by_agent"] = {
            profile["id"]: 0.0 for profile in document["profiles"]
        }
    else:
        for basis in ("family", "same_zone", "nearby_zone"):
            social[f"{basis}_channel_strength"] = 0.0
    return ScenarioSpec.model_validate(document)


@pytest.mark.parametrize(
    "case", ["stress_sensitivity", "source_expressiveness", "channel_strength"]
)
def test_bosse_contacts_without_weight_shift_nothing_and_draw_as_weighted_ones(
    research_psychology_scenario, stress_delta, case: str
) -> None:
    """#87 (元 #301): a total weight of 0 is no propagation, not a ValueError of the zip.

    Each weight is expressiveness × channel strength × stress_sensitivity, so one factor of
    0 makes every contribution 0. The contacts are recorded as a contact without a share
    is, and the contact draws are the ones of the same contacts with weight.
    """
    weighted_scenario = research_psychology_scenario(social=True)
    results = []
    for scenario in (weighted_scenario, _without_weight(weighted_scenario, case)):
        world = WorldState.from_scenario(scenario)
        ids = sorted(world.agents)
        world.agents[ids[0]].stress = 0.15
        world.agents[ids[1]].stress = 0.85
        view = WorldView.from_world(world)
        deltas = tuple(stress_delta(agent_id, 0, view.agents[agent_id].stress) for agent_id in ids)
        native.rules("social_propagation").start_draw_log()
        try:
            result = _propagate(
                world,
                create_social(world.model_configs),
                view,
                deltas,
                CounterRNG(19),
                zone_neighbors={},
            )
        finally:
            draws = native.rules("social_propagation").take_draw_log()
        results.append((result, deltas, draws))
    (weighted, _, weighted_draws), (unweighted, deltas, draws) = results

    assert weighted.document["contributing_contact_count"] > 0
    assert draws == weighted_draws
    assert unweighted.deltas == deltas
    assert unweighted.document["contributing_contact_count"] == 0
    assert unweighted.document["total_stress_added"] == 0.0
    assert unweighted.runtime_state["agent_cooldown_until"] == {}
    assert [(item.recipient_id, item.source_id) for item in unweighted.contacts] == [
        (item.recipient_id, item.source_id) for item in weighted.contacts
    ]
    for item in unweighted.contacts:
        assert item.stress_contribution == 0.0
        assert item.influence_weight == 0.0
        assert item.aggregate_rate == 0.0
        assert item.pre_pair_cap_contribution == 0.0
        assert item.pair_cap_contribution == 0.0
        assert item.aggregate_scale == 1.0
