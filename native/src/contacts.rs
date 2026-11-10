//! What the per-tick contact models share lives in `sheltersim_core::contacts`; this file gives it
//! to the lanes under its old path and adds what needs Python: reading exact numbers, the pair key
//! as a Python result, and `fsum` for tests. Anything the Python code would reject, or any value of
//! another type, raises `Unsupported` so the caller runs that code instead.

use pyo3::prelude::*;
use pyo3::types::{PyFloat, PyInt};

pub(crate) use sheltersim_core::contacts::{
    key_head, nearby_buckets, quoted_ids, select_contacts, zone_members, Basis, Bucket,
};

use crate::core::errors::OrPy;
use crate::core::mathx::py_fsum;
use crate::Unsupported;

/// math.fsum of finite values, for tests.
#[pyfunction]
pub fn fsum(values: Vec<f64>) -> PyResult<f64> {
    py_fsum(&values).ok_or_else(|| Unsupported::new_err("values are not finite"))
}

pub(crate) fn exact_float(value: &Bound<'_, PyAny>) -> PyResult<f64> {
    value
        .downcast_exact::<PyFloat>()
        .map(|number| number.value())
        .map_err(|_| Unsupported::new_err("not a float"))
}

pub(crate) fn exact_int(value: &Bound<'_, PyAny>) -> PyResult<i64> {
    value
        .downcast_exact::<PyInt>()
        .map_err(|_| Unsupported::new_err("not an int"))?
        .extract()
        .map_err(|_| Unsupported::new_err("int out of range"))
}

/// `[first,second]`; Unsupported for bytes that are not UTF-8.
pub(crate) fn pair_key(first: &[u8], second: &[u8]) -> PyResult<String> {
    sheltersim_core::contacts::pair_key(first, second).or_py()
}
