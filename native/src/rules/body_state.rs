//! `record_social_event` of `domain/body_state.py`: the glue of `sheltersim_core::rules::body_state`
//! (lane P8 a2). It reads what Python holds (the event as its row, the model selection and the
//! operators the configs select, which the Python factory decides for a model the core does not
//! know), runs the core's kernel on the world (`with_world`), and returns.


use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use sheltersim_core::config::SectionId;
use sheltersim_core::world::WorldCell;
use sheltersim_core::rules::body_state::{self as rules, records_event, SocialEvent};

use crate::core::errors::OrPy;
use crate::core::row::{attribute, exact_int, exact_str, str_value, unsupported};
use crate::core::world_core::PyWorldCell;
use crate::host::PyHost;
use crate::rules::social_dialogue::{mapping_proxy, payload_fields};

/// The payload's fields the rule reads of an event of this type.
fn keys_of(event_type: &str) -> &'static [&'static str] {
    const COMMON: [&str; 4] = ["contact_minutes", "message_id", "topic_id", "interaction_duration_minutes"];
    match event_type {
        "proximity_contact_update" => &[
            COMMON[0], COMMON[1], COMMON[2], COMMON[3], "first_agent_id", "second_agent_id", "observed_minute", "distance_m",
        ],
        "information_contact_update" => &[COMMON[0], COMMON[1], COMMON[2], COMMON[3], "sender_agent_id", "recipient_agent_id"],
        "social_interaction" => &[
            COMMON[0], COMMON[1], COMMON[2], COMMON[3], "interaction_type", "agent_id", "participant_agent_id",
        ],
        _ => &[COMMON[0], COMMON[1], COMMON[2], COMMON[3], "zone_id"],
    }
}

/// `record_social_event`: whether the namespace is there and the configs' models are read by the
/// core through the native host (lane P9-1a), at the points the rule read them.
#[pyfunction]
pub(crate) fn record_social_event<'py>(world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>, minute: &Bound<'py, PyAny>) -> PyResult<()> {
    let py = world.py();
    let event_type = attribute(event, intern!(py, "event_type"))?;
    // Any other event changes nothing, with the namespace or without it.
    if str_value(&event_type).is_some_and(|event_type| !records_event(event_type)) {
        return Ok(());
    }
    let cell = PyWorldCell(world);
    let mut host = PyHost::of_world(world, PyDict::new(py));
    if rules::namespace_absent(&cell, &mut host).map_err(|error| error.0)? {
        return Ok(());
    }
    let event_type = exact_str(&event_type)?;
    let minute = exact_int(minute)?;
    let payload = attribute(event, intern!(py, "payload"))?;
    if !payload.is_exact_instance_of::<PyDict>() && !payload.get_type().is(mapping_proxy(py)?) {
        return unsupported("the payload is not frozen JSON");
    }
    let event_id = attribute(event, intern!(py, "event_id"))?;
    let event_id = exact_str(&event_id)?;
    let causation_id = attribute(event, intern!(py, "causation_id"))?;
    let causation_id = if causation_id.is_none() { None } else { Some(exact_str(&causation_id)?) };
    let inputs = rules::Inputs::read(&cell, &mut host).map_err(|error| error.0)?;
    let models = match &inputs.models {
        Ok(models) => models.clone(),
        Err(fault) => return Err(host.raise_section(SectionId::BodyModels, fault)),
    };
    let payload = payload_fields(&payload, keys_of(event_type))?;
    let event = SocialEvent { event_id, event_type, causation_id, payload: &payload };
    cell.with(|world| rules::record_social_event(world, &event, minute, &inputs.selection, &models)).or_py()?.or_py()
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(record_social_event, m)?)?;
    Ok(())
}
