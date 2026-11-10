//! The float operations of the model live in `sheltersim_core::mathx`; this file gives them to the
//! lanes under their old path and adds what needs Python: a `MathError` as Python's exception
//! with Python's text, and a `Num` read from or made into a Python number.
//! `native/clippy.toml` bans the `f64` methods that `sheltersim_core::mathx` replaces.

use pyo3::exceptions::{PyOverflowError, PyValueError, PyZeroDivisionError};
use pyo3::prelude::*;
use pyo3::types::{PyFloat, PyInt};
use pyo3::IntoPyObjectExt;

pub(crate) use sheltersim_core::mathx::*;

/// The exception CPython raised where `exp`, `expm1` or `pow` returned this error.
pub(crate) fn math_error(error: MathError) -> PyErr {
    let text = error.text();
    match error {
        MathError::Range | MathError::PowerRange => PyOverflowError::new_err(text),
        MathError::ZeroToNegative => PyZeroDivisionError::new_err(text),
        MathError::NegativeToFraction => PyValueError::new_err(text),
    }
}

/// A `Num` from and to a Python number.
pub(crate) trait PyNum: Sized {
    /// An exact int that fits i64 or an exact float; anything else is left to Python.
    fn from_py(value: &Bound<'_, PyAny>) -> Option<Self>;
    fn to_py<'py>(self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>>;
}

impl PyNum for Num {
    fn from_py(value: &Bound<'_, PyAny>) -> Option<Self> {
        if let Ok(number) = value.downcast_exact::<PyFloat>() {
            return Some(Num::Float(number.value()));
        }
        value
            .downcast_exact::<PyInt>()
            .ok()?
            .extract()
            .ok()
            .map(Num::Int)
    }

    fn to_py<'py>(self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        match self {
            Num::Int(value) => value.into_bound_py_any(py),
            Num::Float(value) => value.into_bound_py_any(py),
        }
    }
}
