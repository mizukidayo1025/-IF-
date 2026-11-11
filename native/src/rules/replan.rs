//! `_replan_journeys` of `experiments/phases/update.py`: the glue of the core's kernel
//! (`sheltersim_core::rules::replan`, lane c6 of Issue #152). It reads what Python holds once
//! (the Run's graph and `staff_service`, whether `model_state` holds the namespaces, the sections of
//! the configs, the floor plan), and runs each person's two steps on the world borrowed whole: a
//! refusal before the first stop says Unsupported, with nothing changed; past it, a stop is written,
//! and a refusal is a RuntimeError.
//!
//! The Python objects of the events a step took out of the queue are let go here. A support whose
//! staff the person can no longer reach ends in a continuity receipt the core recorded; the glue
//! projects it into `shared_operations_v1` (`care_receipts::ingest_applied`, which reads the
//! EventLog's records as Python holds them) and finishes the person's walk (`finish_movement`), each
//! a KernelRefusal where it refuses, as the Python steps it called were, and the core counts the
//! replan after them.

use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyString;
use sheltersim_core::config::overlay::overlay_schema as parse_overlay_schema;
use sheltersim_core::rules::kit::{check_metric_state, POLICY_FAULT};
use sheltersim_core::rules::overlay::{Beds, OVERLAY_KEY};
use sheltersim_core::rules::replan::{self as core, Replanning, Second};

use crate::core::census;
use crate::core::configs;
use crate::core::errors::{self, OrPy};
use crate::core::row::{attribute, exact_int, exact_str};
use crate::core::run::policies_of;
use crate::core::world_core::{namespace_held, with_world};
use crate::rules::movement::mirror_held;
use crate::rules::travel::{metric_state_held, read_queue_rows, rows_of, Inputs};
use crate::state::events::{write_result, EventWriter};
use crate::Unsupported;

/// Say Unsupported no more: a stop has been written.
fn after_a_write(py: Python<'_>, result: PyResult<()>) -> PyResult<()> {
    match result {
        Err(error) if error.is_instance_of::<Unsupported>(py) => {
            census::caught(py, "replan after a stop", &error);
            Err(PyRuntimeError::new_err(format!("a kernel said Unsupported after a replan was written: {}", error.value(py))))
        }
        other => other,
    }
}

/// A refusal of the Run's `staff_service` whose read raised, as reading it again raises.
fn policy_fault<T>(controller: &Bound<'_, PyAny>, result: PyResult<T>) -> PyResult<T> {
    let py = controller.py();
    match result {
        Err(error) if error.is_instance_of::<Unsupported>(py) && error.value(py).to_string() == POLICY_FAULT => {
            let service = controller.getattr(intern!(py, "staff_service"))?;
            service.getattr(intern!(py, "policy_version"))?;
            service.getattr(intern!(py, "cooldown_minutes"))?;
            Err(error)
        }
        result => result,
    }
}

/// `_replan_journeys`. `state` is the movement namespace, which the row stands for.
#[pyfunction]
pub(crate) fn replan_journeys<'py>(
    controller: &Bound<'py, PyAny>,
    state: &Bound<'py, PyAny>,
    agent_ids: &Bound<'py, PyAny>,
    minute: &Bound<'py, PyAny>,
) -> PyResult<()> {
    let _ = state;
    let py = controller.py();
    let world = attribute(controller, intern!(py, "world"))?;
    let minute = exact_int(minute)?;
    mirror_held(&world)?;
    EventWriter::of(&world)?;
    let metric_state = metric_state_held(&world)?;
    with_world(&world, |cell| check_metric_state(cell, metric_state))?.or_py()?;
    let ids = agent_ids.try_iter()?.collect::<PyResult<Vec<_>>>()?;
    let ids = ids.iter().map(|id| Ok(Arc::<str>::from(exact_str(id)?))).collect::<PyResult<Vec<Arc<str>>>>()?;
    if ids.is_empty() {
        return Ok(());
    }

    // What the steps read of Python, read once: no step changes it.
    let graph = rows_of(controller)?;
    read_queue_rows(&world)?;
    let inputs = Inputs::of(&world)?;
    let overlay = namespace_held(&world, intern!(py, OVERLAY_KEY), "the world holds another overlay state")?;
    let schema = configs::kept_section(&world, |configs| &configs.overlay_schema, parse_overlay_schema)?;
    let policies = policies_of(controller)?;
    let replanning = Replanning {
        routing: inputs.routing(&graph),
        walking: inputs.walking(),
        beds: Beds { overlay: &overlay, movement: &inputs.movement, schema: &schema, walking: &inputs.section, geo: inputs.plan.get().map(|geo| &**geo) },
        movement: &inputs.section,
        staff_service: &policies.services.staff,
    };
    let queue = EventWriter::of(&world)?.queue().clone();
    let forget = |taken: &[u64]| queue.get().forget(taken);

    let mut written = false;
    for agent_id in &ids {
        let result = (|| -> PyResult<()> {
            let Some(journey) = with_world(&world, |cell| core::journey_of(cell, agent_id))?.or_py()? else {
                return Ok(());
            };
            // Says Unsupported only before its first write.
            let taken = write_result(py, with_world(&world, |cell| core::stop(cell, agent_id, &journey, minute))?)?;
            written = true;
            forget(&taken)?;
            let second = with_world(&world, |cell| core::second_step(cell, &replanning, agent_id, &journey, minute))?;
            let second = policy_fault(controller, inputs.finish(py, write_result(py, second)))?;
            match second {
                Second::Done(taken) => forget(&taken),
                Second::Continuity { event, minute: realized, taken } => {
                    forget(&taken)?;
                    crate::rules::care_receipts::ingest_applied(&world, &event, realized)
                        .map_err(|error| errors::kernel_refusal(py, "care_receipts.ingest", error))?;
                    crate::rules::movement::finish_movement(&world, PyString::new(py, agent_id).as_any())
                        .map_err(|error| errors::kernel_refusal(py, "movement.finish_movement", error))?;
                    with_world(&world, |cell| core::after_continuity(cell))?.or_py()
                }
            }
        })();
        if written {
            after_a_write(py, result)?;
        } else {
            // Nothing is written yet: a refusal changes nothing.
            result?;
        }
    }
    Ok(())
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(replan_journeys, m)?)?;
    Ok(())
}
