//! `staff_dynamics_v1`: the row is the core's (`sheltersim_core::state::staff_dynamics`); here are
//! its Python side and its validator.

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyFloat, PyInt};
use pyo3::IntoPyObjectExt;

pub(crate) use sheltersim_core::state::staff_dynamics::*;

use crate::core::row::{load_row, namespace_is_absent, py_row, py_str_enum, PyField};
use crate::core::world_core::{mirror_of, Misfit, NamespaceMirror, OrMisfit, RowType};
use crate::state::entities::{entities_of, EntityStore};

py_str_enum!(LastActivity);
py_row! {
    StaffRecord {
        consecutive_work_minutes, total_work_minutes, total_rest_minutes, fatigue,
        rest_until_minute, last_activity,
    }
}
py_row! { Shift { start_minute, end_minute } }
py_row! {
    ConfigRow {
        schema_version, policy_version, source_id, max_continuous_work_minutes,
        max_continuous_work_minutes_by_staff, rest_minutes, shift_windows_by_staff,
        off_shift_fatigue_recovery_per_minute,
    }
}
py_row! { StaffDynamicsRow { schema_version, policy_version, source_id, config, staff } }

impl PyField for Recovery {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        if let Ok(number) = value.downcast_exact::<PyFloat>() {
            return Ok(Self::float(number.value()));
        }
        // bool is an int subclass, so the exact check turns it down too.
        let number: i64 = value.downcast_exact::<PyInt>().map_err(|_| Misfit::new("not a float or an int"))?.extract().or_misfit()?;
        if number.unsigned_abs() > 1 << 53 {
            return Err(Misfit::new("an int a float can't hold"));
        }
        Ok(Self::int(number))
    }

    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        if self.is_int() {
            (self.value as i64).into_bound_py_any(py)
        } else {
            Ok(PyFloat::new(py, self.value).into_any())
        }
    }
}

/// `validate_staff_dynamics_state`: accepts, or leaves it to its Python checks and their error text.
#[pyfunction]
fn validate(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = world.py();
    if namespace_is_absent(world, intern!(py, NAME))? {
        return Ok(true);
    }
    let mirror = mirror_of(world, NAME)?;
    let store = entities_of(world)?;
    NamespaceMirror::with_row::<StaffDynamicsRow, _>(&mirror, |row| {
        if !row.is_valid_without_staff_ids() {
            return Ok(false);
        }
        EntityStore::with_rows(&store, |rows| {
            let known = |id: &std::sync::Arc<str>| rows.staff.get(id).is_some();
            Ok(row.staff.len() == rows.staff.len()
                && row.staff.keys().all(known)
                && row
                    .config
                    .max_continuous_work_minutes_by_staff
                    .0
                    .as_ref()
                    .is_none_or(|limits| limits.keys().all(known))
                && row
                    .config
                    .shift_windows_by_staff
                    .0
                    .as_ref()
                    .is_none_or(|windows| windows.keys().all(known)))
        })
    })
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("ROW", Py::new(py, RowType::new("staff_dynamics", load_row::<StaffDynamicsRow>))?)?;
    m.add_function(wrap_pyfunction!(validate, m)?)?;
    Ok(())
}
