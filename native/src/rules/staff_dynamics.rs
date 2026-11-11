//! `domain/staff_dynamics.py`: the entries of its kernels, whose rules are the core's
//! (`sheltersim_core::rules::staff_dynamics`, P8b-1). Each reads what Python holds (whether the
//! namespace is there, the fatigue formula of the configs), calls the core on the world and converts
//! what it returns. What the core refuses is a KernelRefusal at the Python entry.
//!
//! Kept here: `canonical_staff_dynamics_config` and the state of a world built from a Scenario
//! (decision 4: building a world stays out of the core) (docs/rust-port/lanes/s-staff-dynamics.md).

use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyList, PyString};
use sheltersim_core::config::staff::StaffFatigue;
use sheltersim_core::rules::staff_dynamics as core;

use crate::core::configs;
use crate::core::errors::{self, OrPy};
use crate::core::row::{attribute, exact_dict, exact_int, exact_list, exact_str, namespace_is_absent, unsupported, PyField};
use crate::core::world_core::with_world;
use crate::state::staff_dynamics::{shifts_are_valid, Shift, StaffRecord, MAX_REST_MINUTES, MAX_SHIFTS, MINUTES_PER_DAY, MINUTES_PER_WEEK, NAME, POLICY_V1, POLICY_V2};

const SOURCE_V1: &str = "ASM-STAFF-DYNAMICS-V1";
const SOURCE_V2: &str = "ASM-STAFF-DYNAMICS-V2";
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

/// `create_staff_fatigue_formula`, for the formulas the core knows, kept for the configs' stamp
/// (`core/configs.rs`). A later registered one, or anything but plain documents, is refused.
pub(crate) fn fatigue_formula(world: &Bound<'_, PyAny>) -> PyResult<Arc<StaffFatigue>> {
    configs::kept_value(world, |configs| &configs.staff_fatigue, StaffFatigue::parse)
}

// ---- The entries -----------------------------------------------------------------------------------

/// `update_staff_dynamics`.
#[pyfunction]
pub(crate) fn update(world: &Bound<'_, PyAny>, minute: &Bound<'_, PyAny>) -> PyResult<()> {
    if namespace_is_absent(world, intern!(world.py(), NAME))? {
        return Ok(());
    }
    let minute = exact_int(minute)?;
    let formula = fatigue_formula(world)?;
    with_world(world, |world| core::update(world, minute, &formula))?.or_py()
}

/// `staff_dynamics_on_shift`.
#[pyfunction(name = "on_shift")]
fn on_shift_now(world: &Bound<'_, PyAny>, staff_id: &Bound<'_, PyAny>, minute: &Bound<'_, PyAny>) -> PyResult<bool> {
    if namespace_is_absent(world, intern!(world.py(), NAME))? {
        return Ok(true);
    }
    let id = exact_str(staff_id)?;
    let minute = exact_int(minute)?;
    with_world(world, |world| core::on_shift_now(world, id, minute))?.or_py()
}

/// `staff_dynamics_is_available`.
#[pyfunction(name = "is_available")]
pub(crate) fn is_available_now(world: &Bound<'_, PyAny>, staff_id: &Bound<'_, PyAny>, minute: &Bound<'_, PyAny>) -> PyResult<bool> {
    if namespace_is_absent(world, intern!(world.py(), NAME))? {
        return Ok(true);
    }
    if staff_id.downcast_exact::<PyString>().is_err() {
        return unsupported("the id is not a str");
    }
    let id = exact_str(staff_id)?;
    let minute = exact_int(minute)?;
    let absent = namespace_is_absent(world, intern!(world.py(), "staff_operations_v1"))?;
    with_world(world, |world| core::is_available_now(world, id, minute, absent))?.or_py()
}

/// `add_staff`: one record.
#[pyfunction]
fn add_staff(world: &Bound<'_, PyAny>, staff_id: &Bound<'_, PyAny>) -> PyResult<()> {
    if namespace_is_absent(world, intern!(world.py(), NAME))? {
        return Ok(());
    }
    let id = exact_str(staff_id)?;
    with_world(world, |world| core::add_staff(world, id))?.or_py()
}

// ---- What a new world starts from (decision 4) -----------------------------------------------------

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
        staff.set_item(id, PyField::to_py(&record, py)?)?;
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
    core::fatigue(saturating, consecutive, maximum).ok_or_else(|| crate::Unsupported::new_err("the formula's inputs are out of range"))
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
