"""The worlds and inputs of the core's tests of the people models' steps (Issue #225, lane M10a).

The social model (`update_social_propagation`), the information model's contacts
(`sample_contacts`, `planned_contacts`) and each person's minute (`update_psychology`, with the
trace's document of `record_psychology_trace`) are kernels of `native/core/src/rules/`. Their
pytest files (`integration/test_social_propagation_row.py`, `test_information_row.py`,
`test_psychology_update.py`, `test_trace_documents.py`) held what the deleted Python rules answered
as digests; the tests that called a kernel on a world they built moved to
`native/core/tests/social_steps.rs`, `information_steps.rs` and `psychology_steps.rs`, which write
the same texts and keep those digests. This module builds the same worlds and inputs and writes
them to `native/core/tests/data/model_steps.json`:

- `worlds`: the Project screen's generated Scenario at minute 0 (`plain`), and the worlds the
  pytest files edited from it, as variants (`core_texts.changes`): the social model's world
  (`_small`), with every stress sensitivity 0 (`insensitive`), the information model's claims
  (`claims`), the psychology tests' shuffled worlds (`psychology-<seed>`) and the trace tests'
  (`trace-<seed>`).
- `policies`: what the Run reads of each policy object a test made (`RunCore.policies_document`'s
  entry of the controller's attribute), the same way: `stress_policy`, `information_policy` or
  `psychology`. A policy is a variant of the one before it that needs the fewest changes.
- `social`, `information`, `psychology`, `trace`: each test's cases, with the values it drew from
  `random.Random` (each person's stress, the deltas' stress, the final stress of the trace).

The digests stay in the Rust tests, as they stood in the pytest files.
"""

from __future__ import annotations

import json
import random
import tempfile
from dataclasses import replace
from typing import Any

from support.core_texts import Fixture, variant

# The social models of the pytest file, and their source IDs.
SOCIAL_POLICIES = {
    "sparse": ("StressPropagationPolicy", "ASM-SOCIAL-PROPAGATION-V1"),
    "channel": ("ExpressivenessChannelOpennessPolicy", "ASM-BOSSE-AFFECT-CONTAGION-V1"),
    "degroot": ("DeGrootContactPolicy", "ASM-DEGROOT-CONTACT-V1"),
}
SOCIAL_KEY = "social_propagation_v1"
# The coefficients that `test_int_coefficients_of_a_scenario_give_the_python_rule_s_answers`
# wrote as ints.
SOCIAL_INTS = (
    {"max_pair_stress_increase": 0},
    {"max_agent_stress_increase_per_minute": 0},
    {"stress_spread_per_minute": 0, "natural_recovery_per_minute": 0},
    {"family_channel_strength": 1, "same_zone_channel_strength": 0},
    {"nearby_zone_channel_strength": 1, "max_pair_stress_increase": 0},
)
# The seeds of the psychology tests' shuffled worlds: `models` 0..7 (`one-percent` is 1, `ints`
# 0..2), and of the trace tests' (`models` 0..5).
PSYCHOLOGY_SEEDS = range(8)
TRACE_SEEDS = range(6)
# The seeds of the information tests past 64 bits.
BIG_SEEDS = (2**64 + 7, 2**63)


class Policies:
    """What the Run reads of each policy a test made, by name."""

    def __init__(self, scenario: Any) -> None:
        from sheltersim.core.world import WorldState
        from sheltersim.experiments.metrics import MetricsRecorder
        from sheltersim.experiments.runner import OfflineSimulationController
        from sheltersim.experiments.timeline import _MetricSink

        world = WorldState.from_scenario(scenario)
        self.controller = OfflineSimulationController(world, _MetricSink(MetricsRecorder()))
        self.documents: dict[str, Any] = {}
        self.whole: dict[str, Any] = {}

    def add(self, name: str, attribute: str, policy: Any) -> str:
        """The name of the policy's entry: a new one, or the one that is the same."""
        setattr(self.controller, attribute, policy)
        self.controller._run_core.refresh_policies(self.controller)
        document = self.controller._run_core.policies_document()["controller"]["attrs"][attribute]
        document = json.loads(json.dumps(document))
        for known, other in self.whole.items():
            if other == document:
                return known
        assert name not in self.whole, name
        if self.whole:
            variants = (
                variant(base, other, document, dotted=True) for base, other in self.whole.items()
            )
            self.documents[name] = min(variants, key=lambda variant: len(json.dumps(variant)))
        else:
            self.documents[name] = document
        self.whole[name] = document
        return name


def model_steps_fixture() -> dict[str, Any]:
    from sheltersim.core.world import WorldState
    from support.core_fixtures_queues import generated

    scenario = generated()
    fixture = Fixture(dotted=True)
    policies = Policies(scenario)
    fixture.world("plain", WorldState.from_scenario(scenario), None, full=True)
    return {
        "worlds": fixture.worlds,
        "social": _social(scenario, fixture, policies),
        "information": _information(scenario, fixture, policies),
        "psychology": _psychology(scenario, fixture, policies),
        "trace": _trace(scenario, fixture, policies),
        "policies": policies.documents,
    }


# ---- The social model ----------------------------------------------------------------------------


def _social_world(scenario: Any) -> Any:
    """`_small` of the pytest file: a new row, and each person's stress (index % 10) / 10."""
    from sheltersim.core.world import WorldState

    world = WorldState.from_scenario(scenario)
    world.model_state[SOCIAL_KEY] = {
        "schema_version": 1,
        "contacts": {},
        "agent_cooldown_until": {},
    }
    for index, agent in enumerate(world.agents.values()):
        agent.stress = (index % 10) / 10.0
    return world


def _social_policy(policies: Policies, name: str, policy: str, **parameters: Any) -> str:
    from sheltersim.policies import alternatives, social

    kind_name, source = SOCIAL_POLICIES[policy]
    kind = getattr(alternatives if policy == "degroot" else social, kind_name)
    settings = social.SocialPropagationParameters(
        enabled=True, stress_spread_per_minute=0.2, minimum_contact_minutes=1, source_id=source
    )
    return policies.add(name, "stress_policy", kind(replace(settings, **parameters)))


def _minutes(world: Any, draws: random.Random, count: int) -> list[Any]:
    """Each minute's stresses as the pytest file drew them: each person's (in the order of
    `world.agents`, which `social.order` holds), then each delta's (by ID)."""
    minutes = []
    for _ in range(count):
        stresses = [draws.choice((0.0, 1.0, draws.random())) for _ in world.agents]
        bases = [draws.choice((0.0, 1.0, draws.random())) for _ in sorted(world.agents)]
        minutes.append({"stresses": stresses, "bases": bases})
    return minutes


def _social(scenario: Any, fixture: Fixture, policies: Policies) -> dict[str, Any]:
    world = _social_world(scenario)
    fixture.world("social", world, None)
    cases: dict[str, Any] = {
        "order": list(world.agents),
        "step": {},
        "trace": [],
        "trace_minutes": {},
        "no_weight": [],
    }
    for policy in SOCIAL_POLICIES:
        cases["step"][policy] = _social_policy(
            policies, f"social-step-{policy}", policy, cooldown_minutes=2
        )
    # `test_the_document_of_a_trace_is_the_python_rule_s`.
    for policy in SOCIAL_POLICIES:
        for seed in range(4):
            draws = random.Random(seed)
            parameters = {
                "stress_spread_per_minute": draws.choice((0.01, 0.2, 2.0)),
                "max_pair_stress_increase": draws.choice((0.001, 0.05, 0.25)),
                "max_agent_stress_increase_per_minute": draws.choice((0.002, 0.05, 0.25)),
                "minimum_contact_minutes": draws.choice((1, 2)),
                "cooldown_minutes": draws.choice((1, 2, 30)),
                "natural_recovery_per_minute": draws.choice((0.0, 0.01)),
                "max_contacts_per_agent": draws.choice((1, 3, 16)),
            }
            name = f"trace-{policy}-{seed}"
            policy_name = _social_policy(policies, f"social-{name}", policy, **parameters)
            cases["trace"].append({"name": name, "seed": seed, "policy": policy_name})
            # The same for each policy: the draws before them are the same.
            cases["trace_minutes"][str(seed)] = _minutes(world, draws, 5)
    channel = _social_policy(policies, "social-channel", "channel")
    cases["restored"] = cases["group"] = channel
    cases["unsorted"] = {
        "channel": channel,
        "degroot": _social_policy(policies, "social-degroot", "degroot"),
    }
    # `test_contacts_without_weight_shift_nothing`.
    insensitive = scenario.model_dump(mode="json")
    for profile in insensitive["profiles"]:
        profile["stress_sensitivity"] = 0.0
    fixture.world("insensitive", _social_world(type(scenario).model_validate(insensitive)), None)
    ids = sorted(world.agents)
    weightless = {
        "stress_sensitivity": ("insensitive", {}),
        "source_expressiveness": (
            "social",
            {"source_expressiveness_by_agent": {agent_id: 0.0 for agent_id in ids}},
        ),
        "channel_strength": (
            "social",
            {f"{basis}_channel_strength": 0.0 for basis in ("family", "same_zone", "nearby_zone")},
        ),
    }
    for case, (world_name, parameters) in weightless.items():
        name = f"no-weight-{case}"
        cases["no_weight"].append(
            {
                "name": name,
                "world": world_name,
                "policy": _social_policy(policies, f"social-{name}", "channel", **parameters),
            }
        )
    # `test_int_coefficients_of_a_scenario_give_the_python_rule_s_answers`: each case draws its
    # minutes from `random.Random(7)`, so they are the same.
    cases["ints"] = []
    cases["ints_minutes"] = _minutes(world, random.Random(7), 4)
    for ints in SOCIAL_INTS:
        for policy in SOCIAL_POLICIES:
            name = f"ints-{policy}-{sorted(ints.items())}"
            parameters: dict[str, Any] = {"cooldown_minutes": 2, **ints}
            if policy == "channel":
                parameters["source_expressiveness_by_agent"] = {ids[0]: 0}
            cases["ints"].append(
                {
                    "name": name,
                    "policy": _social_policy(policies, f"social-{name}", policy, **parameters),
                }
            )
    return cases


# ---- The information model -----------------------------------------------------------------------


def _information(scenario: Any, fixture: Fixture, policies: Policies) -> dict[str, Any]:
    from sheltersim.core.world import WorldState
    from sheltersim.domain.information import create_information_runtime_state
    from sheltersim.domain.models import InformationSpreadConfig
    from sheltersim.policies.alternatives import HazardInformationPolicy
    from sheltersim.policies.information import (
        EvidenceAwareInformationPolicy,
        InformationSpreadPolicy,
    )

    # `_world_with_claims(scenario, believers=5)` of the pytest file.
    world = WorldState.from_scenario(scenario)
    state = create_information_runtime_state(set(world.agents))
    agents = sorted(world.agents)
    zone = world.agents[agents[0]].zone_id
    for message, time in (("m1", 0), ("m2", 1)):
        state["messages"][message] = {
            "id": message,
            "truth_status": "false",
            "source_id": f"source-{message}",
            "zone_id": zone,
            "time": time,
            "origin_event_id": f"event-{message}",
            "origin_agent_id": agents[0],
        }
    for agent_id in agents[:5]:
        state["beliefs"][agent_id]["m1"] = {
            "message_id": "m1",
            "belief": "heard",
            "source": "SRC",
            "last_contact": 0,
            "last_event_id": "event-m1",
            "exposed": True,
            "belief_strength": 0.5,
            "share_propensity": 0.5,
            "correction_exposed": False,
            "correction_accepted": False,
            "ever_misbelieved": False,
            "ever_forgotten": False,
            "last_correction_accepted": False,
        }
    world.model_state["information_spread_v1"] = state
    fixture.world("claims", world, None)
    config = InformationSpreadConfig(
        enabled=True,
        minimum_contact_minutes=1,
        contact_duration_scale_minutes=1,
        base_transmission_probability=0.5,
        forgetting_rate_per_minute=0.2,
    )
    models = (InformationSpreadPolicy, HazardInformationPolicy, EvidenceAwareInformationPolicy)
    return {
        "models": [
            [
                model.__name__,
                policies.add(f"information-{model.__name__}", "information_policy", model(config)),
            ]
            for model in models
        ],
        "big_seeds": [str(seed) for seed in BIG_SEEDS],
    }


# ---- Each person's minute ------------------------------------------------------------------------


def _shuffled_world(scenario: Any, draws: random.Random) -> Any:
    """`_shuffled_world` of `test_psychology_update.py`."""
    from sheltersim.core.world import WorldState
    from sheltersim.domain.body_state import create_body_state
    from sheltersim.domain.models import Activity, SupportRequestState, SupportRequestStatus

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
    list(world.agents.values())[7].support_request_id = "nothing"
    return world


def _formulas() -> tuple[Any, Any]:
    from sheltersim.policies.registry import BodyDynamicsFormula

    return (
        BodyDynamicsFormula("body-feedback-complete-v1", "S"),
        BodyDynamicsFormula("body-feedback-sleep-debt-v1", "S"),
    )


def _drawn_policies(draws: random.Random, ids: list[str]) -> list[Any]:
    """`_draw_policies` of `test_psychology_update.py`."""
    from sheltersim.policies.alternatives import BoundedStressPolicy
    from sheltersim.policies.psychology import (
        IntegratedDistressLifeStatePolicy,
        LifeStatePsychologyPolicy,
        MeanRevertingDistressPolicy,
        PsychologyParameters,
        SimplePsychologyPolicy,
    )

    full, sleep_only = _formulas()
    body = {
        "uncertainty_drift_per_minute": draws.random() * 0.01,
        "discontent_stress_rate": draws.random() * 0.01,
        "sleep_debt_stress_rate_by_agent": {ids[0]: draws.random() * 0.01},
        "negative_affect_stress_rate_by_agent": {ids[0]: draws.random() * 0.01},
        "discontent_stress_rate_by_agent": {ids[0]: draws.random() * 0.01},
    }

    def rate() -> float:
        return draws.choice((0.0, draws.random() * 0.25))

    parameters = PsychologyParameters(
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
    return [
        SimplePsychologyPolicy(parameters),
        SimplePsychologyPolicy(parameters, include_direct_rest_stress_recovery=False),
        BoundedStressPolicy(parameters),
        MeanRevertingDistressPolicy(parameters),
        LifeStatePsychologyPolicy(parameters, body_config=body),
        LifeStatePsychologyPolicy(parameters, body_config=body, body_formula=full),
        LifeStatePsychologyPolicy(parameters, body_config=body, body_formula=sleep_only),
        LifeStatePsychologyPolicy(
            parameters,
            body_config=body,
            body_formula=full,
            include_direct_rest_stress_recovery=False,
            use_tick_start_stress_for_feedback=True,
        ),
        IntegratedDistressLifeStatePolicy(parameters, body_config=body),
        IntegratedDistressLifeStatePolicy(parameters, body_config=body, body_formula=full),
        IntegratedDistressLifeStatePolicy(parameters, body_config=body, body_formula=sleep_only),
    ]


def _int_policies(ids: list[str]) -> list[Any]:
    """`_int_policies` of `test_psychology_update.py`."""
    from sheltersim.policies.alternatives import BoundedStressPolicy
    from sheltersim.policies.psychology import (
        IntegratedDistressLifeStatePolicy,
        LifeStatePsychologyPolicy,
        MeanRevertingDistressPolicy,
        PsychologyParameters,
        SimplePsychologyPolicy,
    )

    full, sleep_only = _formulas()
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
        LifeStatePsychologyPolicy(parameters, body_config=body, body_formula=full),
        LifeStatePsychologyPolicy(parameters, body_config=body, body_formula=sleep_only),
        IntegratedDistressLifeStatePolicy(parameters, body_config=body),
        IntegratedDistressLifeStatePolicy(parameters, body_config=body, body_formula=full),
    ]


def _psychology(scenario: Any, fixture: Fixture, policies: Policies) -> dict[str, Any]:
    from sheltersim.policies.psychology import (
        IntegratedDistressLifeStatePolicy,
        PsychologyParameters,
    )

    cases: dict[str, Any] = {"models": [], "ints": []}
    for seed in PSYCHOLOGY_SEEDS:
        draws = random.Random(seed)
        world = _shuffled_world(scenario, draws)
        fixture.world(f"psychology-{seed}", world, None)
        ids = sorted(world.agents)
        drawn = _drawn_policies(draws, ids)
        cases["models"].append(
            [
                policies.add(f"psychology-{seed}-{number}", "psychology", policy)
                for number, policy in enumerate(drawn)
            ]
        )
        if seed < 3:
            cases["ints"].append(
                [
                    policies.add(f"psychology-ints-{number}", "psychology", policy)
                    for number, policy in enumerate(_int_policies(ids))
                ]
            )
    # `test_a_one_percent_change_shows`.
    full, _ = _formulas()
    parameters = PsychologyParameters()
    changed = replace(parameters, stress_pressure=parameters.stress_pressure * 1.01)
    cases["one_percent"] = [
        policies.add(
            "psychology-one-percent",
            "psychology",
            IntegratedDistressLifeStatePolicy(parameters, body_formula=full),
        ),
        policies.add(
            "psychology-one-percent-changed",
            "psychology",
            IntegratedDistressLifeStatePolicy(changed, body_formula=full),
        ),
    ]
    return cases


# ---- The trace's documents of each person's minute -----------------------------------------------


def _trace_world(scenario: Any) -> tuple[Any, Any]:
    """The world of `test_trace_documents.py` and its Run's controller, before `_shuffle`."""
    from sheltersim.core.engine import TraceLevel
    from sheltersim.core.world import WorldState
    from sheltersim.experiments.runner import OfflineSimulationController
    from sheltersim.inspection.traces import RunArtifactWriter

    world = WorldState.from_scenario(scenario)
    directory = tempfile.mkdtemp(prefix="core-fixture-trace-")
    artifacts = RunArtifactWriter(scenario, directory, trace_level=TraceLevel.OFF)
    artifacts.write_initial(world, execution_mode="offline")
    return world, OfflineSimulationController(world, artifacts)


def _shuffle(world: Any, draws: random.Random) -> None:
    """`_shuffle` of `test_trace_documents.py`."""
    from sheltersim.domain.models import SupportRequestState, SupportRequestStatus

    extremes = (0.0, 1.0)
    zones = sorted(world.zones)
    people = list(world.agents.values())
    for number, agent in enumerate(people):
        agent.zone_id = draws.choice(zones)
        for name in ("toilet_urgency", "hunger", "fatigue", "hydration_need"):
            setattr(agent.needs, name, draws.random() if number % 3 else draws.choice(extremes))
        agent.stress = draws.random() if number % 3 else draws.choice(extremes)
    for number, agent in enumerate(people[:6]):
        status = list(SupportRequestStatus)[number % 4]
        request = SupportRequestState(f"R{number}", agent.id, 0, 10, status=status)
        world.support_requests[request.id] = request
        agent.support_request_id = request.id


def _trace_policies(draws: random.Random, ids: list[str]) -> list[Any]:
    """`_policies` of `test_trace_documents.py`."""
    from sheltersim.policies.alternatives import BoundedStressPolicy
    from sheltersim.policies.psychology import (
        IntegratedDistressLifeStatePolicy,
        LifeStatePsychologyPolicy,
        MeanRevertingDistressPolicy,
        PsychologyParameters,
        SimplePsychologyPolicy,
    )

    def rate() -> float:
        return draws.choice((0.0, draws.random() * 0.25))

    parameters = PsychologyParameters(
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
        support_wait_stress=rate(),
        natural_recovery_rate_per_minute=rate(),
        baseline_distress_by_agent={ids[0]: draws.random()},
    )
    body = {"uncertainty_drift_per_minute": draws.random() * 0.01}
    full, _ = _formulas()
    return [
        SimplePsychologyPolicy(parameters),
        SimplePsychologyPolicy(parameters, include_direct_rest_stress_recovery=False),
        BoundedStressPolicy(parameters),
        MeanRevertingDistressPolicy(parameters),
        LifeStatePsychologyPolicy(parameters, body_config=body),
        LifeStatePsychologyPolicy(parameters, body_config=body, body_formula=full),
        IntegratedDistressLifeStatePolicy(parameters, body_config=body, body_formula=full),
    ]


def _trace(scenario: Any, fixture: Fixture, policies: Policies) -> list[Any]:
    """`test_the_document_of_each_model_is_the_deleted_rule_s`: by seed, the world and each
    policy with the stresses the social step drew for its deltas (the same at each level)."""
    from sheltersim import native

    cases = []
    for seed in TRACE_SEEDS:
        draws = random.Random(seed)
        world, controller = _trace_world(scenario)
        _shuffle(world, draws)
        name = fixture.world(f"trace-{seed}", world, None)
        view = controller._view_after_movement(world)
        steps = []
        for number, policy in enumerate(_trace_policies(draws, sorted(world.agents))):
            controller.psychology = policy
            controller._run_core.refresh_policies(controller)
            result = native.rules("psychology").update_psychology(controller, view, 0)
            # The social step's stress of every other delta; the others keep the kernel's.
            drawn = [draws.random() for place, _ in enumerate(result.deltas) if place % 2]
            steps.append(
                {
                    "policy": policies.add(f"trace-{seed}-{number}", "psychology", policy),
                    "drawn": drawn,
                }
            )
        cases.append({"seed": seed, "world": name, "steps": steps})
    return cases
