//! The rest of `events/handlers.py`: the glue of `sheltersim_core::rules::handlers` (lane P8 d2 of
//! Issue #152). Each entry reads what Python holds (whether `model_state` holds each namespace, the
//! sections of the configs kept by stamp, the floor plan, the resource service's manager), makes the
//! event's row, calls the core with the world borrowed whole, lets go the objects of the queue's
//! slots the core took out or replaced, and makes the Python values of what it returns: the records
//! the dispatcher appends, and the world's new `resources` dict of a ledger's operation.
//!
//! A handler whose Python rule copied the world is the core's (`core::*_event`, lane P9-1b): it reads
//! what the world's outside holds through the native host and runs in the core's rule transaction
//! (`txn::rule`, policy E16), whose end (`settle`) validates the world (policy E6), looks up the
//! EventLog's records and gives the contracts (policy E15). The population primitives' rules read
//! their inputs through the host too (lane P9-3a).
//!
//! The helpers at the start (`counted`, `get`, `get_is`, `status_member`) are for the kernels still in
//! this crate.

use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString, PyTuple};

use sheltersim_core::config::environment::Infrastructure;
use sheltersim_core::config::proximity::Proximity;
use sheltersim_core::rules::handlers::{
    self as core, FacilityIn, Followed, Infra, LedgerIn, COOLDOWN_UNREAD, COUNTS, INFRASTRUCTURE, OPERATIONAL,
};
use sheltersim_core::rules::kit::Held;
use sheltersim_core::rules::staff_operation_events::FORMULA_UNREAD;

use crate::core::configs;
use crate::core::errors::{self, to_py_err, OrPy};
use crate::core::row::{attribute, exact_int, exact_str};
use crate::core::value::{VPy, V};
use crate::core::world_core::{namespace_held, with_world, PyWorldCell};
use crate::host::PyHost;
use crate::rules::care_receipts::made_record;
use crate::rules::information_events::event_row;
use crate::rules::resources::{raised, replace_resources};
use crate::rules::travel::{movement_held, read_queue_rows};
use crate::rules::service;
use crate::state::events::EventRowPy;
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

// ---- The rule transactions ------------------------------------------------------------------------

/// The core's handler of an operational event (`core::*_event`, lane P9-1b), which runs its rule
/// and the end of it in the core's rule transaction with the native host. The rows of the pending
/// events Python scheduled are read first, before the transaction puts the queue in canonical order;
/// the host is given the handler's own object of the event, which the validation with the event in
/// flight hands Python as the rule did.
fn operational_handled<T>(
    world: &Bound<'_, PyAny>,
    event: &Bound<'_, PyAny>,
    run: impl FnOnce(&PyWorldCell<'_, '_>, &mut PyHost<'_>) -> Result<T, crate::host::HostError>,
) -> PyResult<T> {
    read_queue_rows(world)?;
    let mut host = PyHost::of_world(world, PyDict::new(world.py())).with_in_flight(event);
    run(&PyWorldCell(world), &mut host).map_err(|error| error.0)
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

#[pyfunction]
fn delivery_delay(_handler: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<()> {
    let result = event_row(event).and_then(|row| operational_handled(world, event, |cell, host| core::delivery_delay_event(cell, host, &row)));
    counted(world.py(), "delivery_delay", result)
}

#[pyfunction]
fn water_outage(_handler: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<()> {
    let result = event_row(event).and_then(|row| operational_handled(world, event, |cell, host| core::water_outage_event(cell, host, &row)));
    counted(world.py(), "water_outage", result)
}

#[pyfunction]
fn water_restoration(_handler: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<()> {
    let result = event_row(event).and_then(|row| operational_handled(world, event, |cell, host| core::water_restoration_event(cell, host, &row)));
    counted(world.py(), "water_restoration", result)
}

#[pyfunction]
fn staff_restoration(_handler: &Bound<'_, PyAny>, world: &Bound<'_, PyAny>, event: &Bound<'_, PyAny>) -> PyResult<()> {
    let result = event_row(event).and_then(|row| operational_handled(world, event, |cell, host| core::staff_restoration_event(cell, host, &row)));
    counted(world.py(), "staff_restoration", result)
}

/// `StaffUnavailabilityHandler.apply`, which returns a tuple even when empty. A cooldown or a formula
/// whose read raised raises where the rule reads it.
#[pyfunction]
fn staff_unavailable<'py>(_handler: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyTuple>> {
    let py = world.py();
    let result = (|| {
        let row = event_row(event)?;
        let result = operational_handled(world, event, |cell, host| core::staff_unavailable_event(cell, host, &row));
        let result = read_again(py, result, COOLDOWN_UNREAD, || support_cooldown_minutes(world).map(drop));
        read_again(py, result, FORMULA_UNREAD, || crate::rules::staff::staff_operations(world).map(drop))
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

/// `EffectProgramPopulationHandler.apply`: the core's (`core::population_event`, lane P9-3a), which
/// reads what the population rules read through the native host.
#[pyfunction]
fn population<'py>(_handler: &Bound<'py, PyAny>, world: &Bound<'py, PyAny>, event: &Bound<'py, PyAny>) -> PyResult<()> {
    let row = event_row(event)?;
    let mut host = PyHost::of_world(world, PyDict::new(world.py()));
    core::population_event(&PyWorldCell(world), &mut host, &row).map_err(|error| error.0)
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
    let applied = operational_handled(world, event, |cell, host| core::infrastructure_event(cell, host, &row, kind));
    let (previous, mut followed) = counted(py, label, applied)?;
    // The world has changed: what fails from here fails in Python too.
    let mut host = PyHost::of_world(world, PyDict::new(py));
    let receipts = core::infrastructure_after(&PyWorldCell(world), &mut host, &previous, &row).map_err(|error| error.0)?;
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
