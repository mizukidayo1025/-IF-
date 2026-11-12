//! `_replan_journeys` of `experiments/phases/update.py`: the glue of the core's kernel
//! (`sheltersim_core::rules::replan`, lane c6 of Issue #152). It reads once what the steps read
//! outside the world: the Run's graph and `staff_service`, and, through the native host (lane
//! P9-1a), whether `model_state` holds the namespaces and the sections of the configs; the walking
//! inputs and the floor plan are the travel glue's. It runs each person's two steps on the world
//! borrowed whole: a refusal before the first stop says Unsupported, with nothing changed; past it,
//! a stop is written, and a refusal is a RuntimeError.
//!
//! The Python objects of the events a step took out of the queue are let go here. A support whose
//! staff the person can no longer reach ends in a continuity receipt the core recorded; the core
//! projects it into `shared_operations_v1` (`care_receipts::ingest_applied`, which looks up the
//! EventLog's records through `Host::find_record`) and finishes the person's walk
//! (`movement::finish_through`), each a KernelRefusal where it refuses, as the Python steps it called
//! were, and the core counts the replan after them.

use std::sync::Arc;

use pyo3::exceptions::PyRuntimeError;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use sheltersim_core::config::overlay::overlay_schema as parse_overlay_schema;
use sheltersim_core::config::SectionId;
use sheltersim_core::rules::kit::{self, check_metric_state, POLICY_FAULT};
use sheltersim_core::rules::{care_receipts, movement};
use sheltersim_core::world::METRIC_STATE;
use sheltersim_core::rules::overlay::{Beds, OVERLAY_KEY};
use sheltersim_core::rules::replan::{self as core, Replanning, Second};

use crate::core::census;
use crate::core::errors::{self, OrPy};
use crate::core::row::{attribute, exact_int, exact_str};
use crate::core::run::policies_of;
use crate::core::world_core::{with_world, PyWorldCell};
use crate::host::PyHost;
use crate::rules::travel::{read_queue_rows, rows_of, Inputs};
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
    // What the world holds, read by the core's host once (lane P9-1a): no step adds or takes away a
    // key of `model_state`.
    let cell = PyWorldCell(&world);
    let mut host = PyHost::of_world(&world, PyDict::new(py));
    let holding = kit::holding(&cell, &mut host).map_err(|error| error.0)?;
    movement::check_held(&holding).or_py()?;
    EventWriter::of(&world)?;
    let metric_state = holding.field(METRIC_STATE);
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
    let overlay = holding.held(OVERLAY_KEY, core::OTHER_OVERLAY);
    let schema = kit::section_value_kept(&cell, &mut host, &holding, SectionId::OverlaySchema, |configs| &configs.overlay_schema, parse_overlay_schema)
        .map_err(|error| error.0)?;
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
                    // The receipt's projection and the end of the walk, the core's through the
                    // host (`Host::find_record` for the records the receipt reads, lane P9-1a).
                    care_receipts::ingest_applied(&cell, &mut host, &event, realized)
                        .map_err(|error| errors::kernel_refusal(py, "care_receipts.ingest", error.0))?;
                    let finished = (|| -> PyResult<()> {
                        let holding = kit::holding(&cell, &mut host).map_err(|error| error.0)?;
                        if movement::finish_held(&holding).or_py()? {
                            movement::finish_through(&cell, &mut host, agent_id).map_err(|error| error.0)?;
                        }
                        Ok(())
                    })();
                    finished.map_err(|error| errors::kernel_refusal(py, "movement.finish_movement", error))?;
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
