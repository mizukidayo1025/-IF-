"""Synthetic populations as the Scenario builder generates them (Issue #297).

The browser generates the residents of a Scenario in the Rust core
(`sheltersim_core::scenario::population`), as `SyntheticPopulationGenerator.generate` does for
`InitialScenarioBuilder.build`: one starting zone, the region's age bands and household mix (none
without e-Stat), the generic household templates, the default evacuation selection and population
conditions. Each case is a count and a seed (and, for a few, other bands, mixes or templates),
with the sha256 of the canonical JSON of `GeneratedPopulation.model_dump(mode="json")` and of its
parts. `native/core/tests/scenario_population.rs` reads
`native/core/tests/data/scenario_population.json`.
"""

from __future__ import annotations

import hashlib
import json
from typing import Any

from sheltersim.region.population import (
    DEFAULT_SYNTHETIC_HOUSEHOLD_COMPOSITION,
    AgeBand,
    HouseholdSizeMix,
    SyntheticPopulationGenerator,
)

PARTS = (
    "profile_set",
    "population",
    "person_requirements",
    "household_relationships",
    "assumptions",
    "sources",
)

# Counts and seeds as the screen sends them (1 to 10,000 people; the seed 2026 or a typed one).
COUNTS_AND_SEEDS = [
    (1, 2026),
    (2, 2026),
    (3, 0),
    (5, 1),
    (7, 2026),
    (17, 42),
    (50, 7),
    (100, 2026),
    (100, 2027),
    (100, 9_223_372_036_854_775_807),
    (250, 123_456),
    (1_000, 2026),
    (10_000, 2026),
]

# Bands and mixes the region research could give (not in the browser this week, which has no
# e-Stat), to check the general paths: an adult-only selection drops the child templates.
ADULT_BANDS = [
    {"label": "18-64", "age_min": 18, "age_max": 64, "population_count": 70, "source_ids": ["S1"]},
    {"label": "65+", "age_min": 65, "age_max": 100, "population_count": 30, "source_ids": ["S1"]},
]
WIDE_BANDS = [
    {"label": "0-14", "age_min": 0, "age_max": 14, "population_count": 12, "source_ids": ["S2"]},
    {"label": "15-64", "age_min": 15, "age_max": 64, "population_count": 60, "source_ids": ["S2"]},
    {"label": "65+", "age_min": 65, "age_max": 120, "population_count": 28, "source_ids": ["S3"]},
]
LARGE_MIX = {"size_weights": {"6": 1.0, "1": 0.5, "10": 0.25}, "source_ids": ["S4"]}

OTHER_CASES: list[dict[str, Any]] = [
    {"name": "adult_bands", "count": 40, "seed": 3, "age_bands": ADULT_BANDS},
    {"name": "wide_bands", "count": 60, "seed": 4, "age_bands": WIDE_BANDS},
    {"name": "large_households", "count": 80, "seed": 5, "household_mix": LARGE_MIX},
    {"name": "no_templates", "count": 30, "seed": 6, "composition": None},
]


def _digest(value: Any) -> str:
    text = json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))
    return hashlib.sha256(text.encode("utf-8")).hexdigest()


def _case(name: str, count: int, seed: int, **inputs: Any) -> dict[str, Any]:
    composition = inputs.get("composition", "default")
    bands = inputs.get("age_bands")
    mix = inputs.get("household_mix")
    generated = SyntheticPopulationGenerator().generate(
        count=count,
        seed=seed,
        zone_ids=("sleeping",),
        age_bands=tuple(AgeBand.model_validate(band) for band in bands or ()),
        household_mix=HouseholdSizeMix.model_validate(mix) if mix else None,
        household_composition=DEFAULT_SYNTHETIC_HOUSEHOLD_COMPOSITION
        if composition == "default"
        else None,
    )
    dump = generated.model_dump(mode="json")
    return {
        "name": name,
        "count": count,
        "seed": seed,
        "age_bands": bands,
        "household_mix": mix,
        "composition": composition,
        "sha256": _digest(dump),
        "parts": {part: _digest(dump[part]) for part in PARTS},
        "minors": sum(
            1 for person in dump["population"]["people"].values() if person["age_years"] < 18
        ),
    }


def scenario_population_fixture() -> dict[str, Any]:
    """The cases and Python's digests (`native/core/tests/data/scenario_population.json`)."""
    cases = [_case(f"count_{count}_seed_{seed}", count, seed) for count, seed in COUNTS_AND_SEEDS]
    cases += [_case(**case) for case in OTHER_CASES]
    return {"cases": cases}
