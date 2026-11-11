//! The glue of the core's `person_context` (lane P8 a1): `PersonContext.model_validate` and
//! `_fields_of_person` of a document, for the shadow and the tests. The view parses the contexts
//! in the core.

use std::sync::Arc;

use indexmap::IndexMap;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString, PyTuple};
use sheltersim_core::rules::person_context;

use crate::core::errors::OrPy;
use crate::core::tracked::{as_readable_dict, as_readable_list};
use crate::core::value::{VPy, V};
use crate::Unsupported;

/// A document as a `V`, a tuple as a list (the contract takes either for a tuple field); a value
/// that is not JSON is refused.
fn document_of(value: &Bound<'_, PyAny>) -> PyResult<V> {
    if let Ok(items) = value.downcast_exact::<PyTuple>() {
        return Ok(V::List(items.iter().map(|item| document_of(&item)).collect::<PyResult<_>>()?));
    }
    if let Some(items) = as_readable_list(value) {
        return Ok(V::List(items.iter().map(|item| document_of(&item)).collect::<PyResult<_>>()?));
    }
    if let Some(record) = as_readable_dict(value) {
        let mut map = IndexMap::with_capacity(record.len());
        for (key, item) in record.iter() {
            let key = key.downcast_exact::<PyString>().map_err(|_| Unsupported::new_err("object key is not a str"))?;
            map.insert(Arc::from(key.to_str()?), document_of(&item)?);
        }
        return Ok(V::Map(map));
    }
    <V as VPy>::from_py(value)
}

/// The fields as `_fields_of_person` gives them.
#[pyfunction]
fn fields_of<'py>(raw: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyDict>> {
    let py = raw.py();
    let fields = person_context::parse(&document_of(raw)?).or_py()?;
    let texts = |items: &[Arc<str>]| PyTuple::new(py, items.iter().map(|item| &**item));
    let result = PyDict::new(py);
    result.set_item("caregiver_ids", texts(&fields.caregiver_ids)?)?;
    result.set_item("care_for_ids", texts(&fields.care_for_ids)?)?;
    result.set_item("parent_ids", texts(&fields.parent_ids)?)?;
    result.set_item("roles", texts(&fields.roles)?)?;
    result.set_item("usual_wake_minute", fields.usual_wake_minute)?;
    result.set_item("usual_sleep_minute", fields.usual_sleep_minute)?;
    result.set_item("required_sleep_minutes", fields.required_sleep_minutes)?;
    result.set_item("personal_context_present", fields.personal_context_present)?;
    Ok(result)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(fields_of, m)?)?;
    Ok(())
}
