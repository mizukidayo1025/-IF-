//! `record_social_event` of `domain/body_state.py`: the glue of `sheltersim_core::rules::body_state`
//! (lane P8 a2). It reads what Python holds (the event as its row, the model selection and the
//! operators the configs select, which the Python factory decides for a model the core does not
//! know), runs the core's kernel on the world (`with_world`), and returns.

use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyDict;

use sheltersim_core::config::body::{BodyModels, LINEAR_AFFECT, TRUST_WEIGHTED_AFFECT, DISTANCE_TRUST, DURATION_TRUST};
use sheltersim_core::config::body::{AffectModel, TrustModel};
use sheltersim_core::config::Selection;
use sheltersim_core::rules::kit::Kept;
use sheltersim_core::rules::body_state::{self as rules, records_event, SocialEvent};

use crate::core::configs;
use crate::core::errors::OrPy;
use crate::core::row::{attribute, exact_dict, exact_int, exact_str, namespace_is_absent, str_value, unsupported};
use crate::core::world_core::with_world;
use crate::state::body_state::NAME;
use crate::rules::social_dialogue::{mapping_proxy, payload_fields};
use crate::Unsupported;

/// `selected_models(model_configs)`, kept for the configs' stamp (`core/configs.rs`). What it raises
/// is the rule's to say.
fn selected_models(world: &Bound<'_, PyAny>) -> PyResult<Arc<Selection>> {
    match configs::selection(world)? {
        Ok(selection) => Ok(selection),
        Err(fault) if fault.is_refusal() => unsupported("not a selection"),
        Err(_) => unsupported("the model selection raises"),
    }
}

/// `create_affect` without a selection: the config's, by the function that also refuses a bad one.
fn affect_of_the_config(world: &Bound<'_, PyAny>) -> PyResult<Arc<str>> {
    let py = world.py();
    let configs = attribute(world, intern!(py, "model_configs"))?;
    let raw = exact_dict(&configs)?.get_item(intern!(py, "body_dynamics"))?;
    let config = py
        .import(intern!(py, "sheltersim.domain.body_state"))?
        .getattr(intern!(py, "canonical_body_config"))?
        .call1((raw,))
        .map_err(|_| Unsupported::new_err("the body config raises"))?;
    Ok(Arc::from(exact_str(&config.get_item(intern!(py, "affect_policy"))?)?))
}

/// The affect and relationship models the rule creates, by name, for the tests
/// (`core/configs.rs`'s `config_section`).
pub(crate) fn model_names_of(world: &Bound<'_, PyAny>) -> PyResult<(&'static str, &'static str)> {
    let models = models_of(world, &*selected_models(world)?)?;
    let affect = match models.affect {
        AffectModel::Linear => LINEAR_AFFECT,
        AffectModel::TrustWeighted => TRUST_WEIGHTED_AFFECT,
    };
    let trust = match models.trust {
        TrustModel::DistanceDurationRepeat { .. } => DISTANCE_TRUST,
        TrustModel::DurationOnly => DURATION_TRUST,
    };
    Ok((affect, trust))
}

/// The operators the rule creates, kept for the configs' stamp (`core/configs.rs`). An ID this
/// kernel doesn't know is left to Python's factory.
fn models_of(world: &Bound<'_, PyAny>, selection: &Selection) -> PyResult<Arc<BodyModels>> {
    match models_section(world, selection)? {
        Ok(models) => Ok(models),
        Err(fault) => Err(configs::raise(&fault, || affect_of(world, selection))),
    }
}

fn affect_of(world: &Bound<'_, PyAny>, selection: &Selection) -> PyResult<Arc<str>> {
    match selection.get("affect") {
        Some(affect) => Ok(affect.clone()),
        None => affect_of_the_config(world),
    }
}

/// `models_of` without its raise: the section, or its fault (the host's `fill_section`, lane P9-0).
pub(crate) fn models_section(world: &Bound<'_, PyAny>, selection: &Selection) -> PyResult<Kept<BodyModels>> {
    let py = world.py();
    let relationship = selection.get("relationship").cloned();
    let parse = || match affect_of(world, selection) {
        Ok(affect) => configs::parsed_value(world, |configs| BodyModels::parse(&affect, relationship.as_deref(), configs)),
        Err(error) => Ok(configs::parsed(py, Err(error))),
    };
    configs::kept_with_selection(world, |configs| &configs.body_models, parse)
}

/// The model selection the body's rule reads (`selected_models`), for the host (lane P9-0).
pub(crate) fn selection_of(world: &Bound<'_, PyAny>) -> PyResult<Arc<Selection>> {
    selected_models(world)
}

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

/// `record_social_event`.
#[pyfunction]
pub(crate) fn record_social_event<'py>(world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>, minute: &Bound<'py, PyAny>) -> PyResult<()> {
    let py = world.py();
    let event_type = attribute(event, intern!(py, "event_type"))?;
    // Any other event changes nothing, with the namespace or without it.
    if str_value(&event_type).is_some_and(|event_type| !records_event(event_type)) {
        return Ok(());
    }
    if namespace_is_absent(world, intern!(py, NAME))? {
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
    let selection = selected_models(world)?;
    let models = models_of(world, &selection)?;
    let payload = payload_fields(&payload, keys_of(event_type))?;
    let event = SocialEvent { event_id, event_type, causation_id, payload: &payload };
    with_world(world, |cell| rules::record_social_event(cell, &event, minute, &selection, &models))?.or_py()
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(record_social_event, m)?)?;
    Ok(())
}
