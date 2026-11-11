//! `domain/common_operations.py`: the entry of `record_shared_operation`, whose rule is the core's
//! (`sheltersim_core::rules::common_operations`, P8b-1). It asks first what only Python can say
//! (the namespace's tree is the world's), reads the receipt as its typed core value, looks up the
//! records of the EventLog the receipt's rule reads, and calls the core. What the core refuses is
//! Unsupported, which the entry raises as `KernelRefusal`.

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use sheltersim_core::rules::common_operations::{self as core, Found, Logged, Receipt};

use crate::core::errors::{value_error, OrPy};
use crate::core::row::{attribute, exact_dict, exact_int, exact_str, unsupported};
use crate::core::value::{VPy, V};
use crate::core::world_core::{mirror_of, with_world};
use crate::state::events::{EventRow, EventRowPy, EventStatus};
use crate::state::shared_operations::NAME;

/// The receipt classes of `contracts/operations.py` that `record_shared_operation` records.
const RECEIPT_CLASSES: [&str; 5] = [
    "CareTransitionReceipt",
    "StaffSupportContinuityReceipt",
    "StaffHandoffReceipt",
    "FacilityRecoveryReceipt",
    "StaffOperationReceipt",
];

/// The Python rule's first check: `model_state` holds the namespace's tree (or its detached row).
/// A dict that isn't the row's tree is Unsupported; anything else the rule's `ValueError`.
pub(crate) fn held(world: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = world.py();
    let mirror = mirror_of(world, NAME)?;
    let table = attribute(world, intern!(py, "model_state"))?;
    match exact_dict(&table)?.get_item(NAME)? {
        Some(state) if mirror.try_borrow()?.holds_state(&state) => Ok(()),
        Some(state) if state.is_instance_of::<PyDict>() => unsupported("the world holds another document than the row's"),
        _ => Err(value_error("shared_operations_v1 must be selected in the immutable Scenario")),
    }
}

/// `event_log.find(event_id)` of each ID, as the core reads a record.
pub(crate) fn found_records(world: &Bound<'_, PyAny>, ids: Vec<std::sync::Arc<str>>) -> PyResult<Found> {
    let py = world.py();
    let log = attribute(world, intern!(py, "event_log"))?;
    let mut found = Vec::with_capacity(ids.len());
    for id in ids {
        let record = log.call_method1(intern!(py, "find"), (&*id,))?;
        let logged = if record.is_none() {
            None
        } else {
            let status = attribute(&record, intern!(py, "status"))?.getattr(intern!(py, "_value_"))?;
            let Some(status) = EventStatus::parse(exact_str(&status)?) else {
                return unsupported("a record of another status");
            };
            let realized_minute = exact_int(&attribute(&record, intern!(py, "realized_minute"))?)?;
            let Some(event) = EventRow::from_event(&attribute(&record, intern!(py, "event"))?)? else {
                return unsupported("the receipt's event has no row");
            };
            Some(Logged { status, realized_minute, event })
        };
        found.push((id, logged));
    }
    Ok(Found(found))
}

/// `record_shared_operation(world, receipt)` of a receipt read as the core's.
pub(crate) fn record(world: &Bound<'_, PyAny>, receipt: &Receipt) -> PyResult<()> {
    held(world)?;
    let ids = with_world(world, |world| core::needed_records(world, receipt))?;
    let found = found_records(world, ids)?;
    with_world(world, |world| core::record_shared_operation(world, receipt, &found))?.or_py()
}

/// `record_shared_operation(world, record)` of a receipt object of `contracts/operations.py`: its
/// `model_dump(mode="json")`, read as the core's receipt.
#[pyfunction]
pub(crate) fn record_shared_operation<'py>(world: &Bound<'py, PyAny>, receipt: &Bound<'py, PyAny>) -> PyResult<()> {
    let py = world.py();
    let contracts = py.import(intern!(py, "sheltersim.contracts.operations"))?;
    let mut known = false;
    for name in RECEIPT_CLASSES {
        known |= receipt.is_instance(&contracts.getattr(name)?)?;
    }
    if !known {
        return unsupported("an unknown kind of receipt");
    }
    let options = PyDict::new(py);
    options.set_item(intern!(py, "mode"), "json")?;
    let document = receipt.call_method(intern!(py, "model_dump"), (), Some(&options))?;
    record(world, &Receipt::of(&<V as VPy>::from_py(&document)?).or_py()?)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(record_shared_operation, m)?)?;
    Ok(())
}
