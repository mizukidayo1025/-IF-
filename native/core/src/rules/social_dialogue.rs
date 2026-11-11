//! `domain/social_dialogue.py`: the participants' modeled state a social event carries. Both
//! kernels only read and build, and raise the Python rules' ValueError texts themselves (their Python
//! bodies are gone: `kernel_only`). Moved to the core in lane P8 a2: a context is a `V` here, which
//! the PyO3 crate reads from the event's frozen payload or the caller's mapping.
//!
//! `virtual_dialogue_example` stays in Python: it runs when a history is read, not in a tick.

use std::sync::{Arc, LazyLock};

use indexmap::IndexMap;

use crate::entities::AgentRow;
use crate::error::{Error, Result};
use crate::rules::kit;
use crate::state::body_state::{BodyRow, NAME as BODY_STATE};
use crate::value::V;
use crate::world::World;

const NEEDS: [&str; 4] = ["toilet_urgency", "hunger", "fatigue", "hydration_need"];
const AFFECT: [&str; 6] = ["positive_affect", "negative_affect", "reassurance", "discontent", "uncertainty", "hygiene_need"];

/// The texts of `validate_social_state_context` and `capture_social_state_context`, in the order of
/// their checks.
pub const NOT_ITS_EVENT: &str = "social dialogue state context does not match its event";
pub const STATE_FIELDS: &str = "social dialogue participant state has unsupported fields";
pub const NEEDS_INVALID: &str = "social dialogue needs context is invalid";
pub const AFFECT_INVALID: &str = "social dialogue affect context is invalid";
pub const NOT_UNIT: &str = "social dialogue state values must be finite and within 0..1";
pub const NOT_ONE_OR_TWO: &str = "social dialogue context requires one or two current participants";

// ---- Capture --------------------------------------------------------------------------------------

/// A person's values in a row, which `capture_context` copies; the validation then reads them.
struct Captured {
    stress: f64,
    trust: f64,
    needs: [f64; 4],
}

impl Captured {
    fn of(row: &AgentRow) -> Self {
        Self {
            stress: row.stress,
            trust: row.trust,
            needs: [row.needs.toilet_urgency, row.needs.hunger, row.needs.fatigue, row.needs.hydration_need],
        }
    }

    fn in_bounds(&self) -> bool {
        [self.stress, self.trust].iter().chain(&self.needs).all(|value| (0.0..=1.0).contains(value))
    }
}

/// What `capture_social_state_context` copies: the participants, in sorted order.
struct Capture {
    minute: i64,
    people: Vec<Captured>,
    affects: Vec<Option<[f64; 6]>>,
    /// What `validate_social_state_context` then raises for the copy, or None.
    problem: Option<&'static str>,
}

/// `capture_social_state_context`, for one or two IDs of no repeats in sorted order.
fn read_capture(world: &World, ids: &[&str]) -> Result<Capture> {
    let minute = world.minute_index().map_err(|_| Error::unsupported("a value is not an int"))?;
    let duration = world.duration_minutes().map_err(|_| Error::unsupported("a value is not an int"))?;
    let mut people = Vec::with_capacity(ids.len());
    {
        let rows = world.rows()?;
        for id in ids {
            let Some(row) = rows.agents.get(id) else {
                return Err(Error::unsupported("a participant is not in the world"));
            };
            people.push(Captured::of(row));
        }
    }
    let affects = if kit::namespace_absent(world, BODY_STATE)? {
        vec![None; ids.len()]
    } else {
        // The row's records hold every value of AFFECT, as floats.
        let row = world.row::<BodyRow>(BODY_STATE)?;
        ids.iter()
            .map(|id| {
                row.agents.get(*id).map(|record| {
                    [
                        record.positive_affect,
                        record.negative_affect,
                        record.reassurance,
                        record.discontent,
                        record.uncertainty,
                        record.hygiene_need,
                    ]
                })
            })
            .collect()
    };
    let problem = problem_of(minute, duration, people.iter().all(Captured::in_bounds), &affects);
    Ok(Capture { minute, people, affects, problem })
}

/// The validation's order: the minute, then each participant's values.
fn problem_of(minute: i64, duration: i64, people_ok: bool, affects: &[Option<[f64; 6]>]) -> Option<&'static str> {
    let unit = |value: &f64| (0.0..=1.0).contains(value);
    if !(0..=duration).contains(&minute) {
        Some(NOT_ITS_EVENT)
    } else if !people_ok || !affects.iter().flatten().all(|values| values.iter().all(unit)) {
        Some(NOT_UNIT)
    } else {
        None
    }
}

struct ContextNames {
    schema_version: Arc<str>,
    captured_minute: Arc<str>,
    participants: Arc<str>,
    stress: Arc<str>,
    trust: Arc<str>,
    needs: Arc<str>,
    affect: Arc<str>,
    need_names: [Arc<str>; 4],
    affect_names: [Arc<str>; 6],
}

static NAMES: LazyLock<ContextNames> = LazyLock::new(|| ContextNames {
    schema_version: Arc::from("schema_version"),
    captured_minute: Arc::from("captured_minute"),
    participants: Arc::from("participants"),
    stress: Arc::from("stress"),
    trust: Arc::from("trust"),
    needs: Arc::from("needs"),
    affect: Arc::from("affect"),
    need_names: NEEDS.map(Arc::from),
    affect_names: AFFECT.map(Arc::from),
});

/// The context of a capture, in the keys' order of the Python rule.
fn context_of(ids: &[&str], capture: &Capture) -> V {
    let names = &*NAMES;
    let mut participants = IndexMap::with_capacity(ids.len());
    for ((id, person), affect) in ids.iter().zip(&capture.people).zip(&capture.affects) {
        let mut state = IndexMap::with_capacity(4);
        state.insert(names.stress.clone(), V::Float(person.stress));
        state.insert(names.trust.clone(), V::Float(person.trust));
        let needs = names.need_names.iter().zip(person.needs).map(|(name, value)| (name.clone(), V::Float(value)));
        state.insert(names.needs.clone(), V::Map(needs.collect()));
        let affect = match affect {
            Some(values) => {
                V::Map(names.affect_names.iter().zip(values).map(|(name, value)| (name.clone(), V::Float(*value))).collect())
            }
            None => V::Null,
        };
        state.insert(names.affect.clone(), affect);
        participants.insert(Arc::from(*id), V::Map(state));
    }
    let mut context = IndexMap::with_capacity(3);
    context.insert(names.schema_version.clone(), V::Int(1));
    context.insert(names.captured_minute.clone(), V::Int(capture.minute));
    context.insert(names.participants.clone(), V::Map(participants));
    V::Map(context)
}

/// `capture_social_state_context` of `set(participant_ids)`: the IDs without repeats, in their
/// order. One or two people in the world, or its ValueError; then the copy, or the ValueError of
/// `validate_social_state_context` for it.
pub fn capture_context(world: &World, ids: &[&str]) -> Result<V> {
    if ids.is_empty() || ids.len() > 2 {
        return Err(Error::value_error(NOT_ONE_OR_TWO));
    }
    {
        let rows = world.rows()?;
        if ids.iter().any(|id| !rows.agents.contains_key(id)) {
            return Err(Error::value_error(NOT_ONE_OR_TWO));
        }
    }
    let mut ids = ids.to_vec();
    // sorted(): Python orders str by code point, the same as UTF-8 bytes.
    if ids.len() == 2 && ids[0].as_bytes() > ids[1].as_bytes() {
        ids.swap(0, 1);
    }
    let capture = read_capture(world, &ids)?;
    if let Some(text) = capture.problem {
        return Err(Error::value_error(text));
    }
    Ok(context_of(&ids, &capture))
}

/// `capture_context` of two different people as the value of an event's payload: Unsupported for a
/// value out of bounds.
pub fn context_value(world: &World, first: &str, second: &str) -> Result<V> {
    let (low, high) = if first.as_bytes() <= second.as_bytes() { (first, second) } else { (second, first) };
    let ids = [low, high];
    let capture = read_capture(world, &ids)?;
    if capture.problem.is_some() {
        return Err(Error::unsupported("a participant's value is out of bounds"));
    }
    Ok(context_of(&ids, &capture))
}

// ---- The Python rule's verdict on any context -----------------------------------------------------
//
// `SocialInteractionHandler` words a ValueError of `validate_social_state_context` as its own saved
// text, so this decides as the Python rule does, in its order of checks, for any JSON value an
// event's payload holds.

/// `set(mapping) == set(names)`, of distinct names.
fn keys_are(mapping: &IndexMap<Arc<str>, V>, names: &[&str]) -> bool {
    mapping.len() == names.len() && names.iter().all(|name| mapping.contains_key(*name))
}

/// `_validate_unit(value)` returns (true) or raises its ValueError (false). Unsupported for an int
/// past 64 bits, which `math.isfinite` may refuse with another error.
#[track_caller]
fn unit_verdict(value: &V) -> Result<bool> {
    match value {
        V::Float(number) => Ok(number.is_finite() && (0.0..=1.0).contains(number)),
        V::Int(number) => Ok((0..=1).contains(number)),
        V::Big(_) => Err(Error::unsupported("an int past 64 bits is for the Python rule to judge")),
        // A bool, and every value that is no int or float.
        _ => Ok(false),
    }
}

/// `context.get("schema_version") != 1` is false: 1, `True` or `1.0`.
fn is_version_one(value: &V) -> bool {
    match value {
        V::Bool(flag) => *flag,
        V::Int(number) => *number == 1,
        V::Float(number) => *number == 1.0,
        _ => false,
    }
}

/// The text `validate_social_state_context(context, ids, due_minute)` raises, or None where it
/// returns. `ids` have no repeats.
pub fn context_problem(context: &V, ids: &[&str], due_minute: i64) -> Result<Option<&'static str>> {
    let V::Map(context) = context else {
        return Ok(Some(NOT_ITS_EVENT));
    };
    if !keys_are(context, &["schema_version", "captured_minute", "participants"]) {
        return Ok(Some(NOT_ITS_EVENT));
    }
    if !is_version_one(&context["schema_version"]) {
        return Ok(Some(NOT_ITS_EVENT));
    }
    // `type(captured) is not int or not 0 <= captured <= due_minute`: an int past 64 bits is outside.
    let V::Int(captured) = context["captured_minute"] else {
        return Ok(Some(NOT_ITS_EVENT));
    };
    if !(0..=due_minute).contains(&captured) {
        return Ok(Some(NOT_ITS_EVENT));
    }
    let V::Map(participants) = &context["participants"] else {
        return Ok(Some(NOT_ITS_EVENT));
    };
    if participants.len() != ids.len() || ids.iter().any(|id| !participants.contains_key(*id)) {
        return Ok(Some(NOT_ITS_EVENT));
    }
    for state in participants.values() {
        let V::Map(state) = state else {
            return Ok(Some(STATE_FIELDS));
        };
        if !keys_are(state, &["stress", "trust", "needs", "affect"]) {
            return Ok(Some(STATE_FIELDS));
        }
        if !unit_verdict(&state["stress"])? || !unit_verdict(&state["trust"])? {
            return Ok(Some(NOT_UNIT));
        }
        let V::Map(needs) = &state["needs"] else {
            return Ok(Some(NEEDS_INVALID));
        };
        if !keys_are(needs, &NEEDS) {
            return Ok(Some(NEEDS_INVALID));
        }
        for value in needs.values() {
            if !unit_verdict(value)? {
                return Ok(Some(NOT_UNIT));
            }
        }
        let affect = &state["affect"];
        if matches!(affect, V::Null) {
            continue;
        }
        let V::Map(affect) = affect else {
            return Ok(Some(AFFECT_INVALID));
        };
        if !keys_are(affect, &AFFECT) {
            return Ok(Some(AFFECT_INVALID));
        }
        for value in affect.values() {
            if !unit_verdict(value)? {
                return Ok(Some(NOT_UNIT));
            }
        }
    }
    Ok(None)
}

/// `validate_social_state_context(context, ids, due_minute)` returns (true) or raises its ValueError
/// (false). `ids` have no repeats.
pub fn context_verdict(context: &V, ids: &[&str], due_minute: i64) -> Result<bool> {
    Ok(context_problem(context, ids, due_minute)?.is_none())
}

/// `validate_social_state_context`: Ok, or its ValueError.
pub fn validate_context(context: &V, ids: &[&str], due_minute: i64) -> Result<()> {
    match context_problem(context, ids, due_minute)? {
        None => Ok(()),
        Some(text) => Err(Error::value_error(text)),
    }
}
