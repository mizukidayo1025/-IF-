//! The rest of `events/handlers.py`: the glue of `sheltersim_core::rules::handlers` (lane P8 d2 of
//! Issue #152). Each entry reads what Python holds (whether `model_state` holds each namespace, the
//! sections of the configs kept by stamp, the floor plan, the resource service's manager), makes the
//! event's row, calls the core with the world borrowed whole, lets go the objects of the queue's
//! slots the core took out or replaced, and makes the Python values of what it returns: the records
//! the dispatcher appends, and the world's new `resources` dict of a ledger's operation.
//!
//! A handler whose Python rule copied the world runs the core in the transaction that stands for the
//! copy (`RuleTxn`, lane 6-3, with its copy for a world the transaction refuses); the core ends the
//! transaction's work (`settle`) with the native host of the transaction's world, which validates it
//! (policy E6), looks up the EventLog's records and gives the contracts (policy E15). The population
//! primitives' rules are the population lanes' glue, called in the transaction as before.
//!
//! The helpers at the start (`counted`, `get`, `get_is`, `status_member`) are for the kernels still in
//! this crate.

use std::sync::Arc;

use pyo3::exceptions::{PyTypeError, PyValueError};
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString, PyTuple};

use sheltersim_core::config::environment::Infrastructure;
use sheltersim_core::config::proximity::Proximity;
use sheltersim_core::rules::care_receipts::Made;
use sheltersim_core::rules::handlers::{
    self as core, FacilityIn, Followed, Infra, InfraIn, LedgerIn, PopulationOp, StaffIn, Unread, COOLDOWN_UNREAD, COUNTS, INFRASTRUCTURE, OPERATIONAL, SERVICE_UNREAD,
};
use sheltersim_core::rules::kit::Held;
use sheltersim_core::txn::TxnKind;

use crate::core::configs;
use crate::core::errors::{self, to_py_err, OrPy};
use crate::core::row::{attribute, exact_int, exact_str, str_value};
use crate::core::value::{VPy, V};
use crate::core::world_core::{field_held, namespace_held, with_world, PyWorldCell};
use crate::host::PyHost;
use crate::rules::care_receipts::made_record;
use crate::rules::information_events::event_row;
use crate::rules::population as population_rules;
use crate::rules::resources::{raised, replace_resources};
use crate::rules::staff_operation_events::Gathered;
use crate::rules::transaction::RuleTxn;
use crate::rules::travel::{movement_held, read_queue_rows};
use crate::rules::{population_lifecycle, service};
use crate::state::events::{EventRow, EventRowPy, EventWriter};
use crate::Unsupported;

const SHARED_OPERATIONS: &str = "shared_operations_v1";

// ---- What ran and what was refused --------------------------------------------------------------

/// Count a kernel's result under its label (`sheltersim_core::rules::handlers::COUNTS`).
pub(crate) fn counted<T>(py: Python<'_>, label: &'static str, result: PyResult<T>) -> PyResult<T> {
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

// ---- Reading and making Python's values, for the kernels still in this crate ----------------------
//
// A kernel in the core uses the row versions in `sheltersim_core::rules::kit`.

/// `map.get(key)`, which is None when absent.
pub(crate) fn get<'py>(map: &Bound<'py, PyAny>, key: &str) -> PyResult<Bound<'py, PyAny>> {
    map.call_method1(intern!(map.py(), "get"), (key,))
}

pub(crate) fn get_is(map: &Bound<'_, PyAny>, key: &str, wanted: &str) -> PyResult<bool> {
    Ok(str_value(&get(map, key)?) == Some(wanted))
}

fn models(py: Python<'_>) -> PyResult<Bound<'_, PyModule>> {
    py.import(intern!(py, "sheltersim.events.models"))
}

pub(crate) fn status_member<'py>(py: Python<'py>, member: &str) -> PyResult<Bound<'py, PyAny>> {
    models(py)?.getattr(intern!(py, "EventStatus"))?.getattr(member)
}

// ---- The sections of the configs, kept by stamp ----------------------------------------------------

/// `canonical_proximity_contact_config(model_configs.get("social_proximity"))`, kept for the
/// configs' stamp (`core/configs.rs`); what it raises is relayed as it is.
pub(crate) fn proximity_config(world: &Bound<'_, PyAny>) -> PyResult<Arc<Option<Proximity>>> {
    match proximity_section(world)? {
        Ok(config) => Ok(config),
        Err(fault) => Err(configs::raise(&fault, || parse_proximity(world))),
    }
}

/// `proximity_config` without its raise (the host's `fill_section`, lane P9-0).
pub(crate) fn proximity_section(world: &Bound<'_, PyAny>) -> PyResult<sheltersim_core::rules::kit::Kept<Option<Proximity>>> {
    configs::kept(world, |configs| &configs.proximity, || Ok(configs::parsed(world.py(), parse_proximity(world))))
}

fn parse_proximity(world: &Bound<'_, PyAny>) -> PyResult<Option<Proximity>> {
    let py = world.py();
    let configs = attribute(world, intern!(py, "model_configs"))?;
    let config = py
        .import("sheltersim.domain.social_network")?
        .getattr("canonical_proximity_contact_config")?
        .call1((get(&configs, "social_proximity")?,))?;
    configs::read(Proximity::from_v(&<V as VPy>::from_py(&config)?))
}

/// `model_configs.get("staff", {}).get("support_cooldown_minutes", 30)`, kept for the configs'
/// stamp (`core/configs.rs`); what the reads raise is relayed as it is.
pub(crate) fn support_cooldown_minutes(world: &Bound<'_, PyAny>) -> PyResult<Arc<i64>> {
    match support_cooldown_section(world)? {
        Ok(minutes) => Ok(minutes),
        Err(fault) => Err(configs::raise(&fault, || parse_support_cooldown(world))),
    }
}

/// `support_cooldown_minutes` without its raise (the host's `fill_section`, lane P9-0).
pub(crate) fn support_cooldown_section(world: &Bound<'_, PyAny>) -> PyResult<sheltersim_core::rules::kit::Kept<i64>> {
    configs::kept(world, |configs| &configs.support_cooldown_minutes, || Ok(configs::parsed(world.py(), parse_support_cooldown(world))))
}

fn parse_support_cooldown(world: &Bound<'_, PyAny>) -> PyResult<i64> {
    let py = world.py();
    let configs = attribute(world, intern!(py, "model_configs"))?;
    let cooldown = configs
        .call_method1(intern!(py, "get"), ("staff", PyDict::new(py)))?
        .call_method1(intern!(py, "get"), ("support_cooldown_minutes", 30))?;
    exact_int(&cooldown)
}

/// `ScenarioInfrastructureSpec.model_validate(configs["infrastructure"])`, kept for the configs'
/// stamp, for the sections' tests (`core/configs.rs`). The rules read it through the host
/// (`Host::contract`, policy E15).
pub(crate) fn infrastructure_config(world: &Bound<'_, PyAny>) -> PyResult<Arc<Infrastructure>> {
    match infrastructure_section(world)? {
        Ok(spec) => Ok(spec),
        Err(fault) => Err(configs::raise(&fault, || parse_infrastructure(world))),
    }
}

/// `infrastructure_config` without its raise (the host's `fill_section`, lane P9-0).
pub(crate) fn infrastructure_section(world: &Bound<'_, PyAny>) -> PyResult<sheltersim_core::rules::kit::Kept<Infrastructure>> {
    configs::kept(world, |configs| &configs.infrastructure, || Ok(configs::parsed(world.py(), parse_infrastructure(world))))
}

fn parse_infrastructure(world: &Bound<'_, PyAny>) -> PyResult<Infrastructure> {
    let py = world.py();
    let raw = attribute(world, intern!(py, "model_configs"))?.get_item("infrastructure")?;
    let spec = py
        .import("sheltersim.domain.infrastructure")?
        .getattr("ScenarioInfrastructureSpec")?
        .call_method1("model_validate", (raw,))?;
    let keywords = PyDict::new(py);
    keywords.set_item("mode", "json")?;
    let dumped = spec.call_method("model_dump", (), Some(&keywords))?;
    configs::read(Infrastructure::from_v(&<V as VPy>::from_py(&dumped)?))
}

// ---- What the rules read of the Python world ----------------------------------------------------------

fn held(world: &Bound<'_, PyAny>, name: &'static str, other: &'static str) -> PyResult<Held> {
    namespace_held(world, &PyString::new(world.py(), name), other)
}

fn operational_held(world: &Bound<'_, PyAny>) -> PyResult<Held> {
    held(world, OPERATIONAL, "the world holds another operational state")
}

fn infrastructure_held(world: &Bound<'_, PyAny>) -> PyResult<Held> {
    held(world, INFRASTRUCTURE, "the world holds another infrastructure state")
}

fn program_held(world: &Bound<'_, PyAny>) -> PyResult<Held> {
    held(world, "effect_program_v1", "the world holds another effect_program_v1")
}

/// `SHARED_OPERATIONS_KEY not in world.model_state`.
fn shared_absent(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = world.py();
    Ok(!attribute(world, intern!(py, "model_state"))?.contains(intern!(py, SHARED_OPERATIONS))?)
}

/// Let go the objects of the queue's slots the core took out or gave a new row.
fn forget(world: &Bound<'_, PyAny>, taken: &[u64]) -> PyResult<()> {
    if taken.is_empty() {
        return Ok(());
    }
    EventWriter::of(world)?.queue().get().forget(taken)
}

/// `EventLogRecord(event=..., realized_minute=..., status=EventStatus.APPLIED, detail=...)`.
fn followed_record<'py>(py: Python<'py>, followed: &Followed) -> PyResult<Bound<'py, PyAny>> {
    let kwargs = PyDict::new(py);
    kwargs.set_item(intern!(py, "event"), followed.event.to_py(py)?)?;
    kwargs.set_item(intern!(py, "realized_minute"), followed.minute)?;
    kwargs.set_item(intern!(py, "status"), status_member(py, "APPLIED")?)?;
    kwargs.set_item(intern!(py, "detail"), followed.detail.as_deref())?;
    models(py)?.getattr(intern!(py, "EventLogRecord"))?.call((), Some(&kwargs))
}

/// A handler's result: `tuple(records)`, or None for none.
fn records_tuple<'py>(py: Python<'py>, records: &[Followed]) -> PyResult<Option<Bound<'py, PyTuple>>> {
    if records.is_empty() {
        return Ok(None);
    }
    let records = records.iter().map(|record| followed_record(py, record)).collect::<PyResult<Vec<_>>>()?;
    Ok(Some(PyTuple::new(py, records)?))
}

// ---- The transaction ------------------------------------------------------------------------------

/// `_apply_operational_transaction`: the rule (`body`, on the transaction's world, with the slots it
/// took out) and the core's end of it (`core::settle`), in a transaction that stands for the Python
/// rule's copy through canonical JSON, the EventLog included: the APPLIED record and the follow-ups
/// go to the world's log, and a rollback takes them out again.
fn operational<'py, T>(
    world: &Bound<'py, PyAny>,
    event: &Bound<'py, PyAny>,
    row: &EventRow,
    body: impl FnOnce(&Bound<'py, PyAny>, &mut Vec<u64>) -> PyResult<(Vec<Made>, T)>,
) -> PyResult<(Vec<Made>, T)> {
    let py = world.py();
    // The rows of the pending events Python scheduled, read before the transaction copies the queue.
    read_queue_rows(world)?;
    let txn = RuleTxn::begin(world, TxnKind::Full)?;
    let candidate = txn.world().clone();
    let written = (|| {
        read_queue_rows(&candidate)?;
        let mut taken = Vec::new();
        let done = body(&candidate, &mut taken);
        forget(&candidate, &taken)?;
        let (records, value) = done?;
        let shared = held(&candidate, SHARED_OPERATIONS, "the world holds another document than the row's")?;
        let mut host = PyHost::of_world(&candidate, PyDict::new(py)).with_in_flight(event);
        let mut taken = Vec::new();
        let settled = core::settle(&PyWorldCell(&candidate), &mut host, &shared, row, &records, &mut taken).map_err(|error| error.0);
        forget(&candidate, &taken)?;
        settled?;
        Ok((records, value))
    })();
    txn.finish(written)
}

// ---- ProximityContactHandler -----------------------------------------------------------------------

/// `ProximityContactHandler.apply`: it checks and changes nothing.
#[pyfunction]
fn proximity(_handler: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = world.py();
    let result = (|| {
        let config = proximity_config(world)?;
        let row = event_row(event)?;
        let movement = movement_held(world)?;
        with_world(world, |cell| core::proximity(cell, config.as_ref().as_ref(), &movement, &row))?.or_py()
    })();
    counted(py, "proximity", result)
}

// ---- ResourceLedgerHandler -------------------------------------------------------------------------

/// `ResourceLedgerHandler.apply`, with `apply_resource_operation`'s own kernel.
#[pyfunction]
fn resource_ledger(handler: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = world.py();
    let result = (|| {
        let operation = attribute(handler, intern!(py, "operation"))?;
        let operation = exact_str(&operation)?;
        let row = event_row(event)?;
        let operational = operational_held(world)?;
        let service = held(world, "resource_service_v1", "the world holds another service state")?;
        let ledger = held(world, "resource_ledger_v1", "the world holds another ledger")?;
        let inputs = LedgerIn { operational: &operational, service: &service, ledger: &ledger };
        match with_world(world, |cell| core::resource_ledger(cell, &inputs, &row, operation))? {
            Ok(applied) => replace_resources(world, applied),
            // The handler's `except ResourceLedgerError as exc: raise EventApplicationError(str(exc))`.
            // The operation changed nothing when it turned the event down.
            Err(fail) => {
                let error = raised(py, fail);
                if error.is_instance_of::<errors::ResourceLedgerError>(py) {
                    return errors::failed(py, error.value(py).str()?.to_str()?.to_owned());
                }
                Err(error)
            }
        }
    })();
    counted(py, "resource_ledger", result)
}

// ---- Facility outages and recoveries ---------------------------------------------------------------

fn facility_units<'py>(world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>, restores: bool) -> PyResult<Option<Bound<'py, PyTuple>>> {
    let py = world.py();
    let row = event_row(event)?;
    let program = program_held(world)?;
    let infrastructure = infrastructure_held(world)?;
    let inputs = FacilityIn { program: &program, infrastructure: &infrastructure, shared_absent: shared_absent(world)? };
    let made = with_world(world, |cell| core::facility_units(cell, &inputs, &row, restores))?.or_py()?;
    match made {
        Some(made) => Ok(Some(PyTuple::new(py, [made_record(py, made)?])?)),
        None => Ok(None),
    }
}

#[pyfunction]
fn facility_outage<'py>(_handler: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyTuple>>> {
    counted(world.py(), "facility_outage", facility_units(world, event, false))
}

#[pyfunction]
fn facility_recovery<'py>(_handler: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyTuple>>> {
    counted(world.py(), "facility_recovery", facility_units(world, event, true))
}

// ---- Deliveries, water and staff ------------------------------------------------------------------------

/// An operational event whose rule reads only `operational_events_v1`, in its transaction.
fn simple_operational(
    world: &Bound<'_, PyAny>,
    event: &Bound<'_, PyAny>,
    rule: impl FnOnce(&sheltersim_core::world::World, &Held, &EventRow, &mut Vec<u64>) -> sheltersim_core::error::Result<()>,
) -> PyResult<()> {
    let row = event_row(event)?;
    operational(world, event, &row, |candidate, taken| {
        let operational = operational_held(candidate)?;
        with_world(candidate, |cell| rule(cell, &operational, &row, taken))?.or_py()?;
        Ok((Vec::new(), ()))
    })
    .map(drop)
}

#[pyfunction]
fn delivery_delay(_handler: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<()> {
    let result = simple_operational(world, event, core::delivery_delay);
    counted(world.py(), "delivery_delay", result)
}

#[pyfunction]
fn water_outage(_handler: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<()> {
    let result = simple_operational(world, event, |cell, held, row, _| core::water_outage(cell, held, row));
    counted(world.py(), "water_outage", result)
}

#[pyfunction]
fn water_restoration(_handler: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<()> {
    let result = simple_operational(world, event, |cell, held, row, _| core::water_restoration(cell, held, row));
    counted(world.py(), "water_restoration", result)
}

#[pyfunction]
fn staff_restoration(_handler: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<()> {
    let result = simple_operational(world, event, |cell, held, row, _| core::staff_restoration(cell, held, row));
    counted(world.py(), "staff_restoration", result)
}

/// The rule of a staff member's unavailability on the transaction's world: what it reads, the
/// core's call, and the new `resources` dict of a service it stopped. A read that raised is raised
/// where the core reached it.
fn staff_unavailable_on<'py>(candidate: &Bound<'py, PyAny>, row: &EventRow, taken: &mut Vec<u64>) -> PyResult<Vec<Made>> {
    let py = candidate.py();
    let operational = operational_held(candidate)?;
    let gathered = Gathered::of(candidate, true, false)?;
    let service_inputs = service::Inputs::of_world(candidate);
    let manager = service_inputs.as_ref().ok().map(service::Inputs::manager);
    let service = match (&manager, &service_inputs) {
        (Some(manager), _) => Ok(manager),
        (None, Err(error)) if error.is_instance_of::<Unsupported>(py) => Err(Unread::Refused(error.value(py).to_string().into())),
        (None, _) => Err(Unread::Raised),
    };
    let cooldown = support_cooldown_minutes(candidate);
    let ops = gathered.inputs();
    let inputs = StaffIn {
        operational: &operational,
        ops: &ops,
        service,
        cooldown: cooldown.as_ref().ok().map(|minutes| **minutes),
        metric_state_held: field_held(candidate, intern!(py, "metric_state"))?,
    };
    let result = with_world(candidate, |cell| core::staff_unavailable(cell, &inputs, row, taken))?;
    let done = match result {
        Ok(done) => done,
        Err(failure) => {
            let error = service::failed(py, failure);
            let text = if error.is_instance_of::<Unsupported>(py) { error.value(py).to_string() } else { String::new() };
            return match (&*text, &service_inputs, &cooldown) {
                (SERVICE_UNREAD, Err(read), _) => Err(read.clone_ref(py)),
                (COOLDOWN_UNREAD, _, Err(read)) => Err(read.clone_ref(py)),
                _ => gathered.finish(py, Err(error)),
            };
        }
    };
    if let Some(applied) = done.applied {
        replace_resources(candidate, applied)?;
    }
    Ok(done.records)
}

/// `StaffUnavailabilityHandler.apply`, which returns a tuple even when empty.
#[pyfunction]
fn staff_unavailable<'py>(_handler: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    let py = world.py();
    let result = (|| {
        let row = event_row(event)?;
        operational(world, event, &row, |candidate, taken| Ok((staff_unavailable_on(candidate, &row, taken)?, ()))).map(|(records, ())| records)
    })();
    let records = counted(py, "staff_unavailable", result)?;
    PyTuple::new(py, records.into_iter().map(|made| made_record(py, made)).collect::<PyResult<Vec<_>>>()?)
}

// ---- A resource service cancelled or interrupted -----------------------------------------------------
//
// The rules are `rules/service.rs`.

/// `ResourceServiceManager.cancel`, counted.
#[pyfunction]
fn cancel<'py>(manager: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<()> {
    counted(manager.py(), "resource_service_cancel", service::cancel_service(manager, event))
}

/// `ResourceServiceCancellationHandler.apply`: `self.service.cancel(event)`, called directly.
#[pyfunction]
fn service_cancellation<'py>(handler: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<()> {
    let py = world.py();
    with_world(world, |cell| core::service_cancellation(cell))?.or_py()?;
    let manager = attribute(handler, intern!(py, "service"))?;
    counted(py, "resource_service_cancel", service::cancel_service(&manager, event))
}

/// `ResourceServiceManager.interrupt_for_staff`.
#[pyfunction]
#[pyo3(signature = (manager, staff_id, *, causation_event))]
fn interrupt_for_staff<'py>(manager: &Bound<'py, PyAny>, staff_id: &Bound<'py, PyAny>, causation_event: &Bound<'py, PyAny>) -> PyResult<()> {
    counted(manager.py(), "interrupt_for_staff", service::interrupt_service(manager, staff_id, causation_event))
}

// ---- EffectProgramPopulationHandler ------------------------------------------------------------------
//
// The core checks a primitive against its approved program; the rules themselves are the population
// lanes' (`rules/population.rs`, `rules/population_lifecycle.rs`), called here in a transaction that
// stands for the Python rule's copy.

/// `except (TypeError, ValueError) as exc: raise EventApplicationError(str(exc))` of the rule, for an
/// exception of a Python call made where the rule makes it. Any other error goes on as it is.
fn wrapped(py: Python<'_>, error: PyErr) -> PyErr {
    if error.is_instance_of::<Unsupported>(py) || errors::made_here(py, &error) || !(error.is_instance_of::<PyTypeError>(py) || error.is_instance_of::<PyValueError>(py)) {
        return error;
    }
    match error.value(py).str() {
        Ok(text) => errors::error(py, errors::Outcome::Failed, text.to_string_lossy().into_owned()),
        Err(failure) => failure,
    }
}

/// A population rule's kernel, as the handler's Python rule called that rule of `domain/`: its
/// refusal is that rule's KernelRefusal (`kernel_only` of `label`), which the rule's own `try` then
/// takes as any other ValueError.
fn population_rule(py: Python<'_>, kernel: PyResult<()>, label: &str) -> PyResult<()> {
    kernel.map_err(|error| errors::kernel_refusal(py, label, error))
}

/// `EffectProgramPopulationHandler.apply`.
#[pyfunction]
fn population<'py>(_handler: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<()> {
    let py = world.py();
    if str_value(&attribute(event, intern!(py, "event_type"))?) == Some("population_approach_complete") {
        let kernel = population_rules::apply_arrival_approach_completion(world, event);
        let done = population_rule(py, kernel, "population.apply_arrival_approach_completion");
        return counted(py, "population.approach", done);
    }
    let checked = (|| -> PyResult<PopulationOp> {
        let row = event_row(event)?;
        let program = program_held(world)?;
        with_world(world, |cell| core::population_checks(cell, &program, &row))?.or_py()
    })();
    let op = counted(py, "population.checks", checked)?;
    let payload = attribute(event, intern!(py, "payload"))?;
    if op == PopulationOp::Presence {
        // Past its checks the Python rule lets this rule's exceptions go, as this does.
        let command = payload.get_item("command")?;
        let kernel = population_lifecycle::apply_presence_command(world, event, &command).map(|_| ());
        return population_rule(py, kernel, "population_lifecycle.apply_presence_command");
    }
    // A deferral: the transaction with its retry commits, and the event is suppressed.
    let changed = (|| -> PyResult<Option<PyErr>> {
        let txn = RuleTxn::begin(world, TxnKind::Full)?;
        let written = population_writes(txn.world(), event, &payload, op);
        txn.finish(written)
    })();
    // Past the checks a call's exception is the Python rule's own: it made the same calls.
    match counted(py, "population.copy", changed)? {
        Some(deferred) => Err(deferred),
        None => Ok(()),
    }
}

/// The arrivals or departures of `population`, and the checks of the world they leave through the
/// native host: a deferral (`EventSuppressed`) is kept and returned, to be raised after the commit.
fn population_writes<'py>(candidate: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>, payload: &Bound<'py, PyAny>, op: PopulationOp) -> PyResult<Option<PyErr>> {
    let py = candidate.py();
    let (zone, count) = (payload.get_item("zone_id")?, payload.get_item("count")?);
    let done = if op == PopulationOp::Add {
        let tuple = py.import("builtins")?.getattr("tuple")?;
        let empty = PyTuple::empty(py);
        // The call's arguments, in the order the rule's call evaluates them, in its `try`: what
        // `tuple()` raises there is the rule's `EventApplicationError(str(exc))`.
        let as_tuple = |key: &str| -> PyResult<Bound<'py, PyAny>> {
            tuple.call1((payload.call_method1(intern!(py, "get"), (key, &empty))?,)).map_err(|error| wrapped(py, error))
        };
        let options = population_rules::ArrivalOptions {
            household_size: Some(payload.get_item("household_size")?),
            mobility: Some(payload.get_item("mobility")?),
            language: Some(payload.get_item("language")?),
            admission_zone_ids: Some(as_tuple("admission_zone_ids")?),
            approach_route_id: Some(get(payload, "approach_route_id")?),
            person_context_overrides: Some(as_tuple("person_context_overrides")?),
        };
        let kernel = population_rules::arrivals(candidate, event, &zone, &count, &options).map(|_| ());
        population_rule(py, kernel, "population.queue_arrivals")
    } else {
        let include = payload.call_method1(intern!(py, "get"), ("include_initial", false))?;
        let kernel = population_rules::departures(candidate, event, &zone, &count, Some(&include)).map(|_| ());
        population_rule(py, kernel, "population.queue_departures").and_then(|_| {
            let minute = attribute(event, intern!(py, "due_minute"))?;
            let kernel = population_lifecycle::admit_pending_returns(candidate, &minute).map(|_| ());
            population_rule(py, kernel, "population_lifecycle.admit_pending_returns")
        })
    };
    // The rule's `try`: a deferral keeps what was written and is the outcome; its own failures go on;
    // `except (TypeError, ValueError) as exc: raise EventApplicationError(str(exc))`.
    let deferred = match done {
        Ok(()) => None,
        Err(error) if error.is_instance(py, &models(py)?.getattr(intern!(py, "EventSuppressed"))?) => Some(error),
        Err(error) => return Err(wrapped(py, error)),
    };
    let mut host = PyHost::of_world(candidate, PyDict::new(py));
    use sheltersim_core::host::Host;
    host.validate_world().map_err(|error| error.0)?;
    host.validate_extensions().map_err(|error| error.0)?;
    Ok(deferred)
}

// ---- InfrastructureEventHandler --------------------------------------------------------------------

/// `InfrastructureEventHandler.apply`.
#[pyfunction]
fn infrastructure<'py>(_handler: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyTuple>>> {
    let py = world.py();
    let row = event_row(event)?;
    let (kind, label) = match Infra::of(&row.event_type) {
        Ok(found) => found,
        Err(error) => return counted(py, "infrastructure", Err(to_py_err(error))),
    };
    let applied = (|| {
        let previous = with_world(world, |cell| core::working_units(cell))?.or_py()?;
        let (_, followed) = operational(world, event, &row, |candidate, taken| {
            let infrastructure = infrastructure_held(candidate)?;
            let operational = operational_held(candidate)?;
            let program = program_held(candidate)?;
            let inputs = InfraIn { infrastructure: &infrastructure, operational: &operational, program: &program };
            let mut host = PyHost::of_world(candidate, PyDict::new(py)).with_in_flight(event);
            let followed = core::infrastructure(&PyWorldCell(candidate), &mut host, &inputs, &row, kind, taken).map_err(|error| error.0)?;
            Ok((Vec::new(), followed))
        })?;
        Ok((previous, followed))
    })();
    let (previous, mut followed) = counted(py, label, applied)?;

    // The world has changed: what fails from here fails in Python too.
    let shared_absent = shared_absent(world)?;
    let receipts = with_world(world, |cell| core::infrastructure_receipts(cell, shared_absent, &previous, &row))?.or_py()?;
    followed.extend(receipts.into_iter().map(Followed::from));
    records_tuple(py, &followed)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(stats, m)?)?;
    m.add_function(wrap_pyfunction!(proximity, m)?)?;
    m.add_function(wrap_pyfunction!(resource_ledger, m)?)?;
    m.add_function(wrap_pyfunction!(facility_outage, m)?)?;
    m.add_function(wrap_pyfunction!(facility_recovery, m)?)?;
    m.add_function(wrap_pyfunction!(delivery_delay, m)?)?;
    m.add_function(wrap_pyfunction!(water_outage, m)?)?;
    m.add_function(wrap_pyfunction!(water_restoration, m)?)?;
    m.add_function(wrap_pyfunction!(staff_unavailable, m)?)?;
    m.add_function(wrap_pyfunction!(staff_restoration, m)?)?;
    m.add_function(wrap_pyfunction!(infrastructure, m)?)?;
    m.add_function(wrap_pyfunction!(population, m)?)?;
    m.add_function(wrap_pyfunction!(cancel, m)?)?;
    m.add_function(wrap_pyfunction!(service_cancellation, m)?)?;
    m.add_function(wrap_pyfunction!(interrupt_for_staff, m)?)?;
    Ok(())
}
