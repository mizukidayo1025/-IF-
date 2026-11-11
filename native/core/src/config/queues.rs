//! The section of the queue policy (lane 7-5a of Issue #152): `create_queue_policy` of the two
//! bundled policies, from `model_configs["facility_queues"]`, with the checks of their constructors.
//!
//! Which policy the configs select also turns on the model selection, which has its own section; so
//! this one keeps both policies, each read as it would be: the policy, the `ValueError` its
//! constructor raises (a fault of the class `Value`), or what the kernel refuses.

use super::read::{int, map, Read};
use super::section::{Fault, FaultClass};
use crate::value::V;

pub const LIMIT_TOILET: i64 = 120;
pub const LIMIT_WATER: i64 = 240;
pub const LIMIT_MEAL: i64 = 360;
pub const PERSISTENCE_THRESHOLD: f64 = 0.8;
pub const RETRY_DELAY: i64 = 15;

/// `create_queue_policy`, for the two bundled policies.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum QueuePolicy {
    Strict,
    NeedWeighted { limits: [i64; 3], threshold: f64, retry: i64 },
}

/// What the queue rules read of the configs.
#[derive(Clone, PartialEq, Debug)]
pub struct QueueConfig {
    /// `"facility_geometry" in model_configs`: the need-weighted policy where no model is selected.
    pub has_geometry: bool,
    pub strict: Read<QueuePolicy>,
    pub need_weighted: Read<QueuePolicy>,
}

impl QueueConfig {
    /// `exact_dict(model_configs)`, then each policy as it would be read.
    pub fn parse(configs: &V) -> Read<Self> {
        let configs = map(configs)?;
        let raw = configs.get("facility_queues");
        Ok(Self {
            has_geometry: configs.contains_key("facility_geometry"),
            strict: policy(raw, false),
            need_weighted: policy(raw, true),
        })
    }

    pub fn policy(&self, need_weighted: bool) -> &Read<QueuePolicy> {
        if need_weighted {
            &self.need_weighted
        } else {
            &self.strict
        }
    }
}

/// `raise ValueError(text)` of the queue policy's constructor.
fn config_error<T>(text: &str) -> Read<T> {
    Err(Fault::new(FaultClass::Value, text))
}

const NOT_AN_OBJECT: &str = "facility_queues config must be an object";
const UNSUPPORTED_SERVICE: &str = "queue abandonment limits contain an unsupported service";

/// `exactly_int`: Some for an int (refused past i64), None for any other value.
fn exactly_int(value: &V) -> Read<Option<i64>> {
    match value {
        V::Int(_) | V::Big(_) => int(value).map(Some),
        _ => Ok(None),
    }
}

/// `real_number`: Some for an int or a float that is not a bool, refused for an int past a float.
fn real_number(value: &V) -> Read<Option<f64>> {
    match value {
        V::Float(number) => Ok(Some(*number)),
        V::Int(_) | V::Big(_) => {
            let number = int(value)?;
            if !(-(1 << 53)..=1 << 53).contains(&number) {
                return Err(Fault::refused("a number does not fit a float"));
            }
            Ok(Some(number as f64))
        }
        _ => Ok(None),
    }
}

/// Python's `value != 1`.
fn is_not_one(value: &V) -> bool {
    match value {
        V::Int(number) => *number != 1,
        V::Float(number) => *number != 1.0,
        V::Bool(flag) => !*flag,
        _ => true,
    }
}

/// The checks of `NeedWeightedPhysicalQueuePolicy.__init__` (or of the strict policy), in their order.
fn config_errors(raw: &V, need_weighted: bool) -> Read<()> {
    if matches!(raw, V::Null) {
        return Ok(());
    }
    let V::Map(config) = raw else {
        return config_error(NOT_AN_OBJECT);
    };
    if !need_weighted {
        return Ok(());
    }
    const FIELDS: [&str; 5] = [
        "schema_version",
        "queue_capacity_by_facility",
        "abandon_after_minutes_by_service",
        "retry_delay_minutes",
        "persistence_urgency_threshold",
    ];
    if config.keys().any(|key| !FIELDS.contains(&&**key)) {
        return config_error(NOT_AN_OBJECT);
    }
    if let Some(schema) = config.get("schema_version") {
        // Python's `!=`: `1.0` and `True` pass, as they did in the Python rule.
        if is_not_one(schema) {
            return config_error("facility_queues requires schema_version 1");
        }
    }
    let mut limits: Vec<&V> = Vec::new();
    if let Some(given) = config.get("abandon_after_minutes_by_service") {
        let V::Map(given) = given else {
            return config_error(UNSUPPORTED_SERVICE);
        };
        for (service, limit) in given {
            if !matches!(&**service, "toilet" | "water" | "meal") {
                return config_error(UNSUPPORTED_SERVICE);
            }
            limits.push(limit);
        }
    }
    let within = |value: &V, upper: i64| -> Read<bool> { Ok(exactly_int(value)?.is_some_and(|value| (1..=upper).contains(&value))) };
    for limit in limits {
        if !within(limit, 10080)? {
            return config_error("queue abandonment limits must be within 1..10080 minutes");
        }
    }
    if let Some(retry) = config.get("retry_delay_minutes") {
        if !within(retry, 1440)? {
            return config_error("queue retry delay must be between 1 and 1440 minutes");
        }
    }
    if let Some(threshold) = config.get("persistence_urgency_threshold") {
        if !real_number(threshold)?.is_some_and(|value| (0.0..=1.0).contains(&value)) {
            return config_error("queue persistence threshold must be normalized to 0..1");
        }
    }
    Ok(())
}

/// The policy of `raw` (`model_configs.get("facility_queues")`, None for no key).
fn policy(raw: Option<&V>, need_weighted: bool) -> Read<QueuePolicy> {
    // The bundled factories build the same two classes from the same config.
    if let Some(raw) = raw {
        config_errors(raw, need_weighted)?;
    }
    let Some(raw) = raw else {
        return Ok(if need_weighted {
            QueuePolicy::NeedWeighted { limits: [LIMIT_TOILET, LIMIT_WATER, LIMIT_MEAL], threshold: PERSISTENCE_THRESHOLD, retry: RETRY_DELAY }
        } else {
            QueuePolicy::Strict
        });
    };
    let V::Map(config) = raw else {
        return Err(Fault::refused("the queue config is not a dict"));
    };
    if !need_weighted {
        return Ok(QueuePolicy::Strict);
    }
    let (mut limits, mut threshold) = ([LIMIT_TOILET, LIMIT_WATER, LIMIT_MEAL], PERSISTENCE_THRESHOLD);
    let mut retry = None;
    for (key, value) in config {
        match &**key {
            // `config_errors` has passed it.
            "schema_version" | "queue_capacity_by_facility" => {}
            "abandon_after_minutes_by_service" => {
                let V::Map(given) = value else {
                    return Err(Fault::refused("the queue limits are not a dict"));
                };
                for (service, limit) in given {
                    let place = match &**service {
                        "toilet" => 0,
                        "water" => 1,
                        "meal" => 2,
                        _ => return Err(Fault::refused("a limit of another service")),
                    };
                    limits[place] = int(limit)?;
                }
            }
            "retry_delay_minutes" => retry = Some(int(value)?),
            "persistence_urgency_threshold" => {
                threshold = match value {
                    V::Float(number) => *number,
                    _ => match int(value)? {
                        0 => 0.0,
                        1 => 1.0,
                        _ => return Err(Fault::refused("the threshold is not normalized")),
                    },
                };
            }
            _ => return Err(Fault::refused("the queue config has another field")),
        }
    }
    let good_limit = |limit: &i64| (1..=10080).contains(limit);
    let good_retry = retry.is_none_or(|retry| (1..=1440).contains(&retry));
    // As written, for a NaN.
    if !limits.iter().all(good_limit) || !good_retry || !(0.0..=1.0).contains(&threshold) {
        return Err(Fault::refused("the queue config is out of its bounds"));
    }
    Ok(QueuePolicy::NeedWeighted { limits, threshold, retry: retry.unwrap_or(RETRY_DELAY) })
}
