//! The world's scalars, `world.resources` and `world.id_counters` as the core holds them (Issue
//! #152, PR 5-2): `Scalars`, and the rows of the two owned fields (`OWNED_FIELDS` and
//! `WHOLE_FIELDS` in `core/state_mirror.py`).
//!
//! `WorldState.__setattr__` writes each scalar here (`WorldCore.set_scalar`); a Python value of
//! another type is a `Fit::Misfit`, which a kernel reading it says Unsupported for. The kernels
//! still read the scalars as attributes; their typed accessors are `World`'s.
//!
//! `resources` is an `IdMap<i64>` and `id_counters` a `ValueRow` (a counter may be past i64). Both
//! are trees of `TrackedDict` that forward Python's writes; a Rust rule writes the row, then the
//! tree (`set_id_counter`, `replace_resources`).

use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyInt, PyString};
use sheltersim_core::row::Atom;
use sheltersim_core::world::{self, Fit, ResourcesRow, Scalars, Seed};

use crate::core::errors::OrPy;
use crate::core::world_core::{field_mirror_of, NamespaceMirror, RowType, ValueRow};
use crate::core::row::TreeSink;
use crate::Unsupported;

/// The text of `EventWriter::of` for counters it can't read, as when it took only a plain dict.
pub(crate) const NOT_PLAIN: &str = "id_counters is not a plain dict";

fn exact_i64(value: &Bound<'_, PyAny>) -> Fit<i64> {
    match value.downcast_exact::<PyInt>().map(|number| number.extract::<i64>()) {
        Ok(Ok(number)) => Fit::Ok(number),
        _ => Fit::Misfit,
    }
}

/// Store one scalar of `SCALAR_FIELDS`, as `WorldState.__setattr__` stores it in the object.
pub(crate) fn set_scalar(scalars: &mut Scalars, name: &str, value: &Bound<'_, PyAny>) -> PyResult<()> {
    match name {
        "scenario_id" => {
            scalars.scenario_id = match value.downcast_exact::<PyString>().map(|text| text.to_str()) {
                Ok(Ok(text)) => Fit::Ok(Arc::from(text)),
                _ => Fit::Misfit,
            };
        }
        "seed" => {
            // A bool's exact type is not int.
            scalars.seed = match value.downcast_exact::<PyInt>() {
                Ok(number) => match Seed::from_text(number.str()?.to_str()?) {
                    Some(seed) => Fit::Ok(seed),
                    None => Fit::Misfit,
                },
                Err(_) => Fit::Misfit,
            };
        }
        "duration_minutes" => scalars.duration_minutes = exact_i64(value),
        "minute_index" => scalars.minute_index = exact_i64(value),
        "schema_version" => scalars.schema_version = exact_i64(value),
        _ => return Err(pyo3::exceptions::PyValueError::new_err(format!("{name} is not a scalar of a world"))),
    }
    Ok(())
}

/// The scalars as Python values, a misfit as None: `{"seed": (text, small), ...}`, for the tests.
pub(crate) fn scalars_to_py<'py>(py: Python<'py>, scalars: &Scalars) -> PyResult<Bound<'py, PyDict>> {
    let document = PyDict::new(py);
    document.set_item("scenario_id", scalars.scenario_id.ok().map(|text| &**text))?;
    document.set_item("seed", scalars.seed.ok().map(|seed| (&*seed.text, seed.small)))?;
    document.set_item("duration_minutes", scalars.duration_minutes.ok())?;
    document.set_item("minute_index", scalars.minute_index.ok())?;
    document.set_item("schema_version", scalars.schema_version.ok())?;
    Ok(document)
}

/// The mirror of `world.id_counters` while the field holds its tree, or None.
fn counters_mirror<'py>(world: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, NamespaceMirror>>> {
    match field_mirror_of(world, intern!(world.py(), world::ID_COUNTERS)) {
        Ok(mirror) => Ok(Some(mirror)),
        Err(error) if error.is_instance_of::<Unsupported>(world.py()) => Ok(None),
        Err(error) => Err(error),
    }
}

/// The mirror of `world.id_counters` with a row, for `EventWriter::of`: Unsupported with the
/// writer's old text for anything else.
pub(crate) fn owned_counters<'py>(world: &Bound<'py, PyAny>) -> PyResult<Bound<'py, NamespaceMirror>> {
    let mirror = counters_mirror(world)?.ok_or_else(|| Unsupported::new_err(NOT_PLAIN))?;
    NamespaceMirror::with_value(&mirror, |_| Ok(())).map_err(|_| Unsupported::new_err(NOT_PLAIN))?;
    Ok(mirror)
}

/// `counters.get(name)` from the row: None without the key.
pub(crate) fn id_counter(mirror: &Bound<'_, NamespaceMirror>, name: &str) -> PyResult<Option<i64>> {
    NamespaceMirror::with_row::<ValueRow, _>(mirror, |row| world::id_counter(row, name).or_py())
}

/// `counters[name] = value` from Rust: the row, then the tree, which forwards nothing. A tainted
/// field has only its tree, which takes the value as a plain dict would.
pub(crate) fn write_id_counter(mirror: &Bound<'_, NamespaceMirror>, name: &str, value: i64) -> PyResult<()> {
    let py = mirror.py();
    let written = NamespaceMirror::with_row_tree::<ValueRow, _>(mirror, |row, tree| {
        world::set_id_counter(row, name, value).or_py()?;
        tree.set_entry(name, Atom::Int(value))
    });
    match written {
        Err(error) if error.is_instance_of::<Unsupported>(py) => {
            NamespaceMirror::write_through(mirror, &[], name, &value.into_pyobject(py)?.into_any())
        }
        other => other,
    }
}

/// `world.id_counters[name] = value` from a Rust rule. A world whose field holds no tree (stored
/// past `WorldState.__setattr__`) gets the plain dict write it always got.
pub(crate) fn set_id_counter(world: &Bound<'_, PyAny>, counters: &Bound<'_, PyDict>, name: &Bound<'_, PyString>, value: i64) -> PyResult<()> {
    match counters_mirror(world)? {
        Some(mirror) if mirror.get().root_is(counters.as_any()) => write_id_counter(&mirror, name.to_str()?, value),
        _ => counters.set_item(name, value),
    }
}

/// `world.resources = next`, a new dict, from a Rust rule: the field's new tree, and whoever holds
/// the old one keeps its old values. A world whose field holds no tree gets `next` as it is.
pub(crate) fn replace_resources(world: &Bound<'_, PyAny>, next: &Bound<'_, PyDict>) -> PyResult<()> {
    let py = world.py();
    let name = intern!(py, world::RESOURCES);
    let stored = match field_mirror_of(world, name) {
        Ok(mirror) => NamespaceMirror::replace_root(&mirror, next)?.into_bound(py),
        Err(error) if error.is_instance_of::<Unsupported>(py) => next.clone().into_any(),
        Err(error) => return Err(error),
    };
    crate::state::entities::raw_setattr(world, name, stored.as_ptr())
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("RESOURCES", Py::new(py, RowType::of::<ResourcesRow>(world::RESOURCES))?)?;
    m.add("ID_COUNTERS", Py::new(py, RowType::of::<ValueRow>(world::ID_COUNTERS))?)?;
    Ok(())
}
