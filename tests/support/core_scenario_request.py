"""Requests of a Scenario and what `InitialScenarioRequest` makes of them (Issue #295).

The browser validates the request of `scenario-preview` in the Rust core
(`sheltersim_core::scenario::request`), as pydantic's lax mode does: ints from floats, bools and
text, floats from text, bools from text and numbers, the bounds, the nested initial inventory and
the model validators. Each case here is a request as `json.loads` gives it, with what Python
answers: the dump (`model_dump(mode="json")` as canonical JSON, which the server writes and the
form's `input_sha256` hashes) or `invalid` (the server's 400). A case that Python accepts but the
browser does not compute (a field the screen never sends, at a value other than its default) says
`"browser": "unsupported"`. `coercions` gives each scalar field each value of `SCALARS` with the
rest of the request at `BASE`. `native/core/tests/scenario_request.rs` reads
`native/core/tests/data/scenario_request.json`.
"""

from __future__ import annotations

import copy
import hashlib
import json
from typing import Any

from pydantic import ValidationError

from sheltersim.experiments.initial_scenario import InitialScenarioRequest

BASE: dict[str, Any] = {"municipality": "墨田区", "disaster": "earthquake", "evacuees": 100}

# Values of each kind JSON can carry, for the coercion of each scalar field.
SCALARS: list[Any] = [
    True,
    False,
    0,
    1,
    -1,
    2,
    7,
    20,
    21,
    90,
    91,
    100,
    101,
    10_000,
    10_001,
    100_000,
    100_001,
    10**30,
    -(10**30),
    0.0,
    -0.0,
    1.0,
    1.5,
    2.5,
    7.0,
    0.0003,
    0.0004,
    1e-320,
    1e20,
    9.2e18,
    9.3e18,
    1e308,
    None,
    "",
    " ",
    "1",
    "0",
    "12",
    " 12 ",
    "\t12\n",
    "12　",
    " 12",
    "+5",
    "-0",
    "-0.0",
    "0012",
    "00",
    "1_000",
    "1_0",
    " 1_0 ",
    "1_0 ",
    "+1_0",
    " +1_0 ",
    "+_1",
    "_1",
    "1_",
    "1__0",
    "1_2.0",
    "12.",
    "12.0",
    "12.000",
    "12.0_0",
    "12.5",
    "1.",
    ".5",
    "1.e2",
    "1e2",
    "1E2",
    "1e-3",
    "1.5e1",
    "1_e1",
    "1e_1",
    "1._5",
    "1e400",
    "1e-999",
    "9223372036854775807",
    "9223372036854775808",
    "1" * 25,
    "inf",
    "+inf",
    "nan",
    "infinity",
    "0x10",
    "1 2",
    "１２",
    "١٢",
    "true",
    "True ",
    "tRuE",
    "yes",
    "Y",
    "on",
    "off",
    "f",
    "N",
    "no",
    "2",
    [],
    [1],
    {},
    {"a": 1},
]

SCALAR_FIELDS = (
    "municipality",
    "disaster",
    "evacuees",
    "duration_days",
    "seed",
    "gross_area_m2",
    "accessible_toilet_count",
    "staff_count",
    "default_supply_start_day",
    "season",
    "municipality_code",
    "social_propagation_enabled",
    "information_spread_enabled",
)

POLICY_DEFAULTS = {
    name: field.default
    for name, field in InitialScenarioRequest.model_fields.items()
    if name.endswith("_policy_id")
}


def _python(request: Any) -> dict[str, Any]:
    """What Python answers for one request: its dump as canonical JSON, or that it is refused."""
    try:
        parsed = InitialScenarioRequest.model_validate(copy.deepcopy(request))
    except (ValidationError, ValueError):
        return {"outcome": "invalid"}
    dump = json.dumps(
        parsed.model_dump(mode="json"), ensure_ascii=False, sort_keys=True, separators=(",", ":")
    )
    return {
        "outcome": "ok",
        "dump": dump,
        "input_sha256": hashlib.sha256(dump.encode("utf-8")).hexdigest(),
    }


def _screen_requests() -> list[tuple[str, dict[str, Any]]]:
    """Requests as `/demo-v2` sends them (`scenarioRequest` in demo-v2.js), and echoed dumps."""
    full = {
        "municipality": "墨田区",
        "disaster": "earthquake",
        "evacuees": 100,
        "duration_days": 2.5,
        "staff_count": 0,
        "gross_area_m2": 820.5,
        "accessible_toilet_count": 20,
        "facility_conditions": ["施設種別（利用者入力）: 学校"],
        "operational_conditions": {
            "initial_resources": {
                "status": "entered",
                "food_meal_units": 300,
                "water_ml": 450000,
                "source_kind": "operator",
            }
        },
        "field_origins": {
            "municipality": "user",
            "disaster": "user",
            "evacuees": "user",
            "duration_days": "user",
            "staff_count": "user",
            "gross_area_m2": "user",
            "accessible_toilet_count": "user",
            "facility_conditions": "user",
            "operational_conditions": "user",
        },
    }
    incomplete = copy.deepcopy(full)
    incomplete["operational_conditions"]["initial_resources"].update(
        status="incomplete", water_ml=None
    )
    echoed = InitialScenarioRequest.model_validate(
        {
            **BASE,
            "season": "winter",
            "seed": 7,
            "municipality_code": "13107",
            "facility_conditions": ["ペット", "授乳・乳幼児", "個室・プライバシー", "車いす対応"],
            "operational_conditions": {
                "initial_resources": {
                    "status": "incomplete",
                    "water_ml": 2000,
                    "source_ids": ["SRC-OPERATOR-INITIAL-CONDITIONS"],
                }
            },
            "field_origins": {"municipality": "user", "seed": "user", "duration_days": "default"},
        }
    ).model_dump(mode="json")
    merged = {**echoed, **{key: full[key] for key in ("evacuees", "staff_count")}}
    return [
        ("minimal", dict(BASE)),
        ("form_full", full),
        ("form_incomplete_stock", incomplete),
        ("form_ten_thousand", {**BASE, "evacuees": 10_000, "duration_days": 0.5}),
        ("form_no_toilets", {**BASE, "accessible_toilet_count": 0, "staff_count": 100}),
        ("echoed_dump", echoed),
        ("echoed_dump_merged_with_form", merged),
        ("default_dump", InitialScenarioRequest.model_validate(BASE).model_dump(mode="json")),
        (
            "default_dump_python_mode",
            json.loads(
                json.dumps(InitialScenarioRequest.model_validate(BASE).model_dump(mode="python"))
            ),
        ),
    ]


def _unsupported() -> list[tuple[str, dict[str, Any]]]:
    """Requests Python accepts with a field the browser computes only at its default."""
    cases: list[tuple[str, dict[str, Any]]] = []
    for name, default in sorted(POLICY_DEFAULTS.items()):
        annotation = InitialScenarioRequest.model_fields[name].annotation
        other = next(value for value in annotation.__args__ if value != default)
        cases.append((f"policy_{name}", {**BASE, name: other}))
    resources = {"status": "entered", "water_ml": 1, "food_meal_units": 1}
    cases += [
        ("social_propagation_off", {**BASE, "social_propagation_enabled": False}),
        ("information_spread_off", {**BASE, "information_spread_enabled": "no"}),
        ("seed_beyond_i64", {**BASE, "seed": 10**30}),
        ("seed_text_beyond_i64", {**BASE, "seed": "9223372036854775808"}),
        ("target_date", {**BASE, "target_date": "2026-08-01"}),
        ("weather_area_code", {**BASE, "weather_area_code": "130000"}),
        ("municipality_code_wide_digits", {**BASE, "municipality_code": "１３１０７"}),
        (
            "facility_zones",
            {**BASE, "facility_zones": [{"action": "exclude", "zone_id": "pet_area"}]},
        ),
        ("zone_area_shares", {**BASE, "zone_area_shares": {"sleeping": 0.5}}),
        ("model_parameter_overrides", {**BASE, "model_parameter_overrides": {"a.b": 1.0}}),
        (
            "evacuation_selection",
            {**BASE, "evacuation_selection": {"age_band_multipliers": {"65+": 1.5}}},
        ),
        ("population_conditions", {**BASE, "population_conditions": {"pet_household_rate": 0.2}}),
        (
            "operational_scope",
            {**BASE, "operational_conditions": {"source_geographic_scope": "墨田区"}},
        ),
        (
            "operational_equipment",
            {
                **BASE,
                "operational_conditions": {
                    "equipment": [{"facility_id": "toilet_1", "capacity": 2}]
                },
            },
        ),
        (
            "stock_warehouse",
            {
                **BASE,
                "operational_conditions": {
                    "initial_resources": {**resources, "warehouse_id": "other"}
                },
            },
        ),
        (
            "stock_zone",
            {
                **BASE,
                "operational_conditions": {"initial_resources": {**resources, "zone_id": "z"}},
            },
        ),
        (
            "stock_source_kind",
            {
                **BASE,
                "operational_conditions": {
                    "initial_resources": {**resources, "source_kind": "official"}
                },
            },
        ),
        (
            "stock_source_ids",
            {
                **BASE,
                "operational_conditions": {
                    "initial_resources": {**resources, "source_ids": ["SRC-CITY-STOCK"]}
                },
            },
        ),
        (
            "stock_beyond_i64",
            {
                **BASE,
                "operational_conditions": {"initial_resources": {**resources, "water_ml": 10**30}},
            },
        ),
    ]
    return cases


def _structures() -> list[tuple[str, Any]]:
    """Shapes, nested inventory and the model validators."""

    def stock(**values: Any) -> dict[str, Any]:
        return {**BASE, "operational_conditions": {"initial_resources": values}}

    return [
        ("not_an_object", [BASE]),
        ("seed_text_too_long", {**BASE, "seed": "1" * 4301}),
        ("seed_text_longest", {**BASE, "seed": "0" * 4299 + "7"}),
        ("missing_municipality", {"disaster": "earthquake", "evacuees": 100}),
        ("missing_evacuees", {"municipality": "墨田区", "disaster": "earthquake"}),
        ("extra_field", {**BASE, "estat_area_code": "13107"}),
        ("municipality_long", {**BASE, "municipality": "区" * 160}),
        ("municipality_too_long", {**BASE, "municipality": "区" * 161}),
        ("disaster_too_long", {**BASE, "disaster": "x" * 81}),
        ("season_long", {**BASE, "season": "冬" * 40}),
        ("season_too_long", {**BASE, "season": "冬" * 41}),
        ("municipality_code_four", {**BASE, "municipality_code": "1310"}),
        ("municipality_code_letters", {**BASE, "municipality_code": "1310a"}),
        ("municipality_code_newline", {**BASE, "municipality_code": "13107\n"}),
        ("duration_rounds_to_zero", {**BASE, "duration_days": 0.0003}),
        ("duration_one_minute", {**BASE, "duration_days": 1 / 1440}),
        ("duration_half_minute", {**BASE, "duration_days": 0.5 / 1440}),
        ("facility_conditions_text", {**BASE, "facility_conditions": "ペット"}),
        ("facility_conditions_number", {**BASE, "facility_conditions": ["ペット", 1]}),
        ("facility_conditions_null", {**BASE, "facility_conditions": None}),
        ("facility_conditions_empty_text", {**BASE, "facility_conditions": [""]}),
        ("field_origins_unknown_field", {**BASE, "field_origins": {"text": "user"}}),
        ("field_origins_itself", {**BASE, "field_origins": {"field_origins": "user"}}),
        ("field_origins_bad_origin", {**BASE, "field_origins": {"municipality": "ai"}}),
        ("field_origins_null", {**BASE, "field_origins": None}),
        (
            "field_origins_every_origin",
            {
                **BASE,
                "field_origins": {
                    "seed": "default",
                    "municipality": "llm_extracted",
                    "model_parameter_overrides": "user",
                },
            },
        ),
        ("policy_unknown", {**BASE, "queue_policy_id": "fifo"}),
        ("policy_null", {**BASE, "queue_policy_id": None}),
        ("policy_default", {**BASE, "queue_policy_id": "need-weighted-physical-queue-v1"}),
        ("propagation_true_text", {**BASE, "social_propagation_enabled": "yes"}),
        ("overrides_null", {**BASE, "model_parameter_overrides": None}),
        ("overrides_list", {**BASE, "model_parameter_overrides": []}),
        ("zones_object", {**BASE, "facility_zones": {}}),
        ("shares_list", {**BASE, "zone_area_shares": []}),
        ("dates_null", {**BASE, "target_date": None, "weather_area_code": None}),
        ("selection_empty", {**BASE, "evacuation_selection": {}}),
        ("selection_null", {**BASE, "evacuation_selection": None}),
        ("selection_extra", {**BASE, "evacuation_selection": {"x": 1}}),
        ("population_empty_defaults", {**BASE, "population_conditions": {"source_ids": []}}),
        ("population_extra", {**BASE, "population_conditions": {"rate": 1}}),
        ("operational_null", {**BASE, "operational_conditions": None}),
        ("operational_empty", {**BASE, "operational_conditions": {}}),
        ("operational_extra", {**BASE, "operational_conditions": {"stock": {}}}),
        ("operational_lists_empty", {**BASE, "operational_conditions": {"scheduled": []}}),
        ("stock_empty", stock()),
        ("stock_null", {**BASE, "operational_conditions": {"initial_resources": None}}),
        ("stock_unknown_with_water", stock(status="unknown", water_ml=1)),
        ("stock_unknown_with_ids", stock(source_ids=["SRC-OPERATOR-INITIAL-CONDITIONS"])),
        ("stock_entered", stock(status="entered", water_ml=0, food_meal_units=0)),
        (
            "stock_entered_text_numbers",
            stock(status="entered", water_ml="2_000", food_meal_units=3.0),
        ),
        ("stock_entered_bools", stock(status="entered", water_ml=True, food_meal_units=False)),
        ("stock_entered_missing_food", stock(status="entered", water_ml=1)),
        ("stock_entered_negative", stock(status="entered", water_ml=-1, food_meal_units=1)),
        ("stock_entered_fraction", stock(status="entered", water_ml=1.5, food_meal_units=1)),
        ("stock_incomplete_food", stock(status="incomplete", food_meal_units=5)),
        ("stock_incomplete_none", stock(status="incomplete")),
        ("stock_unknown_status", stock(status="partial")),
        (
            "stock_duplicate_ids",
            stock(
                status="entered",
                water_ml=1,
                food_meal_units=1,
                source_ids=["SRC-OPERATOR-INITIAL-CONDITIONS"] * 2,
            ),
        ),
        (
            "stock_unstable_id",
            stock(status="entered", water_ml=1, food_meal_units=1, source_ids=["bad id"]),
        ),
        ("stock_ids_text", stock(source_ids="SRC-OPERATOR-INITIAL-CONDITIONS")),
        ("stock_warehouse_default", stock(warehouse_id="main_warehouse", zone_id=None)),
        ("stock_warehouse_number", stock(warehouse_id=1)),
        ("stock_source_kind_unknown", stock(source_kind="city")),
        ("stock_extra", stock(unit="ml")),
    ]


def scenario_request_fixture() -> dict[str, Any]:
    """The cases and Python's answers (`native/core/tests/data/scenario_request.json`)."""
    cases: list[dict[str, Any]] = []

    def add(name: str, request: Any, browser: str | None = None, dump: bool = False) -> None:
        answer = _python(request)
        if browser == "unsupported":
            assert answer["outcome"] == "ok", name
        if not dump:
            # The sha256 stands for the bytes; the screen's cases keep their text to read.
            answer.pop("dump", None)
        cases.append(
            {
                "name": name,
                "request": request,
                "python": answer,
                "browser": browser or answer["outcome"],
            }
        )

    for name, request in _screen_requests():
        add(name, request, dump=True)
    for name, request in _structures():
        add(name, request)
    for name, request in _unsupported():
        add(name, request, "unsupported")
    # Five numeric characters but not ASCII digits: pydantic's `\d` takes the decimal digits of
    # Unicode only, which the browser does not tell apart from other numeric characters, so it
    # leaves them all unsupported (Python refuses these).
    answer = _python(circled := {**BASE, "municipality_code": "①②③④⑤"})
    assert answer["outcome"] == "invalid"
    cases.append(
        {
            "name": "municipality_code_circled",
            "request": circled,
            "python": answer,
            "browser": "unsupported",
        }
    )
    # Each scalar field with each value of SCALARS, the rest of the request at BASE: the browser's
    # answer as "ok:<input_sha256>", "invalid" or "unsupported", in the order of SCALARS.
    coercions: dict[str, list[str]] = {}
    for field in SCALAR_FIELDS:
        answers = coercions.setdefault(field, [])
        for value in SCALARS:
            answer = _python({**BASE, field: value})
            browser = answer["outcome"]
            if browser == "ok":
                dumped = json.loads(answer["dump"])
                if field.endswith("_enabled") and dumped[field] is False:
                    browser = "unsupported"
                if field == "seed" and dumped[field] >= 2**63:
                    browser = "unsupported"
            if field == "municipality_code" and isinstance(value, str) and len(value) == 5:
                browser = answer["outcome"] if value.isascii() else "unsupported"
            answers.append(f"ok:{answer['input_sha256']}" if browser == "ok" else browser)
    return {"base": BASE, "cases": cases, "coercions": coercions, "scalars": SCALARS}
