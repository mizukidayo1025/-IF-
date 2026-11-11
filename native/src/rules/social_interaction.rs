//! `SocialInteractionHandler` of `events/handlers.py`: the glue of
//! `sheltersim_core::rules::social_interaction` (lane P8 d1). The dispatch loop hands the event's row
//! when the queue has one; the handler's `apply` makes it of the `ScheduledEvent`. What the rare
//! types read outside the world is gathered first: the EventLog's record of the contact a
//! cooperation or an information reaction answers, and the information model of a reaction.

use pyo3::prelude::*;

use sheltersim_core::rules::social_interaction::{self as rules, interaction_type, trigger_of, Inputs, Said};

use crate::core::errors::{self, OrPy};
use crate::core::world_core::with_world;
use crate::rules::common_operations::found_records;
use crate::rules::information_events::{event_row, information_spread, state_held};
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
    let mut inputs = Inputs::default();
    if let Some(trigger) = trigger_of(event) {
        inputs.trigger = found_records(world, vec![trigger.clone()])?.0.pop();
    }
    if interaction_type(event) == Some("information_reaction") {
        inputs.information = Some((information_spread(world)?, state_held(world)?));
    }
    with_world(world, |world| rules::apply_event(world, event, &inputs))?.or_py()
}

/// `query_pressure_per_claim` as an information reaction reads it, for the configs' tests.
pub(crate) fn pressure_gain(world: &Bound<'_, PyAny>) -> PyResult<f64> {
    rules::pressure_gain(&information_spread(world)?).or_py()
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(apply, m)?)?;
    Ok(())
}
