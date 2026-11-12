//! `SocialInteractionHandler` of `events/handlers.py`: the glue of
//! `sheltersim_core::rules::social_interaction` (lane P8 d1). The dispatch loop hands the event's row
//! when the queue has one; the handler's `apply` makes it of the `ScheduledEvent`. What the rare
//! types read outside the world (the EventLog's record of the contact a cooperation or an
//! information reaction answers, and the information model of a reaction) the core reads through
//! the native host (`Inputs::read`, lane P9-1a).

use pyo3::prelude::*;

use pyo3::types::PyDict;

use sheltersim_core::rules::social_interaction::{self as rules, Said};

use crate::core::errors::{self, OrPy};
use crate::core::world_core::PyWorldCell;
use crate::host::PyHost;
use crate::rules::information_events::{event_row, information_spread};
use crate::state::events::{EventRef, EventRow};

/// `SocialInteractionHandler.apply`. Raises what the Python rule raises for an event that does not
/// apply; the dispatch loop calls `apply_ref`, which has no exception to pay for.
#[pyfunction]
fn apply<'py>(_handler: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<()> {
    match apply_row(world, &event_row(event)?)? {
        None => Ok(()),
        Some((outcome, text)) => errors::raise_noted(world.py(), outcome, text),
    }
}

/// The rule of a due event: None for an applied event, or the outcome and text of one that does
/// not apply.
pub(crate) fn apply_ref(world: &Bound<'_, PyAny>, event: EventRef<'_, '_>) -> PyResult<Said> {
    match event {
        EventRef::Row(row) => apply_row(world, row),
        EventRef::Object(event) => apply_row(world, &event_row(event)?),
    }
}

fn apply_row(world: &Bound<'_, PyAny>, event: &EventRow) -> PyResult<Said> {
    let mut host = PyHost::of_world(world, PyDict::new(world.py()));
    rules::apply(&PyWorldCell(world), &mut host, event).map_err(|error| error.0)
}

/// `query_pressure_per_claim` as an information reaction reads it, for the configs' tests.
pub(crate) fn pressure_gain(world: &Bound<'_, PyAny>) -> PyResult<f64> {
    rules::pressure_gain(&information_spread(world)?).or_py()
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(apply, m)?)?;
    Ok(())
}
