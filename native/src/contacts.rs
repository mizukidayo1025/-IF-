//! What the per-tick contact models share lives in `sheltersim_core::contacts`; this file adds what
//! needs Python: reading an exact int, and `fsum` for tests. Any value of another type raises
//! `Unsupported`.

use pyo3::prelude::*;
use pyo3::types::PyInt;

use crate::core::mathx::py_fsum;
use crate::Unsupported;

/// math.fsum of finite values, for tests.
#[pyfunction]
pub fn fsum(values: Vec<f64>) -> PyResult<f64> {
    py_fsum(&values).ok_or_else(|| Unsupported::new_err("values are not finite"))
}

pub(crate) fn exact_int(value: &Bound<'_, PyAny>) -> PyResult<i64> {
    value
        .downcast_exact::<PyInt>()
        .map_err(|_| Unsupported::new_err("not an int"))?
        .extract()
        .map_err(|_| Unsupported::new_err("int out of range"))
}
