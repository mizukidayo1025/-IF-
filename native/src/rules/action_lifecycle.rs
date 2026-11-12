//! `domain/action_lifecycle.py`: the glue of the kernels of `sheltersim_core::rules::
//! action_lifecycle` (lane P8 a2). Each entry reads what Python holds (the arguments, the event as
//! its row, whether the log holds it applied), runs the core's kernel on the world and the rows of
//! the open actions (`ActiveActions::on_world`, `core/actions.rs`), and returns. The Python object of
//! a controller's open actions (`ActiveActions`) is registered in this module.

use pyo3::exceptions::PyTypeError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyBool;

use sheltersim_core::rules::action_lifecycle as rules;

pub(crate) use crate::core::actions::ActiveActions;
use crate::core::row::{attribute, exact_int, exact_str, unsupported};
use crate::state::events::{applied_row, EventRow};

pub(crate) use sheltersim_core::rules::action_lifecycle::{ends_any_action, Proposal};
use sheltersim_core::rules::action_lifecycle::Ending;

fn store_of<'a, 'py>(value: &'a Bound<'py, PyAny>) -> PyResult<&'a Bound<'py, ActiveActions>> {
    value.downcast::<ActiveActions>().map_err(|_| PyTypeError::new_err("active_actions is not the store"))
}

// ---- record_action_proposal --------------------------------------------------------------------------

/// `record_action_proposal` of a committed proposal, for a Rust caller too. Unsupported, with
/// nothing changed, for the Python rule on `active.as_plain()`.
pub(crate) fn commit_action(world: &Bound<'_, PyAny>, active: &Bound<'_, ActiveActions>, proposal: &Proposal<'_>) -> PyResult<()> {
    active.try_borrow_mut()?.on_world(world, |cell, rows| rules::commit_action(cell, rows, proposal))
}

/// `record_action_proposal`.
#[pyfunction]
#[pyo3(signature = (world, proposal, *, target_id, committed, active_actions))]
pub(crate) fn record_action_proposal(
    world: &Bound<'_, PyAny>,
    proposal: &Bound<'_, PyAny>,
    target_id: &Bound<'_, PyAny>,
    committed: &Bound<'_, PyAny>,
    active_actions: &Bound<'_, PyAny>,
) -> PyResult<()> {
    let py = world.py();
    let Ok(committed) = committed.downcast_exact::<PyBool>() else {
        return unsupported("committed is not a bool");
    };
    if !committed.is_true() {
        return Ok(());
    }
    let active = store_of(active_actions)?;
    let agent_id = attribute(proposal, intern!(py, "agent_id"))?;
    let action = attribute(&attribute(proposal, intern!(py, "action"))?, intern!(py, "value"))?;
    let trigger = attribute(proposal, intern!(py, "trigger"))?;
    commit_action(
        world,
        active,
        &Proposal {
            agent_id: exact_str(&agent_id)?,
            action: exact_str(&action)?,
            target_id: if target_id.is_none() { None } else { Some(exact_str(target_id)?) },
            minute: exact_int(&attribute(proposal, intern!(py, "minute_index"))?)?,
            trigger: exact_str(&trigger)?,
        },
    )
}

// ---- The applied events ------------------------------------------------------------------------------

/// `record_applied_action_completion`.
#[pyfunction]
#[pyo3(signature = (world, event, minute, *, active_actions))]
fn record_applied_action_completion(
    world: &Bound<'_, PyAny>,
    event: &Bound<'_, PyAny>,
    minute: &Bound<'_, PyAny>,
    active_actions: &Bound<'_, PyAny>,
) -> PyResult<()> {
    let py = world.py();
    let event_type = attribute(event, intern!(py, "event_type"))?;
    // Nothing below changes anything for another type of event.
    if !ends_any_action(exact_str(&event_type)?) {
        return Ok(());
    }
    store_of(active_actions)?;
    let event_id = attribute(event, intern!(py, "event_id"))?;
    let Some(row) = applied_row(world, exact_str(&event_id)?, event)? else {
        return Ok(());
    };
    complete_row(world, active_actions, &row, exact_int(minute)?)
}

fn complete_row(world: &Bound<'_, PyAny>, active_actions: &Bound<'_, PyAny>, row: &EventRow, minute: i64) -> PyResult<()> {
    complete(world, active_actions, &Ending::of(row), minute)
}

fn complete(world: &Bound<'_, PyAny>, active_actions: &Bound<'_, PyAny>, event: &Ending<'_>, minute: i64) -> PyResult<()> {
    if !ends_any_action(event.event_type) {
        return Ok(());
    }
    // A plain dict of open actions is the Python rule's.
    let Ok(active) = active_actions.downcast::<ActiveActions>() else {
        return unsupported("active_actions is not the store");
    };
    let mut this = active.try_borrow_mut()?;
    // Most applied events end no open action: the world is not borrowed for them.
    if this.read(world.py(), |rows| rules::closes(rows, event).is_none())? {
        return Ok(());
    }
    this.on_world(world, |cell, rows| rules::complete_for_event(cell, rows, event, minute))
}

/// `record_applied_population_exit`.
#[pyfunction]
#[pyo3(signature = (world, event, minute, *, active_actions))]
fn record_applied_population_exit(
    world: &Bound<'_, PyAny>,
    event: &Bound<'_, PyAny>,
    minute: &Bound<'_, PyAny>,
    active_actions: &Bound<'_, PyAny>,
) -> PyResult<()> {
    let py = world.py();
    let event_type = attribute(event, intern!(py, "event_type"))?;
    if exact_str(&event_type)? != "effect_program_population" {
        return Ok(());
    }
    let active = store_of(active_actions)?;
    let event_id = attribute(event, intern!(py, "event_id"))?;
    let event_id = exact_str(&event_id)?;
    if applied_row(world, event_id, event)?.is_none() {
        return Ok(());
    }
    let minute = exact_int(minute)?;
    active.try_borrow_mut()?.on_world(world, |cell, rows| rules::population_exit(cell, rows, event_id, minute))
}

/// `finalize_active_actions`.
#[pyfunction]
#[pyo3(signature = (world, minute, *, active_actions))]
fn finalize_active_actions(world: &Bound<'_, PyAny>, minute: &Bound<'_, PyAny>, active_actions: &Bound<'_, PyAny>) -> PyResult<()> {
    let active = store_of(active_actions)?;
    let minute = exact_int(minute)?;
    active.try_borrow_mut()?.on_world(world, |cell, rows| rules::finalize(cell, rows, minute))
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<ActiveActions>()?;
    m.add_function(wrap_pyfunction!(record_action_proposal, m)?)?;
    m.add_function(wrap_pyfunction!(record_applied_action_completion, m)?)?;
    m.add_function(wrap_pyfunction!(record_applied_population_exit, m)?)?;
    m.add_function(wrap_pyfunction!(finalize_active_actions, m)?)?;
    Ok(())
}
