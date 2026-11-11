//! `order_staff_operation` of `domain/staff_operations.py`: the glue of
//! `sheltersim_core::rules::staff_order` (lane P8 b2). It reads what Python holds
//! (`staff_operation_events::Gathered`: the namespaces, the floor plan and the staff operations
//! formula), reads the rows of the events Python scheduled, and calls the core with the world.
//! `effects.rs` calls `order_operation` for an EffectProgram's `staff_task_order`, in its
//! transaction.

use pyo3::prelude::*;

pub(crate) use sheltersim_core::rules::staff_order::Order;

use crate::core::row::exact_str;
use crate::core::world_core::with_world;
use crate::rules::staff_operation_events::Gathered;
use crate::rules::travel::read_queue_rows;
use crate::state::events::write_result;

/// `order_staff_operation`: the task's ID. Its checks raise before any change.
pub(crate) fn order_operation(world: &Bound<'_, PyAny>, order: &Order<'_>) -> PyResult<String> {
    let py = world.py();
    read_queue_rows(world)?;
    let gathered = Gathered::of(world, true, true)?;
    let result = with_world(world, |cell| sheltersim_core::rules::staff_order::order_operation(cell, &gathered.inputs(), order))?;
    gathered.finish(py, write_result(py, result))
}

/// `order_staff_operation`, called from Python with its keywords: for the tests, which compare it
/// with the Python rule on copies of one world.
#[pyfunction]
#[pyo3(signature = (world, *, staff_id, task_kind, target_zone_id, target_id, reason, causation_event_id, source_id))]
#[allow(clippy::too_many_arguments)]
fn order(
    world: &Bound<'_, PyAny>,
    staff_id: &Bound<'_, PyAny>,
    task_kind: &Bound<'_, PyAny>,
    target_zone_id: &Bound<'_, PyAny>,
    target_id: &Bound<'_, PyAny>,
    reason: &Bound<'_, PyAny>,
    causation_event_id: &Bound<'_, PyAny>,
    source_id: &Bound<'_, PyAny>,
) -> PyResult<String> {
    let optional = |value: &Bound<'_, PyAny>| -> PyResult<Option<String>> {
        if value.is_none() {
            Ok(None)
        } else {
            Ok(Some(exact_str(value)?.to_owned()))
        }
    };
    let target_id = optional(target_id)?;
    let source_id = optional(source_id)?;
    order_operation(
        world,
        &Order {
            staff_id: exact_str(staff_id)?,
            task_kind: exact_str(task_kind)?,
            target_zone_id: exact_str(target_zone_id)?,
            target_id: target_id.as_deref(),
            reason: exact_str(reason)?,
            causation_event_id: exact_str(causation_event_id)?,
            source_id: source_id.as_deref(),
        },
    )
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(order, m)?)?;
    Ok(())
}
