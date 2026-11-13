"""The standard shelter's design as Python makes it, for the core's port (Issue #296).

`native/core/tests/scenario_facility.rs` builds the standard synthetic shelter
(`StandardShelterGenerator.generate`, region/facility.py) in the core, for the inputs the
Scenario screen can give (#291): the people, the floor area (or none), the accessible toilets,
the staff (or none) and the facility conditions; no typed zone changes, area shares, equipment or
selected shelter. `scenario_facility.json` holds the coefficients the generator reads
(`constants`), and cases of random inputs and edge values with the sha256 and length of the
design's canonical JSON (`model_dump(mode="json")` written as `storage.projects.canonical`
writes it) or the text of the ValueError Python raises; `samples` holds two designs in full.
`benchmarks/core_fixtures.py scenario_facility` writes it.
"""

from __future__ import annotations

import hashlib
import random
from typing import Any

SEED = 296
RANDOM_CASES = 240
CONDITIONS = (
    "ペット",
    "授乳",
    "乳幼児",
    "個室",
    "プライバシー",
    "車いす対応",
    "車椅子",
    "バリアフリー",
    "Pet",
    "infant",
    "family",
    "privacy",
    "wheelchair",
    "施設種別（利用者入力）: 学校（体育館）",
    "施設種別（利用者入力）: 公民館",
    "授乳・乳幼児",
    "個室・プライバシー",
)
# Inputs a person might give or that sit on an edge: (people, area, accessible toilets, staff,
# conditions).
EDGES: list[tuple[int, float | None, int, int | None, tuple[str, ...]]] = [
    (1, None, 1, None, ()),
    (1, None, 0, 0, ()),
    (1, 1.0, 0, None, ()),
    (2, 3.5, 1, None, ()),
    (12, None, 1, None, ("ペット",)),
    (100, None, 1, None, ()),
    (100, None, 2, 6, ("施設種別（利用者入力）: 学校（体育館）",)),
    (100, 50.0, 1, None, ()),
    (100, 900.0, 2, None, ()),
    (300, 500.0, 20, None, ()),
    (400, None, 1, 100, ()),
    (500, 100000.0, 1, None, ()),
    (2000, 3000.0, 1, None, ()),
    (10000, None, 1, None, ()),
    (10000, None, 20, 100, ("ペット", "授乳", "個室", "車いす")),
    (30, 333.3, 1, None, ()),
    (90, None, 1, None, ("ペット", "授乳・乳幼児", "個室・プライバシー", "車いす対応")),
    (60, None, 0, None, ("授乳・乳幼児", "個室・プライバシー")),
    (40, None, 1, 0, ()),
    (25, None, 1, None, ("車いす対応",)),
    (50, 80.0, 1, 1, ("その他の条件",)),
    (7, 12.5, 3, 2, ("ペット", "privacy")),
]


def facility_input(
    population: int,
    area: float | None,
    toilets: int,
    staff: int | None,
    conditions: tuple[str, ...],
) -> dict[str, Any]:
    """A case's input, as the core's test reads it."""
    return {
        "population": population,
        "gross_area_m2": area,
        "accessible_toilet_count": toilets,
        "staff_count": staff,
        "facility_conditions": list(conditions),
    }


def _random_inputs() -> list[dict[str, Any]]:
    draw = random.Random(SEED)
    inputs = []
    for _ in range(RANDOM_CASES):
        kind = draw.random()
        if kind < 0.5:
            population = draw.randint(1, 60)
        elif kind < 0.85:
            population = draw.randint(60, 600)
        else:
            population = int(10 ** draw.uniform(2.7, 4.0))
        area = None
        if draw.random() < 0.45:
            area = round(10 ** draw.uniform(0, 5), draw.choice((0, 1, 3, 6)))
            area = max(area, 0.5)
        toilets = draw.choice((0, 1, 1, 1, 2, 3, 5, 20, draw.randint(0, 20)))
        staff = None if draw.random() < 0.6 else draw.randint(0, 100)
        conditions = tuple(draw.sample(CONDITIONS, draw.choice((0, 0, 1, 1, 2, 3, 4))))
        inputs.append(facility_input(population, area, toilets, staff, conditions))
    return inputs


def generate(case: dict[str, Any]) -> Any:
    """The design the builder asks of the generator for a screen request (InitialScenarioBuilder
    .build: the area's source when an area is entered, no zone changes, shares or equipment)."""
    from sheltersim.region.facility import FacilityDesignRequest, StandardShelterGenerator

    area = case["gross_area_m2"]
    return StandardShelterGenerator().generate(
        FacilityDesignRequest(
            population=case["population"],
            gross_area_m2=area,
            accessible_toilet_count=case["accessible_toilet_count"],
            staff_count=case["staff_count"],
            facility_conditions=tuple(case["facility_conditions"]),
            area_source_ids=("SRC-OPERATOR-AREA",) if area is not None else (),
        )
    )


def _design_bytes(design: Any) -> bytes:
    from sheltersim.storage.projects import canonical

    return canonical(design.model_dump(mode="json")).encode("utf-8")


def constants() -> dict[str, Any]:
    """The coefficients `standard_service_counts` reads, which the core holds as constants."""
    from sheltersim.experiments import sanitation_diagnostics, service_capacity
    from sheltersim.experiments.scenario import PsychologyModelConfig, ToiletServiceConfig
    from sheltersim.region.conditions import default_resource_service_config
    from sheltersim.region.models import ACCESSIBLE_ROUTE_MIN_WIDTH_M

    toilet = ToiletServiceConfig()
    resource = default_resource_service_config()
    return {
        "accessible_route_min_width_m": ACCESSIBLE_ROUTE_MIN_WIDTH_M,
        "people_per_toilet_initial": sanitation_diagnostics.PEOPLE_PER_TOILET["initial"],
        "planning_toilet_uses_per_person_day": (
            sanitation_diagnostics.PLANNING_TOILET_USES_PER_PERSON_DAY
        ),
        "toilet_minimum_minutes": toilet.minimum_minutes,
        "toilet_maximum_minutes": toilet.maximum_minutes,
        "operating_minutes_per_day": service_capacity.OPERATING_MINUTES_PER_DAY,
        "target_utilization": service_capacity.TARGET_UTILIZATION,
        "resource_service_minutes": resource.service_minutes,
        "hydration_growth_per_minute": resource.hydration_growth_per_minute,
        "hydration_relief": resource.hydration_relief,
        "hydration_action_threshold": resource.hydration_action_threshold,
        "hunger_growth": PsychologyModelConfig().hunger_growth,
        "hunger_relief": resource.hunger_relief,
        "hunger_action_threshold": resource.hunger_action_threshold,
    }


def facility_case(case: dict[str, Any]) -> dict[str, Any]:
    """A case with the sha256 and length of its design, or the text of Python's ValueError."""
    try:
        data = _design_bytes(generate(case))
    except ValueError as error:
        return {"input": case, "raises": str(error)}
    return {"input": case, "sha256": hashlib.sha256(data).hexdigest(), "length": len(data)}


def scenario_facility_fixture() -> dict[str, Any]:
    import json

    edges = [facility_input(*item) for item in EDGES]
    samples = {
        name: json.loads(_design_bytes(generate(facility_input(*EDGES[index]))))
        for name, index in (("twelve_with_pets", 4), ("one_tiny_area", 2))
    }
    return {
        "constants": constants(),
        "cases": [facility_case(case) for case in (*edges, *_random_inputs())],
        "samples": samples,
    }
