//! `domain/social_network.py`: the glue of `sheltersim_core::rules::social_network` (lane P8 a2),
//! and the readers the event handlers still in this crate share: `mapping_get` of a Python payload,
//! and the body state's relief of a social action for `rules/social_interaction.rs`, which reads the
//! body state's tree where it is tainted until its lane moves it.

use pyo3::exceptions::PyKeyError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyString;

use sheltersim_core::rules::social_network::{self as rules, observes_event};

use crate::core::errors::OrPy;
use crate::core::row::{attribute, exact_float, exact_int, exact_str, unsupported};
use crate::core::tracked::as_readable_dict;
use crate::core::world_core::{held_state, raw_dict_get, with_world, Held, NamespaceMirror};
use crate::state::body_state::BodyRow;
use crate::rules::social_dialogue::payload_fields;

/// `mapping[key]`, or None for a key it does not have, of a Python mapping (the core's, of a `V`,
/// is `sheltersim_core::rules::kit::mapping_get`).
pub(crate) fn mapping_get<'py>(mapping: &Bound<'py, PyAny>, key: &Bound<'py, PyString>) -> PyResult<Option<Bound<'py, PyAny>>> {
    match mapping.get_item(key) {
        Ok(value) => Ok(Some(value)),
        Err(error) if error.is_instance_of::<PyKeyError>(mapping.py()) => Ok(None),
        Err(_) => unsupported("a payload cannot be read"),
    }
}

/// What a reader of `body_state_v1` reads: its row, which holds what the tree holds (and is all
/// there is while the namespace is detached), or the dict Python holds past a tainted row.
pub(crate) enum Body<'py> {
    Row(Bound<'py, NamespaceMirror>),
    Tree(Bound<'py, PyAny>),
}

/// `world.model_state.get("body_state_v1")`: its row, a readable dict, or None.
pub(crate) fn body_state_of<'py>(world: &Bound<'py, PyAny>) -> PyResult<Option<Body<'py>>> {
    match held_state(world, "body_state_v1")? {
        None => Ok(None),
        Some(Held::Row(mirror)) => Ok(Some(Body::Row(mirror))),
        Some(Held::Value(body)) => {
            if as_readable_dict(&body).is_none() {
                return unsupported("the body state is not a plain dict");
            }
            Ok(Some(Body::Tree(body)))
        }
    }
}

/// `get(name, {})` of a dict Python holds, read as a dict: None for no such key.
fn body_part<'py>(body: &Bound<'py, PyAny>, name: &Bound<'py, PyString>) -> PyResult<Option<Bound<'py, PyAny>>> {
    let Some(part) = raw_dict_get(body, name.as_any())? else {
        return Ok(None);
    };
    if as_readable_dict(&part).is_none() {
        return unsupported("a part of the body state is not a plain dict");
    }
    Ok(Some(part))
}

fn float_or<'py>(record: Option<&Bound<'py, PyAny>>, name: &Bound<'py, PyString>, default: f64) -> PyResult<f64> {
    let Some(record) = record else {
        return Ok(default);
    };
    match raw_dict_get(record, name.as_any())? {
        Some(value) => exact_float(&value),
        None => Ok(default),
    }
}

/// `social_action_affect_transfer * signal` of the actor: the relief of a social action, which
/// `SocialInteractionHandler` and `observe_event` compute alike (`rules::relief`). A row is read by
/// the core; a dict Python holds past it gives the core its three values.
pub(crate) fn action_relief<'py>(body: Option<&Body<'py>>, actor_id: &Bound<'py, PyString>) -> PyResult<f64> {
    let py = actor_id.py();
    let body = match body {
        Some(Body::Row(mirror)) => {
            let actor = actor_id.to_str()?;
            return NamespaceMirror::with_row::<BodyRow, _>(mirror, |row| rules::action_relief(Some(row), actor).or_py());
        }
        Some(Body::Tree(body)) => body,
        None => return rules::action_relief(None, actor_id.to_str()?).or_py(),
    };
    let record = match body_part(body, intern!(py, "agents"))? {
        Some(agents) => raw_dict_get(&agents, actor_id.as_any())?,
        None => None,
    };
    if let Some(record) = &record {
        if as_readable_dict(record).is_none() {
            return unsupported("a body record is not a plain dict");
        }
    }
    let positive = float_or(record.as_ref(), intern!(py, "positive_affect"), 0.5)?;
    let negative = float_or(record.as_ref(), intern!(py, "negative_affect"), 0.0)?;
    let config = body_part(body, intern!(py, "config"))?;
    let transfer = float_or(config.as_ref(), intern!(py, "social_action_affect_transfer"), 0.02)?;
    Ok(rules::relief(positive, negative, transfer))
}

/// `record_observed_contact` of each (recipient, source), for `rules/social_propagation.rs`, which
/// has written its own namespace by now: on Unsupported it calls the Python rule, and nothing has
/// been written here.
pub(crate) fn record_observed_contacts(world: &Bound<'_, PyAny>, pairs: &[(&str, &str)], minute: i64) -> PyResult<()> {
    with_world(world, |cell| rules::record_observed_contacts(cell, pairs, minute))?.or_py()
}

/// `observe_event`: the event's contact or interaction, recorded, and the body's trust copied.
#[pyfunction]
pub(crate) fn observe_event<'py>(world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>, minute: &Bound<'py, PyAny>) -> PyResult<()> {
    let py = world.py();
    let event_type = attribute(event, intern!(py, "event_type"))?;
    if !observes_event(exact_str(&event_type)?) {
        return Ok(());
    }
    let event_type = exact_str(&event_type)?;
    let keys: &[&str] = match event_type {
        "information_contact_update" => &["recipient_agent_id", "sender_agent_id", "message_id"],
        "proximity_contact_update" => &["first_agent_id", "second_agent_id", "observed_minute"],
        _ => &["interaction_type", "agent_id", "participant_agent_id"],
    };
    let payload = payload_fields(&attribute(event, intern!(py, "payload"))?, keys)?;
    let minute = exact_int(minute)?;
    with_world(world, |cell| rules::observe_event(cell, event_type, &payload, minute))?.or_py()
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(observe_event, m)?)?;
    Ok(())
}
