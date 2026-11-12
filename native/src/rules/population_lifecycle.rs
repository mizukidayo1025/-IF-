//! `domain/population_lifecycle.py`: the glue of the core's kernels
//! (`sheltersim_core::rules::population_lifecycle`, lanes c7 and P9-3a of Issue #152). Each entry
//! reads its Python arguments, makes the core call with the native host, and makes the Python
//! values of what it returns. The core reads what the world's outside holds through the host (what
//! `model_state` holds, the configs' sections, the floor plan, a relocation's movement model, the
//! EventLog's records of a command's program) and runs a presence command in its rule transaction
//! (`sheltersim_core::txn::rule`), with the validation of the world it changed through the host.
//!
//! What stays here: the parse of a command a Python caller gives (`PopulationPresenceCommand`, a
//! typed command or a dict, whose dump the core takes), `_apply_presence_action` of a candidate that
//! is not the world, and the validation of a world the core does not hold (a save's world before its
//! core) from its Python values (`validate_values`, policy E6).

use std::sync::Arc;

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};

use sheltersim_core::error::Result as CoreResult;
use sheltersim_core::rules::kit::{self, Held};
use sheltersim_core::rules::population::{OTHER_PROGRAM, PRESENCE};
use sheltersim_core::rules::population_lifecycle::{self as core, Acted, Command, Facts, Logged, Read, TaskFacts, KEY};
use sheltersim_core::value::V;

use crate::core::errors::{self, OrPy};
use crate::core::row::{attribute, exact_dict, exact_int, exact_str};
use crate::core::value::VPy;
use crate::core::world_core::{state_value, with_world, PyWorldCell};
use crate::host::HostError;
use crate::rules::population::{as_refusal, event_row, host_of, optional_text, plan_read, planning, schedule_population_retry, text};
use crate::state::events::{EventRow, EventWriter, QueueStore};
use crate::Unsupported;

const EFFECT_PROGRAM: &str = "effect_program_v1";

fn presence_held(world: &Bound<'_, PyAny>) -> PyResult<Held> {
    Ok(host_of(world).holding_now()?.held(PRESENCE, "the world holds another presence state"))
}

/// `current_household_id`.
#[pyfunction]
fn current_household_id(world: &Bound<'_, PyAny>, agent_id: &Bound<'_, PyAny>) -> PyResult<Option<String>> {
    let agent_id = exact_str(agent_id)?;
    let presence = presence_held(world)?;
    let found = planning(with_world(world, |cell| core::household_of(cell, &presence, agent_id))?.or_py())?;
    Ok(found.map(|text| text.to_string()))
}

/// What `PopulationPresenceCommand` validated, read back.
fn command_of(typed: &Bound<'_, PyAny>, ids: &Bound<'_, PyAny>) -> PyResult<Command> {
    let py = typed.py();
    let mut source_ids = Vec::new();
    for source in attribute(typed, intern!(py, "source_ids"))?.try_iter()? {
        source_ids.push(text(&source?)?);
    }
    let expected = attribute(typed, intern!(py, "expected_return_minute"))?;
    let mut people = Vec::new();
    for id in ids.try_iter()? {
        people.push(text(&id?)?);
    }
    Ok(Command {
        action: text(&attribute(typed, intern!(py, "action"))?)?,
        zone_id: optional_text(&attribute(typed, intern!(py, "zone_id"))?)?,
        household_id: optional_text(&attribute(typed, intern!(py, "household_id"))?)?,
        expected_return: if expected.is_none() { None } else { Some(exact_int(&expected)?) },
        source_ids,
        ids: people,
    })
}

/// The command on `candidate`, whose plan reads `world` too (the same world in a transaction):
/// what the core reads of each through its own host.
fn act(world: &Bound<'_, PyAny>, candidate: &Bound<'_, PyAny>, row: &EventRow, command: &Command) -> PyResult<Acted> {
    let (cell, other) = (PyWorldCell(world), PyWorldCell(candidate));
    if world.is(candidate) {
        return core::act(&cell, &mut host_of(world), row, command).map_err(|error| error.0);
    }
    let mut host = host_of(candidate);
    let read = Read::read(&other, &mut host, &*command.action == "relocate").map_err(|error| error.0)?;
    let mut world_host = host_of(world);
    let effects = kit::holding(&cell, &mut world_host).map_err(|error| error.0)?.held(EFFECT_PROGRAM, OTHER_PROGRAM);
    // The plan reads the pending events as rows (an archive cancels the person's).
    crate::rules::travel::read_queue_rows(world)?;
    crate::rules::travel::read_queue_rows(candidate)?;
    let logged = if matches!(&*command.action, "temporary_exit" | "return") {
        core::logged_through(&cell, &mut world_host, &effects, row).map_err(|error| error.0)?
    } else {
        Logged::new()
    };
    let reads = with_world(world, |world| core::world_reads(world, &effects, &logged, row, command))?;
    read.run::<_, HostError, _>(&other, |world, inputs| core::presence_action(world, inputs, row, command, &reads)).map_err(|error| error.0)
}

/// What a command wrote, made Python's: the queue's objects of the events it settled let go, and
/// its receipt.
fn written<'py>(candidate: &Bound<'py, PyAny>, written: core::Written) -> PyResult<Bound<'py, PyAny>> {
    if !written.taken.is_empty() {
        let queue = EventWriter::of(candidate)?.queue().clone();
        queue.get().forget(&written.taken)?;
    }
    written.receipt.to_py(candidate.py())
}

/// `_apply_presence_action`: the command on `candidate`, whose deferral schedules the retry on
/// `world` itself.
#[pyfunction]
fn apply_presence_action<'py>(
    world: &Bound<'py, PyAny>,
    candidate: &Bound<'py, PyAny>,
    event: &Bound<'py, PyAny>,
    typed: &Bound<'py, PyAny>,
    ids: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let (row, command) = planning((|| Ok((event_row(event)?, command_of(typed, ids)?)))())?;
    match act(world, candidate, &row, &command)? {
        Acted::Done(done) => written(candidate, done),
        Acted::Deferred(reason) => {
            schedule_population_retry(world, event, "presence-retry", "population exit")?;
            errors::suppressed(py, core::deferred_text(&reason))
        }
    }
}

/// `apply_population_presence_command` called from Python, with a typed command or a dict: the
/// contract's parse here, whose `TypeError` or `ValueError` is the rule's text, and the rule the
/// core's (`presence_command_typed`) on the contract's dump.
#[pyfunction]
pub(crate) fn apply_presence_command<'py>(world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>, command: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let (row, dumped) = planning((|| {
        let module = py.import("sheltersim.domain.population_lifecycle")?;
        let contract = module.getattr("PopulationPresenceCommand")?;
        let parsed = if command.is_instance(&contract)? { Ok(command.clone()) } else { contract.call_method1("model_validate", (command,)) };
        let typed = match parsed {
            Ok(typed) => typed,
            Err(error) if !error.is_instance_of::<Unsupported>(py) && (error.is_instance_of::<PyTypeError>(py) || error.is_instance_of::<PyValueError>(py)) => {
                return errors::failed(py, format!("population command is invalid: {}", error.value(py).str()?));
            }
            Err(error) => return Err(error),
        };
        let options = PyDict::new(py);
        options.set_item("mode", "json")?;
        let dumped = V::from_py(&typed.call_method("model_dump", (), Some(&options))?)?;
        Ok((event_row(event)?, dumped))
    })())?;
    let receipt = core::presence_command_typed(&PyWorldCell(world), &mut host_of(world), &row, &dumped).map_err(|error| error.0)?;
    receipt.to_py(py)
}

/// `admit_pending_returns`.
#[pyfunction]
pub(crate) fn admit_pending_returns<'py>(world: &Bound<'py, PyAny>, minute: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    let minute = exact_int(minute)?;
    let admitted = core::admit_returns_through(&PyWorldCell(world), &mut host_of(world), minute).map_err(|error| error.0)?;
    PyTuple::new(world.py(), admitted.iter().map(|id| &**id))
}

/// `complete_presence_relocation`.
#[pyfunction]
pub(crate) fn complete_presence_relocation(world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<()> {
    let row = event_row(event)?;
    core::complete_relocation_through(&PyWorldCell(world), &mut host_of(world), &row).map(drop).map_err(|error| error.0)
}

/// `_zone_has_room`.
#[pyfunction]
#[pyo3(signature = (world, zone_id, *, excluding=None))]
fn zone_has_room(world: &Bound<'_, PyAny>, zone_id: &Bound<'_, PyAny>, excluding: Option<&str>) -> PyResult<bool> {
    let zone_id = exact_str(zone_id)?;
    let cell = PyWorldCell(world);
    let mut host = host_of(world);
    let read = Read::read(&cell, &mut host, false).map_err(|error| error.0)?;
    planning(read.run::<_, HostError, _>(&cell, |cell, inputs| core::zone_has_room(cell, inputs, zone_id, excluding)).map_err(|error| error.0))
}

// ---- `validate_population_lifecycle_state` -------------------------------------------------------

/// `validate_population_lifecycle_state`: its ValueErrors in its order of checks, and Unsupported, a
/// `KernelRefusal` in Python, for what the Python rule's contracts decided. It is part of the whole
/// world's validation (policy E6), which reads what Python holds as it is: the namespace's tree
/// while it is attached (a write past the mirror included), the people, zones and tasks as their
/// objects. The rules themselves (`core::validate` in their plans) read the rows.
#[pyfunction]
fn validate_population_lifecycle_state(world: &Bound<'_, PyAny>) -> PyResult<()> {
    planning(validate_values(world))
}

/// `validate_population_lifecycle_state` of the Python values of a world. The tasks are read as
/// their objects, as the rule read them.
fn validate_values(world: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = world.py();
    let Some(state) = state_value(world, KEY)? else {
        return Ok(());
    };
    let has_presence = exact_dict(&attribute(world, intern!(py, "model_state"))?)?.contains(PRESENCE)?;
    let presence = if has_presence { state_value(world, PRESENCE)? } else { None };
    let deferred = |error: PyErr| plan_read(py, &error);
    let agents = attribute(world, intern!(py, "agents"))?;
    let zones = attribute(world, intern!(py, "zones"))?;
    let has_agent = |id: &str| -> CoreResult<bool> { agents.contains(id).map_err(deferred) };
    let has_zone = |id: &str| -> CoreResult<bool> { zones.contains(id).map_err(deferred) };
    let tasks = |id: &str| -> CoreResult<Vec<TaskFacts>> {
        (|| -> PyResult<Vec<TaskFacts>> {
            let mut found = Vec::new();
            for task in exact_dict(&attribute(world, intern!(py, "active_tasks"))?)?.values() {
                if exact_str(&attribute(&task, intern!(py, "id"))?)? != id {
                    continue;
                }
                let agent = attribute(&task, intern!(py, "agent_id"))?;
                found.push(TaskFacts {
                    agent_id: if agent.is_none() { None } else { Some(text(&agent)?) },
                    kind: text(&attribute(&task, intern!(py, "kind"))?)?,
                    due_minute: exact_int(&attribute(&task, intern!(py, "due_minute"))?)?,
                });
            }
            Ok(found)
        })()
        .map_err(deferred)
    };
    let event = |event_id: &str| -> CoreResult<Option<Arc<EventRow>>> {
        (|| {
            let store = attribute(&attribute(world, intern!(py, "event_queue"))?, intern!(py, "_store"))?;
            let queue = store.downcast::<QueueStore>().map_err(|_| Unsupported::new_err("the queue is not in Rust"))?;
            queue.get().row(py, event_id)
        })()
        .map_err(deferred)
    };
    core::validate_facts(
        &Facts { state: Some(&state), presence: has_presence.then_some(presence.as_ref()), has_agent: &has_agent, has_zone: &has_zone, tasks: &tasks, event: &event },
        None,
    )
    .or_py()
}

/// `validate_population_lifecycle_state` as a validator of `state_mirror._RUST_VALIDATORS`: true, or
/// Unsupported, and the validation where it runs then raises its own text.
#[pyfunction]
fn validate(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    as_refusal(validate_population_lifecycle_state(world)).map(|()| true)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(current_household_id, m)?)?;
    m.add_function(wrap_pyfunction!(apply_presence_action, m)?)?;
    m.add_function(wrap_pyfunction!(admit_pending_returns, m)?)?;
    m.add_function(wrap_pyfunction!(complete_presence_relocation, m)?)?;
    m.add_function(wrap_pyfunction!(apply_presence_command, m)?)?;
    m.add_function(wrap_pyfunction!(zone_has_room, m)?)?;
    m.add_function(wrap_pyfunction!(validate_population_lifecycle_state, m)?)?;
    m.add_function(wrap_pyfunction!(validate, m)?)?;
    Ok(())
}
