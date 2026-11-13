"""The builder's constants and parts as Python has them, for the core's port (Issue #299).

`InitialScenarioBuilder.build` joins the request, the regional research, the standard shelter, the
synthetic population and the compiled operational conditions into a Scenario. The core joins them
the same way (`scenario::builder`), with what the request cannot change written once as
constants: the model configs a screen request always gets and the IDs Python's modules define
(`builder_constants`, which `native/core/tools/scenario_constants.py` writes to
`native/core/src/scenario/constants.json`; `tests/unit/experiments/test_initial_scenario.py`
fails when that file is stale).

`scenario_builder.json` holds, for some small cases of the scenario corpus (#293), the parts the
builder joins as Python made them (`builder_parts`): the request and research the server read,
the population (`GeneratedPopulation.model_dump`), the compiled conditions, the generated context
and the service-capacity warnings. `native/core/tests/scenario_builder.rs` joins them in the core
and compares the Scenario with the corpus's sha256, before the core makes those parts itself
(#297, #298). `benchmarks/core_fixtures.py scenario_builder` writes it.
"""

from __future__ import annotations

import json
from typing import Any

# Small corpus cases: one person, water and food entered, 90 days of deliveries, a place the
# address search does not know, no staff, the municipality code's shelters (addresses removed).
CASES = (
    "text-one-person",
    "text-water-ml",
    "form-ninety-days",
    "text-unknown-place",
    "text-staff-zero",
    "text-municipality-code",
)
# What the builder reads of `GeneratedPopulation.model_dump` (the rest is left out).
POPULATION_KEYS = (
    "profile_set",
    "population",
    "household_sizes",
    "age_bands",
    "resident_age_distribution",
    "evacuee_age_distribution",
    "household_mix",
    "evacuation_selection",
    "household_relationships",
    "person_requirements",
    "generator",
    "assumptions",
    "sources",
)
# The model configs every screen request gets, as the validated Scenario holds them.
CONSTANT_CONFIGS = (
    "agent_dynamics",
    "behavior",
    "body_dynamics",
    "facility_queues",
    "geometry_overlay",
    "information_spread",
    "movement_model",
    "operational_events",
    "policy_registry",
    "psychology",
    "shared_operations",
    "simulator_extensions",
    "social_propagation",
    "staff",
    "staff_dynamics",
    "staff_operations",
    "state_contracts",
    "toilet_service",
    "zone_environment",
)


def builder_constants() -> dict[str, Any]:
    """What the core's builder holds as constants."""
    from sheltersim.contracts.policies import CORE_MODEL_SUITE_MODELS, CORE_MODEL_SUITE_V2
    from sheltersim.domain.agent_dynamics import (
        INSTALLED_FACILITY_ANNOUNCEMENT_SOURCE_ID,
        ORIENTATION_FACILITY_KINDS,
        ORIENTATION_FACILITY_SOURCE_ID,
    )
    from sheltersim.domain.body_state import BODY_SOURCE_ID
    from sheltersim.domain.geometry_overlay import (
        INSTALLED_FACILITY_PLACEMENT_SOURCE_ID,
        TEMPORARY_ROOM_BED_SOURCE_ID,
        TEMPORARY_ROOM_INTAKE_SOURCE_ID,
    )
    from sheltersim.domain.movement import MOVEMENT_SOURCE_ID
    from sheltersim.domain.psychological_state import INTEGRATED_PSYCHOLOGY_POLICY_VERSION
    from sheltersim.domain.staff_operations import STAFF_OPERATIONS_SOURCE_ID
    from sheltersim.domain.temporary_spaces import TEMPORARY_SPACES_SOURCE_ID
    from sheltersim.experiments.initial_scenario import (
        InitialScenarioBuilder,
        InitialScenarioRequest,
    )
    from sheltersim.experiments.model_suite import (
        CORE_MODEL_SUITE_SOURCE_ID,
        SLEEP_DEBT_BALANCE_SOURCE_ID,
    )
    from sheltersim.experiments.scenario import TOILET_SERVICE_SOURCE_ID, ScenarioSpec
    from sheltersim.policies.psychology import IntegratedDistressLifeStatePolicy
    from sheltersim.policies.queueing import NeedWeightedPhysicalQueuePolicy
    from sheltersim.policies.relationships import DistanceDurationRepeatTrustPolicy
    from sheltersim.region.conditions import (
        RESOURCE_SERVICE_ASSUMPTION_ID,
        STANDARD_PROVISIONING_SOURCE_ID,
    )
    from sheltersim.region.population import OPERATOR_POPULATION_SOURCE_ID

    request = InitialScenarioRequest(municipality="合成市", disaster="地震", evacuees=3)
    scenario = InitialScenarioBuilder().build(request).scenario
    validated = json.loads(
        ScenarioSpec.model_validate(scenario.model_dump(mode="json")).canonical_json()
    )
    policies = {
        name: getattr(request, name)
        for name in type(request).model_fields
        if name.endswith("_policy_id")
    }
    return {
        "schema_version": 1,
        "model_configs": {key: validated["model_configs"][key] for key in CONSTANT_CONFIGS},
        "policy_ids": policies,
        "core_suite_models": dict(CORE_MODEL_SUITE_MODELS),
        "ids": {
            "TOILET_SERVICE_SOURCE_ID": TOILET_SERVICE_SOURCE_ID,
            "STANDARD_PROVISIONING_SOURCE_ID": STANDARD_PROVISIONING_SOURCE_ID,
            "RESOURCE_SERVICE_ASSUMPTION_ID": RESOURCE_SERVICE_ASSUMPTION_ID,
            "OPERATOR_POPULATION_SOURCE_ID": OPERATOR_POPULATION_SOURCE_ID,
            "BODY_SOURCE_ID": BODY_SOURCE_ID,
            "STAFF_OPERATIONS_SOURCE_ID": STAFF_OPERATIONS_SOURCE_ID,
            "MOVEMENT_SOURCE_ID": MOVEMENT_SOURCE_ID,
            "TEMPORARY_ROOM_BED_SOURCE_ID": TEMPORARY_ROOM_BED_SOURCE_ID,
            "TEMPORARY_ROOM_INTAKE_SOURCE_ID": TEMPORARY_ROOM_INTAKE_SOURCE_ID,
            "INSTALLED_FACILITY_PLACEMENT_SOURCE_ID": INSTALLED_FACILITY_PLACEMENT_SOURCE_ID,
            "TEMPORARY_SPACES_SOURCE_ID": TEMPORARY_SPACES_SOURCE_ID,
            "ORIENTATION_FACILITY_SOURCE_ID": ORIENTATION_FACILITY_SOURCE_ID,
            "INSTALLED_FACILITY_ANNOUNCEMENT_SOURCE_ID": INSTALLED_FACILITY_ANNOUNCEMENT_SOURCE_ID,
            "PSYCHOLOGY_SOURCE_ID": IntegratedDistressLifeStatePolicy.source_id,
            "INTEGRATED_PSYCHOLOGY_POLICY_VERSION": INTEGRATED_PSYCHOLOGY_POLICY_VERSION,
            "CORE_MODEL_SUITE_SOURCE_ID": CORE_MODEL_SUITE_SOURCE_ID,
            "CORE_MODEL_SUITE_V2": CORE_MODEL_SUITE_V2,
            "SLEEP_DEBT_BALANCE_SOURCE_ID": SLEEP_DEBT_BALANCE_SOURCE_ID,
            "NEED_WEIGHTED_QUEUE_SOURCE_ID": NeedWeightedPhysicalQueuePolicy.source_id,
            "DISTANCE_TRUST_SOURCE_ID": DistanceDurationRepeatTrustPolicy.source_id,
        },
        "orientation_facility_kinds": sorted(ORIENTATION_FACILITY_KINDS),
    }


def constants_text() -> str:
    """The text of `native/core/src/scenario/constants.json`."""
    return json.dumps(builder_constants(), sort_keys=True, ensure_ascii=False, indent=1) + "\n"


def builder_parts(name: str) -> dict[str, Any]:
    """The parts `InitialScenarioBuilder.build` joins for a corpus case, as Python makes them,
    and what the server answered (the corpus's values)."""
    from sheltersim.experiments.initial_scenario import (
        InitialScenarioBuilder,
        InitialScenarioRequest,
    )
    from sheltersim.experiments.scenario_connections import connect_generated_context
    from sheltersim.experiments.service_capacity import service_capacity_diagnostic
    from sheltersim.region.conditions import with_standard_provisioning
    from sheltersim.region.models import RegionResearchResult
    from sheltersim.region.population import (
        DEFAULT_SYNTHETIC_HOUSEHOLD_COMPOSITION,
        SyntheticPopulationGenerator,
        age_bands_from_observations,
        household_mix_from_observations,
    )
    from support.scenario_corpus import case_outputs

    outputs = case_outputs(name)
    answer = outputs["answer"]
    request = InitialScenarioRequest.model_validate(answer["request"])
    research = RegionResearchResult.model_validate(answer["research"])
    age_bands = age_bands_from_observations(research.population_observations)
    household_mix = household_mix_from_observations(research.population_observations)
    draft = InitialScenarioBuilder().build(
        request, research=research, age_bands=age_bands, household_mix=household_mix
    )
    design = draft.facility_design
    population = SyntheticPopulationGenerator().generate(
        count=request.evacuees,
        seed=request.seed,
        zone_ids=("sleeping",),
        age_bands=age_bands,
        household_mix=household_mix,
        evacuation_selection=request.evacuation_selection,
        conditions=request.population_conditions,
        household_composition=(
            None
            if request.population_conditions.household_composition is not None
            else DEFAULT_SYNTHETIC_HOUSEHOLD_COMPOSITION
        ),
    )
    duration_minutes = max(1, round(request.duration_days * 1440))
    conditions, provisioning = with_standard_provisioning(
        request.operational_conditions,
        population=request.evacuees,
        duration_minutes=duration_minutes,
        supply_start_day=request.default_supply_start_day,
    )
    initial_stock, events, configs, blockers = conditions.compile(
        duration_minutes=duration_minutes,
        zone_ids={zone.id for zone in design.zones},
        facilities=design.facilities,
        staff=design.staff,
    )
    return {
        "request": answer["request"],
        "research": answer["research"],
        "input": answer["scenario"]["provenance"]["scenario_input"],
        "population": {
            key: value
            for key, value in population.model_dump(mode="json").items()
            if key in POPULATION_KEYS
        },
        "conditions": {
            "initial_stock": initial_stock.model_dump(mode="json") if initial_stock else None,
            "scheduled_events": [event.model_dump(mode="json") for event in events],
            "model_configs": [[key, value] for key, value in configs.items()],
            "blockers": list(blockers),
            "provenance_sources": [dict(item) for item in conditions.provenance_sources()],
            "provisioning": provisioning,
            "request_provenance_sources": [
                dict(item) for item in request.operational_conditions.provenance_sources()
            ],
            "request_referenced_source_ids": list(
                request.operational_conditions.referenced_source_ids()
            ),
        },
        "connections": json.loads(
            json.dumps(
                connect_generated_context(population, design.editable_geometry, design.geometry)
            )
        ),
        "service_capacity_warnings": list(service_capacity_diagnostic(draft.scenario)["warnings"]),
        "expected": {
            "approved_hash": answer["approved_hash"],
            "length": len(outputs["scenario_bytes"]),
            "warnings": answer["warnings"],
            "approval_blockers": answer["approval_blockers"],
            "can_start": answer["can_start"],
            "geometry_hash": answer["geometry_hash"],
        },
    }


def scenario_builder_fixture() -> dict[str, Any]:
    return {
        "constants": builder_constants(),
        "cases": {name: builder_parts(name) for name in CASES},
    }
