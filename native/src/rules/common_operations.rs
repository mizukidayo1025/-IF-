//! `domain/common_operations.py`: the entry of `record_shared_operation`, whose rule is the core's
//! (`sheltersim_core::rules::common_operations`, P8b-1). It asks first what only Python can say
//! (the namespace's tree is the world's), reads the receipt as its typed core value, looks up the
//! records of the EventLog the receipt's rule reads, and calls the core. What the core refuses is
//! Unsupported, which the entry raises as `KernelRefusal`.

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyDict;
use sheltersim_core::rules::common_operations::{self as core, Found, Logged, Receipt};

use crate::core::errors::OrPy;
use crate::core::row::{attribute, exact_int, exact_str, unsupported};
use crate::core::value::{VPy, V};
use crate::core::world_core::PyWorldCell;
use crate::host::PyHost;
use crate::state::events::{EventRow, EventRowPy, EventStatus};

/// The receipt classes of `contracts/operations.py` that `record_shared_operation` records.
const RECEIPT_CLASSES: [&str; 5] = [
    "CareTransitionReceipt",
    "StaffSupportContinuityReceipt",
    "StaffHandoffReceipt",
    "FacilityRecoveryReceipt",
    "StaffOperationReceipt",
];

/// `event_log.find(event_id)` of each ID, as the core reads a record.
pub(crate) fn found_records(world: &Bound<'_, PyAny>, ids: Vec<std::sync::Arc<str>>) -> PyResult<Found> {
    let py = world.py();
    let log = attribute(world, intern!(py, "event_log"))?;
    let mut found = Vec::with_capacity(ids.len());
    for id in ids {
        let record = log.call_method1(intern!(py, "find"), (&*id,))?;
        let logged = if record.is_none() { None } else { Some(logged_of(&record)?) };
        found.push((id, logged));
    }
    Ok(Found(found))
}

/// An `EventLogRecord` as the core reads it: its status, minute and event's row.
pub(crate) fn logged_of(record: &Bound<'_, PyAny>) -> PyResult<Logged> {
    let py = record.py();
    let status = attribute(record, intern!(py, "status"))?.getattr(intern!(py, "_value_"))?;
    let Some(status) = EventStatus::parse(exact_str(&status)?) else {
        return unsupported("a record of another status");
    };
    let realized_minute = exact_int(&attribute(record, intern!(py, "realized_minute"))?)?;
    let Some(event) = EventRow::from_event(&attribute(record, intern!(py, "event"))?)? else {
        return unsupported("the receipt's event has no row");
    };
    Ok(Logged { status, realized_minute, event })
}

/// `record_shared_operation(world, receipt)` of a receipt read as the core's: the core checks the
/// namespace and looks up the EventLog's records through the native host (`record_through`, lane
/// P9-1a).
pub(crate) fn record(world: &Bound<'_, PyAny>, receipt: &Receipt) -> PyResult<()> {
    let mut host = PyHost::of_world(world, PyDict::new(world.py()));
    core::record_through(&PyWorldCell(world), &mut host, receipt).map_err(|error| error.0)
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
