//! `domain/population_lifecycle.py`: the glue of the core's kernels
//! (`sheltersim_core::rules::population_lifecycle`, lane c7 of Issue #152). Each entry reads its
//! arguments and what Python holds (whether `model_state` holds each namespace, the sections of the
//! configs, the floor plan, a relocation's movement model, the EventLog's records of a command's
//! program), runs the core on the world borrowed whole, and makes the Python values of what it
//! returns. The core writes the rows: a person archived leaves `agents`, and one restored is a new
//! row at its end, whose object the entity store makes; the attached trees follow their rows when
//! the world is given back.
//!
//! What stays here: the command's contract (`PopulationPresenceCommand`, read back as the core's
//! `Command`), the transaction that stands for the Python rule's copy (`RuleTxn`, lane 6-3) and
//! the validation of the world it changed through the native host (`Host::validate_world`,
//! `Host::validate_extensions`), the retry of a deferred command, and letting go the Python objects
//! of the events an archive settled (`QueueStore::forget`). A world the core does not hold (a
//! save's world before its core) is validated from its Python values (`validate_values`).

use std::sync::Arc;

use pyo3::exceptions::{PyKeyError, PyTypeError, PyValueError};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyTuple};

use sheltersim_core::error::{Error as CoreError, Result as CoreResult};
use sheltersim_core::host::Host;
use sheltersim_core::rules::kit::Held;
use sheltersim_core::rules::population::{NO_ROW, PRESENCE};
use sheltersim_core::rules::population_lifecycle::{self as core, Acted, Command, Facts, Inputs, Logged, Relocation, TaskFacts, KEY};
use sheltersim_core::world::World;

use crate::core::configs;
use crate::core::errors::{self, OrPy};
use crate::core::row::{attribute, exact_dict, exact_int, exact_str};
use crate::core::value::VPy;
use crate::core::world_core::{state_value, with_world};
use crate::host::PyHost;
use crate::rules::population::{as_refusal, event_row, get_map, held, optional_text, plan_read, planning, schedule_population_retry, text, PopulationIn};
use crate::rules::transaction::RuleTxn;
use crate::state::events::{EventRow, EventRowPy, EventWriter, QueueStore};
use crate::Unsupported;
use sheltersim_core::txn::TxnKind;

const EFFECT_PROGRAM: &str = "effect_program_v1";

/// Whether `model_configs.get("layout_routing", {}).get("max_agents_in_transit_by_edge", {})` is
/// truthy: the section `transit_capacities`, kept for the stamp of `model_configs`.
pub(crate) fn transit_capacities_of(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    Ok(*configs::kept_python(world, |configs| &configs.transit_capacities, transit_capacities)?)
}

fn transit_capacities(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let configs = attribute(world, intern!(world.py(), "model_configs"))?;
    get_map(&get_map(&configs, "layout_routing")?, "max_agents_in_transit_by_edge")?.is_truthy()
}

fn lifecycle_held(world: &Bound<'_, PyAny>) -> PyResult<Held> {
    held(world, KEY, "the world holds another lifecycle state")
}

fn presence_held(world: &Bound<'_, PyAny>) -> PyResult<Held> {
    held(world, PRESENCE, "the world holds another presence state")
}

/// What a relocation reads of the configs, each read's error the refusal the plan gives.
fn relocation_of(world: &Bound<'_, PyAny>) -> Relocation {
    let py = world.py();
    let transit_capacities = transit_capacities_of(world).map_err(|error| plan_read(py, &error));
    let timing = (|| -> PyResult<_> {
        let movement = crate::rules::movement::movement_of(world)?;
        if !movement.times_routes() {
            return Ok(None);
        }
        Ok(Some(configs::read(movement.model.clone())?))
    })()
    .map_err(|error| plan_read(py, &error));
    Relocation { transit_capacities, timing }
}

/// What the core's rules read of the Python world.
pub(crate) struct LifecycleIn {
    read: PopulationIn,
    lifecycle: Held,
    relocation: Option<Relocation>,
}

impl LifecycleIn {
    /// `relocation`: whether the rule plans a relocation, which reads the floor plan and the
    /// movement model.
    pub(crate) fn of(world: &Bound<'_, PyAny>, relocation: bool) -> PyResult<Self> {
        Ok(Self { read: PopulationIn::reading(world, false, relocation)?, lifecycle: lifecycle_held(world)?, relocation: relocation.then(|| relocation_of(world)) })
    }

    pub(crate) fn inputs(&self) -> Inputs<'_> {
        Inputs { population: &self.read.inputs, lifecycle: self.lifecycle.clone(), relocation: self.relocation.as_ref() }
    }

    /// The core's result, with what reading the floor plan raised where the core read the plan.
    pub(crate) fn finish<T>(&self, py: Python<'_>, result: PyResult<T>) -> PyResult<T> {
        self.read.finish(py, result)
    }

    fn run<T>(&self, world: &Bound<'_, PyAny>, kernel: impl FnOnce(&World, &Inputs<'_>) -> CoreResult<T>) -> PyResult<T> {
        let inputs = self.inputs();
        let result = with_world(world, |cell| kernel(cell, &inputs))?.or_py();
        self.finish(world.py(), result)
    }
}

/// Whether this error is the refusal of a pending event without a row.
fn no_row(py: Python<'_>, error: &PyErr) -> bool {
    error.is_instance_of::<Unsupported>(py) && error.value(py).to_string() == NO_ROW
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

/// The EventLog's records of the program of the event's provenance, as the plan of an exit or a
/// return reads them: the row of each `effect_program_population` event found.
fn logged(world: &Bound<'_, PyAny>, effects: &Held, row: &EventRow) -> PyResult<Logged> {
    let py = world.py();
    let mut found = Logged::new();
    let ids = with_world(world, |cell| core::program_event_ids(cell, effects, row))?.or_py()?;
    if ids.is_empty() {
        return Ok(found);
    }
    let log = attribute(world, intern!(py, "event_log"))?;
    for event_id in ids {
        let record = log.call_method1(intern!(py, "find"), (&*event_id,))?;
        if record.is_none() {
            continue;
        }
        let event = attribute(&record, intern!(py, "event"))?;
        if !attribute(&event, intern!(py, "event_type"))?.eq("effect_program_population")? {
            continue;
        }
        let read = match EventRow::from_event(&event) {
            Ok(Some(row)) => Ok(row),
            Ok(None) => Err(CoreError::unsupported("the event has no row")),
            Err(error) => Err(plan_read(py, &error)),
        };
        found.insert(event_id, read);
    }
    Ok(found)
}

/// The command on `candidate`, whose plan reads `world` too (the same world in a transaction).
fn act(world: &Bound<'_, PyAny>, candidate: &Bound<'_, PyAny>, row: &EventRow, command: &Command) -> PyResult<Acted> {
    let py = world.py();
    let same = world.is(candidate);
    let read = LifecycleIn::of(candidate, &*command.action == "relocate")?;
    let effects = held(world, EFFECT_PROGRAM, "the world holds another effect program state")?;
    let once = || -> PyResult<Acted> {
        let logged = if matches!(&*command.action, "temporary_exit" | "return") { logged(world, &effects, row)? } else { Logged::new() };
        let inputs = read.inputs();
        let acted = if same {
            with_world(candidate, |cell| {
                let reads = core::world_reads(cell, &effects, &logged, row, command);
                core::presence_action(cell, &inputs, row, command, &reads)
            })?
        } else {
            let reads = with_world(world, |cell| core::world_reads(cell, &effects, &logged, row, command))?;
            with_world(candidate, |cell| core::presence_action(cell, &inputs, row, command, &reads))?
        };
        read.finish(py, acted.or_py())
    };
    match once() {
        Err(error) if no_row(py, &error) => {
            crate::rules::travel::read_queue_rows(world)?;
            if !same {
                crate::rules::travel::read_queue_rows(candidate)?;
            }
            once()
        }
        result => result,
    }
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

/// `apply_population_presence_command`. The command's contract stays Python's, and what it refuses
/// is the Python rule's text. The command runs in a transaction that stands for the Python rule's
/// copy (`from_dict(deepcopy(to_dict()))`, the EventLog included); anything but its receipt rolls
/// it back. A deferral then schedules its retry on the world as it was, as the Python rule did on
/// the world it had copied.
#[pyfunction]
pub(crate) fn apply_presence_command<'py>(world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>, command: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let (row, command) = planning((|| {
        let module = py.import("sheltersim.domain.population_lifecycle")?;
        let contract = module.getattr("PopulationPresenceCommand")?;
        // The contract's parse in the rule's `try`: its TypeError or ValueError is the rule's text.
        let parsed = (|| -> PyResult<_> {
            let typed = if command.is_instance(&contract)? { command.clone() } else { contract.call_method1("model_validate", (command,))? };
            let ids = typed.call_method0("selected_ids")?;
            Ok((typed, ids))
        })();
        let (typed, ids) = match parsed {
            Ok(parsed) => parsed,
            Err(error) if !error.is_instance_of::<Unsupported>(py) && (error.is_instance_of::<PyTypeError>(py) || error.is_instance_of::<PyValueError>(py)) => {
                return errors::failed(py, format!("population command is invalid: {}", error.value(py).str()?));
            }
            Err(error) => return Err(error),
        };
        if !exact_dict(&attribute(world, intern!(py, "model_state"))?)?.contains(KEY)? {
            return errors::failed(py, "population lifecycle contract is not installed");
        }
        Ok((event_row(event)?, command_of(&typed, &ids)?))
    })())?;
    let txn = RuleTxn::begin(world, TxnKind::Full)?;
    let checked = planning(with_world(txn.world(), |cell| core::check_command(cell, &row, command.expected_return)).and_then(OrPy::or_py));
    let (txn, dropped) = txn.keep(checked)?;
    if dropped {
        txn.rollback()?;
        return errors::suppressed(py, core::DROPPED);
    }
    let acted = act(txn.world(), txn.world(), &row, &command);
    let (txn, acted) = txn.keep(acted)?;
    let done = match acted {
        Acted::Done(done) => done,
        Acted::Deferred(reason) => {
            txn.rollback()?;
            schedule_population_retry(world, event, "presence-retry", "population exit")?;
            return errors::suppressed(py, core::deferred_text(&reason));
        }
    };
    let candidate = txn.world().clone();
    let receipt = written(&candidate, done);
    let (txn, receipt) = txn.keep(receipt)?;
    let checked = (|| -> PyResult<()> {
        validate_population_lifecycle_state(&candidate)?;
        let mut host = PyHost::of_world(&candidate, PyDict::new(py));
        host.validate_world().map_err(|error| error.0)?;
        host.validate_extensions().map_err(|error| error.0)?;
        Ok(())
    })();
    if let Err(error) = checked {
        txn.rollback()?;
        if error.is_instance_of::<PyTypeError>(py) || error.is_instance_of::<PyKeyError>(py) || error.is_instance_of::<PyValueError>(py) {
            return errors::failed(py, format!("population command failed validation: {}", error.value(py).str()?));
        }
        return Err(error);
    }
    txn.commit()?;
    Ok(receipt)
}

/// `admit_pending_returns`: nothing is read past the namespace while no one waits.
#[pyfunction]
pub(crate) fn admit_pending_returns<'py>(world: &Bound<'py, PyAny>, minute: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    let py = world.py();
    let minute = exact_int(minute)?;
    let lifecycle = lifecycle_held(world)?;
    if planning(with_world(world, |cell| core::nothing_to_admit(cell, &lifecycle))?.or_py())? {
        return Ok(PyTuple::empty(py));
    }
    let read = LifecycleIn::of(world, false)?;
    let admitted = crate::rules::population::with_rows(world, || read.run(world, |cell, inputs| core::admit_returns(cell, inputs, minute)))?;
    PyTuple::new(py, admitted.iter().map(|id| &**id))
}

/// `complete_presence_relocation`.
#[pyfunction]
pub(crate) fn complete_presence_relocation(world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<()> {
    let row = event_row(event)?;
    let read = LifecycleIn::of(world, false)?;
    crate::rules::population::with_rows(world, || read.run(world, |cell, inputs| core::complete_relocation(cell, inputs, &row))).map(drop)
}

/// `_zone_has_room`.
#[pyfunction]
#[pyo3(signature = (world, zone_id, *, excluding=None))]
fn zone_has_room(world: &Bound<'_, PyAny>, zone_id: &Bound<'_, PyAny>, excluding: Option<&str>) -> PyResult<bool> {
    let zone_id = exact_str(zone_id)?;
    let read = LifecycleIn::of(world, false)?;
    planning(read.run(world, |cell, inputs| core::zone_has_room(cell, inputs, zone_id, excluding)))
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
