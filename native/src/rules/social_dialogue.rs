//! `domain/social_dialogue.py`: the glue of `sheltersim_core::rules::social_dialogue` (lane P8 a2).
//! A context is read into a `V` here, from an event's frozen payload or the caller's mapping; a
//! value of no JSON type is Unsupported, a `KernelRefusal` in Python, as before.
//!
//! `virtual_dialogue_example` stays in Python: it runs when a history is read, not in a tick.

use std::sync::Arc;

use indexmap::IndexMap;
use pyo3::intern;
use pyo3::prelude::*;
use pyo3::sync::GILOnceCell;
use pyo3::types::{PyBool, PyFloat, PyInt, PyList, PyString, PyTuple, PyType};

use sheltersim_core::rules::social_dialogue as rules;

use crate::core::errors::OrPy;
use crate::core::row::{exact_int, exact_str, unsupported};
use crate::core::tracked::{as_readable_dict, as_readable_list};
use crate::core::value::{VPy, V};
use crate::core::world_core::with_world;

static MAPPING_PROXY: GILOnceCell<Py<PyType>> = GILOnceCell::new();

pub(crate) fn mapping_proxy(py: Python<'_>) -> PyResult<&Bound<'_, PyType>> {
    let proxy = MAPPING_PROXY.get_or_try_init(py, || {
        Ok::<_, PyErr>(py.import("types")?.getattr("MappingProxyType")?.downcast_into::<PyType>()?.unbind())
    })?;
    Ok(proxy.bind(py))
}

/// The fields of a payload a rule reads, as a map of those it has (`key in payload`), each value
/// read as `context_of` reads it: the rest of the payload, such as a conversation's context, is
/// not read. Unsupported for a payload that is not a mapping.
pub(crate) fn payload_fields(payload: &Bound<'_, PyAny>, keys: &[&'static str]) -> PyResult<V> {
    let py = payload.py();
    let mut fields = IndexMap::with_capacity(keys.len());
    if let Some(dict) = as_readable_dict(payload) {
        for key in keys {
            if let Some(value) = dict.get_item(PyString::intern(py, key))? {
                fields.insert(Arc::from(*key), context_of(&value, 1)?);
            }
        }
    } else if payload.get_type().is(mapping_proxy(py)?) {
        for key in keys {
            let key_object = PyString::intern(py, key);
            if payload.contains(&key_object)? {
                fields.insert(Arc::from(*key), context_of(&payload.get_item(&key_object)?, 1)?);
            }
        }
    } else {
        return unsupported("a payload cannot be read");
    }
    Ok(V::Map(fields))
}

/// A context or a payload's value as a `V`: a plain mapping (dict or mapping proxy) of str keys, a
/// tuple or a list, and the JSON scalars. Unsupported for a value of another type, whose answer is
/// its own.
fn context_of(value: &Bound<'_, PyAny>, depth: usize) -> PyResult<V> {
    const REFUSED: &str = "a value of another type is for the Python rule to judge";
    if depth > crate::MAX_DEPTH {
        return unsupported(REFUSED);
    }
    if value.is_none() {
        return Ok(V::Null);
    }
    if let Ok(flag) = value.downcast_exact::<PyBool>() {
        return Ok(V::Bool(flag.is_true()));
    }
    if value.is_exact_instance_of::<PyInt>() || value.is_exact_instance_of::<PyFloat>() || value.is_exact_instance_of::<PyString>() {
        return <V as VPy>::from_py(value);
    }
    let items: Option<Vec<Bound<'_, PyAny>>> = if let Ok(items) = value.downcast_exact::<PyTuple>() {
        Some(items.iter().collect())
    } else {
        as_readable_list(value).map(|items| items.iter().collect())
    };
    if let Some(items) = items {
        return items.iter().map(|item| context_of(item, depth + 1)).collect::<PyResult<Vec<V>>>().map(V::List);
    }
    let entries: Vec<(Bound<'_, PyAny>, Bound<'_, PyAny>)> = if let Some(dict) = as_readable_dict(value) {
        dict.iter().collect()
    } else if value.get_type().is(mapping_proxy(value.py())?) {
        let mut entries = Vec::new();
        for entry in value.call_method0(intern!(value.py(), "items"))?.try_iter()? {
            entries.push(entry?.extract()?);
        }
        entries
    } else {
        return unsupported(REFUSED);
    };
    let mut map = IndexMap::with_capacity(entries.len());
    for (key, item) in entries {
        let key = Arc::from(exact_str(&key)?);
        map.insert(key, context_of(&item, depth + 1)?);
    }
    Ok(V::Map(map))
}

/// The IDs of `set(participant_ids)`, exact strs in a tuple or a list, in their order.
fn id_set<'py>(participant_ids: &Bound<'py, PyAny>) -> PyResult<Vec<Bound<'py, PyString>>> {
    let items: Vec<Bound<'py, PyAny>> = if let Ok(ids) = participant_ids.downcast_exact::<PyTuple>() {
        ids.iter().collect()
    } else if let Ok(ids) = participant_ids.downcast_exact::<PyList>() {
        ids.iter().collect()
    } else {
        return unsupported("the participants are neither a tuple nor a list");
    };
    let mut ids: Vec<Bound<'py, PyString>> = Vec::with_capacity(items.len());
    for item in items {
        exact_str(&item)?;
        let id = item.downcast_into::<PyString>()?;
        let mut known = false;
        for other in &ids {
            known = known || other.to_str()? == id.to_str()?;
        }
        if !known {
            ids.push(id);
        }
    }
    Ok(ids)
}

/// `validate_social_state_context`.
#[pyfunction]
fn validate_context(context: &Bound<'_, PyAny>, participant_ids: &Bound<'_, PyAny>, due_minute: &Bound<'_, PyAny>) -> PyResult<()> {
    let ids = id_set(participant_ids)?;
    let ids = ids.iter().map(|id| id.to_str()).collect::<PyResult<Vec<&str>>>()?;
    let due_minute = exact_int(due_minute)?;
    let context = context_of(context, 0)?;
    rules::validate_context(&context, &ids, due_minute).or_py()
}

/// `capture_social_state_context`: one or two current participants' state, as JSON values.
#[pyfunction]
fn capture_context<'py>(world: &Bound<'py, PyAny>, participant_ids: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyAny>> {
    let ids = id_set(participant_ids)?;
    let ids = ids.iter().map(|id| id.to_str()).collect::<PyResult<Vec<&str>>>()?;
    let context = with_world(world, |cell| rules::capture_context(cell, &ids))?.or_py()?;
    context.to_py(world.py())
}

/// `capture_context` of two different people as the value of an event's payload.
pub(crate) fn context_value(world: &Bound<'_, PyAny>, first: &str, second: &str) -> PyResult<V> {
    with_world(world, |cell| rules::context_value(cell, first, second))?.or_py()
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(validate_context, m)?)?;
    m.add_function(wrap_pyfunction!(capture_context, m)?)?;
    Ok(())
}
