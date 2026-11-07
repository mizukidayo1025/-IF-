//! `domain/staff_dynamics.py`: staff shift, rest and fatigue. Its rules are these kernels alone (lane
//! L10, `kernel_only`), and what they refuse is a KernelRefusal there; the checks of a Scenario's
//! config and of an override stay in Python (docs/rust-port/lanes/s-staff-dynamics.md).
//!
//! `update` plans everything from the rows first, so an Unsupported changes nothing. Every other
//! kernel only reads, or builds a new value.

use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyList, PyString};

use crate::core::errors;
use crate::core::mathx::{exp, py_max, py_min};
use crate::core::row::{
    attribute, exact_dict, exact_int, exact_list, exact_str, namespace_is_absent,
    unsupported, Field,
};
use crate::core::world_core::{mirror_of, NamespaceMirror};
use crate::state::entities::{entities_of, EntityStore};
use crate::state::staff_dynamics::{
    shifts_are_valid, ConfigRow, LastActivity, Shift, StaffDynamicsRow, StaffRecord, MAX_REST_MINUTES, MAX_SHIFTS,
    MINUTES_PER_DAY, MINUTES_PER_WEEK, NAME, POLICY_V1, POLICY_V2,
};

const SOURCE_V1: &str = "ASM-STAFF-DYNAMICS-V1";
const SOURCE_V2: &str = "ASM-STAFF-DYNAMICS-V2";
const LINEAR_FATIGUE: &str = "staff-fatigue-linear-v1";
const SATURATING_FATIGUE: &str = "staff-fatigue-saturating-v1";
const CONFIG_FIELDS: [&str; 8] = [
    "schema_version",
    "policy_version",
    "source_id",
    "max_continuous_work_minutes",
    "max_continuous_work_minutes_by_staff",
    "rest_minutes",
    "shift_windows_by_staff",
    "off_shift_fatigue_recovery_per_minute",
];
/// Above this an int does not always fit a float, and Python's int / int is correctly rounded.
const FLOAT_EXACT_INT: u64 = 1 << 53;

// ---- What the rules read from the rest of the world ------------------------------------------

/// `state["staff_status"]` of `operational_events_v1`, or None without that namespace: the input
/// of `staff_is_available` in `domain/operational.py`.
pub(crate) fn staff_status<'py>(world: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, PyAny>>> {
    let py = world.py();
    let table = attribute(world, intern!(py, "model_state"))?;
    let Some(state) = exact_dict(&table)?.get_item(intern!(py, "operational_events_v1"))? else {
        return Ok(None);
    };
    if state.is_none() {
        return Ok(None);
    }
    match exact_dict(&state)?.get_item(intern!(py, "staff_status"))? {
        Some(status) => {
            exact_dict(&status)?;
            Ok(Some(status))
        }
        None => unsupported("operational_events_v1 has no staff_status"),
    }
}

/// `staff_is_available`.
pub(crate) fn is_available(status: Option<&Bound<'_, PyAny>>, id: &Bound<'_, PyString>) -> PyResult<bool> {
    let Some(status) = status else {
        return Ok(true);
    };
    let py = id.py();
    match exact_dict(status)?.get_item(id)? {
        None => Ok(true),
        Some(record) if record.is_none() => Ok(true),
        Some(record) => match exact_dict(&record)?.get_item(intern!(py, "available"))? {
            Some(available) => match available.downcast_exact::<pyo3::types::PyBool>() {
                Ok(available) => Ok(available.is_true()),
                Err(_) => unsupported("available is not a bool"),
            },
            None => unsupported("a staff status has no available"),
        },
    }
}

/// `_on_shift`. A config of the oldest schema has no windows, and the Python rule raises.
pub(crate) fn on_shift(config: &ConfigRow, id: &str, minute: i64) -> PyResult<bool> {
    let Some(windows) = &config.shift_windows_by_staff.0 else {
        return unsupported("the config has no shift windows");
    };
    let Some(shifts) = windows.get(id) else {
        return Ok(true);
    };
    let week_minute = minute.rem_euclid(MINUTES_PER_WEEK);
    Ok(shifts
        .iter()
        .any(|shift| shift.start_minute <= week_minute && week_minute < shift.end_minute))
}

// ---- The fatigue formula -----------------------------------------------------------------------

struct Fatigue<'py> {
    saturating: bool,
    limits: Option<Bound<'py, PyDict>>,
}

/// `create_staff_fatigue_formula`, for the formulas this kernel knows. A later registered one, or
/// anything but plain documents, is the Python rule's.
fn fatigue_formula<'py>(world: &Bound<'py, PyAny>) -> PyResult<Fatigue<'py>> {
    let py = world.py();
    let configs = attribute(world, intern!(py, "model_configs"))?;
    let configs = exact_dict(&configs)?;
    let selected = match configs.get_item(intern!(py, "policy_registry"))? {
        Some(registry) => exact_dict(&registry)?.get_item(intern!(py, "staff_fatigue"))?,
        None => None,
    };
    let saturating = match &selected {
        Some(selected) => match exact_str(selected)? {
            LINEAR_FATIGUE => false,
            SATURATING_FATIGUE => true,
            _ => return unsupported("another fatigue formula"),
        },
        None => false,
    };
    let limits = match configs.get_item(intern!(py, "staff_dynamics"))? {
        Some(config) => {
            match exact_dict(&config)?.get_item(intern!(py, "max_continuous_work_minutes_by_staff"))? {
                Some(limits) => Some(exact_dict(&limits)?.clone()),
                None => None,
            }
        }
        None => None,
    };
    Ok(Fatigue { saturating, limits: limits.filter(|limits| !limits.is_empty()) })
}

/// `StaffFatigueFormula.fatigue`, or None where Python's result isn't sure to be f64 division.
#[inline]
fn fatigue_of(saturating: bool, consecutive: i64, maximum: i64) -> Option<f64> {
    if maximum == 0
        || consecutive.unsigned_abs() > FLOAT_EXACT_INT
        || maximum.unsigned_abs() > FLOAT_EXACT_INT
    {
        return None;
    }
    let fraction = consecutive as f64 / maximum as f64;
    if saturating {
        return Some(1.0 - exp(-fraction).ok()?);
    }
    Some(py_min(1.0, fraction))
}

// ---- update_staff_dynamics ---------------------------------------------------------------------

/// What one tick needs to know of one member, read before the row is.
struct Member<'py> {
    id: Bound<'py, PyString>,
    working: bool,
    /// `staff_is_available`; only asked of who has no task.
    available: bool,
    maximum: Option<i64>,
}

struct Parameters {
    saturating: bool,
    default_maximum: i64,
    rest_minutes: i64,
    recovery: Option<f64>,
    duration: i64,
    minute: i64,
}

/// One member's next record: the loop body of `update_staff_dynamics`. None when an int leaves
/// what the kernel holds or a config key the Python rule reads is not there.
fn next_record(
    record: &StaffRecord,
    working: bool,
    available_on_shift: bool,
    maximum: i64,
    parameters: &Parameters,
) -> Option<StaffRecord> {
    let mut next = *record;
    if working {
        next.consecutive_work_minutes = next.consecutive_work_minutes.checked_add(1)?;
        next.total_work_minutes = next.total_work_minutes.checked_add(1)?;
        next.fatigue = fatigue_of(parameters.saturating, next.consecutive_work_minutes, maximum)?;
        next.last_activity = LastActivity::Working;
        return Some(next);
    }
    if !available_on_shift {
        next.last_activity = LastActivity::OffDuty;
        next.consecutive_work_minutes = 0;
        next.total_rest_minutes = next.total_rest_minutes.checked_add(1)?;
        next.fatigue = py_max(0.0, next.fatigue - parameters.recovery?);
        return Some(next);
    }
    if parameters.minute < next.rest_until_minute {
        next.total_rest_minutes = next.total_rest_minutes.checked_add(1)?;
        next.last_activity = LastActivity::Resting;
        return Some(next);
    }
    if next.rest_until_minute != 0 {
        next.consecutive_work_minutes = 0;
        next.fatigue = 0.0;
        next.rest_until_minute = 0;
    }
    if next.consecutive_work_minutes >= maximum {
        next.rest_until_minute =
            parameters.duration.min(parameters.minute.checked_add(parameters.rest_minutes)?);
        next.last_activity = LastActivity::Resting;
    } else {
        next.last_activity = LastActivity::OffDuty;
    }
    Some(next)
}

fn plan(
    row: &StaffDynamicsRow,
    members: &[Member<'_>],
    parameters: &Parameters,
) -> PyResult<Vec<StaffRecord>> {
    let mut planned = Vec::with_capacity(members.len());
    for member in members {
        let id = member.id.to_str()?;
        let Some(record) = row.staff.get(id) else {
            return unsupported("a member has no record");
        };
        let maximum = member.maximum.unwrap_or(parameters.default_maximum);
        // In the Python rule's order: _on_shift is only asked of who has no task and is available.
        let available_on_shift =
            !member.working && member.available && on_shift(&row.config, id, parameters.minute)?;
        let Some(next) = next_record(record, member.working, available_on_shift, maximum, parameters)
        else {
            return unsupported("a value leaves what this kernel holds");
        };
        planned.push(next);
    }
    Ok(planned)
}

/// `update_staff_dynamics`.
#[pyfunction]
pub(crate) fn update(world: &Bound<'_, PyAny>, minute: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = world.py();
    if namespace_is_absent(world, intern!(py, NAME))? {
        return Ok(());
    }
    let minute = exact_int(minute)?;
    let duration = exact_int(&attribute(world, intern!(py, "duration_minutes"))?)?;
    let mirror = mirror_of(world, NAME)?;
    let formula = fatigue_formula(world)?;
    let status = staff_status(world)?;

    let store = entities_of(world)?;
    let staff: Vec<(Arc<str>, bool)> = EntityStore::with_rows(&store, |rows| {
        Ok(rows
            .staff
            .iter()
            .map(|(id, row)| (id.clone(), row.active_task_id.is_some()))
            .collect())
    })?;
    let mut members = Vec::with_capacity(staff.len());
    for (id, working) in staff {
        let id = PyString::new(py, &id);
        let maximum = match &formula.limits {
            Some(limits) => limits.get_item(&id)?.map(|limit| exact_int(&limit)).transpose()?,
            None => None,
        };
        let available = working || is_available(status.as_ref(), &id)?;
        members.push(Member { id, working, available, maximum });
    }

    let planned = NamespaceMirror::with_row::<StaffDynamicsRow, _>(&mirror, |row| {
        let parameters = Parameters {
            saturating: formula.saturating,
            default_maximum: row.config.max_continuous_work_minutes,
            rest_minutes: row.config.rest_minutes,
            recovery: row.config.off_shift_fatigue_recovery_per_minute.0.map(|recovery| recovery.value),
            duration,
            minute,
        };
        plan(row, &members, &parameters)
    })?;
    NamespaceMirror::with_row_tree::<StaffDynamicsRow, _>(&mirror, |row, tree| {
        let staff = tree.child(intern!(py, "staff"))?;
        for (member, next) in members.iter().zip(&planned) {
            let id = member.id.to_str()?;
            let Some(record) = row.staff.get_mut(id) else {
                return unsupported("the row changed under the plan");
            };
            let node = staff.child(&member.id)?;
            record.set_all(&node, next)?;
        }
        Ok(())
    })
}

// ---- The readers --------------------------------------------------------------------------------

/// `staff_dynamics_on_shift`.
#[pyfunction(name = "on_shift")]
fn on_shift_now(
    world: &Bound<'_, PyAny>,
    staff_id: &Bound<'_, PyAny>,
    minute: &Bound<'_, PyAny>,
) -> PyResult<bool> {
    if namespace_is_absent(world, intern!(world.py(), NAME))? {
        return Ok(true);
    }
    let id = exact_str(staff_id)?;
    let minute = exact_int(minute)?;
    let mirror = mirror_of(world, NAME)?;
    NamespaceMirror::with_row::<StaffDynamicsRow, _>(&mirror, |row| {
        let Some(record) = row.staff.get(id) else {
            return Ok(false);
        };
        Ok(on_shift(&row.config, id, minute)? && minute >= record.rest_until_minute)
    })
}

/// `operation is None or operation.get("phase") == "idle"` for staff_operations_v1 as the Python
/// rule reads it, which is before anything else, so a bad document always goes to it.
fn operation_is_idle(world: &Bound<'_, PyAny>, id: &Bound<'_, PyString>) -> PyResult<bool> {
    let py = world.py();
    let table = attribute(world, intern!(py, "model_state"))?;
    let Some(operations) = exact_dict(&table)?.get_item(intern!(py, "staff_operations_v1"))? else {
        return Ok(true);
    };
    if operations.is_none() {
        return Ok(true);
    }
    let Some(staff) = exact_dict(&operations)?.get_item(intern!(py, "staff"))? else {
        return Ok(true);
    };
    let Some(operation) = exact_dict(&staff)?.get_item(id)? else {
        return Ok(true);
    };
    if operation.is_none() {
        return Ok(true);
    }
    match exact_dict(&operation)?.get_item(intern!(py, "phase"))? {
        Some(phase) if !phase.is_none() => Ok(exact_str(&phase)? == "idle"),
        _ => Ok(false),
    }
}

/// `staff_dynamics_is_available`.
#[pyfunction(name = "is_available")]
pub(crate) fn is_available_now(
    world: &Bound<'_, PyAny>,
    staff_id: &Bound<'_, PyAny>,
    minute: &Bound<'_, PyAny>,
) -> PyResult<bool> {
    if namespace_is_absent(world, intern!(world.py(), NAME))? {
        return Ok(true);
    }
    let Ok(id) = staff_id.downcast_exact::<PyString>() else {
        return unsupported("the id is not a str");
    };
    let text = exact_str(staff_id)?;
    let minute = exact_int(minute)?;
    let idle = operation_is_idle(world, id)?;
    let mirror = mirror_of(world, NAME)?;
    let eligible = NamespaceMirror::with_row::<StaffDynamicsRow, _>(&mirror, |row| {
        let Some(record) = row.staff.get(text) else {
            return Ok(false);
        };
        Ok(on_shift(&row.config, text, minute)? && minute >= record.rest_until_minute)
    })?;
    if !eligible {
        return Ok(false);
    }
    let status = staff_status(world)?;
    Ok(is_available(status.as_ref(), id)? && idle)
}

// ---- add_staff, and what a new world starts from ---------------------------------------------------

/// `add_staff`: one record, forwarded neither way.
#[pyfunction]
fn add_staff(world: &Bound<'_, PyAny>, staff_id: &Bound<'_, PyAny>) -> PyResult<()> {
    if namespace_is_absent(world, intern!(world.py(), NAME))? {
        return Ok(());
    }
    let id = exact_str(staff_id)?;
    let mirror = mirror_of(world, NAME)?;
    let record = StaffRecord::new();
    NamespaceMirror::with_row::<StaffDynamicsRow, _>(&mirror, |row| {
        match row.staff.get_mut(id) {
            Some(slot) => *slot = record,
            None => {
                row.staff.insert(Arc::from(id), record);
            }
        }
        Ok(())
    })?;
    NamespaceMirror::write_through(&mirror, &["staff"], id, &Field::to_py(&record, world.py())?)
}

/// An exact non-empty str key of a config map.
fn staff_key<'a>(key: &'a Bound<'_, PyAny>) -> PyResult<&'a str> {
    let id = exact_str(key)?;
    if id.is_empty() {
        return unsupported("an empty staff id");
    }
    Ok(id)
}

/// What a check of the Python rule says of a value: it fails, it passes, or the rows can't tell
/// (a type the Python rule may take, such as `schema_version: 1.0`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Fails,
    Passes,
    Unknown,
}

/// `type(value) is int and lower <= value <= upper` (`errors::exactly_int`).
fn int_within(value: &Bound<'_, PyAny>, lower: i64, upper: i64) -> PyResult<Verdict> {
    Ok(match errors::exactly_int(value)? {
        Some(value) if (lower..=upper).contains(&value) => Verdict::Passes,
        _ => Verdict::Fails,
    })
}

/// `isinstance(value, int) and not isinstance(value, bool) and lower <= value <= upper`
/// (`errors::int_not_bool`).
fn plain_int_within(value: &Bound<'_, PyAny>, lower: i64, upper: i64) -> PyResult<Verdict> {
    Ok(match errors::int_not_bool(value)? {
        Some(value) if (lower..=upper).contains(&value) => Verdict::Passes,
        _ => Verdict::Fails,
    })
}

/// `raise ValueError(text)` of `canonical_staff_dynamics_config`.
#[track_caller]
fn config_error<T>(text: &str) -> PyResult<T> {
    Err(errors::value_error(text))
}

/// `bool(value)` of a plain value; Unsupported for an object whose truth runs Python code.
fn truth(value: &Bound<'_, PyAny>) -> PyResult<bool> {
    if let Some(dict) = crate::core::tracked::as_readable_dict(value) {
        return Ok(!dict.is_empty());
    }
    let plain = value.is_none()
        || value.is_exact_instance_of::<PyList>()
        || value.is_exact_instance_of::<PyString>()
        || value.is_exact_instance_of::<PyBool>()
        || value.is_exact_instance_of::<pyo3::types::PyInt>()
        || value.is_exact_instance_of::<PyFloat>();
    if !plain {
        return unsupported("a value whose truth is another class's");
    }
    value.is_truthy()
}

/// `value != 0.0` of a plain value: an int or a float by its value (a bool too), and anything else
/// is not equal. Unsupported for an object whose `!=` runs Python code.
fn not_zero(value: &Bound<'_, PyAny>) -> PyResult<bool> {
    if let Ok(number) = value.downcast_exact::<PyFloat>() {
        return Ok(number.value() != 0.0);
    }
    if value.is_exact_instance_of::<PyBool>() {
        return value.is_truthy();
    }
    if let Some(number) = errors::wide_int_not_bool(value)? {
        return Ok(number != errors::Wide::Int(0));
    }
    let plain = value.is_none()
        || value.is_exact_instance_of::<PyList>()
        || value.is_exact_instance_of::<PyString>()
        || crate::core::tracked::as_readable_dict(value).is_some();
    if !plain {
        return unsupported("a value whose != is another class's");
    }
    Ok(true)
}

/// The recovery the range check passes: a float, or an int that is not a bool, finite and in 0..1.
fn recovery_in_range(value: &Bound<'_, PyAny>) -> PyResult<bool> {
    if let Ok(number) = value.downcast_exact::<PyFloat>() {
        let number = number.value();
        return Ok(number.is_finite() && (0.0..=1.0).contains(&number));
    }
    Ok(matches!(errors::wide_int_not_bool(value)?, Some(errors::Wide::Int(0 | 1))))
}

/// `canonical_staff_dynamics_config`, which `domain/staff_dynamics.py` keeps for a Scenario's
/// check: the new dict, or its ValueError in its order, or Unsupported for a value of a type it
/// takes that the rows do not (a str subclass). A `schema_version` equal to 1 (`1.0`, `True`) is
/// written as the int 1, as the Python check writes it since model version 2.
fn canonical_config<'py>(raw: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    let py = raw.py();
    let empty;
    let raw = if raw.is_none() {
        empty = PyDict::new(py);
        &empty
    } else if !raw.is_instance_of::<PyDict>() {
        return config_error("staff_dynamics config has unsupported fields");
    } else {
        exact_dict(raw)?
    };
    for (key, _) in raw.iter() {
        match key.downcast_exact::<PyString>() {
            Ok(key) if CONFIG_FIELDS.contains(&key.to_str()?) => {}
            Err(_) if key.is_instance_of::<PyString>() => return unsupported("a key is a str of another class"),
            _ => return config_error("staff_dynamics config has unsupported fields"),
        }
    }
    let windows = raw.get_item(intern!(py, "shift_windows_by_staff"))?;
    let recovery = raw.get_item(intern!(py, "off_shift_fatigue_recovery_per_minute"))?;
    let policy = raw.get_item(intern!(py, "policy_version"))?;
    // `raw.get("policy_version") == V1 or (no policy and neither later key)`.
    let legacy = match &policy {
        Some(version) => match version.downcast_exact::<PyString>() {
            Ok(version) => version.to_str()? == POLICY_V1,
            Err(_) if version.is_instance_of::<PyString>() => return unsupported("another policy"),
            // Not V1, and the check of the policy below fails it.
            Err(_) => false,
        },
        None => windows.is_none() && recovery.is_none(),
    };
    // One text for the schema, the policy and the source: any that fails raises it.
    // `schema_version != 1`, for what a Scenario's JSON holds.
    let schema = match raw.get_item(intern!(py, "schema_version"))? {
        None => Verdict::Passes,
        Some(schema) if schema.is_exact_instance_of::<pyo3::types::PyInt>() => {
            if schema.extract::<i64>().is_ok_and(|schema| schema == 1) {
                Verdict::Passes
            } else {
                Verdict::Fails
            }
        }
        Some(schema) if schema.is_exact_instance_of::<PyBool>() => {
            if schema.is_truthy()? {
                Verdict::Passes
            } else {
                Verdict::Fails
            }
        }
        Some(schema) if schema.is_exact_instance_of::<PyFloat>() => {
            if schema.downcast_exact::<PyFloat>()?.value() == 1.0 {
                Verdict::Passes
            } else {
                Verdict::Fails
            }
        }
        Some(schema)
            if schema.is_none()
                || schema.is_exact_instance_of::<PyString>()
                || schema.is_exact_instance_of::<PyList>()
                || schema.is_exact_instance_of::<PyDict>() =>
        {
            Verdict::Fails
        }
        Some(_) => Verdict::Unknown,
    };
    let policy_verdict = match &policy {
        None => Verdict::Passes,
        Some(version) => match version.downcast_exact::<PyString>() {
            Ok(version) if matches!(version.to_str()?, POLICY_V1 | POLICY_V2) => Verdict::Passes,
            Ok(_) => Verdict::Fails,
            // `in` a set hashes it: an int, a float, None or a bool is neither, and a list raises.
            Err(_) if version.hash().is_ok() => Verdict::Fails,
            Err(_) => Verdict::Unknown,
        },
    };
    let source = raw.get_item(intern!(py, "source_id"))?;
    let source_verdict = match &source {
        None => Verdict::Passes,
        Some(source) if !source.is_instance_of::<PyString>() => Verdict::Fails,
        Some(source) => match source.downcast_exact::<PyString>() {
            Ok(text) if text.to_str()?.is_empty() => Verdict::Fails,
            Ok(_) => Verdict::Passes,
            Err(_) => Verdict::Unknown,
        },
    };
    let checks = [schema, policy_verdict, source_verdict];
    if checks.contains(&Verdict::Fails) {
        return config_error("unsupported staff_dynamics policy or source");
    }
    if checks.contains(&Verdict::Unknown) {
        return unsupported("another schema or source");
    }
    // Policy v1 with shifts or a recovery: `config["shift_windows_by_staff"] or recovery != 0.0`.
    if legacy {
        let any_windows = match &windows {
            None => false,
            Some(windows) => truth(windows)?,
        };
        let any_recovery = match &recovery {
            None => false,
            Some(recovery) => not_zero(recovery)?,
        };
        if any_windows || any_recovery {
            return config_error("staff shift and off-shift recovery assumptions require policy v2");
        }
    }
    for (name, default, upper) in [
        (intern!(py, "max_continuous_work_minutes"), 240, MINUTES_PER_DAY),
        (intern!(py, "rest_minutes"), 30, MAX_REST_MINUTES),
    ] {
        let verdict = match raw.get_item(name)? {
            Some(value) => plain_int_within(&value, 1, upper)?,
            None => {
                if (1..=upper).contains(&default) {
                    Verdict::Passes
                } else {
                    Verdict::Fails
                }
            }
        };
        match verdict {
            Verdict::Passes => {}
            Verdict::Fails => return config_error(&format!("staff_dynamics {name} is outside its supported range")),
            Verdict::Unknown => return unsupported("a limit is an int of another type"),
        }
    }
    if let Some(limits) = raw.get_item(intern!(py, "max_continuous_work_minutes_by_staff"))? {
        if !limits.is_instance_of::<PyDict>() {
            return config_error("staff_dynamics per-staff work limits must be an object");
        }
        for (id, limit) in exact_dict(&limits)?.iter() {
            let id_passes = id.is_instance_of::<PyString>() && !id.is_empty()?;
            if !id_passes || int_within(&limit, 1, MINUTES_PER_DAY)? == Verdict::Fails {
                return config_error("staff_dynamics per-staff work limits must be integer minutes in 1..1440");
            }
            staff_key(&id)?;
        }
    }
    if let Some(windows) = &windows {
        if !windows.is_instance_of::<PyDict>() {
            return config_error("staff_dynamics shift_windows_by_staff must be an object");
        }
        let windows = exact_dict(windows)?;
        for (id, rows) in windows.iter() {
            let id_passes = id.is_instance_of::<PyString>() && !id.is_empty()?;
            let rows_pass = rows.is_instance_of::<PyList>() && rows.len()? <= MAX_SHIFTS;
            if !id_passes || !rows_pass {
                return config_error("staff_dynamics shift windows require a staff ID and at most 21 intervals");
            }
            staff_key(&id)?;
            let mut shifts = Vec::new();
            for row in exact_list(&rows)?.iter() {
                let fits = match row.downcast::<PyDict>() {
                    Err(_) => false,
                    Ok(row) => {
                        let start = row.get_item(intern!(py, "start_minute"))?;
                        let end = row.get_item(intern!(py, "end_minute"))?;
                        match (start, end, row.len()) {
                            (Some(start), Some(end), 2) => match (int_within(&start, 0, MINUTES_PER_WEEK)?, int_within(&end, 0, MINUTES_PER_WEEK)?) {
                                (Verdict::Passes, Verdict::Passes) => {
                                    let shift = Shift { start_minute: start.extract()?, end_minute: end.extract()? };
                                    let fits = shift.start_minute < shift.end_minute;
                                    shifts.push(shift);
                                    fits
                                }
                                _ => false,
                            },
                            _ => false,
                        }
                    }
                };
                if !fits {
                    return config_error("staff shift intervals must be non-empty [start,end) weekly minutes");
                }
                exact_dict(&row)?;
            }
            if !shifts_are_valid(&shifts) {
                return config_error("staff shift intervals for one person cannot overlap");
            }
        }
    }
    if let Some(recovery) = &recovery {
        if !recovery_in_range(recovery)? {
            return config_error("staff_dynamics off-shift recovery must be within 0..1 per minute");
        }
    }
    let policy = if legacy { POLICY_V1 } else { POLICY_V2 };

    // {defaults, **raw}: every key of raw is one of the defaults, so this is their order.
    let config = PyDict::new(py);
    let take = |name: &Bound<'py, PyString>, default: Bound<'py, PyAny>| -> PyResult<()> {
        match raw.get_item(name)? {
            Some(value) => config.set_item(name, value),
            None => config.set_item(name, default),
        }
    };
    // The schema passed, so it equals 1: always the int.
    config.set_item(intern!(py, "schema_version"), 1_i64)?;
    take(intern!(py, "policy_version"), PyString::new(py, policy).into_any())?;
    take(
        intern!(py, "source_id"),
        PyString::new(py, if legacy { SOURCE_V1 } else { SOURCE_V2 }).into_any(),
    )?;
    take(intern!(py, "max_continuous_work_minutes"), 240_i64.into_pyobject(py)?.into_any())?;
    take(intern!(py, "max_continuous_work_minutes_by_staff"), PyDict::new(py).into_any())?;
    take(intern!(py, "rest_minutes"), 30_i64.into_pyobject(py)?.into_any())?;
    take(intern!(py, "shift_windows_by_staff"), PyDict::new(py).into_any())?;
    take(
        intern!(py, "off_shift_fatigue_recovery_per_minute"),
        PyFloat::new(py, if legacy { 0.0 } else { 0.005 }).into_any(),
    )?;
    Ok(config)
}

/// `create_staff_dynamics_state`.
#[pyfunction]
fn create_state<'py>(world: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    let py = world.py();
    let configs = attribute(world, intern!(py, "model_configs"))?;
    let none = py.None().into_bound(py);
    let raw = exact_dict(&configs)?.get_item(intern!(py, "staff_dynamics"))?.unwrap_or(none);
    let config = canonical_config(&raw)?;
    let limits_key = intern!(py, "max_continuous_work_minutes_by_staff");
    let policy = config.get_item(intern!(py, "policy_version"))?;
    let source = config.get_item(intern!(py, "source_id"))?;
    if config.get_item(limits_key)?.is_some_and(|limits| limits.is_empty().unwrap_or(false)) {
        config.del_item(limits_key)?;
    }
    let world_staff = attribute(world, intern!(py, "staff"))?;
    let mut ids: Vec<Bound<'py, PyAny>> = exact_dict(&world_staff)?.keys().iter().collect();
    for id in &ids {
        exact_str(id)?;
    }
    ids.sort_unstable_by(|a, b| {
        let (first, second) = (exact_str(a).unwrap_or(""), exact_str(b).unwrap_or(""));
        first.as_bytes().cmp(second.as_bytes())
    });
    let staff = PyDict::new(py);
    let record = StaffRecord::new();
    for id in ids {
        staff.set_item(id, Field::to_py(&record, py)?)?;
    }
    let state = PyDict::new(py);
    state.set_item(intern!(py, "schema_version"), 1_i64)?;
    state.set_item(intern!(py, "policy_version"), policy)?;
    state.set_item(intern!(py, "source_id"), source)?;
    state.set_item(intern!(py, "config"), config)?;
    state.set_item(intern!(py, "staff"), staff)?;
    Ok(state)
}

/// The fatigue formula alone, for the bit comparison of the tests.
#[pyfunction]
fn fatigue_value(saturating: bool, consecutive: i64, maximum: i64) -> PyResult<f64> {
    fatigue_of(saturating, consecutive, maximum)
        .ok_or_else(|| crate::Unsupported::new_err("the formula's inputs are out of range"))
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(update, m)?)?;
    m.add_function(wrap_pyfunction!(on_shift_now, m)?)?;
    m.add_function(wrap_pyfunction!(is_available_now, m)?)?;
    m.add_function(wrap_pyfunction!(add_staff, m)?)?;
    m.add_function(wrap_pyfunction!(create_state, m)?)?;
    m.add_function(wrap_pyfunction!(fatigue_value, m)?)?;
    Ok(())
}
