//! `experiments/metrics.py` and `experiments/operational_kpis.py`: the KPI columns of a minute, read
//! from the rows in one call. They only read, so a call changes nothing.
//!
//! The columns of `MetricSnapshot` are computed in the core (`sheltersim_core::rules::metrics`);
//! `snapshot_fields` is its glue. `kpi_values` still reads here: it passes Python's own values of
//! `world.resources` and of the snapshot through.
//!
//! The Python rules' errors are raised with their classes and texts. What only a state the checks of
//! the Python rules never let through reaches is Unsupported, which the entries raise as
//! `KernelRefusal`: a tainted row, a staff queue naming no task (a KeyError of the Python rule).

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyDict, PyList, PyString, PyTuple};

use std::sync::Arc;

use sheltersim_core::rules::metrics::{self, Folds, Inputs, OpenWait};

use crate::core::errors::{self, OrPy};
use crate::core::mathx::py_sum;
use crate::core::row::{attribute, exact_dict, exact_int, exact_list, exact_str, namespace_is_absent, unsupported};
use crate::core::world_core::{field_held, field_mirror_of, with_world, NamespaceMirror};
use crate::rules::staff::open_waits;
use crate::state::entities::{entities_of, EntityStore};
use crate::state::events::EventWriter;
use crate::state::metric_state::MetricRow;
use crate::state::resource_ledger::{held_mirror, LedgerRow};

/// `"toilet" in kind.casefold()` of each facility kind that isn't ASCII, by Python's own fold, which
/// Rust doesn't have: the folds the core's `sample` reads.
fn folds_of(py: Python<'_>, kinds: Vec<Arc<str>>, folds: &mut Folds) -> PyResult<()> {
    for kind in kinds {
        let folded = PyString::new(py, &kind).call_method0(intern!(py, "casefold"))?;
        let toilet = exact_str(&folded)?.contains("toilet");
        folds.insert(kind, toilet);
    }
    Ok(())
}

/// `MetricSnapshot.from_world`: the fields of the dataclass in their order, for a sample minute
/// that is the world's. `unsettled` is `world.unsettled_support_requests()` as a list.
///
/// The pilot of a kernel split into a core function and its glue (step5-plan.md, PR 5-4): this
/// gathers what Python holds (the open requests' waits, the event log, the folds of kinds that
/// aren't ASCII, whether `metric_state` is its slot's tree), runs `sheltersim_core::rules::metrics::
/// sample` on the world borrowed whole, and makes the Python tuple of what it returns.
#[pyfunction]
fn snapshot_fields<'py>(
    world: &Bound<'py, PyAny>,
    unsettled: &Bound<'py, PyList>,
    sample_minute: &Bound<'py, PyAny>,
) -> PyResult<Bound<'py, PyTuple>> {
    let py = world.py();
    let sample = exact_int(sample_minute)?;
    let open = open_waits(world, unsettled, sample, true)?;
    let waits = open
        .requests
        .iter()
        .map(|request| {
            Ok(OpenWait {
                agent: &request.agent,
                request_id: exact_str(&request.id)?,
                waited: request.waited,
                reasons: open.reasons_of(request),
                without_qualified_staff: open.has_no_qualified_staff(request),
            })
        })
        .collect::<PyResult<Vec<_>>>()?;
    // The refusals of a world whose events are not Rust's, as before; the log is the world's.
    EventWriter::of(world)?;
    let metric_state_held = field_held(world, intern!(py, "metric_state"))?;
    let mut folds = Folds::default();
    // Once, or twice when a kind isn't ASCII: no borrow is held while Python folds it.
    let taken = loop {
        let taken = {
            let inputs = Inputs { sample, waits: &waits, folds: &folds, metric_state_held };
            with_world(world, |world| -> sheltersim_core::error::Result<_> {
                let unfolded = metrics::unfolded_kinds(world, inputs.folds)?;
                if !unfolded.is_empty() {
                    return Ok(Err(unfolded));
                }
                metrics::sample(world, &inputs).map(Ok)
            })?
            .or_py()?
        };
        match taken {
            Ok(taken) => break taken,
            Err(unfolded) => folds_of(py, unfolded, &mut folds)?,
        }
    };
    let fields: [Bound<'py, PyAny>; 20] = [
        sample_minute.clone(),
        taken.toilet_queue_length.into_pyobject(py)?.into_any(),
        taken.toilet_active_users.into_pyobject(py)?.into_any(),
        taken.toilet_working_units.into_pyobject(py)?.into_any(),
        taken.toilet_wait_count.into_pyobject(py)?.into_any(),
        taken.toilet_wait_total_minutes.into_pyobject(py)?.into_any(),
        taken.toilet_wait_mean_minutes.into_pyobject(py)?.into_any(),
        taken.toilet_wait_max_minutes.into_pyobject(py)?.into_any(),
        taken.stress_mean.into_pyobject(py)?.into_any(),
        taken.stress_high_count.into_pyobject(py)?.into_any(),
        taken.staff_assigned_task_count.into_pyobject(py)?.into_any(),
        taken.staff_busy_count.into_pyobject(py)?.into_any(),
        taken.staff_load_ratio.into_pyobject(py)?.into_any(),
        taken.unresolved_support_request_count.into_pyobject(py)?.into_any(),
        PyString::new(py, &taken.source_ids_json).into_any(),
        taken.support_no_qualified_worker_count.into_pyobject(py)?.into_any(),
        taken.support_no_qualified_worker_request_minutes.into_pyobject(py)?.into_any(),
        taken.support_fairness_promoted_assignment_count.into_pyobject(py)?.into_any(),
        taken.support_wait_staff_busy_request_minutes.into_pyobject(py)?.into_any(),
        taken.support_wait_staff_off_shift_request_minutes.into_pyobject(py)?.into_any(),
    ];
    PyTuple::new(py, fields)
}

/// `operational_kpi_values`: the columns in the order of the Python rule's dict.
#[pyfunction]
fn kpi_values<'py>(world: &Bound<'py, PyAny>, snapshot: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    let py = world.py();
    if exact_int(&attribute(world, intern!(py, "schema_version"))?)? != 6
        || namespace_is_absent(world, intern!(py, "resource_service_v1"))?
    {
        return Err(errors::value_error("operational KPIs require resource-service WorldState v6"));
    }
    let minute = attribute(world, intern!(py, "minute_index"))?;
    if exact_int(&attribute(snapshot, intern!(py, "minute_index"))?)? != exact_int(&minute)? {
        return Err(errors::value_error("operational KPI minute must match WorldState"));
    }
    let store = entities_of(world)?;
    let (toilet, hunger, hydration, water_queue, meal_queue, water_active, meal_active) =
        EntityStore::with_rows(&store, |rows| {
            let count = rows.agents.len();
            if count == 0 {
                // The Python rule's mean of no one.
                return Err(pyo3::exceptions::PyZeroDivisionError::new_err("division by zero"));
            }
            let mean = |need: fn(&crate::state::entities::NeedRow) -> f64| {
                let values: Vec<f64> = rows.agents.iter().map(|(_, agent)| need(&agent.needs)).collect();
                py_sum(&values) / count as f64
            };
            let means = (mean(|needs| needs.toilet_urgency), mean(|needs| needs.hunger), mean(|needs| needs.hydration_need));
            let (mut water_queue, mut meal_queue, mut water_active, mut meal_active) = (0, 0, 0, 0);
            for (_, facility) in rows.facilities.iter() {
                match &*facility.kind {
                    "water_station" => {
                        water_queue += facility.resource_queue_agent_ids.len();
                        water_active += facility.resource_active_agent_ids.len();
                    }
                    "meal_distribution" => {
                        meal_queue += facility.resource_queue_agent_ids.len();
                        meal_active += facility.resource_active_agent_ids.len();
                    }
                    _ => {}
                }
            }
            Ok((means.0, means.1, means.2, water_queue, meal_queue, water_active, meal_active))
        })?;
    let resources = attribute(world, intern!(py, "resources"))?;
    let resources = exact_dict(&resources)?;
    let stock = |name: &Bound<'py, PyString>| -> PyResult<Bound<'py, PyAny>> {
        resources.get_item(name)?.map_or_else(|| unsupported("a resource is missing"), Ok)
    };
    let ledger = held_mirror(world)?;
    let (water_reserved, food_reserved) = NamespaceMirror::with_row::<LedgerRow, _>(&ledger, |row| {
        Ok((row.accounts.of(0).reserved, row.accounts.of(1).reserved))
    })?;
    let metrics = field_mirror_of(world, intern!(py, "metric_state"))?;
    const WAITS: [(&str, &str, &str, &str); 2] = [
        ("water_service_wait_count", "water_service_wait_total_minutes", "water_service_wait_mean_minutes", "water_service_wait_max_minutes"),
        ("meal_service_wait_count", "meal_service_wait_total_minutes", "meal_service_wait_mean_minutes", "meal_service_wait_max_minutes"),
    ];
    let read = |row: &MetricRow, name: &str| row.get(name).copied().unwrap_or(0.0);
    let (wait_values, outage, stock_stall) = NamespaceMirror::with_row::<MetricRow, _>(&metrics, |row| {
        let wait = |names: &(&str, &str, &str, &str)| (read(row, names.0), read(row, names.1), read(row, names.3));
        Ok((
            [wait(&WAITS[0]), wait(&WAITS[1])],
            read(row, "resource_service_wait_water_outage_count"),
            read(row, "resource_service_wait_stock_count"),
        ))
    })?;
    let values = PyDict::new(py);
    values.set_item(intern!(py, "minute_index"), &minute)?;
    values.set_item(intern!(py, "toilet_urgency_mean"), toilet)?;
    values.set_item(intern!(py, "hunger_need_mean"), hunger)?;
    values.set_item(intern!(py, "hydration_need_mean"), hydration)?;
    values.set_item(intern!(py, "water_queue_length"), water_queue)?;
    values.set_item(intern!(py, "meal_queue_length"), meal_queue)?;
    values.set_item(intern!(py, "water_active_service_count"), water_active)?;
    values.set_item(intern!(py, "meal_active_service_count"), meal_active)?;
    values.set_item(intern!(py, "water_ml_remaining"), stock(intern!(py, "water_ml"))?)?;
    values.set_item(intern!(py, "food_meal_units_remaining"), stock(intern!(py, "food_meal_units"))?)?;
    values.set_item(intern!(py, "water_ml_reserved"), water_reserved)?;
    values.set_item(intern!(py, "food_meal_units_reserved"), food_reserved)?;
    for name in [
        intern!(py, "stress_mean"),
        intern!(py, "staff_busy_count"),
        intern!(py, "staff_load_ratio"),
        intern!(py, "unresolved_support_request_count"),
        intern!(py, "support_no_qualified_worker_count"),
        intern!(py, "support_no_qualified_worker_request_minutes"),
        intern!(py, "support_fairness_promoted_assignment_count"),
        intern!(py, "support_wait_staff_busy_request_minutes"),
        intern!(py, "support_wait_staff_off_shift_request_minutes"),
    ] {
        values.set_item(name, attribute(snapshot, name)?)?;
    }
    for (names, (count, total, longest)) in WAITS.iter().zip(wait_values) {
        values.set_item(names.0, count)?;
        values.set_item(names.1, total)?;
        values.set_item(names.2, if count != 0.0 { total / count } else { 0.0 })?;
        values.set_item(names.3, longest)?;
    }
    values.set_item(intern!(py, "water_outage_stall_count"), outage)?;
    values.set_item(intern!(py, "resource_stock_stall_count"), stock_stall)?;
    Ok(values)
}

/// What `capture` makes a snapshot of: the class and its fields, in the order of the dataclass.
struct SnapshotClass {
    class: Py<PyAny>,
    new: Py<PyAny>,
    fields: Vec<Py<PyString>>,
}

static SNAPSHOT: GILOnceCell<SnapshotClass> = GILOnceCell::new();

/// `MetricsRecorder.capture` of the minute that is the world's, for the driver's `_MetricSink`: all
/// of it, or Unsupported with nothing changed and the Python rule then raises or does what's left.
#[pyfunction]
pub(crate) fn capture<'py>(world: &Bound<'py, PyAny>, recorder: &Bound<'py, PyAny>, minute: &Bound<'py, PyAny>) -> PyResult<()> {
    let py = world.py();
    let class = SNAPSHOT.get_or_try_init(py, || -> PyResult<SnapshotClass> {
        let class = py.import("sheltersim.experiments.metrics")?.getattr("MetricSnapshot")?;
        let fields = class.getattr("__dataclass_fields__")?.downcast_into::<PyDict>()?;
        let fields = fields.keys().iter().map(|name| Ok(name.downcast_into::<PyString>()?.unbind())).collect::<PyResult<Vec<_>>>()?;
        Ok(SnapshotClass { class: class.unbind(), new: py.import("builtins")?.getattr("object")?.getattr("__new__")?.unbind(), fields })
    })?;
    let sample = exact_int(minute)?;
    if exact_int(&attribute(world, intern!(py, "minute_index"))?)? != sample {
        return unsupported("the metric minute is not the world's");
    }
    let unsettled = world.call_method0(intern!(py, "unsettled_support_requests"))?;
    let unsettled = match unsettled.downcast_exact::<PyList>() {
        Ok(list) => list.clone(),
        Err(_) => PyList::new(py, unsettled.try_iter()?.collect::<PyResult<Vec<_>>>()?)?,
    };
    let values = snapshot_fields(world, &unsettled, minute)?;
    if values.len() != class.fields.len() {
        return unsupported("the snapshot has other fields");
    }
    let snapshots = attribute(recorder, intern!(py, "snapshots"))?;
    let snapshots = exact_list(&snapshots)?;
    if let Some(last) = snapshots.len().checked_sub(1) {
        if exact_int(&attribute(&snapshots.get_item(last)?, intern!(py, "minute_index"))?)? >= sample {
            return unsupported("metric snapshots must have strictly increasing minutes");
        }
    }
    let resources = crate::rules::resources::metric_values(world)?;
    let kept = attribute(recorder, intern!(py, "resource_snapshots"))?;
    let row = match &resources {
        None if !kept.is_none() => return unsupported("the ledger was removed"),
        None => None,
        Some(_) if kept.is_none() && !snapshots.is_empty() => return unsupported("the ledger came late"),
        Some(resources) => {
            let row = PyDict::new(py);
            row.set_item(intern!(py, "minute_index"), minute)?;
            row.update(resources.as_mapping())?;
            Some(row)
        }
    };
    if !kept.is_none() {
        exact_list(&kept)?;
    }
    let snapshot = class.new.bind(py).call1((class.class.bind(py),))?;
    for (name, value) in class.fields.iter().zip(values.iter()) {
        // SAFETY: all three are live objects; the function is in the stable ABI and sets an error
        // on failure. The dataclass is frozen: this is what its `__init__` does through `object`.
        if unsafe { pyo3::ffi::PyObject_GenericSetAttr(snapshot.as_ptr(), name.as_ptr(), value.as_ptr()) } != 0 {
            return Err(PyErr::fetch(py));
        }
    }
    if let Some(row) = row {
        if kept.is_none() {
            recorder.setattr(intern!(py, "resource_snapshots"), PyList::new(py, [row])?)?;
        } else {
            exact_list(&kept)?.append(row)?;
        }
    }
    snapshots.append(snapshot)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(capture, m)?)?;
    m.add_function(wrap_pyfunction!(snapshot_fields, m)?)?;
    m.add_function(wrap_pyfunction!(kpi_values, m)?)?;
    Ok(())
}
