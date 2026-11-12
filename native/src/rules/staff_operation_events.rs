//! The events of an autonomous staff operation (`domain/staff_operations.py`): the glue of
//! `sheltersim_core::rules::staff_operation_events` (lane P8 b2). The core reads, through the native
//! host (`core::Gathered`, lane P9-1a), whether `model_state` holds `staff_operations_v1`,
//! `movement_v1`, `body_state_v1`, `effect_program_v1` and `shared_operations_v1`, the floor plan
//! and the staff operations formula of the configs; each entry makes the event's row, calls the core with the world borrowed whole, lets go the
//! objects of the queue's slots the core took out, and makes the Python values of what it returns:
//! the applied records of the receipts, which the dispatcher appends.
//!
//! The floor plan and the formula are read before the core runs, and what their reads raised is
//! raised where the core reads them (`FORMULA_UNREAD`, `PLAN_UNREAD`, and `SNAPSHOT_PLAN_UNREAD` as
//! the staff positions' `KernelRefusal`). The admission of an intake is the population rules' (in the
//! core, lane P9-3a): the core plans the completion, checks the waiting and admits, and writes the
//! rest.

use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyTuple;
use sheltersim_core::rules::care_receipts::Made;
use sheltersim_core::rules::overlay::PLAN_UNREAD;
use sheltersim_core::rules::staff_operation_events::{self as core, Completed, Inputs, Stopping, FORMULA_UNREAD, SNAPSHOTS, SNAPSHOT_PLAN_UNREAD};

use crate::core::errors::{self, OrPy};
use crate::core::row::{attribute, exact_int, exact_str};
use crate::core::world_core::{with_world, PyWorldCell};
use crate::host::{HostError, PyHost};
use crate::rules::care_receipts::made_record;
use crate::rules::handlers::counted;
use crate::rules::information_events::event_row;
use crate::rules::travel::read_queue_rows;
use crate::state::events::{write_result, EventRow, EventWriter};
use crate::Unsupported;

/// What the rules read of the Python world, read once per entry by the core through the native host
/// before it borrows the world (`core::Gathered`, lane P9-1a).
pub(crate) struct Gathered<'py> {
    world: Bound<'py, PyAny>,
    read: core::Gathered<HostError>,
}

impl<'py> Gathered<'py> {
    /// What the rules read, with the floor plan where `geo` and the formula where `formula`.
    pub(crate) fn of(world: &Bound<'py, PyAny>, geo: bool, formula: bool) -> PyResult<Self> {
        let mut host = PyHost::of_entry(world);
        let read = core::Gathered::read(&PyWorldCell(world), &mut host, geo, formula).map_err(|error| error.0)?;
        Ok(Self { world: world.clone(), read })
    }

    pub(crate) fn inputs(&self) -> Inputs<'_> {
        self.read.inputs()
    }

    /// The core's result, with what a read raised where the core read what it could not get.
    pub(crate) fn finish<T>(&self, py: Python<'_>, result: PyResult<T>) -> PyResult<T> {
        let Err(error) = result else {
            return result;
        };
        if !error.is_instance_of::<Unsupported>(py) {
            return Err(error);
        }
        let text = error.value(py).to_string();
        let plan = self.read.plan.as_ref().and_then(|plan| plan.as_ref().err()).map(|read| read.0.clone_ref(py));
        let formula = self.read.formula.as_ref().and_then(|formula| formula.as_ref().err());
        match (&*text, plan, formula) {
            (PLAN_UNREAD, Some(read), _) => Err(read),
            (SNAPSHOT_PLAN_UNREAD, Some(read), _) => Err(errors::kernel_refusal(py, SNAPSHOTS, read)),
            // What reading the formula again raises (policy E14).
            (FORMULA_UNREAD, _, Some(_)) => match crate::rules::staff::staff_operations(&self.world) {
                Err(read) => Err(read),
                Ok(_) => Err(error),
            },
            _ => Err(error),
        }
    }
}

/// Let go the objects of the queue's slots the core took out (`QueueStore::forget`).
fn forget(world: &Bound<'_, PyAny>, taken: &[u64]) -> PyResult<()> {
    if taken.is_empty() {
        return Ok(());
    }
    EventWriter::of(world)?.queue().get().forget(taken)
}

/// The receipts as the handler returns them: a tuple of applied records, or None.
fn receipts<'py>(py: Python<'py>, made: Vec<Made>) -> PyResult<Option<Bound<'py, PyTuple>>> {
    if made.is_empty() {
        return Ok(None);
    }
    let records = made.into_iter().map(|made| made_record(py, made)).collect::<PyResult<Vec<_>>>()?;
    Ok(Some(PyTuple::new(py, records)?))
}

// ---- interrupt_staff_operation ---------------------------------------------------------------------

/// `interrupt_staff_operation`, for the kernels of this crate that hold the event's object.
pub(crate) fn interrupt_operation<'py>(world: &Bound<'py, PyAny>, staff_id: &str, event: &Bound<'py, PyAny>, current_event_id: Option<&str>) -> PyResult<bool> {
    let py = world.py();
    let event_id = exact_str(&attribute(event, intern!(py, "event_id"))?)?.to_owned();
    let due_minute = exact_int(&attribute(event, intern!(py, "due_minute"))?)?;
    read_queue_rows(world)?;
    let gathered = Gathered::of(world, true, false)?;
    let mut taken = Vec::new();
    let stopping = Stopping { event_id: &event_id, due_minute };
    let result = with_world(world, |cell| core::interrupt(cell, &gathered.inputs(), staff_id, &stopping, current_event_id, &mut taken))?;
    forget(world, &taken)?;
    gathered.finish(py, write_result(py, result))
}

/// `interrupt_staff_operation`, called from Python.
#[pyfunction]
#[pyo3(signature = (world, staff_id, event, *, current_event_id=None))]
fn interrupt<'py>(world: &Bound<'py, PyAny>, staff_id: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>, current_event_id: Option<&str>) -> PyResult<bool> {
    counted(world.py(), "interrupt", interrupt_operation(world, exact_str(staff_id)?, event, current_event_id))
}

/// `interrupt_staff_operation(world, staff_id, event, current_event_id=event.event_id)` and then
/// `raise EventSuppressed(text)`.
fn stop_and_suppress<T>(world: &Bound<'_, PyAny>, staff_id: &str, row: &EventRow, text: &str) -> PyResult<T> {
    let py = world.py();
    read_queue_rows(world)?;
    let gathered = Gathered::of(world, true, false)?;
    let mut taken = Vec::new();
    let result = with_world(world, |cell| core::stop_and_suppress(cell, &gathered.inputs(), staff_id, row, text, &mut taken))?;
    forget(world, &taken)?;
    gathered.finish(py, write_result(py, result))?;
    Err(pyo3::exceptions::PyRuntimeError::new_err("a stop that did not suppress"))
}

// ---- Arrival -----------------------------------------------------------------------------------------

/// `arrive_staff_operation`.
#[pyfunction]
fn arrive<'py>(_handler: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<()> {
    let py = world.py();
    let step = (|| -> PyResult<Option<Arc<str>>> {
        let row = event_row(event)?;
        let gathered = Gathered::of(world, false, false)?;
        with_world(world, |cell| core::arrive(cell, &gathered.inputs(), &row))?.or_py()
    })();
    match counted(py, "arrive", step)? {
        None => Ok(()),
        Some(id) => stop_and_suppress(world, &id, &event_row(event)?, "staff became unavailable before reaching the assigned task"),
    }
}

// ---- Relief ------------------------------------------------------------------------------------------

/// `StaffOperationReliefHandler.apply`.
#[pyfunction]
fn relief<'py>(_handler: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyTuple>>> {
    let py = world.py();
    let row = event_row(event)?;
    read_queue_rows(world)?;
    let gathered = Gathered::of(world, true, true)?;
    let mut taken = Vec::new();
    let result = with_world(world, |cell| core::relief(cell, &gathered.inputs(), &row, &mut taken))?;
    forget(world, &taken)?;
    let made = counted(py, "relief", gathered.finish(py, write_result(py, result)))?;
    receipts(py, made.into_iter().collect())
}

// ---- Completion --------------------------------------------------------------------------------------

/// `StaffOperationCompletionHandler.apply`, with `complete_staff_operation` inside it.
#[pyfunction]
fn complete<'py>(_handler: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyTuple>>> {
    let py = world.py();
    let row = event_row(event)?;
    let gathered = Gathered::of(world, false, false)?;
    let step = with_world(world, |cell| core::complete_or_intake(cell, &gathered.inputs(), &row))?.or_py();
    let made = match counted(py, "complete", step)? {
        Completed::Done(made) => made,
        Completed::Unavailable(id) => return stop_and_suppress(world, &id, &row, "staff became unavailable before completing the assigned task"),
        Completed::Intake(plan) => {
            // What the admission will ask, while this can still refuse; then the admission, which
            // is the work of an intake (the core's, lane P9-3a).
            let (cell, mut host) = (PyWorldCell(world), PyHost::of_entry(world));
            counted(py, "complete", sheltersim_core::rules::population::check_waiting(&cell, &mut host).map_err(|error| error.0))?;
            let now = exact_int(&attribute(world, intern!(py, "minute_index"))?)?;
            sheltersim_core::rules::population::admit_arrivals(&cell, &mut host, now).map_err(|error| error.0)?;
            with_world(world, |cell| core::write_complete(cell, &gathered.inputs(), *plan))?.or_py()?
        }
    };
    receipts(py, made)
}

// ---- interrupt_staff_trips_on_closed_routes ------------------------------------------------------

/// `interrupt_staff_trips_on_closed_routes`.
#[pyfunction]
pub(crate) fn interrupt_trips<'py>(world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    let py = world.py();
    let event_id = exact_str(&attribute(event, intern!(py, "event_id"))?)?.to_owned();
    let due_minute = exact_int(&attribute(event, intern!(py, "due_minute"))?)?;
    read_queue_rows(world)?;
    let gathered = Gathered::of(world, true, false)?;
    let mut taken = Vec::new();
    let stopping = Stopping { event_id: &event_id, due_minute };
    let result = with_world(world, |cell| core::interrupt_trips(cell, &gathered.inputs(), &stopping, &mut taken))?;
    forget(world, &taken)?;
    let interrupted = gathered.finish(py, write_result(py, result))?;
    PyTuple::new(py, interrupted.iter().map(|id| &**id))
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(interrupt_trips, m)?)?;
    m.add_function(wrap_pyfunction!(interrupt, m)?)?;
    m.add_function(wrap_pyfunction!(arrive, m)?)?;
    m.add_function(wrap_pyfunction!(relief, m)?)?;
    m.add_function(wrap_pyfunction!(complete, m)?)?;
    Ok(())
}
