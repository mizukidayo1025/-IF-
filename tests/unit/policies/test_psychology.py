"""Psychology policies: sleep debt, mean-reverting distress, latent distress and the
integrated life-state policy, with their traces.

Each model's rules are native/src/rules/psychology.rs: a test runs the steps of a tick on a world
(support/psychology.py) and reads the deltas and the trace document the Run's sink is given. The
sleep debt and the mean reversion were also Python functions (sleep_debt_step and
mean_reverting_step of policies/psychology.py), deleted in lane L10; what they answered here was
recorded once as digests (ANSWERS), which the kernel's values must equal on every OS.

The research social operator is checked in tests/unit/policies/test_social.py, and
full Runs with these policies in tests/integration/test_research_psychology.py.
"""

from __future__ import annotations

import hashlib
import json
import math
from dataclasses import replace
from types import SimpleNamespace
from typing import Any

import pytest
from pydantic import ValidationError
from support.paths import BASELINE_100_7D, SCENARIO_100_AGENTS
from support.psychology import apply_deltas, psychology_step
from support.world import view_of

from sheltersim.core.engine import TraceLevel
from sheltersim.core.kernels import KernelRefusal
from sheltersim.core.world import WorldState
from sheltersim.domain.facilities import ToiletServiceManager
from sheltersim.domain.information import INFORMATION_SPREAD_STATE_KEY
from sheltersim.domain.models import (
    Activity,
    AgentInformationState,
    InformationBeliefStatus,
    InformationSpreadConfig,
    InformationTruthStatus,
    SupportRequestState,
    TypedMessage,
)
from sheltersim.domain.psychological_state import PSYCHOLOGICAL_STATE_KEY
from sheltersim.domain.staff import StaffService
from sheltersim.events.handlers import EventDispatcher, FacilityOutageHandler
from sheltersim.events.models import EventKind, ScheduledEvent
from sheltersim.experiments.runner import OfflineSimulationController
from sheltersim.experiments.scenario import PsychologyModelConfig, ScenarioSpec
from sheltersim.inspection.traces import RunArtifactWriter
from sheltersim.policies.psychology import (
    AgentStateDelta,
    IntegratedDistressLifeStatePolicy,
    LifeStatePsychologyPolicy,
    MeanRevertingDistressPolicy,
    PsychologyParameters,
    SimplePsychologyPolicy,
)
from sheltersim.policies.registry import create_psychology

# What the deleted sleep_debt_step and the mean-reverting model's Python rule answered at 45a9bc9,
# as sha256 of the values' hex, recorded once on Linux.
ANSWERS: dict[str, str] = {
    "mean-reversion-10": ("c6afdf1fc8dd7b7cd390d2cd117bae7b1da1f74cd38d5722bbcb1f8fb0e98c21"),
    "mean-reversion-2000": ("c3ebc44f3d0f20f76edb4207257049e3bc7d37d352da1cfb77773f4b97263536"),
    "mean-reversion-shock": ("4c2266e9e6da26dbb9566ef55cb752ffa5edf800f946919ee6a24ba9d35e3708"),
    "recovery-rates": ("fb632e6e938d51e2e3f1ef731b3d8c53832a121626434cf007bd938943a317f5"),
    "sleep-debt-420": ("c642b2e917a5e2f352ed054dc973567d8eb4fa9475c0e16f003fcd0e907cbba1"),
    "sleep-debt-480": ("12b4f52f2b4a923a51259118bbccb373ed641215cd9a18abc8f2f0e5e34fdeaa"),
    "sleep-debt-600": ("f3dff529ab2272801bcc6504030ff521a74225ae5e2cc1bf2f7868694bef925f"),
}


def _answers(name: str, values: list[float]) -> None:
    digest = hashlib.sha256()
    for value in values:
        digest.update(value.hex().encode() + b"\n")
    assert digest.hexdigest() == ANSWERS[name], name


def _one_person_world() -> WorldState:
    document = json.loads(SCENARIO_100_AGENTS.read_text(encoding="utf-8"))
    document["profiles"] = document["profiles"][:1]
    document["scheduled_events"] = []
    document["model_configs"] = {}
    return WorldState.from_scenario(ScenarioSpec.model_validate(document))


# ---- Sleep debt -------------------------------------------------------------------------------


class _SleepDebt:
    """One person's sleep debt through the life-state model, minute by minute.

    The required sleep and the sleep efficiency are the view's; the activity is the person's.
    """

    def __init__(
        self,
        *,
        required_sleep_minutes: int | None,
        awake_rate: float,
        rest_recovery_rate: float,
        sleep_recovery_rate: float,
        sleep_efficiency: float = 1.0,
    ) -> None:
        self.world = _one_person_world()
        self.agent = next(iter(self.world.agents.values()))
        self.policy = LifeStatePsychologyPolicy(
            PsychologyParameters(),
            body_config={
                "awake_sleep_debt_minutes_per_minute": awake_rate,
                "rest_sleep_debt_recovery_per_minute": rest_recovery_rate,
                "sleep_sleep_debt_recovery_per_minute": sleep_recovery_rate,
            },
        )
        view = view_of(self.world, include_state_hash=False)
        self.viewed = replace(
            view.agents[self.agent.id],
            required_sleep_minutes=required_sleep_minutes,
            rest_efficiency_multiplier=sleep_efficiency,
        )
        self.view = view

    def step(self, debt: float, activity: Activity) -> float:
        self.agent.activity = activity
        viewed = replace(self.viewed, sleep_debt_minutes=debt, activity=activity)
        view = replace(self.view, agents={self.agent.id: viewed})
        step = psychology_step(self.world, self.policy, view=view)
        return step.deltas[0].sleep_debt_minutes


@pytest.mark.parametrize("required_sleep_minutes", [420, 480, 600])
def test_sleep_debt_balances_a_repeating_required_sleep_schedule(
    required_sleep_minutes: int,
) -> None:
    awake_minutes = 1440 - required_sleep_minutes
    awake_rate = required_sleep_minutes / awake_minutes
    debt_of = _SleepDebt(
        required_sleep_minutes=required_sleep_minutes,
        awake_rate=0.5,
        rest_recovery_rate=0.0,
        sleep_recovery_rate=1.0,
    )
    debt = 0.0
    for _ in range(awake_minutes):
        debt = debt_of.step(debt, Activity.IDLE)
    assert debt == pytest.approx(required_sleep_minutes)
    awake = debt
    for _ in range(required_sleep_minutes):
        debt = debt_of.step(debt, Activity.SLEEPING)
    assert debt == pytest.approx(0.0, abs=1e-9)
    assert awake_rate * awake_minutes == pytest.approx(required_sleep_minutes)
    _answers(f"sleep-debt-{required_sleep_minutes}", [awake, debt])


def test_sleep_debt_rest_and_unspecified_requirement_are_explicit_assumptions() -> None:
    debt_of = _SleepDebt(
        required_sleep_minutes=None,
        awake_rate=0.5,
        rest_recovery_rate=0.0,
        sleep_recovery_rate=1.0,
    )
    assert debt_of.step(120.0, Activity.RESTING) == 120.0
    assert debt_of.step(120.0, Activity.IDLE) == 120.5
    # The deleted sleep_debt_step raised on an efficiency that is not a number; no view holds one.
    nan = _SleepDebt(
        required_sleep_minutes=480,
        awake_rate=0.5,
        rest_recovery_rate=0.0,
        sleep_recovery_rate=1.0,
        sleep_efficiency=float("nan"),
    )
    with pytest.raises(KernelRefusal, match="sleep efficiency"):
        OfflineSimulationController._psychology_update(
            SimpleNamespace(
                world=nan.world,
                psychology=nan.policy,
                artifacts=SimpleNamespace(trace_level=TraceLevel.OFF),
            ),
            replace(nan.view, agents={nan.agent.id: nan.viewed}),
            0,
        )


# ---- Mean reversion ---------------------------------------------------------------------------


def _queued_minutes(
    stress: float, *, baseline: float, forcing: float, rate: float, minutes: int
) -> list[float]:
    """One queued person's stress over the minutes of the mean-reverting model, whose only
    forcing is the constant `queued_stress`."""
    world = _one_person_world()
    agent = next(iter(world.agents.values()))
    agent.activity = Activity.QUEUED
    agent.stress = stress
    policy = MeanRevertingDistressPolicy(
        PsychologyParameters(
            toilet_growth=0.0,
            hunger_growth=0.0,
            awake_fatigue_growth=0.0,
            stress_pressure=0.0,
            queued_stress=forcing,
            support_wait_stress=0.0,
            natural_recovery_rate_per_minute=rate,
            baseline_distress_by_agent={agent.id: baseline},
        )
    )
    values = []
    for _ in range(minutes):
        apply_deltas(world, psychology_step(world, policy).deltas)
        values.append(agent.stress)
    return values


def _exact(value: float, baseline: float, forcing: float, rate: float, minutes: float) -> float:
    """The closed form of dS/dt = rate (baseline - S) + forcing after `minutes`."""
    decay = math.exp(-rate * minutes)
    return baseline + (value - baseline) * decay + forcing * (1.0 - decay) / rate


def test_exact_mean_reversion_matches_the_closed_form_and_has_a_bounded_equilibrium() -> None:
    minutes = _queued_minutes(0.2, baseline=0.1, forcing=0.005, rate=0.05, minutes=10)
    assert minutes[-1] == pytest.approx(_exact(0.2, 0.1, 0.005, 0.05, 10.0), abs=1e-12)
    _answers("mean-reversion-10", minutes)

    sustained = _queued_minutes(0.1, baseline=0.1, forcing=0.005, rate=0.05, minutes=2000)
    assert sustained[-1] == pytest.approx(0.2, abs=1e-9)
    assert all(0.0 <= value <= 1.0 for value in sustained)
    _answers("mean-reversion-2000", sustained[-1:])

    shock = _queued_minutes(0.8, baseline=0.0, forcing=0.0, rate=0.01, minutes=300)
    assert shock[-1] < 0.05
    _answers("mean-reversion-shock", shock[-1:])


def test_recovery_rate_changes_decay_speed_and_nonfinite_rates_are_rejected() -> None:
    slow = _queued_minutes(0.9, baseline=0.0, forcing=0.0, rate=0.005, minutes=60)[-1]
    fast = _queued_minutes(0.9, baseline=0.0, forcing=0.0, rate=0.05, minutes=60)[-1]
    assert fast < slow
    _answers("recovery-rates", [slow, fast])
    with pytest.raises(ValueError, match="natural_recovery_rate_per_minute"):
        PsychologyParameters(natural_recovery_rate_per_minute=math.inf)


def test_acute_distress_is_a_compatibility_alias_and_recovers_at_restless_ticks(
    research_psychology_scenario,
) -> None:
    world = WorldState.from_scenario(research_psychology_scenario())
    agent = world.agents["A001"]
    agent.stress = 0.8
    assert agent.acute_distress == agent.stress
    agent.acute_distress = 0.6
    assert agent.stress == 0.6

    step = psychology_step(
        world, create_psychology(world.model_configs), trace_level=TraceLevel.FULL
    )
    assert step.delta(agent.id).stress == pytest.approx(0.6 * math.exp(-0.02))
    assert step.document is not None
    assert step.document["policy_version"] == "mean-reverting-distress-v1"


def test_personal_recovery_rates_allow_individual_decay_speeds() -> None:
    document = json.loads(SCENARIO_100_AGENTS.read_text(encoding="utf-8"))
    document["profiles"] = document["profiles"][:2]
    for profile in document["profiles"]:
        profile["initial_needs"] = {"toilet_urgency": 0.0, "hunger": 0.0, "fatigue": 0.0}
    document["scheduled_events"] = []
    document["model_configs"]["psychology"] = {
        "natural_recovery_rate_per_minute": 0.01,
        "natural_recovery_rate_by_agent": {"A001": 0.005, "A002": 0.05},
    }
    document["model_configs"]["policy_registry"] = {
        "schema_version": 1,
        "psychology": "mean-reverting-distress-v1",
    }
    world = WorldState.from_scenario(ScenarioSpec.model_validate(document))
    for agent in world.agents.values():
        agent.stress = 0.8
    step = psychology_step(world, create_psychology(world.model_configs))
    by_id = {delta.agent_id: delta.stress for delta in step.deltas}
    assert by_id["A002"] < by_id["A001"]
    assert by_id["A001"] == pytest.approx(0.8 * math.exp(-0.005))
    assert by_id["A002"] == pytest.approx(0.8 * math.exp(-0.05))


def test_daily_latent_distress_accumulates_final_acute_values_and_is_checkpointable(
    research_psychology_scenario, stress_delta
) -> None:
    scenario = research_psychology_scenario(duration=1440)
    world = WorldState.from_scenario(scenario)
    policy = create_psychology(world.model_configs)
    assert isinstance(policy, MeanRevertingDistressPolicy)
    controller = SimpleNamespace(psychology=policy)
    for minute in range(1440):
        OfflineSimulationController._update_long_term_psychology(
            controller,
            world,
            None,
            (stress_delta("A001", minute, 0.8), stress_delta("A002", minute, 0.2)),
            minute,
        )
    state = world.model_state[PSYCHOLOGICAL_STATE_KEY]
    assert state["agents"]["A001"]["completed_days"] == 1
    assert state["agents"]["A001"]["long_term_distress"] == pytest.approx(0.084)
    world.minute_index = 1440
    world.validate()
    assert WorldState.from_dict(world.to_dict()).model_state == world.model_state


def test_integrated_life_state_policy_uses_one_exact_recovery_path_and_trace() -> None:
    document = json.loads(SCENARIO_100_AGENTS.read_text(encoding="utf-8"))
    document["id"] = "integrated-life-state-research-test"
    document["duration_minutes"] = 5
    document["profiles"] = document["profiles"][:2]
    document["scheduled_events"] = []
    document["model_configs"]["psychology"] = {
        "natural_recovery_rate_per_minute": 0.02,
        "sleeping_stress_recovery": 0.2,
        "baseline_distress_by_agent": {},
        "chronic_retention": 0.9,
        "chronic_acute_weight": 0.08,
        "chronic_high_distress_weight": 0.02,
        "high_distress_threshold": 0.7,
    }
    document["model_configs"]["policy_registry"] = {
        "schema_version": 1,
        "psychology": "mean-reverting-life-state-v1",
    }
    world = WorldState.from_scenario(ScenarioSpec.model_validate(document))
    agent = world.agents["A001"]
    agent.stress = 0.8
    agent.activity = Activity.SLEEPING
    agent.support_request_id = "support-wait-001"
    world.support_requests[agent.support_request_id] = SupportRequestState(
        id=agent.support_request_id,
        agent_id=agent.id,
        requested_minute=0,
        timeout_minute=20,
    )
    world.model_state["body_state_v1"]["agents"][agent.id]["sleep_debt_minutes"] = 300.0

    policy = create_psychology(world.model_configs)
    assert isinstance(policy, IntegratedDistressLifeStatePolicy)
    step = psychology_step(world, policy, trace_level=TraceLevel.FULL)
    delta = step.delta(agent.id)
    stress = step.person(agent.id)["stress"]
    components = step.contribution(agent.id)["component_contributions"]
    # No direct rest recovery: sleeping only reverts toward the baseline.
    assert "recovery" not in components
    assert components["natural_recovery"] == pytest.approx((0.0 - 0.8) * (1.0 - math.exp(-0.02)))
    support_multiplier = (1.0 - math.exp(-0.02)) / 0.02
    assert components["staff_support_wait"] == pytest.approx(
        policy.parameters.support_wait_stress * support_multiplier
    )
    assert policy.parameters.support_wait_stress > 0.0
    assert stress["raw_delta"] == pytest.approx(sum(components.values()))
    assert stress["candidate_before_clip"] == pytest.approx(stress["before"] + stress["raw_delta"])
    assert stress["psychology_clipped_delta"] == pytest.approx(delta.stress - agent.stress)
    assert delta.sleep_debt_minutes < 300.0
    assert delta.stress == pytest.approx(agent.stress + sum(components.values()))


def test_false_claim_pressure_reaches_uncertainty_stress_and_trace() -> None:
    document = json.loads(SCENARIO_100_AGENTS.read_text(encoding="utf-8"))
    document["id"] = "integrated-rumor-psychology-test"
    document["duration_minutes"] = 5
    document["profiles"] = document["profiles"][:2]
    document["scheduled_events"] = []
    document["model_configs"]["psychology"] = {
        "natural_recovery_rate_per_minute": 0.01,
        "baseline_distress_by_agent": {},
        "chronic_retention": 0.9,
        "chronic_acute_weight": 0.08,
        "chronic_high_distress_weight": 0.02,
        "high_distress_threshold": 0.7,
    }
    document["model_configs"]["information_spread"] = InformationSpreadConfig(
        enabled=True
    ).to_dict()
    document["model_configs"]["policy_registry"] = {
        "schema_version": 1,
        "psychology": "mean-reverting-life-state-v1",
        "information": "evidence-aware-rumor-v1",
    }
    world = WorldState.from_scenario(ScenarioSpec.model_validate(document))
    runtime = world.model_state[INFORMATION_SPREAD_STATE_KEY]
    message = TypedMessage(
        id="false-water-claim",
        truth_status=InformationTruthStatus.FALSE,
        source_id="social-source",
        zone_id=world.agents["A001"].zone_id,
        time=0,
        origin_event_id="rumor-event",
        origin_agent_id="A001",
    )
    runtime["messages"][message.id] = message.to_dict()
    runtime["beliefs"]["A001"][message.id] = AgentInformationState(
        message_id=message.id,
        belief=InformationBeliefStatus.HEARD,
        source="A001",
        last_contact=0,
        last_event_id="rumor-event",
        belief_strength=1.0,
    ).to_dict()
    world.model_state["body_state_v1"]["agents"]["A001"]["uncertainty"] = 0.2
    policy = create_psychology(world.model_configs)
    with_rumor = psychology_step(world, policy, trace_level=TraceLevel.FULL)
    rumor_agent = with_rumor.delta("A001")
    rumor_trace = with_rumor.person("A001")

    runtime["beliefs"]["A001"][message.id] = AgentInformationState(
        message_id=message.id,
        belief=InformationBeliefStatus.REJECTED,
        source="trusted-staff",
        last_contact=1,
        last_event_id="correction-event",
        belief_strength=0.0,
        correction_exposed=True,
        correction_accepted=True,
        ever_misbelieved=True,
    ).to_dict()
    corrected = psychology_step(world, policy, trace_level=TraceLevel.FULL)
    corrected_agent = corrected.delta("A001")
    corrected_trace = corrected.person("A001")

    effect = rumor_trace["information_belief_effect"]
    assert effect["active_false_claim_pressure"] == 1.0
    assert rumor_trace["stress"]["components"]["information_belief_via_uncertainty"] > 0.0
    assert effect["source_records"] == [
        {
            "message_id": message.id,
            "origin_event_id": "rumor-event",
            "belief_event_id": "rumor-event",
            "source_id": message.source_id,
            "origin_minute": 0,
        }
    ]
    assert rumor_agent.uncertainty > corrected_agent.uncertainty
    assert rumor_agent.stress > corrected_agent.stress
    assert corrected_trace["life_state"]["information_belief_pressure"]["after"] == 0.0


def test_psychology_trace_delta_fields_reconstruct_applied_stress_delta(
    research_psychology_scenario,
) -> None:
    world = WorldState.from_scenario(research_psychology_scenario())
    agent = world.agents["A001"]
    agent.stress = 0.8
    policy = create_psychology(world.model_configs)
    step = psychology_step(world, policy, trace_level=TraceLevel.FULL)
    serialized = step.person(agent.id)["stress"]
    assert sum(serialized["components"].values()) == pytest.approx(serialized["raw_delta"])
    assert (
        serialized["raw_delta"] + serialized["clip_delta"] + serialized["social_delta"]
    ) == pytest.approx(serialized["applied_delta"])


def test_psychology_update_is_pure_bounded_and_keeps_trust_until_outcome_exists() -> None:
    world = WorldState.from_scenario(ScenarioSpec.load(SCENARIO_100_AGENTS))
    agent = world.agents["A001"]
    agent.activity = Activity.RESTING
    agent.needs.toilet_urgency = 0.9995
    agent.needs.hunger = 0.3
    agent.needs.fatigue = 0.001
    agent.stress = 0.999
    agent.trust = 0.2
    state_before = world.canonical_json()
    policy = SimplePsychologyPolicy()

    deltas = psychology_step(world, policy).deltas
    first_delta = next(delta for delta in deltas if delta.agent_id == "A001")

    assert world.canonical_json() == state_before
    assert deltas == psychology_step(world, policy).deltas
    assert 0.0 <= first_delta.toilet_urgency <= 1.0
    assert 0.0 <= first_delta.hunger <= 1.0
    assert 0.0 <= first_delta.fatigue <= 1.0
    assert 0.0 <= first_delta.stress <= 1.0
    assert first_delta.fatigue < agent.needs.fatigue
    assert first_delta.trust == agent.trust

    apply_deltas(world, deltas)
    world.validate()
    assert world.agents["A001"].needs.fatigue < 0.001
    assert world.agents["A001"].trust == 0.2


def _delta(agent_id: str, minute: int, **values: Any) -> AgentStateDelta:
    return AgentStateDelta(
        agent_id=agent_id,
        minute_index=minute,
        toilet_urgency=values.get("toilet_urgency", 0.1),
        hunger=values.get("hunger", 0.2),
        fatigue=values.get("fatigue", 0.3),
        stress=values.get("stress", 0.4),
        trust=values.get("trust", 0.5),
        hydration_need=values.get("hydration_need", 0.0),
    )


def test_psychology_application_refuses_stale_or_duplicate_updates_atomically() -> None:
    world = WorldState.from_scenario(ScenarioSpec.load(SCENARIO_100_AGENTS))
    update = _delta("A001", 0)
    stale_update = _delta("A002", 1)
    initial = world.canonical_json()

    with pytest.raises(KernelRefusal, match="not of this minute"):
        OfflineSimulationController._apply_state_deltas(None, world, (update, stale_update))
    assert world.canonical_json() == initial

    with pytest.raises(KernelRefusal, match="two deltas"):
        OfflineSimulationController._apply_state_deltas(None, world, (update, update))
    assert world.canonical_json() == initial


def test_psychology_trace_separates_need_queue_and_staff_causes(causal_stress_scenario) -> None:
    scenario = causal_stress_scenario()
    world = WorldState.from_scenario(scenario)
    outage = ScheduledEvent(
        event_id="toilet-outage",
        kind=EventKind.EXOGENOUS,
        due_minute=0,
        priority=0,
        sequence=world.allocate_event_sequence(),
        event_type="facility_outage",
        payload={"facility_id": "toilet-1", "units_out": 1},
        source_id="test-scenario",
    )
    world.schedule_event(outage)
    EventDispatcher({"facility_outage": FacilityOutageHandler()}).dispatch_due(world, 0)

    world.agents["A001"].activity = Activity.IDLE
    ToiletServiceManager(world).request("A001", "toilet-1")
    support = StaffService(world, task_duration_minutes=5).request("A002")
    StaffService(world, task_duration_minutes=5).assign_pending(0)
    world.minute_index = 5

    step = psychology_step(
        world,
        SimplePsychologyPolicy(
            PsychologyParameters(
                stress_pressure=0.1,
                queued_stress=0.01,
                support_wait_stress=0.02,
            )
        ),
        trace_level=TraceLevel.FULL,
    )
    assert step.document is not None
    assert step.document["assumption_id"] == "ASM-H04-CAUSAL-STRESS-V1"
    queue_trace = step.person("A001")
    support_trace = step.person("A002")

    assert queue_trace["stress"]["components"]["unmet_need_pressure"] > 0.0
    assert queue_trace["stress"]["components"]["facility_queue_wait"] == pytest.approx(0.01)
    queue_cause = next(
        item for item in queue_trace["direct_causes"] if item["kind"] == "facility_queue_wait"
    )
    assert queue_cause["wait_minutes"] == 5
    assert queue_cause["source_id"] == "ASM-TOILET-SERVICE-DURATION-V1"
    assert queue_cause["facility_capacity"] == 1
    assert queue_cause["facility_working_units"] == 0
    assert "toilet-outage" in queue_cause["capacity_event_context_ids"]
    assert support_trace["stress"]["components"]["staff_support_wait"] == pytest.approx(0.02)
    support_cause = next(
        item for item in support_trace["direct_causes"] if item["kind"] == "staff_support_wait"
    )
    assert support_cause["support_request_id"] == support.request_id
    assert support_cause["assigned_staff_id"] == "staff-1"
    assert support_trace["source_references"]["support_source_id"] == (
        "ASM-STAFF-SUPPORT-LIFECYCLE-V1"
    )

    capacity_before = world.facilities["toilet-1"].working_units
    apply_deltas(world, step.deltas)
    assert world.facilities["toilet-1"].working_units == capacity_before == 0


def test_scenario_psychology_coefficients_are_validated_and_source_tagged(
    tmp_path, causal_stress_scenario
) -> None:
    scenario = causal_stress_scenario(stress_pressure=0.2)
    config = PsychologyModelConfig.model_validate(scenario.model_configs["psychology"])
    assert config.source_id == "ASM-H04-CAUSAL-STRESS-V1"
    assert config.policy_version == "causal-stress-v1"
    assert config.stress_pressure == pytest.approx(0.2)

    world = WorldState.from_scenario(scenario)
    writer = RunArtifactWriter(scenario, tmp_path / "run", trace_level=TraceLevel.OFF)
    writer.write_initial(world)
    manifest = json.loads((tmp_path / "run" / "manifest.json").read_text(encoding="utf-8"))
    assert manifest["model_assumptions"]["psychology"]["source_id"] == config.source_id
    assert manifest["model_assumptions"]["psychology"]["values"]["stress_pressure"] == 0.2

    low_sensitivity = psychology_step(
        world,
        SimplePsychologyPolicy(PsychologyParameters(stress_pressure=0.01, support_wait_stress=0.0)),
    ).deltas[0]
    high_sensitivity = psychology_step(
        world,
        SimplePsychologyPolicy(PsychologyParameters(stress_pressure=0.2, support_wait_stress=0.0)),
    ).deltas[0]
    assert high_sensitivity.stress > low_sensitivity.stress

    with pytest.raises(ValidationError):
        causal_stress_scenario(stress_pressure=0.251)


def test_psychology_view_skips_audit_hash_without_changing_inputs() -> None:
    world = WorldState.from_scenario(ScenarioSpec.load(BASELINE_100_7D))
    before = world.canonical_json()
    hashed = view_of(world)
    psychology_view = view_of(world, include_state_hash=False)

    assert hashed.state_hash is not None
    assert psychology_view.state_hash is None
    assert psychology_view.agents == hashed.agents
    assert psychology_view.facilities == hashed.facilities
    policy = SimplePsychologyPolicy()
    assert (
        psychology_step(world, policy, view=hashed).deltas
        == psychology_step(world, policy, view=psychology_view).deltas
    )
    assert world.canonical_json() == before


@pytest.mark.parametrize(
    ("field", "value"),
    [
        ("hunger", float("nan")),
        ("hunger", 1.5),
        ("stress", -0.5),
        ("trust", float("inf")),
        ("fatigue", True),
        ("hydration_need", "0.5"),
        # The deleted rule accepted an int within bounds; the kernels' deltas hold floats.
        ("hunger", 1),
    ],
)
def test_applying_a_delta_refuses_what_is_not_a_float_within_bounds(
    generated_scenario, field, value
) -> None:
    world = WorldState.from_scenario(generated_scenario)
    delta = _delta(next(iter(world.agents)), world.minute_index, hydration_need=0.1)
    object.__setattr__(delta, field, value)
    before = world.canonical_json()
    with pytest.raises(KernelRefusal):
        OfflineSimulationController._apply_state_deltas(None, world, (delta,))
    assert world.canonical_json() == before
