//! `V`, the JSON value of `sheltersim_core::value`, under its old path, and what needs Python:
//! a `V` read from a Python value and made into one (`VPy`).

use std::sync::Arc;

use indexmap::IndexMap;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString};
use pyo3::IntoPyObjectExt;

pub(crate) use sheltersim_core::value::V;

use crate::core::tracked::{as_readable_dict, as_readable_list};
use crate::{Unsupported, MAX_DEPTH};

/// A `V` from and to a Python value.
pub(crate) trait VPy: Sized {
    /// Exact scalar types only: a str or int subclass, a tuple or a key that is not a str raises
    /// `Unsupported`, because it would not come back the same. So does a dict or list subclass
    /// other than the tracked containers (`core/tracked.rs`), which are their items.
    fn from_py(value: &Bound<'_, PyAny>) -> PyResult<Self>;

    /// New plain dicts and lists, in the order of the map.
    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>>;
}

impl VPy for V {
    fn from_py(value: &Bound<'_, PyAny>) -> PyResult<Self> {
        read(value, 0)
    }

    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        match self {
            V::Null => Ok(py.None().into_bound(py)),
            V::Bool(flag) => flag.into_bound_py_any(py),
            V::Int(number) => number.into_bound_py_any(py),
            V::Big(text) => py.get_type::<PyInt>().call1((&**text,)),
            V::Float(number) => number.into_bound_py_any(py),
            V::Str(text) => (&**text).into_bound_py_any(py),
            V::List(items) => {
                let list = PyList::empty(py);
                for item in items {
                    list.append(item.to_py(py)?)?;
                }
                Ok(list.into_any())
            }
            V::Map(map) => {
                let dict = PyDict::new(py);
                for (key, item) in map {
                    dict.set_item(&**key, item.to_py(py)?)?;
                }
                Ok(dict.into_any())
            }
        }
    }
}

fn read(value: &Bound<'_, PyAny>, depth: usize) -> PyResult<V> {
    if depth > MAX_DEPTH {
        return Err(Unsupported::new_err("value nests too deeply"));
    }
    if value.is_none() {
        return Ok(V::Null);
    }
    if let Ok(number) = value.downcast_exact::<PyFloat>() {
        return Ok(V::Float(number.value()));
    }
    if let Ok(text) = value.downcast_exact::<PyString>() {
        return Ok(V::Str(Arc::from(text.to_str()?)));
    }
    if let Ok(flag) = value.downcast_exact::<PyBool>() {
        return Ok(V::Bool(flag.is_true()));
    }
    if let Ok(number) = value.downcast_exact::<PyInt>() {
        return Ok(match number.extract::<i64>() {
            Ok(small) => V::Int(small),
            Err(_) => V::Big(number.repr()?.to_str()?.into()),
        });
    }
    if let Some(object) = as_readable_dict(value) {
        let mut map = IndexMap::with_capacity(object.len());
        for (key, item) in object.iter() {
            let key = key
                .downcast_exact::<PyString>()
                .map_err(|_| Unsupported::new_err("object key is not a str"))?;
            map.insert(Arc::from(key.to_str()?), read(&item, depth + 1)?);
        }
        return Ok(V::Map(map));
    }
    if let Some(array) = as_readable_list(value) {
        let mut items = Vec::with_capacity(array.len());
        for item in array.iter() {
            items.push(read(&item, depth + 1)?);
        }
        return Ok(V::List(items));
    }
    Err(Unsupported::new_err("value is not a plain JSON value"))
}
