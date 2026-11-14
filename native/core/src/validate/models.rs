//! The pydantic models `WorldState.validate` reads, built with the contracts' pydantic
//! (`contracts::pydantic`, #346): `ResourceServiceConfig` (`experiments/scenario.py`), whose
//! `ValidationError` text a refusal shows, and `PresenceState` (`contracts/presence.py`), of which
//! only whether it passes counts.

use std::sync::Arc;

use crate::contracts::pydantic::{self, Ctx, Field, Fields, FloatS, In, IntS, Len, Lit, Model, Pattern, StrS, S};
use crate::error::{Error, Result};
use crate::value::V;

/// `StableId` of experiments/scenario.py.
const STABLE_ID: S = S::Str(StrS { strip: true, min: Some(1), max: Some(64), pattern: Some(Pattern::Stable) });
/// `Identifier` of contracts/extensions.py.
const IDENT: S = S::Str(StrS { strip: false, min: None, max: None, pattern: Some(Pattern::Identifier) });

const fn int(gt: i64, le: i64) -> S {
    S::Int(IntS { strict: false, ge: None, gt: Some(gt), le: Some(le) })
}

const fn float(ge: Option<f64>, gt: Option<f64>, le: f64) -> S {
    S::Float(FloatS { strict: false, finite: true, ge, gt, le: Some(le) })
}

const fn strict_count() -> S {
    S::Int(IntS { strict: true, ge: Some(0), gt: None, le: None })
}

const fn required(name: &'static str, s: S) -> Field {
    Field { name, s, default: None }
}

const fn with(name: &'static str, s: S, default: fn() -> V) -> Field {
    Field { name, s, default: Some(default) }
}

macro_rules! float_default {
    ($name:ident, $value:expr) => {
        fn $name() -> V {
            V::Float($value)
        }
    };
}

float_default!(hydration_action_threshold, 0.25);
float_default!(hunger_action_threshold, 0.7);
float_default!(hydration_relief, 0.35);
float_default!(hunger_relief, 0.5);
float_default!(utility_base, 0.18);
float_default!(water_need_weight, 1.0);
float_default!(meal_need_weight, 0.85);

fn water_service() -> V {
    V::Str(Arc::from("water_service"))
}

fn meal_service() -> V {
    V::Str(Arc::from("meal_service"))
}

fn null() -> V {
    V::Null
}

fn empty() -> V {
    V::List(Vec::new())
}

fn version(number: i64) -> V {
    V::Int(number)
}

pub static RESOURCE_SERVICE_CONFIG: Model = Model {
    name: "ResourceServiceConfig",
    fields: &[
        required("schema_version", S::Lit(&[Lit::Int(1)])),
        required("policy_version", S::Lit(&[Lit::Str("resource-service-v1")])),
        required("source_id", STABLE_ID),
        required("source_type", S::Lit(&[Lit::Str("assumption")])),
        required("service_minutes", int(0, 1440)),
        required("water_ml_per_service", int(0, 100_000)),
        required("food_meal_units_per_service", int(0, 100)),
        required("hydration_growth_per_minute", float(Some(0.0), None, 1.0)),
        with("hydration_action_threshold", float(Some(0.0), None, 1.0), hydration_action_threshold),
        with("hunger_action_threshold", float(Some(0.0), None, 1.0), hunger_action_threshold),
        with("hydration_relief", float(None, Some(0.0), 1.0), hydration_relief),
        with("hunger_relief", float(None, Some(0.0), 1.0), hunger_relief),
        with("water_utility_base", float(Some(0.0), None, 10.0), utility_base),
        with("water_utility_need_weight", float(Some(0.0), None, 10.0), water_need_weight),
        with("meal_utility_base", float(Some(0.0), None, 10.0), utility_base),
        with("meal_utility_need_weight", float(Some(0.0), None, 10.0), meal_need_weight),
        with("water_staff_skill", STABLE_ID, water_service),
        with("meal_staff_skill", STABLE_ID, meal_service),
    ],
    before: None,
    after: None,
    recursive: false,
};

/// `PresenceRecord.coherent_return`.
fn coherent_return(fields: &mut Fields) -> std::result::Result<(), String> {
    let status = match fields.get("status") {
        Some(V::Str(status)) => &**status,
        _ => "",
    };
    let since = match fields.get("since_minute") {
        Some(V::Int(since)) => *since,
        _ => 0,
    };
    if status == "temporarily_absent" && matches!(fields.get("expected_return_minute"), Some(V::Int(minute)) if *minute < since) {
        return Err("expected return precedes departure".to_owned());
    }
    if status == "admitted" && matches!(fields.get("transition_zone_id"), None | Some(V::Null)) {
        return Err("admitted presence requires a zone".to_owned());
    }
    if status == "temporarily_absent" && !fields.get("archive_path").is_some_and(V::is_truthy) {
        return Err("temporary absence requires a retained state reference".to_owned());
    }
    Ok(())
}

static PRESENCE_RECORD: Model = Model {
    name: "PresenceRecord",
    fields: &[
        required("agent_id", IDENT),
        with("household_id", S::Opt(&IDENT), null),
        required(
            "status",
            S::Lit(&[
                Lit::Str("admitted"),
                Lit::Str("approaching"),
                Lit::Str("awaiting_admission"),
                Lit::Str("temporarily_absent"),
                Lit::Str("departed"),
                Lit::Str("transferred"),
            ]),
        ),
        required("since_minute", strict_count()),
        with("transition_zone_id", S::Opt(&IDENT), null),
        with("return_zone_id", S::Opt(&IDENT), null),
        with("expected_return_minute", S::Opt(&strict_count()), null),
        with("archive_path", S::Opt(&S::Tuple(&IDENT, Len { min: Some(1), max: Some(5) })), null),
        with("source_event_ids", S::Tuple(&IDENT, Len { min: None, max: None }), empty),
    ],
    before: None,
    after: Some(coherent_return),
    recursive: false,
};

fn transition_version() -> V {
    version(2)
}

fn recorded_event_minute() -> V {
    V::Str(Arc::from("recorded_event_minute"))
}

/// `PresenceTransition`, but for `state_snapshot_hash`'s pattern (`^[a-f0-9]{64}$`), which
/// `presence_state` checks after.
static PRESENCE_TRANSITION: Model = Model {
    name: "PresenceTransition",
    fields: &[
        with("schema_version", S::Lit(&[Lit::Int(2)]), transition_version),
        required("transition_id", IDENT),
        required("event_id", IDENT),
        required("agent_id", IDENT),
        required("minute", strict_count()),
        required(
            "action",
            S::Lit(&[
                Lit::Str("arrived"),
                Lit::Str("approaching"),
                Lit::Str("approach_completed"),
                Lit::Str("waiting"),
                Lit::Str("admitted"),
                Lit::Str("departed"),
                Lit::Str("temporary_exit"),
                Lit::Str("returned"),
                Lit::Str("transferred"),
                Lit::Str("reunited"),
                Lit::Str("relocated"),
            ]),
        ),
        with("from_zone_id", S::Opt(&IDENT), null),
        with("to_zone_id", S::Opt(&IDENT), null),
        with("from_household_id", S::Opt(&IDENT), null),
        with("to_household_id", S::Opt(&IDENT), null),
        with("causation_id", S::Opt(&IDENT), null),
        with("source_ids", S::Tuple(&IDENT, Len { min: None, max: None }), empty),
        with("state_snapshot_hash", S::Opt(&S::Str(StrS { strip: false, min: None, max: None, pattern: None })), null),
        with("time_semantics", S::Lit(&[Lit::Str("recorded_event_minute")]), recorded_event_minute),
    ],
    before: None,
    after: None,
    recursive: false,
};

fn version_one() -> V {
    version(1)
}

static PRESENCE_STATE: Model = Model {
    name: "PresenceState",
    fields: &[
        with("schema_version", S::Lit(&[Lit::Int(1)]), version_one),
        required("people", S::Dict(&IDENT, &S::Model(&PRESENCE_RECORD))),
        with("history", S::Tuple(&S::Model(&PRESENCE_TRANSITION), Len { min: None, max: Some(20_000) }), empty),
    ],
    before: None,
    after: None,
    recursive: false,
};

/// A `ValidationError` of a value the world holds: its dicts and lists are the world's tracked
/// trees, which the error names as their types (`input_type=TrackedDict`). Each error of the text
/// ends with its input's type and the line of its documentation, which a value's `repr` cannot
/// hold (a newline in it is escaped).
pub fn tracked(error: Error) -> Error {
    match error {
        Error::Value(text) => Error::Value(
            text.replace(", input_type=dict]\n    For further", ", input_type=TrackedDict]\n    For further")
                .replace(", input_type=list]\n    For further", ", input_type=TrackedList]\n    For further"),
        ),
        other => other,
    }
}

/// `model.model_validate(input).model_dump(mode="json")` of a value the world holds, or the
/// `ValidationError` (a `ValueError`) with pydantic's text.
pub fn validate_model(model: &'static Model, input: &V) -> Result<V> {
    pydantic::model(model, In::plain(input), &mut Ctx::default())
        .map(V::Map)
        .map_err(|errors| tracked(Error::Value(pydantic::text(model.name, &errors))))
}

/// `ResourceServiceConfig.model_validate(config).model_dump(mode="json")`, or its error.
pub fn resource_service_config(config: &V) -> Result<V> {
    validate_model(&RESOURCE_SERVICE_CONFIG, config)
}

/// `PresenceState.model_validate(raw).model_dump(mode="json")`, or a `ValueError` (its text is not
/// Python's: only the refusal counts).
pub fn presence_state(raw: &V) -> Result<V> {
    let dumped = validate_model(&PRESENCE_STATE, raw)?;
    let hash_fits = |hash: &V| match hash {
        V::Null => true,
        V::Str(hash) => hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        _ => false,
    };
    if let V::Map(state) = &dumped {
        if let Some(V::List(history)) = state.get("history") {
            for receipt in history {
                if let V::Map(receipt) = receipt {
                    if !receipt.get("state_snapshot_hash").is_none_or(hash_fits) {
                        return Err(Error::Value("state_snapshot_hash does not match its pattern".to_owned()));
                    }
                }
            }
        }
    }
    Ok(dumped)
}
