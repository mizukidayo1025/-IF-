//! The rules of `events/effects.py` that run in a tick: the glue of
//! `sheltersim_core::rules::effects` (lane P8 d3 of Issue #152). Each entry reads what Python holds
//! (whether `model_state` holds each namespace, the sections of the configs kept by stamp, the floor
//! plan), makes the event's row, calls the core with the native host (`PyHost`: the contracts of the
//! parameters and of a guard, policy E15, and the whole world's validation, policy E6), and makes the
//! Python values of what it returns.
//!
//! A handler whose Python rule copied the world runs the core's checks on the world, then the core's
//! writes in the transaction that stands for the copy (`RuleTxn`, lane 6-3, with its copy for a world
//! the transaction refuses). A suppressed or failed effect raises the Python rule's text, in its order
//! of checks, with the world as it was (docs/rust-port/lanes/l8-texts-events.md).
//!
//! What a read raised before the core reached it (the zone environment's config, the floor plan, the
//! staff's operations) is raised where the core says it needed it (`CONFIG_UNREAD` and the other
//! `*_UNREAD` refusals), as the Python rule raised it there.

use pyo3::exceptions::{PyKeyError, PyTypeError, PyValueError};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString, PyTuple};

use sheltersim_core::config::environment::ZoneEnvironmentSpec;
use sheltersim_core::rules::care_receipts::Made;
use sheltersim_core::rules::effects::{
    self as core, AddIn, Catch, EnvironmentIn, GeometryIn, GuardIn, HandoffIn, SpaceIn, Structure, StructureIn, CONFIG_UNREAD, COUNTS,
    GEOMETRY_PLAN_UNREAD, OPERATORS, PENDING_UNREAD, SNAPSHOT_UNREAD,
};
use sheltersim_core::rules::kit::Held;
use sheltersim_core::txn::TxnKind;
use std::sync::Arc;

use crate::core::configs;
use crate::core::errors::{self, OrPy};
use crate::core::row::{attribute, exact_str};
use crate::core::value::{VPy, V};
use crate::core::world_core::{namespace_held, with_world, PyWorldCell};
use crate::host::PyHost;
use crate::rules::care_receipts::made_record;
use crate::rules::information_events::event_row;
use crate::rules::movement_geometry::{Geo, GeoPy};
use crate::rules::staff_operation_events::{staff_held, Gathered};
use crate::rules::transaction::RuleTxn;
use crate::rules::travel::read_queue_rows;
use crate::rules::{agent_dynamics, movement, overlay};
use crate::state::events::{EventRef, EventRow};
use crate::Unsupported;

const SHARED_OPERATIONS: &str = "shared_operations_v1";

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

fn infrastructure_held(world: &Bound<'_, PyAny>) -> PyResult<Held> {
    held(world, core::INFRASTRUCTURE, "the world holds another infrastructure state")
}

/// `name in world.model_state`.
fn in_model_state(world: &Bound<'_, PyAny>, name: &str) -> PyResult<bool> {
    attribute(world, intern!(world.py(), "model_state"))?.contains(name)
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

/// A refusal the core said where it needed what a read raised: that read's error instead.
fn unread<T>(py: Python<'_>, result: PyResult<T>, reason: &str, read: Option<PyErr>) -> PyResult<T> {
    match (result, read) {
        (Err(error), Some(read)) if error.is_instance_of::<Unsupported>(py) && error.value(py).to_string() == reason => Err(read),
        (result, _) => result,
    }
}

/// What a Python rule did with an error that a rule of another file raised, called through
/// `kernel_only`: its refusal is that rule's `KernelRefusal`, and `except <catch> as exc: raise
/// EventApplicationError(str(exc))`.
fn by_name_wrapped(py: Python<'_>, label: &str, error: PyErr, catch: Catch) -> PyErr {
    let error = errors::kernel_refusal(py, label, error);
    let taken = error.is_instance_of::<PyValueError>(py)
        || (catch != Catch::Value && error.is_instance_of::<PyTypeError>(py))
        || (catch == Catch::ValueKeyOrType && error.is_instance_of::<PyKeyError>(py));
    if !taken {
        return error;
    }
    match error.value(py).str() {
        Ok(text) => errors::error(py, errors::Outcome::Failed, text.to_string_lossy().into_owned()),
        Err(failure) => failure,
    }
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

/// `effect_event_guard_or_suppress`, as `EventDispatcher.dispatch_one` asks it before the handler of
/// an event with an EffectProgram's marker: Ok when the effect goes on, else the outcome
/// (`EventSuppressed` for a false guard, `EventApplicationError` for one that can't be read). Read
/// only; Unsupported or a Python call's exception leaves the event to the Python dispatcher.
pub(crate) fn guard_or_suppress<'py>(world: &Bound<'py, PyAny>, event: EventRef<'_, 'py>) -> PyResult<()> {
    let py = world.py();
    let result = (|| {
        let made;
        let row = match event {
            EventRef::Row(row) => row,
            EventRef::Object(event) => {
                made = event_row(event)?;
                &made
            }
        };
        let guarded = Guarded::of(world)?;
        raised(core::guard_or_suppress(&PyWorldCell(world), &mut host_of(world), &guarded.ins(), row))
    })();
    counted(py, "dispatch_guard", result)
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

// ---- EffectProgramFacilityHandler ----------------------------------------------------------------

fn facility_rule(world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>, expire: bool) -> PyResult<()> {
    let row = event_row(event)?;
    let (program, infrastructure) = (program_held(world)?, infrastructure_held(world)?);
    let plan = with_world(world, |world| core::plan_facility(world, &program, &infrastructure, &row, expire))?.or_py()?;
    let txn = RuleTxn::begin(world, TxnKind::Effect)?;
    let candidate = txn.world().clone();
    let written = raised(core::write_facility(&PyWorldCell(&candidate), &mut host_of(&candidate), &plan));
    txn.finish(written)
}

/// `EffectProgramFacilityHandler.apply`.
#[pyfunction]
fn facility(handler: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<()> {
    let result = expire_of(handler).and_then(|expire| facility_rule(world, event, expire));
    counted(world.py(), "facility", result)
}

// ---- EffectProgramEnvironmentHandler -------------------------------------------------------------

/// `ScenarioZoneEnvironmentConfig.model_validate(model_configs["zone_environment"])`, kept for the
/// configs' stamp (`core/configs.rs`); what it raises is relayed as it is.
pub(crate) fn zone_environment_spec(world: &Bound<'_, PyAny>) -> PyResult<Arc<ZoneEnvironmentSpec>> {
    let py = world.py();
    let parse = || -> PyResult<ZoneEnvironmentSpec> {
        let config_data = attribute(world, intern!(py, "model_configs"))?.get_item("zone_environment")?;
        let config = py
            .import(intern!(py, "sheltersim.domain.environment"))?
            .getattr(intern!(py, "ScenarioZoneEnvironmentConfig"))?
            .call_method1(intern!(py, "model_validate"), (config_data,))?;
        let keywords = PyDict::new(py);
        keywords.set_item("mode", "json")?;
        let dumped = config.call_method(intern!(py, "model_dump"), (), Some(&keywords))?;
        configs::read(ZoneEnvironmentSpec::from_v(&<V as VPy>::from_py(&dumped)?))
    };
    match configs::kept(world, |configs| &configs.zone_environment, || Ok(configs::parsed(py, parse())))? {
        Ok(spec) => Ok(spec),
        Err(fault) => Err(configs::raise(&fault, parse)),
    }
}

fn environment_rule(world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>, expire: bool) -> PyResult<()> {
    let py = world.py();
    let row = event_row(event)?;
    // The expiry events are read as rows of the queue.
    read_queue_rows(world)?;
    let program = program_held(world)?;
    let environment = held(world, core::ENVIRONMENT, "the world holds another zone environment state")?;
    let config = zone_environment_spec(world);
    let ins = EnvironmentIn {
        program: &program,
        environment: &environment,
        service_present: in_model_state(world, "resource_service_v1")?,
        config: config.as_ref().ok().map(|config| &**config),
    };
    let planned = raised(core::plan_environment(&PyWorldCell(world), &mut host_of(world), &ins, &row, expire));
    let plan = unread(py, planned, CONFIG_UNREAD, config.as_ref().err().map(|read| read.clone_ref(py)))?;
    let txn = RuleTxn::begin(world, TxnKind::Effect)?;
    let candidate = txn.world().clone();
    let written = raised(core::write_environment(&PyWorldCell(&candidate), &mut host_of(&candidate), &plan));
    txn.finish(written)
}

/// `EffectProgramEnvironmentHandler.apply`.
#[pyfunction]
fn environment(handler: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<()> {
    let result = expire_of(handler).and_then(|expire| environment_rule(world, event, expire));
    counted(world.py(), "environment", result)
}

// ---- EffectProgramTemporarySpaceHandler ----------------------------------------------------------

fn temporary_space_rule(world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>, expire: bool) -> PyResult<()> {
    let row = event_row(event)?;
    let guarded = Guarded::of(world)?;
    let spaces = overlay::spaces_held(world)?;
    let guard = guarded.ins();
    let ins = SpaceIn { guard: &guard, spaces: &spaces };
    let plan = raised(core::plan_temporary_space(&PyWorldCell(world), &mut host_of(world), &ins, &row, expire))?;
    let txn = RuleTxn::begin(world, TxnKind::Effect)?;
    let candidate = txn.world().clone();
    let written = (|| {
        let spaces = overlay::spaces_held(&candidate)?;
        raised(core::write_temporary_space(&PyWorldCell(&candidate), &mut host_of(&candidate), &spaces, &row, &plan))
    })();
    txn.finish(written)
}

/// `EffectProgramTemporarySpaceHandler.apply`.
#[pyfunction]
fn temporary_space(handler: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<()> {
    let result = expire_of(handler).and_then(|expire| temporary_space_rule(world, event, expire));
    counted(world.py(), "temporary_space", result)
}

// ---- EffectProgramGeometryHandler ----------------------------------------------------------------

/// The label of `apply_geometry_change` as `kernel_only` named it.
const APPLY_GEOMETRY: &str = "overlay.apply_geometry_change";

fn geometry_rule(world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>, expire: bool) -> PyResult<()> {
    let py = world.py();
    let row = event_row(event)?;
    let guarded = Guarded::of(world)?;
    let overlay_state = overlay::overlay_held(world)?;
    let guard = guarded.ins();
    let ins = GeometryIn { guard: &guard, overlay: &overlay_state };
    raised(core::check_geometry(&PyWorldCell(world), &mut host_of(world), &ins, &row, expire))?;
    let txn = RuleTxn::begin(world, TxnKind::Effect)?;
    let candidate = txn.world().clone();
    let written = (|| {
        // What `apply_geometry_change` read first, in its own call: its error is the event's failure.
        let inputs = overlay::ChangeIn::of(&candidate).map_err(|error| by_name_wrapped(py, APPLY_GEOMETRY, error, Catch::ValueKeyOrType))?;
        let applied = raised(core::write_geometry(&PyWorldCell(&candidate), &mut host_of(&candidate), &inputs.change(), &row, expire));
        // A floor plan `apply_geometry_change` could not read: what that raised, in its call.
        let plan_read = inputs.plan_error(py).map(|read| by_name_wrapped(py, APPLY_GEOMETRY, read, Catch::ValueKeyOrType));
        unread(py, applied, GEOMETRY_PLAN_UNREAD, plan_read)
    })();
    txn.finish(written)
}

/// `EffectProgramGeometryHandler.apply`.
#[pyfunction]
fn geometry(handler: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<()> {
    let result = expire_of(handler).and_then(|expire| geometry_rule(world, event, expire));
    counted(world.py(), "geometry", result)
}

// ---- EffectProgramStructureHandler ---------------------------------------------------------------

/// What the structure's rules read of the Python world.
struct Structured {
    guarded: Guarded,
    shared: bool,
    infrastructure: Held,
}

impl Structured {
    fn of(world: &Bound<'_, PyAny>) -> PyResult<Self> {
        Ok(Self { guarded: Guarded::of(world)?, shared: in_model_state(world, SHARED_OPERATIONS)?, infrastructure: infrastructure_held(world)? })
    }
}

/// `(record,)` of a receipt, or None.
fn receipt_tuple(py: Python<'_>, made: Option<Made>) -> PyResult<Bound<'_, PyAny>> {
    Ok(match made {
        Some(made) => PyTuple::new(py, [made_record(py, made)?])?.into_any(),
        None => py.None().into_bound(py),
    })
}

fn staff_task_order(world: &Bound<'_, PyAny>, row: &EventRow) -> PyResult<()> {
    let py = world.py();
    let read = Structured::of(world)?;
    let guard = read.guarded.ins();
    let ins = StructureIn { guard: &guard, shared: read.shared, infrastructure: &read.infrastructure };
    let order = raised(core::plan_task_order(&PyWorldCell(world), &mut host_of(world), &ins, row))?;
    let txn = RuleTxn::begin(world, TxnKind::Effect)?;
    let candidate = txn.world().clone();
    // Its checks raise the rule's text before any change; the transaction rolls back.
    let written = (|| {
        read_queue_rows(&candidate)?;
        let gathered = Gathered::of(&candidate, true, true)?;
        let result = raised(core::write_task_order(&PyWorldCell(&candidate), &mut host_of(&candidate), &gathered.inputs(), &order));
        gathered.finish(py, result)
    })();
    txn.finish(written)
}

fn staff_handoff<'py>(world: &Bound<'py, PyAny>, row: &EventRow) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let read = Structured::of(world)?;
    let guard = read.guarded.ins();
    let ins = StructureIn { guard: &guard, shared: read.shared, infrastructure: &read.infrastructure };
    let plan = raised(core::plan_handoff(&PyWorldCell(world), &mut host_of(world), &ins, row))?;
    let txn = RuleTxn::begin(world, TxnKind::Effect)?;
    let candidate = txn.world().clone();
    let written = (|| {
        read_queue_rows(&candidate)?;
        let operations = staff_held(&candidate)?;
        let mut taken = Vec::new();
        raised(core::write_handoff(&PyWorldCell(&candidate), &mut host_of(&candidate), &HandoffIn { operations: &operations }, &plan, row, &mut taken))
    })();
    let sources = txn.finish(written)?;
    // Past here the world has changed, and what is raised goes on as the Python rule's does.
    let shared = in_model_state(world, SHARED_OPERATIONS)?;
    let made = with_world(world, |world| core::handoff_receipt(world, shared, &plan, sources))?.or_py()?;
    receipt_tuple(py, made)
}

fn facility_repair<'py>(world: &Bound<'py, PyAny>, row: &EventRow) -> PyResult<Bound<'py, PyAny>> {
    let py = world.py();
    let read = Structured::of(world)?;
    let guard = read.guarded.ins();
    let ins = StructureIn { guard: &guard, shared: read.shared, infrastructure: &read.infrastructure };
    let made = raised(core::facility_repair(&PyWorldCell(world), &mut host_of(world), &ins, row))?;
    receipt_tuple(py, made)
}

fn set_staff_role(world: &Bound<'_, PyAny>, row: &EventRow) -> PyResult<()> {
    let read = Structured::of(world)?;
    let guard = read.guarded.ins();
    let ins = StructureIn { guard: &guard, shared: read.shared, infrastructure: &read.infrastructure };
    let plan = raised(core::plan_role(&PyWorldCell(world), &mut host_of(world), &ins, row))?;
    let txn = RuleTxn::begin(world, TxnKind::Effect)?;
    let candidate = txn.world().clone();
    let written = (|| {
        let operations = staff_held(&candidate)?;
        raised(core::write_role(&PyWorldCell(&candidate), &mut host_of(&candidate), &operations, &plan))
    })();
    txn.finish(written)
}

/// The label of the staff positions, as `kernel_only` named them.
const SNAPSHOTS: &str = "movement.staff_position_snapshots";

fn add_structure(world: &Bound<'_, PyAny>, row: &EventRow) -> PyResult<&'static str> {
    let py = world.py();
    let program = program_held(world)?;
    let plan = with_world(world, |world| core::plan_add(world, &program, row))?.or_py()?;
    let txn = RuleTxn::begin(world, TxnKind::Effect)?;
    let candidate = txn.world().clone();
    let written = (|| {
        let operational = held(&candidate, core::OPERATIONAL, "the world holds another operational state")?;
        let dynamics = held(&candidate, "staff_dynamics_v1", "the world holds another staff dynamics state")?;
        let operations = staff_held(&candidate)?;
        let movement_state = held(&candidate, "movement_v1", "the world holds another movement state")?;
        let overlay_state = overlay::overlay_held(&candidate)?;
        let overlay_schema = overlay::schema_of(&candidate)?;
        let policy = agent_dynamics::policy_kept(&candidate)?;
        // What the staff positions read (a toilet's place reads the plan through the host).
        let plan_read: PyResult<Arc<Geo>> = Geo::of(&candidate);
        let staff_ops = movement::staff_ops(&candidate);
        let ins = AddIn {
            operational: &operational,
            dynamics: &dynamics,
            operations: &operations,
            movement: &movement_state,
            overlay: &overlay_state,
            overlay_schema: &overlay_schema,
            policy: &policy,
            geo: plan_read.as_ref().ok().map(|geo| &**geo),
            staff_ops: staff_ops.as_ref().ok(),
        };
        let added = raised(core::write_add(&PyWorldCell(&candidate), &mut host_of(&candidate), &ins, &plan));
        // The staff positions read the plan, then the staff's operations, in their own call.
        let snapshot_read = match (&plan_read, &staff_ops) {
            (Err(read), _) | (_, Err(read)) => Some(errors::kernel_refusal(py, SNAPSHOTS, read.clone_ref(py))),
            _ => None,
        };
        unread(py, added, SNAPSHOT_UNREAD, snapshot_read)
    })();
    txn.finish(written)
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
        Structure::Handoff => counted(py, "structure.staff_handoff", staff_handoff(world, &row)),
        Structure::Repair => counted(py, "structure.facility_repair", facility_repair(world, &row)),
        Structure::Role => counted(py, "structure.set_staff_role", set_staff_role(world, &row).map(|()| none())),
        Structure::Add => counted(py, "structure.add", add_structure(world, &row).map(|_| none())),
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
