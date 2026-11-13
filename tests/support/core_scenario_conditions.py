"""Operational conditions, context and service capacity of generated Scenarios (Issue #298).

The browser compiles the operational conditions of a Scenario in the Rust core
(`sheltersim_core::scenario::conditions`, `connections`, `service_capacity`), as
`InitialScenarioBuilder.build` does with `with_standard_provisioning`,
`OperationalConditions.compile`, `connect_generated_context` and `service_capacity_diagnostic`.
This module writes `native/core/tests/data/scenario_conditions.json`, which
`native/core/tests/scenario_conditions.rs` reads:

- `resource_service_config`: `default_resource_service_config()`'s dump, which the core keeps as
  a constant.
- `provisioning`: the initial inventory as the request gives it (unknown, entered, incomplete),
  people, minutes, supply day and facility kinds, with the sha256 of Python's answers (the
  provisioned deliveries and record, the compiled stock, events, model configs and blockers, the
  provenance sources and referenced source IDs before and after provisioning). One case keeps
  the whole document to read.
- `built`: Scenarios the builder made from form requests, with what the core rebuilds from them:
  the `population_context` (from the same people and seed), the `spatial_constraints` (from the
  builder's editable plan and floor plan, which the case keeps) and the service-capacity
  diagnostic (from the parts of the Scenario it reads, which the case keeps; profiles as a count).
"""

from __future__ import annotations

import hashlib
import json
from types import SimpleNamespace
from typing import Any

from sheltersim.experiments.initial_scenario import InitialScenarioBuilder, InitialScenarioRequest
from sheltersim.experiments.service_capacity import service_capacity_diagnostic
from sheltersim.region.conditions import (
    OperationalConditions,
    default_resource_service_config,
    with_standard_provisioning,
)

STOCKS: dict[str, dict[str, Any]] = {
    "unknown": {},
    "entered": {"status": "entered", "water_ml": 450_000, "food_meal_units": 300},
    "entered_zero": {"status": "entered", "water_ml": 0, "food_meal_units": 0},
    "incomplete_water": {"status": "incomplete", "water_ml": 2_000},
    "incomplete_food": {
        "status": "incomplete",
        "food_meal_units": 5,
        "source_ids": ["SRC-OPERATOR-INITIAL-CONDITIONS"],
    },
}
PEOPLE = (1, 100, 10_000)
MINUTES = (1, 1_440, 4_320, 4_321, 10_080, 20_160, 129_600)
SUPPLY_DAYS = (1, 4, 91)
FACILITY_SETS: dict[str, tuple[str, ...]] = {
    "both": ("water_station", "meal_distribution", "toilet"),
    "water_only": ("water_station",),
    "none": (),
}


def _canonical(value: Any) -> str:
    return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"))


def _digest(value: Any) -> str:
    return hashlib.sha256(_canonical(value).encode("utf-8")).hexdigest()


def provisioning_answer(
    stock: dict[str, Any], people: int, minutes: int, supply_day: int, kinds: tuple[str, ...]
) -> dict[str, Any]:
    """What Python makes of one request's conditions, as one document."""
    conditions = OperationalConditions.model_validate({"initial_resources": stock})
    provisioned, record = with_standard_provisioning(
        conditions, population=people, duration_minutes=minutes, supply_start_day=supply_day
    )
    facilities = tuple(
        SimpleNamespace(id=f"{kind}_{index}", kind=kind, capacity=1, working_units=None)
        for index, kind in enumerate(kinds)
    )
    initial_stock, events, model_configs, blockers = provisioned.compile(
        duration_minutes=minutes, zone_ids={"sleeping"}, facilities=facilities, staff=()
    )
    return {
        "provisioned_scheduled": provisioned.model_dump(mode="json")["scheduled"],
        "record": record,
        "initial_stock": initial_stock.model_dump(mode="json") if initial_stock else None,
        "events": [event.model_dump(mode="json") for event in events],
        "model_configs": model_configs,
        "blockers": list(blockers),
        "request_sources": list(conditions.provenance_sources()),
        "provisioned_sources": list(provisioned.provenance_sources()),
        "request_referenced": list(conditions.referenced_source_ids()),
        "provisioned_referenced": list(provisioned.referenced_source_ids()),
    }


def _provisioning_cases() -> list[dict[str, Any]]:
    cases = []

    def add(stock: str, people: int, minutes: int, day: int, facilities: str) -> None:
        answer = provisioning_answer(STOCKS[stock], people, minutes, day, FACILITY_SETS[facilities])
        cases.append(
            {
                "stock": stock,
                "people": people,
                "minutes": minutes,
                "supply_day": day,
                "facilities": facilities,
                "sha256": _digest(answer),
            }
        )

    for stock in STOCKS:
        for people in PEOPLE:
            for minutes in MINUTES:
                for day in SUPPLY_DAYS:
                    add(stock, people, minutes, day, "both")
        for facilities in FACILITY_SETS:
            add(stock, 100, 10_080, 4, facilities)
    return cases


BUILT_REQUESTS: dict[str, dict[str, Any]] = {
    "example": {"evacuees": 100},
    "one_person": {"evacuees": 1, "duration_days": 0.5},
    "six_people": {"evacuees": 6, "seed": 7},
    "no_staff_no_toilets": {"evacuees": 80, "staff_count": 0, "accessible_toilet_count": 0},
    "crowded": {"evacuees": 1_000, "gross_area_m2": 300, "staff_count": 2},
    "incomplete_stock": {
        "evacuees": 50,
        "operational_conditions": {"initial_resources": {"status": "incomplete", "water_ml": 1}},
    },
    "entered_stock": {
        "evacuees": 40,
        "accessible_toilet_count": 20,
        "operational_conditions": {
            "initial_resources": {"status": "entered", "water_ml": 1, "food_meal_units": 1}
        },
    },
}


def _diagnostic_input(document: dict[str, Any]) -> dict[str, Any]:
    """The parts of a Scenario's dump the diagnostic reads; the profiles as their count."""
    configs = document["model_configs"]
    kept = {
        name: configs[name]
        for name in ("toilet_service", "resource_service", "movement_model")
        if name in configs
    }
    if "psychology" in configs:
        kept["psychology"] = {
            key: configs["psychology"][key]
            for key in ("hunger_growth", "hunger_growth_by_agent")
            if key in configs["psychology"]
        }
    if "facility_geometry" in configs:
        kept["facility_geometry"] = {
            "facility_points": dict.fromkeys(configs["facility_geometry"]["facility_points"], 0)
        }
    return {
        "id": document["id"],
        "profile_count": len(document["profiles"]),
        "facilities": [
            {key: item[key] for key in ("id", "kind", "capacity", "working_units", "zone_id")}
            for item in document["facilities"]
        ],
        "staff": [
            {key: item[key] for key in ("id", "zone_id", "skills")} for item in document["staff"]
        ],
        "model_configs": kept,
    }


def _built_cases() -> list[dict[str, Any]]:
    cases = []
    for name, fields in BUILT_REQUESTS.items():
        request = InitialScenarioRequest.model_validate(
            {"municipality": "墨田区", "disaster": "earthquake", **fields}
        )
        draft = InitialScenarioBuilder().build(request)
        scenario = draft.scenario
        configs = scenario.model_configs
        document = scenario.model_dump(mode="json")
        editable = draft.editable_geometry.model_dump(mode="json")
        cases.append(
            {
                "name": name,
                "evacuees": request.evacuees,
                "seed": request.seed,
                "population_context": _digest(configs["population_context"]),
                "editable_geometry": editable,
                "routes": configs["facility_geometry"]["routes"],
                "spatial_constraints": _digest(configs["spatial_constraints"]),
                "diagnostic_input": _diagnostic_input(document),
                "diagnostic": service_capacity_diagnostic(scenario),
            }
        )
    # Without the configs the diagnostic falls back to the contracts' defaults.
    bare = json.loads(json.dumps(cases[0]["diagnostic_input"]))
    for config in ("toilet_service", "psychology", "movement_model"):
        bare["model_configs"].pop(config)
    example = InitialScenarioBuilder().build(
        InitialScenarioRequest.model_validate(
            {"municipality": "墨田区", "disaster": "earthquake", **BUILT_REQUESTS["example"]}
        )
    )
    stripped = example.scenario.model_copy(
        update={
            "model_configs": {
                key: value
                for key, value in example.scenario.model_configs.items()
                if key not in {"toilet_service", "psychology", "movement_model"}
            }
        }
    )
    cases.append(
        {
            "name": "example_without_configs",
            "diagnostic_input": bare,
            "diagnostic": service_capacity_diagnostic(stripped),
        }
    )
    return cases


def scenario_conditions_fixture() -> dict[str, Any]:
    """The cases and Python's answers (`native/core/tests/data/scenario_conditions.json`)."""
    return {
        "resource_service_config": _canonical(
            default_resource_service_config().model_dump(mode="json")
        ),
        "stocks": STOCKS,
        "facility_sets": FACILITY_SETS,
        "provisioning": _provisioning_cases(),
        "provisioning_example": {
            "case": {"stock": "unknown", "people": 100, "minutes": 10_080, "supply_day": 4},
            "answer": provisioning_answer(STOCKS["unknown"], 100, 10_080, 4, FACILITY_SETS["both"]),
        },
        "built": _built_cases(),
    }
