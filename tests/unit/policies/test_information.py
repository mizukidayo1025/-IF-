"""Information spread: strict opt-in configuration, transmission probability, and the
checks on Scenario events and saved state.

Rumors and corrections spreading in full Runs are checked in
tests/integration/test_information_spread.py.
"""

from __future__ import annotations

import json

import pytest
from pydantic import ValidationError
from support.paths import SCENARIO_100_AGENTS

from sheltersim import native
from sheltersim.core.rng import CounterRNG
from sheltersim.core.world import StateInvariantError, WorldState
from sheltersim.domain.information import INFORMATION_SPREAD_STATE_KEY
from sheltersim.domain.models import InformationSpreadConfig
from sheltersim.experiments.scenario import ScenarioSpec
from sheltersim.policies.behavior import UtilityBehaviorPolicy, WorldView


def test_information_config_is_strict_and_off_by_default() -> None:
    config = InformationSpreadConfig.from_mapping()

    assert not config.enabled
    assert config.provenance()["source_id"] == "ASM-INFORMATION-SPREAD-V1"
    with pytest.raises(ValueError, match="unknown information_spread settings"):
        InformationSpreadConfig.from_mapping({"enabled": True, "free_text": "unsafe"})
    with pytest.raises(ValueError, match="max_contacts_per_agent"):
        InformationSpreadConfig.from_mapping({"max_contacts_per_agent": 17})

    baseline_world = WorldState.from_scenario(ScenarioSpec.load(SCENARIO_100_AGENTS))
    proposal = UtilityBehaviorPolicy().propose(
        WorldView.from_world(baseline_world), "A001", CounterRNG(baseline_world.seed)
    )
    help_candidate = next(
        candidate for candidate in proposal.candidates if candidate.action.value == "seek_help"
    )
    assert help_candidate.trigger == (
        "support_need>=0.25 and maximum need/stress>=0.50 "
        "and no_open_support_request and support_cooldown_elapsed"
    )


def test_transmission_probability_depends_on_trust_and_contact_duration(
    information_spread_config,
) -> None:
    config = InformationSpreadConfig.from_mapping(
        information_spread_config(
            base_transmission_probability=0.5,
            trust_sensitivity=1.0,
            contact_duration_scale_minutes=5,
        )
    )

    def probability(trust: float, minutes: int) -> float:
        """A same-zone claim's chance in native/src/rules/information.rs, the sparse model's."""
        return native.rules("information").transmission_probability_of(
            False,
            trust,
            False,
            False,
            minutes,
            False,
            float(config.contact_duration_scale_minutes),
            config.base_transmission_probability,
            config.correction_transmission_multiplier,
            config.trust_sensitivity,
            config.family_contact_multiplier,
            config.adjacent_zone_contact_multiplier,
        )

    low_trust = probability(0.0, 5)
    short_trusted_contact = probability(1.0, 1)
    long_trusted_contact = probability(1.0, 5)

    assert low_trust == 0.0
    assert short_trusted_contact == 0.2
    assert long_trusted_contact == 1.0


def test_scenario_rejects_information_events_without_opt_in_or_causal_correction(
    information_spread_scenario,
) -> None:
    document = json.loads(SCENARIO_100_AGENTS.read_text(encoding="utf-8"))
    document["scheduled_events"] = [
        {
            "id": "rumor-event",
            "due_minute": 0,
            "sequence": 1,
            "event_type": "information_rumor",
            "payload": {
                "message_id": "claim-1",
                "truth_status": "true",
                "origin_agent_id": "A001",
            },
            "source_id": "scenario-source",
        }
    ]

    with pytest.raises(ValidationError, match="requires enabled information_spread"):
        ScenarioSpec.model_validate(document)

    scenario = information_spread_scenario(truth_status="true")
    changed = scenario.model_dump(mode="json")
    changed["scheduled_events"][1]["causation_id"] = "other-event"
    with pytest.raises(ValidationError, match="causally reference an earlier rumor"):
        ScenarioSpec.model_validate(changed)


def test_world_rejects_invalid_persisted_information_state(information_spread_scenario) -> None:
    world = WorldState.from_scenario(information_spread_scenario())
    world.model_state[INFORMATION_SPREAD_STATE_KEY]["query_pressure"]["A001"] = 1.1

    with pytest.raises(StateInvariantError, match="query pressure"):
        world.validate()
