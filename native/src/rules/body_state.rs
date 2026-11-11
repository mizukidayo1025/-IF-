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
use sheltersim_core::rules::body_state::{self as rules, records_event, SocialEvent};
use sheltersim_core::rules::kit;

use crate::core::configs;
use crate::core::errors::OrPy;
use crate::core::row::{attribute, exact_dict, exact_int, exact_str, str_value, unsupported};
use crate::core::world_core::with_world;
use crate::state::body_state::NAME;
use crate::rules::social_dialogue::{frozen_value, mapping_proxy};
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
    let py = world.py();
    let affect = || match selection.get("affect") {
        Some(affect) => Ok(affect.clone()),
        None => affect_of_the_config(world),
    };
    let relationship = selection.get("relationship").cloned();
    let parse = || match affect() {
        Ok(affect) => configs::parsed_value(world, |configs| BodyModels::parse(&affect, relationship.as_deref(), configs)),
        Err(error) => Ok(configs::parsed(py, Err(error))),
    };
    match configs::kept_with_selection(world, |configs| &configs.body_models, parse)? {
        Ok(models) => Ok(models),
        Err(fault) => Err(configs::raise(&fault, affect)),
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
    if with_world(world, |cell| kit::namespace_absent(cell, NAME))?.or_py()? {
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
    let payload = frozen_value(&payload)?;
    let event = SocialEvent { event_id, event_type, causation_id, payload: &payload };
    with_world(world, |cell| rules::record_social_event(cell, &event, minute, &selection, &models))?.or_py()
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(record_social_event, m)?)?;
    Ok(())
}
