//! Rust kernels for 避難所IF！ (docs/rust-port.md).
//!
//! `canonical_json` writes the bytes of
//! `json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False,
//! allow_nan=False).encode()` for plain JSON values: dict with str keys, list,
//! tuple, str, int, float, bool and None, and the tracked containers (`core/tracked.rs`). Anything else, and NaN or infinity,
//! raises `Unsupported`, so the Python caller falls back to the json module and
//! keeps its exact results and errors.

use pyo3::create_exception;
use pyo3::exceptions::PyException;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyBytes, PyFloat, PyInt, PyString, PyTuple};
use sha2::{Digest, Sha256};

use crate::core::errors::OrPy;
use crate::core::tracked::{as_readable_dict, as_readable_list};

mod checkpoint;
mod contacts;
mod core;
mod eventlog;
mod history;
mod records;
mod validation;

pub(crate) use sheltersim_core::json::{python_float_repr, write_json_string};

// The `state` and `rules` modules, found by build.rs.
include!(concat!(env!("OUT_DIR"), "/lanes.rs"));

mod exception {
    use super::*;
    create_exception!(sheltersim_native, Unsupported, PyException);
}

/// The exception of a kernel that leaves a value to Python. A type of its own so that building one
/// says where (`core/census.rs`), at every site, without each site saying so.
pub(crate) struct Unsupported;

impl Unsupported {
    #[track_caller]
    #[inline]
    pub(crate) fn new_err<A>(reason: A) -> PyErr
    where
        A: AsRef<str> + pyo3::PyErrArguments + Send + Sync + 'static,
    {
        core::census::raised(reason.as_ref());
        exception::Unsupported::new_err(reason)
    }

    /// The exception of a refusal the core counted when it built it (`sheltersim_core::error`).
    pub(crate) fn counted(reason: String) -> PyErr {
        exception::Unsupported::new_err(reason)
    }
}

// SAFETY: the type object is that of the exception above, which is what this type stands for.
unsafe impl pyo3::PyTypeInfo for Unsupported {
    const NAME: &'static str = "Unsupported";
    const MODULE: Option<&'static str> = Some("sheltersim_native");

    fn type_object_raw(py: Python<'_>) -> *mut pyo3::ffi::PyTypeObject {
        exception::Unsupported::type_object_raw(py)
    }
}

// Deeper values are left to the json module, which raises RecursionError itself.
pub(crate) const MAX_DEPTH: usize = 512;

pub(crate) use sheltersim_core::json::Encoder;

/// The canonical JSON of Python values, written through the core's `Encoder`.
pub(crate) trait PyEncoder {
    /// A plain JSON value of Python: dict with str keys, list, tuple, str, int, float, bool and
    /// None, and the tracked containers. Anything else raises `Unsupported`.
    fn value(&mut self, value: &Bound<'_, PyAny>, depth: usize) -> PyResult<()>;

    /// A finite float; NaN and infinity raise `Unsupported`.
    fn float(&mut self, value: f64) -> PyResult<()>;
}

impl PyEncoder for Encoder {
    fn value(&mut self, value: &Bound<'_, PyAny>, depth: usize) -> PyResult<()> {
        if depth > MAX_DEPTH {
            return Err(Unsupported::new_err("value nests too deeply"));
        }
        if value.is_none() {
            self.null();
        } else if let Ok(text) = value.downcast::<PyString>() {
            self.string(text.to_str()?);
        } else if let Ok(flag) = value.downcast::<PyBool>() {
            self.bool(flag.is_true());
        } else if let Ok(number) = value.downcast::<PyInt>() {
            match number.extract::<i64>() {
                Ok(small) => self.int(small),
                // json writes int.__repr__ of any other int.
                Err(_) => {
                    let text = py_int_repr(number)?;
                    self.out.extend_from_slice(text.as_bytes());
                }
            }
        } else if let Ok(number) = value.downcast::<PyFloat>() {
            self.float(number.value())?;
        } else if let Some(object) = as_readable_dict(value) {
            let mut entries: Vec<(Bound<'_, PyAny>, Bound<'_, PyAny>)> =
                Vec::with_capacity(object.len());
            for (key, item) in object.iter() {
                entries.push((key, item));
            }
            let mut items: Vec<(&str, &Bound<'_, PyAny>)> = Vec::with_capacity(entries.len());
            for (key, item) in &entries {
                let key = key
                    .downcast::<PyString>()
                    .map_err(|_| Unsupported::new_err("object key is not a str"))?;
                items.push((key.to_str()?, item));
            }
            // Python orders str by code point, the same as UTF-8 bytes.
            items.sort_unstable_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
            self.out.push(b'{');
            for (index, (key, item)) in items.iter().enumerate() {
                if index > 0 {
                    self.out.push(b',');
                }
                self.string(key);
                self.out.push(b':');
                self.value(item, depth + 1)?;
            }
            self.out.push(b'}');
        } else if let Some(array) = as_readable_list(value) {
            self.out.push(b'[');
            for (index, item) in array.iter().enumerate() {
                if index > 0 {
                    self.out.push(b',');
                }
                self.value(&item, depth + 1)?;
            }
            self.out.push(b']');
        } else if let Ok(array) = value.downcast::<PyTuple>() {
            self.out.push(b'[');
            for (index, item) in array.iter().enumerate() {
                if index > 0 {
                    self.out.push(b',');
                }
                self.value(&item, depth + 1)?;
            }
            self.out.push(b']');
        } else {
            return Err(Unsupported::new_err("value is not a plain JSON value"));
        }
        Ok(())
    }

    #[inline]
    fn float(&mut self, value: f64) -> PyResult<()> {
        self.try_float(value).or_py()
    }
}

fn py_int_repr(number: &Bound<'_, PyInt>) -> PyResult<String> {
    let int_type = number.py().get_type::<PyInt>();
    int_type
        .getattr("__repr__")?
        .call1((number,))?
        .extract::<String>()
}

pub(crate) fn encode(value: &Bound<'_, PyAny>) -> PyResult<Vec<u8>> {
    let mut encoder = Encoder {
        out: Vec::with_capacity(256),
    };
    encoder.value(value, 0)?;
    Ok(encoder.out)
}

/// The canonical JSON bytes of a plain JSON value.
#[pyfunction]
fn canonical_json<'py>(py: Python<'py>, value: &Bound<'py, PyAny>) -> PyResult<Bound<'py, PyBytes>> {
    let bytes = encode(value)?;
    Ok(PyBytes::new(py, &bytes))
}

/// The SHA-256 hex digest of canonical_json(value).
#[pyfunction]
fn canonical_sha256(value: &Bound<'_, PyAny>) -> PyResult<String> {
    let bytes = encode(value)?;
    let digest = Sha256::digest(&bytes);
    Ok(digest.iter().map(|byte| format!("{byte:02x}")).collect())
}

/// float.__repr__ of value, for tests.
#[pyfunction]
fn float_repr(value: f64) -> String {
    python_float_repr(value)
}

/// The JSON of a float as the Encoder writes it, for tests.
#[pyfunction]
fn float_json(value: f64) -> PyResult<String> {
    let mut encoder = Encoder { out: Vec::with_capacity(32) };
    encoder.float(value)?;
    Ok(String::from_utf8(encoder.out).expect("a float is ASCII"))
}

#[pymodule]
fn sheltersim_native(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add("Unsupported", module.py().get_type::<Unsupported>())?;
    module.add_function(wrap_pyfunction!(core::census::census_switch, module)?)?;
    module.add_function(wrap_pyfunction!(core::census::census_caught, module)?)?;
    module.add_function(wrap_pyfunction!(core::census::census_take, module)?)?;
    module.add_function(wrap_pyfunction!(core::census::census_take_errors, module)?)?;
    module.add_function(wrap_pyfunction!(core::configs::watch_config_writes, module)?)?;
    module.add_function(wrap_pyfunction!(core::configs::config_writes_in_tick, module)?)?;
    module.add_function(wrap_pyfunction!(rules::movement_geometry::stale_geometry_hits, module)?)?;
    module.add_function(wrap_pyfunction!(core::configs::config_section, module)?)?;
    module.add_function(wrap_pyfunction!(core::configs::config_parses, module)?)?;
    module.add_function(wrap_pyfunction!(core::parallel::parallel_state, module)?)?;
    module.add_function(wrap_pyfunction!(core::parallel::set_people_per_helper, module)?)?;
    module.add_function(wrap_pyfunction!(core::tracked::install_tracked_types, module)?)?;
    module.add_function(wrap_pyfunction!(core::tracked::install_id_map_type, module)?)?;
    module.add_function(wrap_pyfunction!(core::tracked::install_detached_types, module)?)?;
    module.add_class::<core::world_core::WorldCore>()?;
    module.add_class::<core::world_core::NamespaceMirror>()?;
    module.add_class::<core::world_core::RowType>()?;
    module.add_class::<core::run::RunCore>()?;
    register_lanes(module)?;
    module.add_function(wrap_pyfunction!(canonical_json, module)?)?;
    module.add_function(wrap_pyfunction!(canonical_sha256, module)?)?;
    module.add_function(wrap_pyfunction!(float_repr, module)?)?;
    module.add_function(wrap_pyfunction!(float_json, module)?)?;
    module.add_function(wrap_pyfunction!(records::scheduled_event_checked, module)?)?;
    module.add_function(wrap_pyfunction!(records::view_state_hash, module)?)?;
    module.add_function(wrap_pyfunction!(checkpoint::sensitive_candidates, module)?)?;
    module.add_function(wrap_pyfunction!(checkpoint::chunk_hashes, module)?)?;
    module.add_function(wrap_pyfunction!(checkpoint::content_chunk_starts, module)?)?;
    module.add_function(wrap_pyfunction!(checkpoint::same_saved_records, module)?)?;
    module.add_function(wrap_pyfunction!(checkpoint::index_row, module)?)?;
    module.add_function(wrap_pyfunction!(checkpoint::dumps_length, module)?)?;
    module.add_function(wrap_pyfunction!(checkpoint::record_json, module)?)?;
    module.add_function(wrap_pyfunction!(history::same_json_items, module)?)?;
    module.add_class::<history::HistoryPins>()?;
    module.add_function(wrap_pyfunction!(validation::movement_positions_valid, module)?)?;
    module.add_function(wrap_pyfunction!(validation::world_agents_valid, module)?)?;
    module.add_class::<validation::ValuesSnapshot>()?;
    module.add_function(wrap_pyfunction!(contacts::fsum, module)?)?;
    module.add_class::<eventlog::EventStore>()?;
    module.add_function(wrap_pyfunction!(eventlog::split_json_array, module)?)?;
    module.add_function(wrap_pyfunction!(eventlog::saved_record_fields, module)?)?;
    Ok(())
}
