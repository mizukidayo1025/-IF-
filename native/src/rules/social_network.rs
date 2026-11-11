//! `domain/social_network.py`: the glue of `sheltersim_core::rules::social_network` (lane P8 a2),
//! and `mapping_get` of a Python payload, which the event handlers still in this crate share.

use pyo3::exceptions::PyKeyError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyString;

use sheltersim_core::rules::social_network::{self as rules, observes_event};

use crate::core::errors::OrPy;
use crate::core::row::{attribute, exact_int, exact_str, unsupported};
use crate::core::world_core::with_world;
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
