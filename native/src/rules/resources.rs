//! The entries of the rules of `domain/resources.py` that run in a tick, whose logic is the core's
//! (`sheltersim_core::rules::resources`, P8b-1). Each asks first what only Python can say (the
//! ledger is in `model_state`, and its tree is the slot's), reads the event as its row, calls the
//! core, and turns what it returns into Python: the `ResourceLedgerError` with Python's `repr` of a
//! payload value, and the world's new `resources` dict, which replaces the old one as the Python
//! rule's did (whoever holds the old one keeps the old values).

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString};
use sheltersim_core::rules::resources::{self as core, Applied, Fail, OperationArg, OperationEvent};

use crate::core::errors::{self, ledger_error, to_py_err, OrPy};
use crate::core::row::{attribute, namespace_is_absent, unsupported};
use crate::core::tracked::as_readable_dict;
use crate::core::value::{VPy, V};
use crate::core::world_core::{with_world, NamespaceMirror};
use crate::state::events::{EventRow, EventRowPy};
use crate::state::resource_ledger::{held_mirror, RESOURCE_IDS};
use crate::state::world_fields;

pub(crate) use sheltersim_core::rules::resources::Operation;

/// The error of an operation that did nothing.
fn raised(py: Python<'_>, fail: Fail) -> PyErr {
    match fail {
        Fail::Error(error) => to_py_err(error),
        Fail::Ledger(fault) => {
            let mut repr_error = None;
            let text = fault.text(|value| match value.to_py(py).and_then(|value| errors::repr(&value)) {
                Ok(text) => text,
                Err(error) => {
                    repr_error = Some(error);
                    String::new()
                }
            });
            repr_error.unwrap_or_else(|| ledger_error(text))
        }
    }
}

/// `world.resources = {**world.resources, resource: remaining}`, a new dict.
fn replace_resources(world: &Bound<'_, PyAny>, applied: Applied) -> PyResult<()> {
    let resources = attribute(world, intern!(world.py(), "resources"))?;
    let next = as_readable_dict(&resources).map_or_else(|| unsupported("the resources are not a dict"), |resources| Ok(resources.copy()?))?;
    next.set_item(RESOURCE_IDS[applied.index], applied.remaining)?;
    world_fields::replace_resources(world, &next)
}

/// `apply_resource_operation`. Its checks are the Python rule's, in its order, with its texts.
#[pyfunction]
pub(crate) fn apply_operation<'py>(world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>, operation: &Bound<'py, PyAny>) -> PyResult<()> {
    let py = world.py();
    let table = attribute(world, intern!(py, "model_state"))?;
    let Some(table) = as_readable_dict(&table) else {
        return unsupported("model_state is another kind of mapping");
    };
    if table.get_item(intern!(py, "resource_ledger_v1"))?.is_none() {
        return Err(ledger_error("resource ledger is not enabled for this scenario"));
    }
    held_mirror(world)?;
    let operation = match errors::str_of(operation)? {
        Some(text @ ("delivery" | "loss" | "reserve" | "release" | "consume")) => OperationArg::Text(text),
        _ => OperationArg::Other(errors::repr(operation)?),
    };
    // A `ScheduledEvent` is read as its row; any other object (a test's) by its attributes.
    let row = EventRow::from_event(event)?;
    let read = |name: &Bound<'py, PyString>| -> PyResult<V> { <V as VPy>::from_py(&attribute(event, name)?) };
    let (payload, source_id, event_id) = match &row {
        Some(row) => (row.payload.clone(), row.source_id.as_ref().map_or(V::Null, |id| V::Str(id.clone())), V::Str(row.event_id.clone())),
        None => (read(intern!(py, "payload"))?, read(intern!(py, "source_id"))?, read(intern!(py, "event_id"))?),
    };
    let event = OperationEvent { payload: &payload, source_id: &source_id, event_id: &event_id };
    match with_world(world, |world| core::apply_operation(world, &event, operation))? {
        Ok(applied) => replace_resources(world, applied),
        Err(fail) => Err(raised(py, fail)),
    }
}

/// An operation a rule planned without its event (the water and meal service). What the Python
/// rule turned down is its `ResourceLedgerError`, and a state its check of the ledger turned down is
/// Unsupported, both with nothing changed. Every ID is checked by the caller to be a stable ID.
pub(crate) fn apply_planned(world: &Bound<'_, PyAny>, _mirror: &Bound<'_, NamespaceMirror>, planned: &Operation<'_>) -> PyResult<()> {
    match with_world(world, |world| core::apply_planned(world, planned))? {
        Ok(applied) => replace_resources(world, applied),
        Err(fail) => Err(raised(world.py(), fail)),
    }
}

const COLUMNS: [[&str; 6]; 2] = [
    [
        "water_ml_initial",
        "water_ml_delivered",
        "water_ml_served",
        "water_ml_losses",
        "water_ml_remaining",
        "water_ml_reserved",
    ],
    [
        "food_meal_units_initial",
        "food_meal_units_delivered",
        "food_meal_units_served",
        "food_meal_units_losses",
        "food_meal_units_remaining",
        "food_meal_units_reserved",
    ],
];

/// `resource_metric_values`.
#[pyfunction]
pub(crate) fn metric_values<'py>(world: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyDict>>> {
    let py = world.py();
    if namespace_is_absent(world, intern!(py, "resource_ledger_v1"))? {
        return Ok(None);
    }
    held_mirror(world)?;
    let balances = with_world(world, |world| core::metric_values(world))?.or_py()?;
    let result = PyDict::new(py);
    for (names, values) in COLUMNS.iter().zip(&balances) {
        for (name, value) in names.iter().zip(values) {
            result.set_item(*name, *value)?;
        }
    }
    Ok(Some(result))
}

/// `reservation_id in ledger["used_reservation_ids"]`, without reading the list. The list holds only
/// str, so anything else was never used.
#[pyfunction]
fn reservation_id_was_used(world: &Bound<'_, PyAny>, reservation_id: &Bound<'_, PyAny>) -> PyResult<bool> {
    held_mirror(world)?;
    let Some(id) = errors::str_of(reservation_id)? else {
        return Ok(false);
    };
    with_world(world, |world| core::reservation_id_was_used(world, id))?.or_py()
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(apply_operation, m)?)?;
    m.add_function(wrap_pyfunction!(metric_values, m)?)?;
    m.add_function(wrap_pyfunction!(reservation_id_was_used, m)?)?;
    Ok(())
}
