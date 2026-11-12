//! The rules of `events/effects.py` that run in a tick: the glue of
//! `sheltersim_core::rules::effects` (lane P8 d3 of Issue #152). Each entry reads what Python holds
//! (whether `model_state` holds each namespace, the sections of the configs kept by stamp, the floor
//! plan), makes the event's row, calls the core with the native host (`PyHost`: the contracts of the
//! parameters and of a guard, policy E15, and the whole world's validation, policy E6), and makes the
//! Python values of what it returns.
//!
//! A handler is the core's (`core::*_event`, lane P9-1b): it reads what the world's outside holds
//! through the host, runs its checks on the world, then its writes in the core's rule transaction
//! that stands for the Python rule's copy (`txn::rule`, policy E16; a world whose Python views a
//! transaction does not give back is the handler's `KernelRefusal`). A suppressed or failed effect
//! raises the Python rule's text, in its order of checks, with the world as it was
//! (docs/rust-port/lanes/l8-texts-events.md).
//!
//! What a read raised before the core reached it (the zone environment's config, the floor plan, the
//! staff's operations) is raised where the core says it needed it (`CONFIG_UNREAD` and the other
//! `*_UNREAD` refusals), as the Python rule raised it there.

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString, PyTuple};

use sheltersim_core::config::environment::ZoneEnvironmentSpec;
use sheltersim_core::rules::care_receipts::Made;
use sheltersim_core::rules::effects::{self as core, GuardIn, Structure, CONFIG_UNREAD, COUNTS, OPERATORS, PENDING_UNREAD};
use sheltersim_core::rules::staff_operation_events::FORMULA_UNREAD;
use sheltersim_core::rules::kit::Held;
use std::sync::Arc;

use crate::core::configs;
use crate::core::errors::OrPy;
use crate::core::row::{attribute, exact_str};
use crate::core::value::{VPy, V};
use crate::core::world_core::{namespace_held, with_world, PyWorldCell};
use crate::host::PyHost;
use crate::rules::care_receipts::made_record;
use crate::rules::information_events::event_row;
use crate::rules::travel::read_queue_rows;
use crate::state::events::EventRow;
use crate::Unsupported;


// ---- What ran and what was refused --------------------------------------------------------------

/// Count a kernel's result under its label (`sheltersim_core::rules::effects::COUNTS`).
fn counted<T>(py: Python<'_>, label: &'static str, result: PyResult<T>) -> PyResult<T> {
    match &result {
        Ok(_) => COUNTS.ran(label),
        Err(error) if error.is_instance_of::<Unsupported>(py) => COUNTS.refused(label, &error.value(py).to_string()),
        Err(_) => {}
    }
    result
}

/// What has run in this process: `(counts, refusals by reason)`.
#[pyfunction]
fn stats<'py>(py: Python<'py>) -> PyResult<(Bound<'py, PyDict>, Bound<'py, PyDict>)> {
    let counts = PyDict::new(py);
    for (label, times) in COUNTS.ran_counts() {
        counts.set_item(label, times)?;
    }
    let reasons = PyDict::new(py);
    for (reason, times) in COUNTS.refusal_counts() {
        reasons.set_item(reason, times)?;
    }
    Ok((counts, reasons))
}

// ---- What the rules read of the Python world ------------------------------------------------------

fn held(world: &Bound<'_, PyAny>, name: &'static str, other: &'static str) -> PyResult<Held> {
    namespace_held(world, &PyString::new(world.py(), name), other)
}

fn program_held(world: &Bound<'_, PyAny>) -> PyResult<Held> {
    held(world, core::MARKER, "the world holds another effect_program_v1")
}

/// What a guard reads of the Python world.
struct Guarded {
    program: Held,
    ledger: Held,
}

impl Guarded {
    fn of(world: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self { program: program_held(world)?, ledger: held(world, "resource_ledger_v1", "the world holds another ledger")? })
    }

    fn ins(&self) -> GuardIn<'_> {
        GuardIn { program: &self.program, ledger: &self.ledger }
    }
}

/// The native host of a world, for its contracts and its validation.
fn host_of<'py>(world: &Bound<'py, PyAny>) -> PyHost<'py> {
    PyHost::of_world(world, PyDict::new(world.py()))
}

/// The core's result, with the host's Python error.
fn raised<T>(result: Result<T, crate::host::HostError>) -> PyResult<T> {
    result.map_err(|error| error.0)
}

fn expire_of(handler: &Bound<'_, PyAny>) -> PyResult<bool> {
    handler.getattr(intern!(handler.py(), "expire"))?.extract()
}

// ---- The guard -----------------------------------------------------------------------------------

/// `should_apply_effect_guard`. Most events have no marker, and need nothing but that answer.
#[pyfunction]
fn should_apply_guard(world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = world.py();
    let result = (|| {
        let provenance = attribute(event, intern!(py, "provenance"))?;
        let marker = intern!(py, "effect_program_v1");
        if !provenance.contains(marker)? || provenance.get_item(marker)?.is_none() {
            return Ok(true);
        }
        let row = event_row(event)?;
        let guarded = Guarded::of(world)?;
        raised(core::guard(&PyWorldCell(world), &mut host_of(world), &guarded.ins(), &row))
    })();
    counted(py, "guard", result)
}

// ---- What other modules ask ----------------------------------------------------------------------

/// `has_active_facility_modifier`.
#[pyfunction]
fn has_active_modifier(world: &Bound<'_, PyAny>, facility_id: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = world.py();
    let result = (|| {
        let id = exact_str(facility_id)?;
        let program = program_held(world)?;
        with_world(world, |world| core::has_active_modifier(world, &program, id))?.or_py()
    })();
    counted(py, "has_active_modifier", result)
}

/// `has_pending_facility_modifier`, of the rows of the pending events.
#[pyfunction]
fn has_pending_modifier(world: &Bound<'_, PyAny>, facility_id: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = world.py();
    let result = (|| {
        let id = exact_str(facility_id)?;
        read_queue_rows(world)?;
        with_world(world, |world| core::has_pending_modifier(world, id))?.or_py()
    })();
    counted(py, "has_pending_modifier", result)
}

// ---- The handlers ---------------------------------------------------------------------------------
//
// Each is the core's handler (`core::*_event`), which reads what the world's outside holds through
// the host, runs the checks, and writes in the core's rule transaction (lane P9-1b, policy E16).
// The rows of the pending events Python scheduled are read first, for the rules that read them.

/// The core's handler of an effect's event, with the native host.
fn handled<T>(world: &Bound<'_, PyAny>, run: impl FnOnce(&PyWorldCell<'_, '_>, &mut PyHost<'_>) -> Result<T, crate::host::HostError>) -> PyResult<T> {
    raised(run(&PyWorldCell(world), &mut host_of(world)))
}

/// A refusal the core said where the rule reads what only a Python read gives (`reason`): what that
/// read raises, read again now (the configs are the same; a read that does not raise leaves the
/// refusal).
fn read_again<T>(py: Python<'_>, result: PyResult<T>, reason: &str, read: impl FnOnce() -> PyResult<()>) -> PyResult<T> {
    match result {
        Err(error) if error.is_instance_of::<Unsupported>(py) && error.value(py).to_string() == reason => match read() {
            Err(read) => Err(read),
            Ok(()) => Err(error),
        },
        result => result,
    }
}

/// `EffectProgramFacilityHandler.apply`.
#[pyfunction]
fn facility(handler: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<()> {
    let result = (|| {
        let expire = expire_of(handler)?;
        let row = event_row(event)?;
        handled(world, |cell, host| core::facility_event(cell, host, &row, expire))
    })();
    counted(world.py(), "facility", result)
}

/// `ScenarioZoneEnvironmentConfig.model_validate(model_configs["zone_environment"])`, kept for the
/// configs' stamp (`core/configs.rs`); what it raises is relayed as it is.
pub(crate) fn zone_environment_spec(world: &Bound<'_, PyAny>) -> PyResult<Arc<ZoneEnvironmentSpec>> {
    match zone_environment_section(world)? {
        Ok(spec) => Ok(spec),
        Err(fault) => Err(configs::raise(&fault, || parse_zone_environment(world))),
    }
}

/// `zone_environment_spec` without its raise (the host's `fill_section`, lane P9-0).
pub(crate) fn zone_environment_section(world: &Bound<'_, PyAny>) -> PyResult<sheltersim_core::rules::kit::Kept<ZoneEnvironmentSpec>> {
    configs::kept(world, |configs| &configs.zone_environment, || Ok(configs::parsed(world.py(), parse_zone_environment(world))))
}

fn parse_zone_environment(world: &Bound<'_, PyAny>) -> PyResult<ZoneEnvironmentSpec> {
    let py = world.py();
    let config_data = attribute(world, intern!(py, "model_configs"))?.get_item("zone_environment")?;
    let config = py
        .import(intern!(py, "sheltersim.domain.environment"))?
        .getattr(intern!(py, "ScenarioZoneEnvironmentConfig"))?
        .call_method1(intern!(py, "model_validate"), (config_data,))?;
    let keywords = PyDict::new(py);
    keywords.set_item("mode", "json")?;
    let dumped = config.call_method(intern!(py, "model_dump"), (), Some(&keywords))?;
    configs::read(ZoneEnvironmentSpec::from_v(&<V as VPy>::from_py(&dumped)?))
}

/// `EffectProgramEnvironmentHandler.apply`. A config whose read raised raises where the rule reads it.
#[pyfunction]
fn environment(handler: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = world.py();
    let result = (|| {
        let expire = expire_of(handler)?;
        let row = event_row(event)?;
        // The expiry events are read as rows of the queue.
        read_queue_rows(world)?;
        let result = handled(world, |cell, host| core::environment_event(cell, host, &row, expire));
        read_again(py, result, CONFIG_UNREAD, || zone_environment_spec(world).map(drop))
    })();
    counted(py, "environment", result)
}

/// `EffectProgramTemporarySpaceHandler.apply`.
#[pyfunction]
fn temporary_space(handler: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<()> {
    let result = (|| {
        let expire = expire_of(handler)?;
        let row = event_row(event)?;
        handled(world, |cell, host| core::temporary_space_event(cell, host, &row, expire))
    })();
    counted(world.py(), "temporary_space", result)
}

/// `EffectProgramGeometryHandler.apply`.
#[pyfunction]
fn geometry(handler: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<()> {
    let result = (|| {
        let expire = expire_of(handler)?;
        let row = event_row(event)?;
        handled(world, |cell, host| core::geometry_event(cell, host, &row, expire))
    })();
    counted(world.py(), "geometry", result)
}

/// `(record,)` of a receipt, or None.
fn receipt_tuple(py: Python<'_>, made: Option<Made>) -> PyResult<Bound<'_, PyAny>> {
    Ok(match made {
        Some(made) => PyTuple::new(py, [made_record(py, made)?])?.into_any(),
        None => py.None().into_bound(py),
    })
}

/// `staff_task_order`: a formula whose read raised raises where the rule reads it.
fn staff_task_order(world: &Bound<'_, PyAny>, row: &EventRow) -> PyResult<()> {
    let py = world.py();
    read_queue_rows(world)?;
    let result = handled(world, |cell, host| core::task_order_event(cell, host, row));
    read_again(py, result, FORMULA_UNREAD, || crate::rules::staff::staff_operations(world).map(drop))
}

/// `EffectProgramStructureHandler.apply`. Its branches are told apart by the payload's fields, in
/// the Python rule's order.
#[pyfunction]
fn structure<'py>(_handler: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let none = || py.None().into_bound(py);
    let row = event_row(event)?;
    match core::structure_of(&row).or_py()? {
        Structure::TaskOrder => counted(py, "structure.staff_task_order", staff_task_order(world, &row).map(|()| none())),
        Structure::Handoff => {
            let result = read_queue_rows(world).and_then(|()| handled(world, |cell, host| core::handoff_event(cell, host, &row)));
            counted(py, "structure.staff_handoff", result.and_then(|made| receipt_tuple(py, made)))
        }
        Structure::Repair => {
            let result = handled(world, |cell, host| core::repair_event(cell, host, &row));
            counted(py, "structure.facility_repair", result.and_then(|made| receipt_tuple(py, made)))
        }
        Structure::Role => counted(py, "structure.set_staff_role", handled(world, |cell, host| core::role_event(cell, host, &row)).map(|()| none())),
        Structure::Add => counted(py, "structure.add", handled(world, |cell, host| core::add_event(cell, host, &row)).map(|_| none())),
    }
}

// ---- validate_effect_program_state -------------------------------------------------------------

/// `validate_effect_program_state`: passes, or Unsupported and the Python rule says what is wrong.
#[pyfunction]
#[pyo3(signature = (world, *, validate_event_history=true))]
fn validate_state(world: &Bound<'_, PyAny>, validate_event_history: bool) -> PyResult<()> {
    let py = world.py();
    let label = if validate_event_history { "validate_state.history" } else { "validate_state.tick" };
    let result = (|| {
        let program = program_held(world)?;
        let check = || raised(core::check_state(&PyWorldCell(world), &mut host_of(world), &program, validate_event_history));
        match check() {
            // The rows of the events Python scheduled, read only when a check needs them.
            Err(error) if error.is_instance_of::<Unsupported>(py) && error.value(py).to_string() == PENDING_UNREAD => {
                read_queue_rows(world)?;
                check()
            }
            other => other,
        }
    })();
    counted(py, label, result)
}

/// For the test that compares this table with `EFFECT_EVENT_TYPES`.
#[pyfunction]
fn event_type_of(phase: &str, op: &str) -> Option<&'static str> {
    core::expected_event_type(phase, op)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(should_apply_guard, m)?)?;
    m.add_function(wrap_pyfunction!(has_active_modifier, m)?)?;
    m.add_function(wrap_pyfunction!(has_pending_modifier, m)?)?;
    m.add_function(wrap_pyfunction!(facility, m)?)?;
    m.add_function(wrap_pyfunction!(environment, m)?)?;
    m.add_function(wrap_pyfunction!(temporary_space, m)?)?;
    m.add_function(wrap_pyfunction!(geometry, m)?)?;
    m.add_function(wrap_pyfunction!(structure, m)?)?;
    m.add_function(wrap_pyfunction!(validate_state, m)?)?;
    m.add_function(wrap_pyfunction!(event_type_of, m)?)?;
    m.add_function(wrap_pyfunction!(stats, m)?)?;
    m.add("OPERATORS", OPERATORS.to_vec())?;
    Ok(())
}
