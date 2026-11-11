//! The events of an autonomous staff operation (`domain/staff_operations.py`): the glue of
//! `sheltersim_core::rules::staff_operation_events` (lane P8 b2). Each entry reads what Python holds
//! (whether `model_state` holds `staff_operations_v1`, `movement_v1`, `body_state_v1`,
//! `effect_program_v1` and `shared_operations_v1`; the floor plan; the staff operations formula of
//! the configs), makes the event's row, calls the core with the world borrowed whole, lets go the
//! objects of the queue's slots the core took out, and makes the Python values of what it returns:
//! the applied records of the receipts, which the dispatcher appends.
//!
//! The floor plan and the formula are read before the core runs, and what their reads raised is
//! raised where the core reads them (`FORMULA_UNREAD`, `PLAN_UNREAD`, and `SNAPSHOT_PLAN_UNREAD` as
//! the staff positions' `KernelRefusal`). The admission of an intake is the population rules' here:
//! the core plans the completion, the glue checks the waiting and admits, and the core writes the
//! rest.

use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString, PyTuple};
use sheltersim_core::config::staff::StaffOperations;
use sheltersim_core::error::Result as CoreResult;
use sheltersim_core::rules::care_receipts::Made;
use sheltersim_core::rules::kit::Held;
use sheltersim_core::rules::overlay::PLAN_UNREAD;
use sheltersim_core::rules::staff_operation_events::{self as core, CompletePlan, Inputs, Planned, Stopping, Work, FORMULA_UNREAD, SNAPSHOTS, SNAPSHOT_PLAN_UNREAD};

use crate::core::errors::{self, OrPy};
use crate::core::row::{attribute, exact_int, exact_str};
use crate::core::tracked::as_readable_dict;
use crate::core::world_core::{namespace_held, raw_dict_get, with_world};
use crate::rules::care_receipts::made_record;
use crate::rules::handlers::{counted, get_is};
use crate::rules::information_events::event_row;
use crate::rules::movement_geometry::{Geo, GeoPy};
use crate::rules::travel::{movement_held, read_queue_rows};
use crate::state::events::{write_result, EventRow, EventWriter};
use crate::state::staff_operations::NAME;
use crate::Unsupported;

const SHARED_OPERATIONS: &str = "shared_operations_v1";

/// What the rules read of the Python world, read once per entry before the core borrows it.
pub(crate) struct Gathered {
    staff: Held,
    movement: Held,
    body: Held,
    program: Held,
    shared_absent: bool,
    /// The floor plan as `Geo::of` read it, where the entry may read it.
    plan: Option<PyResult<Arc<Geo>>>,
    /// The staff operations formula as the configs give it, where the entry may read it.
    formula: Option<PyResult<Arc<StaffOperations>>>,
}

/// Whether `model_state` holds `staff_operations_v1`, as the rules take it: a value that is not a
/// dict is no state (`Absent`); another dict than the row's is refused where a rule reads it.
pub(crate) fn staff_held(world: &Bound<'_, PyAny>) -> PyResult<Held> {
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

impl Gathered {
    /// What the rules read, with the floor plan where `geo` and the formula where `formula`.
    pub(crate) fn of(world: &Bound<'_, PyAny>, geo: bool, formula: bool) -> PyResult<Self> {
        let py = world.py();
        let table = attribute(world, intern!(py, "model_state"))?;
        Ok(Self {
            staff: staff_held(world)?,
            movement: movement_held(world)?,
            body: namespace_held(world, intern!(py, "body_state_v1"), "the world holds another body state")?,
            program: namespace_held(world, intern!(py, "effect_program_v1"), "the world holds another effect_program_v1")?,
            shared_absent: !table.contains(intern!(py, SHARED_OPERATIONS))?,
            plan: geo.then(|| Geo::of(world)),
            formula: formula.then(|| crate::rules::staff::staff_operations(world)),
        })
    }

    pub(crate) fn inputs(&self) -> Inputs<'_> {
        Inputs {
            staff: &self.staff,
            movement: &self.movement,
            body: &self.body,
            program: &self.program,
            shared_absent: self.shared_absent,
            geo: self.plan.as_ref().and_then(|plan| plan.as_ref().ok()),
            formula: self.formula.as_ref().and_then(|formula| formula.as_ref().ok()).map(|formula| &**formula),
        }
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
        let plan = self.plan.as_ref().and_then(|plan| plan.as_ref().err());
        let formula = self.formula.as_ref().and_then(|formula| formula.as_ref().err());
        match (&*text, plan, formula) {
            (PLAN_UNREAD, Some(read), _) => Err(read.clone_ref(py)),
            (SNAPSHOT_PLAN_UNREAD, Some(read), _) => Err(errors::kernel_refusal(py, SNAPSHOTS, read.clone_ref(py))),
            (FORMULA_UNREAD, _, Some(read)) => Err(read.clone_ref(py)),
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
        with_world(world, |cell| -> CoreResult<Option<Arc<str>>> {
            match core::plan_arrive(cell, &gathered.inputs(), &row)? {
                Planned::Go(plan) => core::write_arrive(cell, plan).map(|()| None),
                Planned::Unavailable(id) => Ok(Some(id)),
            }
        })?
        .or_py()
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
    let result = with_world(world, |cell| {
        let inputs = gathered.inputs();
        let plan = core::plan_relief(cell, &inputs, &row)?;
        core::write_relief(cell, &inputs, plan, &mut taken)
    })?;
    forget(world, &taken)?;
    let made = counted(py, "relief", gathered.finish(py, write_result(py, result)))?;
    receipts(py, made.into_iter().collect())
}

// ---- Completion --------------------------------------------------------------------------------------

/// What the plan of a completion led to.
enum Step {
    Done(Vec<Made>),
    Unavailable(Arc<str>),
    Intake(Box<CompletePlan>),
}

/// `StaffOperationCompletionHandler.apply`, with `complete_staff_operation` inside it.
#[pyfunction]
fn complete<'py>(_handler: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyTuple>>> {
    let py = world.py();
    let row = event_row(event)?;
    let gathered = Gathered::of(world, false, false)?;
    let step = with_world(world, |cell| -> CoreResult<Step> {
        let inputs = gathered.inputs();
        match core::plan_complete(cell, &inputs, &row)? {
            Planned::Unavailable(id) => Ok(Step::Unavailable(id)),
            Planned::Go(plan) if matches!(plan.work, Work::Intake) => Ok(Step::Intake(Box::new(plan))),
            Planned::Go(plan) => core::write_complete(cell, &inputs, plan).map(Step::Done),
        }
    })?
    .or_py();
    let made = match counted(py, "complete", step)? {
        Step::Done(made) => made,
        Step::Unavailable(id) => return stop_and_suppress(world, &id, &row, "staff became unavailable before completing the assigned task"),
        Step::Intake(plan) => {
            // What the admission will ask, while this can still refuse; then the admission, which
            // is the work of an intake.
            counted(py, "complete", crate::rules::population::check_waiting_for_admission(world))?;
            let now = attribute(world, intern!(py, "minute_index"))?;
            crate::rules::population::admit_pending_arrivals(world, &now)?;
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

/// `_interrupt_trips_of_event`: a route just closed stops the trips that use it.
pub(crate) fn interrupt_trips_of_event<'py>(world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<()> {
    let py = world.py();
    let payload = attribute(event, intern!(py, "payload"))?;
    let parameters = payload.call_method1(intern!(py, "get"), ("parameters", PyDict::new(py)))?;
    if get_is(&payload, "op", "set_route_access")?
        && parameters.downcast::<pyo3::types::PyMapping>().is_ok()
        && parameters.call_method1(intern!(py, "get"), ("accessible",))?.downcast_exact::<pyo3::types::PyBool>().is_ok_and(|flag| !flag.is_true())
    {
        interrupt_trips(world, event)?;
    }
    Ok(())
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(interrupt_trips, m)?)?;
    m.add_function(wrap_pyfunction!(interrupt, m)?)?;
    m.add_function(wrap_pyfunction!(arrive, m)?)?;
    m.add_function(wrap_pyfunction!(relief, m)?)?;
    m.add_function(wrap_pyfunction!(complete, m)?)?;
    Ok(())
}
