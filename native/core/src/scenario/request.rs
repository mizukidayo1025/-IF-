//! `InitialScenarioRequest` of `experiments/initial_scenario.py` and the contracts in it (Issue
//! #295): what `InitialScenarioRequest.model_validate` accepts of a request `json.loads` read, and
//! its `model_dump(mode="json")`.
//!
//! Pydantic validates in its lax mode: an int field takes `True`, `1.0` and `" 1_0 "`, a float
//! field takes `"1e2"`, a bool field takes `"yes"` and `0`. `int_of`, `float_of` and `bool_of`
//! follow pydantic-core's rules for the values JSON can hold (the cases of
//! `native/core/tests/data/scenario_request.json` were checked against pydantic).
//!
//! The browser computes only what the screen `/demo-v2` and the text parser can ask for: the
//! municipality, disaster, people, days, seed, area, toilets, staff, supply day, season, facility
//! conditions, the initial water and food, and `field_origins`. Every other field must keep its
//! default; a valid other value is `Refusal::Unsupported` (the browser's `browser_unsupported`).
//! A value Python refuses is `Refusal::Invalid` (the server's 400 `invalid project request`),
//! also when another field is unsupported. Of a field the browser does not compute, only the
//! shape is checked; a value of it that Python would refuse may be answered as unsupported.

use std::sync::Arc;

use indexmap::IndexMap;

use crate::value::V;

/// The source of operator-entered conditions (`region/conditions.py`).
pub const OPERATOR_CONDITION_SOURCE_ID: &str = "SRC-OPERATOR-INITIAL-CONDITIONS";
/// The warehouse of the initial inventory (`InitialResourceCondition.warehouse_id`'s default).
pub const DEFAULT_WAREHOUSE_ID: &str = "main_warehouse";

/// Why a request is not computed.
#[derive(Clone, Debug, PartialEq)]
pub enum Refusal {
    /// Python refuses the request (pydantic's ValidationError or a ValueError): the server answers
    /// 400 `{"error": "invalid project request"}`. The text says what, for tests and logs only.
    Invalid(String),
    /// Python accepts it, but the browser does not compute it (`browser_unsupported`). The text
    /// names the field.
    Unsupported(String),
}

/// What the operator said of the initial inventory (`InitialResourceCondition.status`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InventoryStatus {
    Unknown,
    Incomplete,
    Entered,
}

impl InventoryStatus {
    pub fn text(self) -> &'static str {
        match self {
            InventoryStatus::Unknown => "unknown",
            InventoryStatus::Incomplete => "incomplete",
            InventoryStatus::Entered => "entered",
        }
    }
}

/// `InitialResourceCondition` with its warehouse, zone and source kind at their defaults
/// (`DEFAULT_WAREHOUSE_ID`, no zone, `"operator"`).
#[derive(Clone, Debug, PartialEq)]
pub struct InitialResources {
    pub status: InventoryStatus,
    pub water_ml: Option<i64>,
    pub food_meal_units: Option<i64>,
    /// After the model's validator: `[OPERATOR_CONDITION_SOURCE_ID]` once a quantity is entered.
    pub source_ids: Vec<String>,
}

impl Default for InitialResources {
    fn default() -> Self {
        Self { status: InventoryStatus::Unknown, water_ml: None, food_meal_units: None, source_ids: Vec::new() }
    }
}

/// `OperationalConditions` with only `initial_resources` (no equipment, schedule, shifts or
/// infrastructure).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct OperationalConditions {
    pub initial_resources: InitialResources,
}

/// Where a request field's value came from (`field_origins`' values).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldOrigin {
    User,
    LlmExtracted,
    Default,
}

impl FieldOrigin {
    pub fn text(self) -> &'static str {
        match self {
            FieldOrigin::User => "user",
            FieldOrigin::LlmExtracted => "llm_extracted",
            FieldOrigin::Default => "default",
        }
    }
}

/// `InitialScenarioRequest` after validation, the fields the browser computes; every other field
/// is at its default (`POLICY_FIELDS` and the empty ones of `dump`).
#[derive(Clone, Debug, PartialEq)]
pub struct InitialScenarioRequest {
    pub municipality: String,
    pub municipality_code: Option<String>,
    pub disaster: String,
    pub evacuees: i64,
    pub duration_days: f64,
    pub seed: i64,
    pub gross_area_m2: Option<f64>,
    pub accessible_toilet_count: i64,
    pub staff_count: Option<i64>,
    pub default_supply_start_day: i64,
    pub season: Option<String>,
    pub facility_conditions: Vec<String>,
    pub operational_conditions: OperationalConditions,
    /// In the order the request gave them.
    pub field_origins: Vec<(String, FieldOrigin)>,
}

/// The policy fields, their allowed values and the default (the only value the browser computes).
pub const POLICY_FIELDS: [(&str, &[&str], &str); 14] = [
    (
        "behavior_policy_id",
        &["utility-softmax-v1", "sleep-aware-utility-softmax-v2", "priority-rule-v1"],
        "sleep-aware-utility-softmax-v2",
    ),
    (
        "social_policy_id",
        &["sparse-family-zone-contact-v1", "degroot-contact-v1", "expressiveness-channel-openness-v1"],
        "expressiveness-channel-openness-v1",
    ),
    (
        "information_policy_id",
        &["sparse-information-contact-v1", "hazard-information-v1", "evidence-aware-rumor-v1"],
        "evidence-aware-rumor-v1",
    ),
    ("queue_policy_id", &["need-weighted-physical-queue-v1", "strict-fifo-queue-v1"], "need-weighted-physical-queue-v1"),
    (
        "relationship_policy_id",
        &["distance-duration-repeat-trust-v1", "duration-contact-trust-v1"],
        "distance-duration-repeat-trust-v1",
    ),
    ("social_proximity_policy_id", &["disabled-proximity-v1", "distance-sampled-proximity-v1"], "disabled-proximity-v1"),
    (
        "body_dynamics_policy_id",
        &["body-feedback-complete-v1", "body-feedback-complete-v2", "body-feedback-sleep-debt-v1"],
        "body-feedback-complete-v2",
    ),
    ("staff_operations_policy_id", &["staff-urgency-first-v1", "staff-travel-first-v1"], "staff-urgency-first-v1"),
    ("staff_fatigue_policy_id", &["staff-fatigue-linear-v1", "staff-fatigue-saturating-v1"], "staff-fatigue-linear-v1"),
    (
        "zone_temperature_policy_id",
        &["zone-temperature-comfort-band-v1", "zone-temperature-midpoint-v1"],
        "zone-temperature-comfort-band-v1",
    ),
    ("zone_rest_policy_id", &["zone-rest-multiplicative-v1", "zone-rest-limiting-v1"], "zone-rest-multiplicative-v1"),
    ("zone_hygiene_policy_id", &["zone-hygiene-linear-v1", "zone-hygiene-quadratic-v1"], "zone-hygiene-linear-v1"),
    ("zone_privacy_policy_id", &["zone-privacy-threshold-v1", "zone-privacy-proportional-v1"], "zone-privacy-threshold-v1"),
    (
        "care_outcome_policy_id",
        &["hygiene-support-fixed-relief-v1", "hygiene-support-proportional-relief-v1", "typed-care-support-response-v1"],
        "typed-care-support-response-v1",
    ),
];

/// The request fields `field_origins` may name (the model validator's `allowed`).
const ORIGIN_FIELDS: [&str; 36] = [
    "municipality",
    "municipality_code",
    "disaster",
    "evacuees",
    "duration_days",
    "gross_area_m2",
    "accessible_toilet_count",
    "staff_count",
    "default_supply_start_day",
    "season",
    "facility_conditions",
    "seed",
    "behavior_policy_id",
    "social_policy_id",
    "information_policy_id",
    "social_propagation_enabled",
    "information_spread_enabled",
    "facility_zones",
    "zone_area_shares",
    "operational_conditions",
    "evacuation_selection",
    "population_conditions",
    "target_date",
    "weather_area_code",
    "queue_policy_id",
    "relationship_policy_id",
    "social_proximity_policy_id",
    "body_dynamics_policy_id",
    "staff_operations_policy_id",
    "staff_fatigue_policy_id",
    "zone_temperature_policy_id",
    "zone_rest_policy_id",
    "zone_hygiene_policy_id",
    "zone_privacy_policy_id",
    "care_outcome_policy_id",
    "model_parameter_overrides",
];

/// An empty default of a field the browser does not compute.
#[derive(Clone, Copy)]
enum Empty {
    Null,
    List,
    Map,
}

impl Empty {
    fn value(self) -> V {
        match self {
            Empty::Null => V::Null,
            Empty::List => V::List(Vec::new()),
            Empty::Map => V::Map(IndexMap::new()),
        }
    }

    fn holds(self, value: &V) -> bool {
        match (self, value) {
            (Empty::Null, V::Null) => true,
            (Empty::List, V::List(items)) => items.is_empty(),
            (Empty::Map, V::Map(items)) => items.is_empty(),
            _ => false,
        }
    }
}

const EVACUATION_SELECTION: [(&str, Empty); 2] = [("age_band_multipliers", Empty::Map), ("source_ids", Empty::List)];
const POPULATION_CONDITIONS: [(&str, Empty); 11] = [
    ("medical_support_rate", Empty::Null),
    ("medication_support_rate", Empty::Null),
    ("care_support_need_rates", Empty::Map),
    ("pet_household_rate", Empty::Null),
    ("pet_count_weights", Empty::Map),
    ("information_access_weights", Empty::Map),
    ("language_weights", Empty::Map),
    ("additional_access_need_rates", Empty::Map),
    ("daily_rhythms", Empty::List),
    ("household_composition", Empty::Null),
    ("source_ids", Empty::List),
];
/// `OperationalConditions` but `initial_resources`.
const OPERATIONAL_REST: [(&str, Empty); 6] = [
    ("equipment", Empty::List),
    ("scheduled", Empty::List),
    ("staff_shifts", Empty::List),
    ("infrastructure", Empty::Null),
    ("source_geographic_scope", Empty::Null),
    ("source_period", Empty::Null),
];

/// An int pydantic read: a big one only needs its sign, as every int field the browser computes
/// has bounds (but `seed`, whose big values are unsupported).
#[derive(Clone, Copy, Debug, PartialEq)]
enum Int {
    Small(i64),
    BigPositive,
    BigNegative,
}

/// Pydantic's int in lax mode, of a value `json.loads` read; None where it raises.
fn int_of(value: &V) -> Option<Int> {
    match value {
        V::Int(number) => Some(Int::Small(*number)),
        V::Big(text) => Some(if text.starts_with('-') { Int::BigNegative } else { Int::BigPositive }),
        V::Bool(flag) => Some(Int::Small(i64::from(*flag))),
        V::Float(number) => {
            // A float with no fraction that fits an i64 (9.2e18 is read, 9.3e18 is not).
            if number.is_finite() && number.fract() == 0.0 && *number >= -9_223_372_036_854_775_808.0 && *number < 9_223_372_036_854_775_808.0 {
                Some(Int::Small(*number as i64))
            } else {
                None
            }
        }
        V::Str(text) => int_of_str(text),
        _ => None,
    }
}

/// pydantic-core's `str_as_int`: the text without white space around it, as an int, or with a
/// point and only zeros after it (`"12.000"`), the part before the point.
fn int_of_str(text: &str) -> Option<Int> {
    let text = text.trim();
    if text.len() > 4300 {
        return None;
    }
    int_text(text).or_else(|| {
        let (before, after) = text.split_once('.')?;
        if !after.is_empty() && after.bytes().all(|byte| byte == b'0') {
            int_text(before)
        } else {
            None
        }
    })
}

/// An int as Python's `int()` reads ASCII text: a sign, then digits with single underscores
/// between them.
fn int_text(text: &str) -> Option<Int> {
    let (negative, digits) = match text.as_bytes().first()? {
        b'-' => (true, &text[1..]),
        b'+' => (false, &text[1..]),
        _ => (false, text),
    };
    let bytes = digits.as_bytes();
    if bytes.is_empty() || !bytes[0].is_ascii_digit() || !bytes[bytes.len() - 1].is_ascii_digit() {
        return None;
    }
    let mut magnitude: Option<i64> = Some(0);
    let mut previous_underscore = false;
    for &byte in bytes {
        if byte == b'_' {
            if previous_underscore {
                return None;
            }
            previous_underscore = true;
            continue;
        }
        if !byte.is_ascii_digit() {
            return None;
        }
        previous_underscore = false;
        magnitude = magnitude.and_then(|number| number.checked_mul(10)?.checked_add(i64::from(byte - b'0')));
    }
    Some(match (magnitude, negative) {
        (Some(number), false) => Int::Small(number),
        (Some(number), true) => Int::Small(-number),
        // -9223372036854775808 is big here; no bounded field reaches it.
        (None, false) => Int::BigPositive,
        (None, true) => Int::BigNegative,
    })
}

/// Pydantic's float in lax mode with `allow_inf_nan=False`; None where it raises.
fn float_of(value: &V) -> Option<f64> {
    let number = match value {
        V::Float(number) => *number,
        V::Int(number) => *number as f64,
        V::Big(text) => text.parse::<f64>().ok()?,
        V::Bool(flag) => f64::from(u8::from(*flag)),
        V::Str(text) => float_of_str(text)?,
        _ => return None,
    };
    number.is_finite().then_some(number)
}

/// pydantic-core's float of a str: Rust's parse of the text without white space around it, or,
/// when that fails, of the whole text without its underscores (none first, last or doubled).
fn float_of_str(text: &str) -> Option<f64> {
    if let Ok(number) = text.trim().parse::<f64>() {
        return Some(number);
    }
    if !text.contains('_') || text.starts_with('_') || text.ends_with('_') || text.contains("__") {
        return None;
    }
    text.replace('_', "").parse::<f64>().ok()
}

/// Pydantic's bool in lax mode.
fn bool_of(value: &V) -> Option<bool> {
    match value {
        V::Bool(flag) => Some(*flag),
        V::Int(0) => Some(false),
        V::Int(1) => Some(true),
        V::Float(number) if *number == 0.0 => Some(false),
        V::Float(number) if *number == 1.0 => Some(true),
        V::Str(text) => match text.to_ascii_lowercase().as_str() {
            "0" | "off" | "f" | "false" | "n" | "no" => Some(false),
            "1" | "on" | "t" | "true" | "y" | "yes" => Some(true),
            _ => None,
        },
        _ => None,
    }
}

/// The first reason Python would refuse the request and the first field the browser does not
/// compute, while every field is checked.
#[derive(Default)]
struct Checks {
    invalid: Option<String>,
    unsupported: Option<String>,
}

impl Checks {
    fn invalid(&mut self, text: impl Into<String>) {
        if self.invalid.is_none() {
            self.invalid = Some(text.into());
        }
    }

    fn unsupported(&mut self, field: &str) {
        if self.unsupported.is_none() {
            self.unsupported = Some(field.to_string());
        }
    }

    fn outcome<T>(self, value: T) -> Result<T, Refusal> {
        if let Some(text) = self.invalid {
            Err(Refusal::Invalid(text))
        } else if let Some(field) = self.unsupported {
            Err(Refusal::Unsupported(field))
        } else {
            Ok(value)
        }
    }

    /// A bounded int field; None when it is refused.
    fn int(&mut self, field: &str, value: &V, low: i64, high: Option<i64>) -> Option<i64> {
        let Some(number) = int_of(value) else {
            self.invalid(format!("{field}: not an integer"));
            return None;
        };
        match number {
            Int::Small(number) if number >= low && high.is_none_or(|high| number <= high) => Some(number),
            Int::BigPositive if high.is_none() => {
                self.unsupported(field);
                None
            }
            _ => {
                self.invalid(format!("{field}: out of range"));
                None
            }
        }
    }

    /// A float field with `gt=0` and an upper bound.
    fn positive_float(&mut self, field: &str, value: &V, high: f64) -> Option<f64> {
        match float_of(value) {
            Some(number) if number > 0.0 && number <= high => Some(number),
            Some(_) => {
                self.invalid(format!("{field}: out of range"));
                None
            }
            None => {
                self.invalid(format!("{field}: not a finite number"));
                None
            }
        }
    }

    /// A str field with bounds on its length in characters.
    fn text(&mut self, field: &str, value: &V, low: usize, high: usize) -> Option<String> {
        let V::Str(text) = value else {
            self.invalid(format!("{field}: not a string"));
            return None;
        };
        let count = text.chars().count();
        if count < low || count > high {
            self.invalid(format!("{field}: length out of range"));
            return None;
        }
        Some(text.to_string())
    }

    /// A model the browser computes only with empty fields: the request's map at `field`.
    fn empty_model(&mut self, field: &str, value: &V, fields: &[(&str, Empty)]) {
        let V::Map(map) = value else {
            self.invalid(format!("{field}: not an object"));
            return;
        };
        for (key, item) in map {
            match fields.iter().find(|(name, _)| *name == &**key) {
                None => self.invalid(format!("{field}.{key}: extra field")),
                Some((_, empty)) if !empty.holds(item) => self.unsupported(&format!("{field}.{key}")),
                Some(_) => {}
            }
        }
    }
}

/// `str | None` of a request field.
fn optional_text(checks: &mut Checks, field: &str, value: Option<&V>, high: usize) -> Option<String> {
    match value {
        None | Some(V::Null) => None,
        Some(value) => checks.text(field, value, 0, high),
    }
}

/// `municipality_code: str | None` with `pattern=r"^\d{5}$"`. Pydantic's `\d` takes any decimal
/// digit of Unicode; the browser takes ASCII digits and leaves five other numeric characters
/// unsupported.
fn municipality_code(checks: &mut Checks, value: Option<&V>) -> Option<String> {
    let text = match value {
        None | Some(V::Null) => return None,
        Some(V::Str(text)) => text,
        Some(_) => {
            checks.invalid("municipality_code: not a string");
            return None;
        }
    };
    let count = text.chars().count();
    if count == 5 && text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Some(text.to_string());
    }
    if count == 5 && text.chars().all(char::is_numeric) {
        checks.unsupported("municipality_code");
    } else {
        checks.invalid("municipality_code: not five digits");
    }
    None
}

/// `InitialResourceCondition` and its validator.
fn initial_resources(checks: &mut Checks, value: &V) -> InitialResources {
    let mut resources = InitialResources::default();
    let V::Map(map) = value else {
        checks.invalid("operational_conditions.initial_resources: not an object");
        return resources;
    };
    let mut valid = true;
    let mut quantity = |checks: &mut Checks, name: &str, item: &V| -> Option<i64> {
        if matches!(item, V::Null) {
            return None;
        }
        let field = format!("initial_resources.{name}");
        let number = checks.int(&field, item, 0, None);
        if number.is_none() {
            valid = false;
        }
        number
    };
    let mut source_ids: Vec<String> = Vec::new();
    let mut ids_valid = true;
    for (key, item) in map {
        match &**key {
            "status" => match item {
                V::Str(text) if &**text == "unknown" => resources.status = InventoryStatus::Unknown,
                V::Str(text) if &**text == "incomplete" => resources.status = InventoryStatus::Incomplete,
                V::Str(text) if &**text == "entered" => resources.status = InventoryStatus::Entered,
                _ => checks.invalid("initial_resources.status: not a known status"),
            },
            "water_ml" => resources.water_ml = quantity(checks, "water_ml", item),
            "food_meal_units" => resources.food_meal_units = quantity(checks, "food_meal_units", item),
            "warehouse_id" => match item {
                V::Str(text) if &**text == DEFAULT_WAREHOUSE_ID => {}
                V::Str(_) => checks.unsupported("operational_conditions.initial_resources.warehouse_id"),
                _ => checks.invalid("initial_resources.warehouse_id: not a string"),
            },
            "zone_id" => match item {
                V::Null => {}
                V::Str(_) => checks.unsupported("operational_conditions.initial_resources.zone_id"),
                _ => checks.invalid("initial_resources.zone_id: not a string"),
            },
            "source_ids" => match item {
                V::List(items) => {
                    for id in items {
                        match id {
                            V::Str(text) => source_ids.push(text.to_string()),
                            _ => {
                                checks.invalid("initial_resources.source_ids: not a string");
                                ids_valid = false;
                            }
                        }
                    }
                }
                _ => {
                    checks.invalid("initial_resources.source_ids: not a list");
                    ids_valid = false;
                }
            },
            "source_kind" => match item {
                V::Str(text) if &**text == "operator" => {}
                V::Str(text) if ["official", "municipal", "assumption", "inferred"].contains(&&**text) => {
                    checks.unsupported("operational_conditions.initial_resources.source_kind")
                }
                _ => checks.invalid("initial_resources.source_kind: not a known kind"),
            },
            other => checks.invalid(format!("initial_resources.{other}: extra field")),
        }
    }
    if !valid || !ids_valid {
        return resources;
    }
    // `inventory_status_is_explicit`.
    let quantities = [resources.water_ml, resources.food_meal_units];
    let entered = quantities.iter().filter(|value| value.is_some()).count();
    match resources.status {
        InventoryStatus::Unknown if entered > 0 => checks.invalid("unknown initial inventory cannot contain quantities"),
        InventoryStatus::Entered if entered < 2 => {
            checks.invalid("entered initial inventory requires both water and food quantities")
        }
        InventoryStatus::Incomplete if entered == 0 => {
            checks.invalid("incomplete initial inventory must preserve at least one entered quantity")
        }
        _ => {}
    }
    if source_ids.is_empty() && resources.status != InventoryStatus::Unknown && entered > 0 {
        source_ids.push(OPERATOR_CONDITION_SOURCE_ID.to_string());
    }
    let mut seen: Vec<&str> = Vec::new();
    for id in &source_ids {
        if seen.contains(&id.as_str()) {
            checks.invalid("initial inventory source IDs must be unique");
            return resources;
        }
        seen.push(id);
    }
    // `OperationalConditions.unique_targets` asks the IDs to be stable; the browser computes only
    // the operator's own source.
    if source_ids.iter().any(|id| id != OPERATOR_CONDITION_SOURCE_ID) {
        if source_ids.iter().all(|id| stable_id(id)) {
            checks.unsupported("operational_conditions.initial_resources.source_ids");
        } else {
            checks.invalid("condition source IDs must be stable identifiers");
        }
    }
    resources.source_ids = source_ids;
    resources
}

/// `[A-Za-z0-9][A-Za-z0-9._:-]{0,63}` in full.
fn stable_id(text: &str) -> bool {
    let bytes = text.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && bytes[0].is_ascii_alphanumeric()
        && bytes[1..].iter().all(|byte| byte.is_ascii_alphanumeric() || b"._:-".contains(byte))
}

fn operational_conditions(checks: &mut Checks, value: &V) -> OperationalConditions {
    let mut conditions = OperationalConditions::default();
    let V::Map(map) = value else {
        checks.invalid("operational_conditions: not an object");
        return conditions;
    };
    let mut rest: IndexMap<Arc<str>, V> = IndexMap::new();
    for (key, item) in map {
        if &**key == "initial_resources" {
            conditions.initial_resources = initial_resources(checks, item);
        } else {
            rest.insert(key.clone(), item.clone());
        }
    }
    checks.empty_model("operational_conditions", &V::Map(rest), &OPERATIONAL_REST);
    conditions
}

impl InitialScenarioRequest {
    /// `InitialScenarioRequest.model_validate` of the JSON text of a request.
    pub fn of_json(raw: &[u8]) -> Result<Self, Refusal> {
        let parsed: serde_json::Value =
            serde_json::from_slice(raw).map_err(|error| Refusal::Invalid(format!("not JSON: {error}")))?;
        let value = V::from_json(&parsed).ok_or_else(|| Refusal::Invalid("a number Python cannot read".into()))?;
        Self::of_value(&value)
    }

    /// `InitialScenarioRequest.model_validate` of what `json.loads` read.
    pub fn of_value(value: &V) -> Result<Self, Refusal> {
        let V::Map(map) = value else {
            return Err(Refusal::Invalid("the request is not an object".into()));
        };
        let mut checks = Checks::default();
        let mut request = InitialScenarioRequest {
            municipality: String::new(),
            municipality_code: None,
            disaster: String::new(),
            evacuees: 1,
            duration_days: 7.0,
            seed: 2026,
            gross_area_m2: None,
            accessible_toilet_count: 1,
            staff_count: None,
            default_supply_start_day: 4,
            season: None,
            facility_conditions: Vec::new(),
            operational_conditions: OperationalConditions::default(),
            field_origins: Vec::new(),
        };
        for required in ["municipality", "disaster", "evacuees"] {
            if !map.contains_key(required) {
                checks.invalid(format!("{required}: missing"));
            }
        }
        for (key, item) in map {
            let field = &**key;
            match field {
                "municipality" => request.municipality = checks.text(field, item, 1, 160).unwrap_or_default(),
                "municipality_code" => request.municipality_code = municipality_code(&mut checks, Some(item)),
                "disaster" => request.disaster = checks.text(field, item, 1, 80).unwrap_or_default(),
                "evacuees" => request.evacuees = checks.int(field, item, 1, Some(10_000)).unwrap_or(1),
                "duration_days" => request.duration_days = checks.positive_float(field, item, 90.0).unwrap_or(7.0),
                "seed" => request.seed = checks.int(field, item, 0, None).unwrap_or(2026),
                "gross_area_m2" => {
                    request.gross_area_m2 =
                        if matches!(item, V::Null) { None } else { checks.positive_float(field, item, 100_000.0) }
                }
                "accessible_toilet_count" => {
                    request.accessible_toilet_count = checks.int(field, item, 0, Some(20)).unwrap_or(1)
                }
                "staff_count" => {
                    request.staff_count = if matches!(item, V::Null) { None } else { checks.int(field, item, 0, Some(100)) }
                }
                "default_supply_start_day" => {
                    request.default_supply_start_day = checks.int(field, item, 1, Some(91)).unwrap_or(4)
                }
                "season" => request.season = optional_text(&mut checks, field, Some(item), 40),
                "facility_conditions" => match item {
                    V::List(items) => {
                        for condition in items {
                            if let Some(text) = checks.text(field, condition, 0, usize::MAX) {
                                request.facility_conditions.push(text);
                            }
                        }
                    }
                    _ => checks.invalid("facility_conditions: not a list"),
                },
                "operational_conditions" => request.operational_conditions = operational_conditions(&mut checks, item),
                "field_origins" => match item {
                    V::Map(origins) => {
                        for (name, origin) in origins {
                            let origin = match origin {
                                V::Str(text) if &**text == "user" => FieldOrigin::User,
                                V::Str(text) if &**text == "llm_extracted" => FieldOrigin::LlmExtracted,
                                V::Str(text) if &**text == "default" => FieldOrigin::Default,
                                _ => {
                                    checks.invalid(format!("field_origins.{name}: not a known origin"));
                                    continue;
                                }
                            };
                            request.field_origins.push((name.to_string(), origin));
                        }
                    }
                    _ => checks.invalid("field_origins: not an object"),
                },
                "social_propagation_enabled" | "information_spread_enabled" => match bool_of(item) {
                    Some(true) => {}
                    Some(false) => checks.unsupported(field),
                    None => checks.invalid(format!("{field}: not a boolean")),
                },
                "model_parameter_overrides" => match item {
                    V::Null => {}
                    V::Map(items) if items.is_empty() => {}
                    V::Map(_) => checks.unsupported(field),
                    _ => checks.invalid("model_parameter_overrides: not an object"),
                },
                "target_date" | "weather_area_code" => {
                    if !matches!(item, V::Null) {
                        checks.unsupported(field)
                    }
                }
                "facility_zones" => match item {
                    V::List(items) if items.is_empty() => {}
                    V::List(_) => checks.unsupported(field),
                    _ => checks.invalid("facility_zones: not a list"),
                },
                "zone_area_shares" => match item {
                    V::Map(items) if items.is_empty() => {}
                    V::Map(_) => checks.unsupported(field),
                    _ => checks.invalid("zone_area_shares: not an object"),
                },
                "evacuation_selection" => checks.empty_model(field, item, &EVACUATION_SELECTION),
                "population_conditions" => checks.empty_model(field, item, &POPULATION_CONDITIONS),
                _ => match POLICY_FIELDS.iter().find(|(name, _, _)| *name == field) {
                    Some((_, allowed, default)) => match item {
                        V::Str(text) if &**text == *default => {}
                        V::Str(text) if allowed.contains(&&**text) => checks.unsupported(field),
                        _ => checks.invalid(format!("{field}: not an allowed policy")),
                    },
                    None => checks.invalid(format!("{field}: extra field")),
                },
            }
        }
        if checks.invalid.is_none() {
            // `duration_is_whole_minutes`.
            if crate::mathx::py_round_int(request.duration_days * 1440.0).is_none_or(|minutes| minutes < 1) {
                checks.invalid("duration must be at least one minute");
            }
            if request.field_origins.iter().any(|(name, _)| !ORIGIN_FIELDS.contains(&name.as_str())) {
                checks.invalid("field_origins contains an unknown request field");
            }
        }
        checks.outcome(request)
    }

    /// The scenario's length in minutes, `round(duration_days * 1440)`.
    pub fn duration_minutes(&self) -> i64 {
        crate::mathx::py_round_int(self.duration_days * 1440.0).unwrap_or(0)
    }

    /// `model_dump(mode="json")`, the fields in the model's order.
    pub fn dump(&self) -> V {
        fn text(value: &str) -> V {
            V::Str(Arc::from(value))
        }
        fn optional<T>(value: &Option<T>, make: impl Fn(&T) -> V) -> V {
            value.as_ref().map_or(V::Null, make)
        }
        fn map(entries: Vec<(&str, V)>) -> V {
            V::Map(entries.into_iter().map(|(key, value)| (Arc::from(key), value)).collect())
        }
        fn empties(fields: &[(&str, Empty)]) -> V {
            map(fields.iter().map(|(name, empty)| (*name, empty.value())).collect())
        }
        let resources = &self.operational_conditions.initial_resources;
        let initial_resources = map(vec![
            ("status", text(resources.status.text())),
            ("water_ml", optional(&resources.water_ml, |number| V::Int(*number))),
            ("food_meal_units", optional(&resources.food_meal_units, |number| V::Int(*number))),
            ("warehouse_id", text(DEFAULT_WAREHOUSE_ID)),
            ("zone_id", V::Null),
            ("source_ids", V::List(resources.source_ids.iter().map(|id| text(id)).collect())),
            ("source_kind", text("operator")),
        ]);
        let mut operational = vec![("initial_resources", initial_resources)];
        operational.extend(OPERATIONAL_REST.iter().map(|(name, empty)| (*name, empty.value())));
        let policy = |name: &str| text(POLICY_FIELDS.iter().find(|(field, _, _)| *field == name).map_or("", |entry| entry.2));
        map(vec![
            ("municipality", text(&self.municipality)),
            ("municipality_code", optional(&self.municipality_code, |code| text(code))),
            ("disaster", text(&self.disaster)),
            ("evacuees", V::Int(self.evacuees)),
            ("duration_days", V::Float(self.duration_days)),
            ("seed", V::Int(self.seed)),
            ("behavior_policy_id", policy("behavior_policy_id")),
            ("social_policy_id", policy("social_policy_id")),
            ("information_policy_id", policy("information_policy_id")),
            ("social_propagation_enabled", V::Bool(true)),
            ("information_spread_enabled", V::Bool(true)),
            ("queue_policy_id", policy("queue_policy_id")),
            ("relationship_policy_id", policy("relationship_policy_id")),
            ("social_proximity_policy_id", policy("social_proximity_policy_id")),
            ("body_dynamics_policy_id", policy("body_dynamics_policy_id")),
            ("staff_operations_policy_id", policy("staff_operations_policy_id")),
            ("staff_fatigue_policy_id", policy("staff_fatigue_policy_id")),
            ("zone_temperature_policy_id", policy("zone_temperature_policy_id")),
            ("zone_rest_policy_id", policy("zone_rest_policy_id")),
            ("zone_hygiene_policy_id", policy("zone_hygiene_policy_id")),
            ("zone_privacy_policy_id", policy("zone_privacy_policy_id")),
            ("care_outcome_policy_id", policy("care_outcome_policy_id")),
            ("model_parameter_overrides", V::Map(IndexMap::new())),
            ("gross_area_m2", optional(&self.gross_area_m2, |area| V::Float(*area))),
            ("accessible_toilet_count", V::Int(self.accessible_toilet_count)),
            ("staff_count", optional(&self.staff_count, |count| V::Int(*count))),
            ("default_supply_start_day", V::Int(self.default_supply_start_day)),
            ("season", optional(&self.season, |season| text(season))),
            ("target_date", V::Null),
            ("weather_area_code", V::Null),
            ("facility_conditions", V::List(self.facility_conditions.iter().map(|condition| text(condition)).collect())),
            ("facility_zones", V::List(Vec::new())),
            ("zone_area_shares", V::Map(IndexMap::new())),
            ("operational_conditions", map(operational)),
            ("evacuation_selection", empties(&EVACUATION_SELECTION)),
            ("population_conditions", empties(&POPULATION_CONDITIONS)),
            (
                "field_origins",
                V::Map(self.field_origins.iter().map(|(name, origin)| (Arc::from(name.as_str()), text(origin.text()))).collect()),
            ),
        ])
    }

    /// The dump's JSON as the server writes it and as the form's `input_sha256` hashes it:
    /// `json.dumps(sort_keys=True, separators=(",", ":"), ensure_ascii=False)`.
    pub fn dump_json(&self) -> Vec<u8> {
        // Every float of the dump is finite: validation refused the others.
        self.dump().to_json().unwrap_or_default()
    }

    /// The scenario input's sha256 of a form request (`workspace.py`'s `structured_input`).
    pub fn input_sha256(&self) -> String {
        crate::checkpoint::sha256_hex(&self.dump_json())
    }
}
