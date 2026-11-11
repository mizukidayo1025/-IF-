//! The glue of the core's `zone_environment` (lane P8 a1): `zone_environment_view(world, zone_id)`
//! as the Python dataclass, for the readers outside a tick, the shadow and the tests. The view reads
//! the zones' conditions in the core.

use std::sync::Arc;

use indexmap::IndexMap;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};
use sheltersim_core::rules::zone_environment::{ZoneInputs, STATE};

use crate::core::errors::OrPy;
use crate::core::tracked::as_readable_dict;
use crate::core::value::{VPy, V};
use crate::Unsupported;

fn texts<'py>(py: Python<'py>, items: &[Arc<str>]) -> PyResult<Bound<'py, PyTuple>> {
    PyTuple::new(py, items.iter().map(|item| &**item))
}

/// `zone_environment_view(world, zone_id)` as the Python dataclass. Says Unsupported for what the
/// Python rule raises on, for what a view could not hold, and for a value that is not JSON. Of the
/// configs, only the two keys the rule reads are taken.
#[pyfunction]
fn view_of<'py>(world: &Bound<'py, PyAny>, zone_id: &str) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let configs = world.getattr(intern!(py, "model_configs"))?;
    let Some(configs) = as_readable_dict(&configs) else {
        return Err(Unsupported::new_err("the configs are not a plain dict"));
    };
    let mut read = IndexMap::new();
    for name in ["zone_environment", "policy_registry"] {
        if let Some(value) = configs.get_item(name)? {
            read.insert(Arc::from(name), <V as VPy>::from_py(&value)?);
        }
    }
    let state = world.getattr(intern!(py, "model_state"))?.call_method1(intern!(py, "get"), (STATE,))?;
    let state = <V as VPy>::from_py(&state)?;
    let conditions = ZoneInputs::read(&read, Some(&state)).and_then(|inputs| inputs.conditions(zone_id)).or_py()?;
    let module = py.import("sheltersim.domain.environment")?;
    let demand_view = module.getattr("ZoneDemandView")?;
    let mut demand = Vec::with_capacity(conditions.demand_modifiers.len());
    for modifier in &conditions.demand_modifiers {
        demand.push(demand_view.call1((
            &*modifier.action,
            modifier.probability_multiplier,
            modifier.duration_multiplier,
            texts(py, &modifier.modifier_event_ids)?,
            texts(py, &modifier.assumption_ids)?,
        ))?);
    }
    let fields = PyDict::new(py);
    fields.set_item("ambient_noise_level", conditions.ambient_noise_level)?;
    fields.set_item("rest_efficiency_multiplier", conditions.rest_efficiency_multiplier)?;
    fields.set_item("noise_modifier_event_ids", texts(py, &conditions.noise_modifier_event_ids)?)?;
    fields.set_item("noise_assumption_ids", texts(py, &conditions.noise_assumption_ids)?)?;
    fields.set_item("demand_modifiers", PyTuple::new(py, demand)?)?;
    fields.set_item("min_rest_decision_minutes", conditions.min_rest_decision_minutes)?;
    fields.set_item("max_rest_decision_minutes", conditions.max_rest_decision_minutes)?;
    fields.set_item("ventilation_effectiveness", conditions.ventilation_effectiveness)?;
    fields.set_item("lighting_level", conditions.lighting_level)?;
    fields.set_item("privacy_level", conditions.privacy_level)?;
    fields.set_item("temperature_c", conditions.temperature_c)?;
    fields.set_item("ventilation_hygiene_growth_per_minute", conditions.ventilation_hygiene_growth_per_minute)?;
    fields.set_item("privacy_discontent_per_minute", conditions.privacy_discontent_per_minute)?;
    fields.set_item("temperature_stress_per_minute", conditions.temperature_stress_per_minute)?;
    fields.set_item("condition_modifier_event_ids", texts(py, &conditions.condition_modifier_event_ids)?)?;
    fields.set_item("condition_assumption_ids", texts(py, &conditions.condition_assumption_ids)?)?;
    module.getattr("ZoneEnvironmentView")?.call((), Some(&fields))
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(view_of, m)?)?;
    Ok(())
}
