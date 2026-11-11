//! The events of `information_spread_v1` in `events/handlers.py`: the glue of
//! `sheltersim_core::rules::information_events` (lane P8 d1). It reads what Python holds (the config
//! and the model selection, kept for the configs' stamp, and whether `model_state` holds the
//! namespace), makes the event's row, and calls the core with the world (`PyWorldCell`) and the
//! native host, which upgrades a state of schema 1 where the Python rule did (policy E11).
//!
//! `InformationSpreadConfig.from_mapping` and `selected_models` stay Python's: they read the
//! Scenario's contract, and neither changes anything. The config's error is raised here, before the
//! event is read, which is where the Python rule raises it.

use std::sync::Arc;

use pyo3::exceptions::PyValueError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString};

use sheltersim_core::config::information::InformationSpread;
use sheltersim_core::config::Selection;
use sheltersim_core::error::Error as CoreError;
use sheltersim_core::rules::information_events::{self as rules, Applied, Inputs, SELECTION_RAISED};
use sheltersim_core::rules::kit::{Held, Kept};
use sheltersim_core::world::MODEL_CONFIGS;

use crate::core::configs;
use crate::core::errors::{self, OrPy};
use crate::core::row::{attribute, unsupported};
use crate::core::tracked::as_readable_dict;
use crate::core::value::{VPy, V};
use crate::core::world_core::{namespace_held, raw_dict_get, with_world, PyWorldCell};
use crate::host::PyHost;
use crate::rules::handlers::{counted, get};
use crate::state::events::{EventRow, EventRowPy};
use crate::state::information::NAME;

/// `InformationSpreadConfig.from_mapping(model_configs.get("information_spread"))`, as the rules
/// read it.
fn parse_spread(world: &Bound<'_, PyAny>) -> PyResult<InformationSpread> {
    let py = world.py();
    let configs = attribute(world, intern!(py, "model_configs"))?;
    let config = py
        .import("sheltersim.domain.models")?
        .getattr("InformationSpreadConfig")?
        .call_method1("from_mapping", (get(&configs, "information_spread")?,))?;
    spread_of(&config, configs.contains("information_spread")?)
}

/// What the rules read of an `InformationSpreadConfig`.
fn spread_of(config: &Bound<'_, PyAny>, present: bool) -> PyResult<InformationSpread> {
    let mut fields = indexmap::IndexMap::with_capacity(InformationSpread::FIELDS.len());
    for name in InformationSpread::FIELDS {
        let value = if name == "present" { V::Bool(present) } else { <V as VPy>::from_py(&config.getattr(name)?)? };
        fields.insert(Arc::from(name), value);
    }
    configs::read(InformationSpread::from_v(&V::Map(fields)))
}

/// The config, kept for the configs' stamp (`core/configs.rs`), or what its parse raised.
pub(crate) fn information_spread(world: &Bound<'_, PyAny>) -> PyResult<Kept<InformationSpread>> {
    configs::kept(world, |configs| &configs.information_spread, || Ok(configs::parsed(world.py(), parse_spread(world))))
}

/// The config as the events take it: what its parse raised is raised again (policy E14), a
/// `ValueError` as the event's failure.
fn runtime_config(world: &Bound<'_, PyAny>) -> PyResult<Arc<InformationSpread>> {
    let py = world.py();
    match information_spread(world)? {
        Ok(config) => Ok(config),
        Err(fault) => {
            let error = configs::raise(&fault, || parse_spread(world));
            if error.is_instance_of::<PyValueError>(py) {
                return errors::failed(py, format!("invalid information_spread config: {}", error.value(py).str()?));
            }
            Err(error)
        }
    }
}

/// Whether `model_state` holds the namespace, as the rules take it: a value that is not a dict is
/// no state (`Absent`), another dict is refused where the rule reads the state.
pub(crate) fn state_held(world: &Bound<'_, PyAny>) -> PyResult<Held> {
    let py = world.py();
    let held = namespace_held(world, intern!(py, NAME), "the world holds another document than the row's")?;
    if let Held::Other(_) = held {
        let table = attribute(world, intern!(py, "model_state"))?;
        if as_readable_dict(&table).is_some() {
            let value = raw_dict_get(&table, PyString::new(py, NAME).as_any())?;
            if !value.is_some_and(|value| value.is_instance_of::<PyDict>()) {
                return Ok(Held::Absent);
            }
        }
    }
    Ok(held)
}

/// The row of a `ScheduledEvent`; Unsupported for one a row cannot hold.
pub(crate) fn event_row(event: &Bound<'_, PyAny>) -> PyResult<EventRow> {
    match EventRow::from_event(event)? {
        Some(row) => Ok(row),
        None => unsupported("an event a row cannot hold"),
    }
}

/// What a core call raised: the corrupt `model_configs` of a selection that raised is what
/// `selected_models` raises (policy E14).
fn relayed<T>(world: &Bound<'_, PyAny>, selection: &Kept<Selection>, result: PyResult<T>) -> PyResult<T> {
    let py = world.py();
    match result {
        Err(error) if matches!(selection, Err(fault) if !fault.is_refusal()) && error.value(py).to_string() == selection_raised() => {
            Err(configs::reraise_selection(world))
        }
        other => other,
    }
}

fn selection_raised() -> String {
    CoreError::corrupt(MODEL_CONFIGS, SELECTION_RAISED).to_string()
}

/// Run an event's core function with what it reads of Python.
fn run_event<'py, T>(
    world: &Bound<'py, PyAny>,
    event: &Bound<'py, PyAny>,
    rule: impl FnOnce(&PyWorldCell<'_, 'py>, &mut PyHost<'py>, &Inputs<'_>, &EventRow) -> Result<T, crate::host::HostError>,
) -> PyResult<T> {
    let py = world.py();
    let config = runtime_config(world)?;
    let held = state_held(world)?;
    let selection: Kept<Selection> = configs::selection(world)?;
    let row = event_row(event)?;
    let inputs = Inputs { config: &config, held: &held, selection: &selection };
    let mut host = PyHost::of_world(world, PyDict::new(py));
    let result = rule(&PyWorldCell(world), &mut host, &inputs, &row).map_err(|error| error.0);
    relayed(world, &selection, result)
}

#[pyfunction]
fn rumor<'py>(_handler: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<()> {
    counted(world.py(), "rumor", run_event(world, event, |cell, host, inputs, row| rules::rumor(cell, host, inputs, row)))
}

#[pyfunction]
fn correction<'py>(_handler: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<()> {
    counted(world.py(), "correction", run_event(world, event, |cell, host, inputs, row| rules::correction(cell, host, inputs, row)))
}

#[pyfunction]
fn contact<'py>(_handler: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<()> {
    match counted(world.py(), "contact", run_event(world, event, |cell, host, inputs, row| rules::contact(cell, host, inputs, row)))? {
        Applied::Applied => Ok(()),
        Applied::Suppressed(text) => errors::suppressed(world.py(), text),
    }
}

/// The config of the events, with the model's and the state's checks, for the configs' tests
/// (`core/configs.rs`).
pub(crate) fn checked_config(world: &Bound<'_, PyAny>) -> PyResult<Arc<InformationSpread>> {
    let config = runtime_config(world)?;
    let held = state_held(world)?;
    with_world(world, |world| rules::check_state(world, &config, &held))?.or_py()?;
    Ok(config)
}

/// `_sample_claim_belief_strength`, for the tests: a draw hides a small change in its probability.
#[pyfunction]
fn claim_strength_of(world: &Bound<'_, PyAny>, config: &Bound<'_, PyAny>, agent_id: &str, source_id: &str, minute: i64, message_id: &str) -> PyResult<f64> {
    let config = spread_of(config, true)?;
    with_world(world, |world| rules::sample_strength(world, &config, agent_id, source_id, minute, message_id))?.or_py()
}

/// A correction's acceptance as the rules draw it, for the tests: `ids` are the key's parts after
/// the minute.
#[pyfunction]
#[allow(clippy::too_many_arguments)]
fn acceptance_of(world: &Bound<'_, PyAny>, config: &Bound<'_, PyAny>, label: &str, trust: f64, source_id: &str, minute: i64, ids: Vec<String>) -> PyResult<bool> {
    let config = spread_of(config, true)?;
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    with_world(world, |world| rules::accepts(world, &config, label, trust, source_id, minute, &ids))?.or_py()
}

/// `selected_models(world.model_configs)["information"]` as the rules read it, for the tests.
#[pyfunction]
fn selected_information_of(world: &Bound<'_, PyAny>) -> PyResult<String> {
    let selection = configs::selection(world)?;
    let information = rules::selected_information(&selection).or_py();
    Ok(relayed(world, &selection, information)?.to_string())
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(selected_information_of, m)?)?;
    m.add_function(wrap_pyfunction!(claim_strength_of, m)?)?;
    m.add_function(wrap_pyfunction!(acceptance_of, m)?)?;
    m.add_function(wrap_pyfunction!(rumor, m)?)?;
    m.add_function(wrap_pyfunction!(correction, m)?)?;
    m.add_function(wrap_pyfunction!(contact, m)?)?;
    Ok(())
}
