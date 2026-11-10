//! Typed rows: the core's `Field`, `state_row!` and `str_enum!` (`sheltersim_core::row`) under
//! their old path, and what needs Python: `PyField`, a row read from a Python value, made into one,
//! and changed by a forwarded write (docs/rust-port/lanes/p5-template-namespace.md).
//!
//! A struct of the core's `state_row!` gets its `PyField` from `py_row!`, which lists the same
//! fields in the same order (a compile-time check says so):
//!
//! ```ignore
//! py_row! { AgentRecord { level, visits } }
//! ```
//!
//! Here, `state_row!` and `str_enum!` are the core's macro and its companion together, for a row
//! of this crate. `py_row!` writes, for the struct:
//! - `from_py`: the fields and nothing else, each of its exact type; anything else is a `Misfit`,
//!   which taints the namespace.
//! - `to_py`: a new plain dict, keys in the order they are listed. List them in the tree's order.
//! - `apply`: a forwarded write. One to a field reads that field again, and one below a field goes
//!   to the field's own `apply`. Deleting a field, or writing a field the struct does not have,
//!   is a `Misfit`.
//!
//! Also here, the `exact_*` readers of a kernel's inputs, which say Unsupported for anything but
//! the one type the Python rule's result is certain for.

use std::sync::Arc;

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBool, PyDict, PyFloat, PyInt, PyList, PyString};
use pyo3::IntoPyObjectExt;
use rustc_hash::FxBuildHasher;

pub(crate) use sheltersim_core::row::{
    map_set, same_names, Atom, Field, IdMap, Items, Maybe, Part, Scalar, StrEnum, TreeSink,
    ABSENT_KEY,
};

use super::tracked::{as_readable_dict, as_readable_list};
use super::value::{V, VPy};
use super::world_core::{apply_to_value, emptied_after, Misfit, OrMisfit, Write};
use crate::Unsupported;

/// What a value of a row does with Python.
pub(crate) trait PyField: Field {
    /// The value, if it is exactly of this type. Runs no Python code.
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit>;
    /// A new plain value for the tree.
    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>>;
    /// A forwarded write inside this value. A scalar has no inside.
    fn apply(&mut self, _path: &[Part<'_>], _write: &Write<'_, '_>) -> Result<(), Misfit> {
        Err(Misfit::new("the write has no field in the row"))
    }
    /// What `to_py(...).get(key)` is, for a reader of one part of a detached namespace
    /// (`NamespaceMirror.document_part`): None for a key the value does not have, or a value that
    /// is not a dict. A `state_row!` record makes only that field; anything else makes the whole.
    fn part_to_py<'py>(&self, py: Python<'py>, key: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
        let whole = self.to_py(py)?;
        match whole.downcast::<PyDict>() {
            Ok(record) => record.get_item(key),
            Err(_) => Ok(None),
        }
    }
    /// `Field::items`, with the items made in Python. Some exactly when `items` is.
    fn py_items(&self) -> Option<&dyn PyItems> {
        None
    }
    /// `Field::part_items`, with the items made in Python. Some exactly when `part_items` is.
    fn part_py_items(&self, _key: &str) -> Option<&dyn PyItems> {
        None
    }
    /// `PyRow::to_py_emptied`. A `state_row!` record makes no item of an emptied list.
    fn to_py_emptied<'py>(&self, py: Python<'py>, emptied: &[&str]) -> PyResult<Bound<'py, PyAny>> {
        emptied_after(self.to_py(py)?, emptied)
    }
}

/// `Items` made in Python.
pub(crate) trait PyItems: Items {
    /// New plain values of `items[start..stop]`, the same as the list's `to_py` holds there.
    fn slice_to_py<'py>(&self, py: Python<'py>, start: usize, stop: usize) -> PyResult<Bound<'py, PyList>>;
}

/// The loader of a namespace whose row is this type: `RowType::new("name", load_row::<ThatType>)`.
pub(crate) fn load_row<T: PyField>(tree: &Bound<'_, PyAny>) -> Result<Box<dyn super::world_core::PyRow>, Misfit> {
    Ok(Box::new(T::from_py(tree)?))
}

/// A scalar as the tree stores it.
pub(crate) fn atom_to_py<'py>(py: Python<'py>, atom: Atom<'_>) -> PyResult<Bound<'py, PyAny>> {
    match atom {
        Atom::Null => Ok(py.None().into_bound(py)),
        Atom::Bool(flag) => flag.into_bound_py_any(py),
        Atom::Int(number) => number.into_bound_py_any(py),
        Atom::Float(number) => Ok(PyFloat::new(py, number).into_any()),
        Atom::Str(text) => Ok(PyString::new(py, text).into_any()),
        Atom::Name(text) => Ok(PyString::intern(py, text).into_any()),
    }
}

impl PyField for f64 {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        Ok(value
            .downcast_exact::<PyFloat>()
            .map_err(|_| Misfit::new("not a float"))?
            .value())
    }

    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        Ok(PyFloat::new(py, *self).into_any())
    }
}

impl PyField for i64 {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        // bool is an int subclass, so the exact check turns it down too.
        Ok(value
            .downcast_exact::<PyInt>()
            .map_err(|_| Misfit::new("not an int"))?
            .extract().or_misfit()?)
    }

    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.into_bound_py_any(py)
    }
}

impl PyField for bool {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        Ok(value
            .downcast_exact::<PyBool>()
            .map_err(|_| Misfit::new("not a bool"))?
            .is_true())
    }

    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.into_bound_py_any(py)
    }
}

impl PyField for Arc<str> {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        Ok(Arc::from(
            value
                .downcast_exact::<PyString>()
                .map_err(|_| Misfit::new("not a str"))?
                .to_str().or_misfit()?,
        ))
    }

    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        Ok(PyString::new(py, self).into_any())
    }
}

impl<T: PyField> PyField for Option<T> {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        if value.is_none() {
            return Ok(None);
        }
        Ok(Some(T::from_py(value)?))
    }

    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        match self {
            Some(value) => value.to_py(py),
            None => Ok(py.None().into_bound(py)),
        }
    }

    fn apply(&mut self, path: &[Part<'_>], write: &Write<'_, '_>) -> Result<(), Misfit> {
        match self {
            Some(value) => value.apply(path, write),
            None => Err(Misfit::new("the write is inside a None")),
        }
    }

    fn py_items(&self) -> Option<&dyn PyItems> {
        self.as_ref().and_then(PyField::py_items)
    }
}

/// The free-form part of a row: takes any JSON, and any write that fits it.
impl PyField for V {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        Ok(<V as VPy>::from_py(value).or_misfit()?)
    }

    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        VPy::to_py(self, py)
    }

    fn apply(&mut self, path: &[Part<'_>], write: &Write<'_, '_>) -> Result<(), Misfit> {
        apply_to_value(self, path, write)
    }

    fn py_items(&self) -> Option<&dyn PyItems> {
        match self {
            V::List(items) => Some(items as &dyn PyItems),
            _ => None,
        }
    }

    fn part_py_items(&self, key: &str) -> Option<&dyn PyItems> {
        match self {
            V::Map(map) => map.get(key).and_then(PyField::py_items),
            _ => None,
        }
    }
}

impl<T: PyField> PyField for Maybe<T> {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        Ok(Self(Some(T::from_py(value)?)))
    }

    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        match &self.0 {
            Some(value) => value.to_py(py),
            None => Err(PyValueError::new_err(ABSENT_KEY)),
        }
    }

    fn apply(&mut self, path: &[Part<'_>], write: &Write<'_, '_>) -> Result<(), Misfit> {
        match &mut self.0 {
            Some(value) => value.apply(path, write),
            None => Err(Misfit::new("the write is inside an absent key")),
        }
    }

    fn py_items(&self) -> Option<&dyn PyItems> {
        self.0.as_ref().and_then(PyField::py_items)
    }
}

impl<T: PyField> PyField for IdMap<T> {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        let dict = as_readable_dict(value).ok_or_else(|| Misfit::new("not a dict"))?;
        let mut map = IdMap::with_capacity_and_hasher(dict.len(), FxBuildHasher);
        for (key, item) in dict.iter() {
            let key = key
                .downcast_exact::<PyString>()
                .map_err(|_| Misfit::new("not a str key"))?;
            map.insert(Arc::from(key.to_str().or_misfit()?), T::from_py(&item)?);
        }
        Ok(map)
    }

    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let dict = PyDict::new(py);
        for (key, value) in self {
            dict.set_item(&**key, value.to_py(py)?)?;
        }
        Ok(dict.into_any())
    }

    fn apply(&mut self, path: &[Part<'_>], write: &Write<'_, '_>) -> Result<(), Misfit> {
        match (path, write) {
            ([], Write::Set { key: Part::Key(key), value }) => {
                let value = T::from_py(value)?;
                match self.get_mut(*key) {
                    Some(slot) => *slot = value,
                    None => {
                        self.insert(Arc::from(*key), value);
                    }
                }
                Ok(())
            }
            ([], Write::Delete { key: Part::Key(key) }) => {
                self.shift_remove(*key)
                    .map(|_| ())
                    .ok_or_else(|| Misfit::new(format!("no key {key:?}")))
            }
            ([], Write::Clear) => {
                self.clear();
                Ok(())
            }
            ([Part::Key(key), rest @ ..], write) => self
                .get_mut(*key)
                .ok_or_else(|| Misfit::new(format!("no key {key:?}")))?
                .apply(rest, write),
            _ => Err(Misfit::new("the write does not fit a map")),
        }
    }
}

impl<T: PyField> PyItems for Vec<T> {
    fn slice_to_py<'py>(&self, py: Python<'py>, start: usize, stop: usize) -> PyResult<Bound<'py, PyList>> {
        let list = PyList::empty(py);
        for item in &self[start..stop] {
            list.append(item.to_py(py)?)?;
        }
        Ok(list)
    }
}

impl<T: PyField> PyField for Vec<T> {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        let list = as_readable_list(value).ok_or_else(|| Misfit::new("not a list"))?;
        list.iter().map(|item| T::from_py(&item)).collect()
    }

    fn py_items(&self) -> Option<&dyn PyItems> {
        Some(self)
    }

    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let list = PyList::empty(py);
        for item in self {
            list.append(item.to_py(py)?)?;
        }
        Ok(list.into_any())
    }

    fn apply(&mut self, path: &[Part<'_>], write: &Write<'_, '_>) -> Result<(), Misfit> {
        match (path, write) {
            ([], Write::Set { key: Part::Index(index), value }) if *index < self.len() => {
                self[*index] = T::from_py(value)?;
            }
            ([], Write::Delete { key: Part::Index(index) }) if *index < self.len() => {
                self.remove(*index);
            }
            ([], Write::DeleteRange { start, stop }) if start <= stop && *stop <= self.len() => {
                self.drain(*start..*stop);
            }
            ([], Write::Clear) => self.clear(),
            ([], Write::Append { value }) => self.push(T::from_py(value)?),
            ([], Write::Extend { values }) => {
                for value in values.iter() {
                    self.push(T::from_py(&value)?);
                }
            }
            ([], Write::Insert { index, value }) if *index <= self.len() => {
                self.insert(*index, T::from_py(value)?);
            }
            ([], Write::Reverse) => self.reverse(),
            ([], Write::ReplaceAll { items }) => {
                *self = items
                    .iter()
                    .map(|item| T::from_py(&item))
                    .collect::<Result<Vec<T>, Misfit>>()?;
            }
            ([Part::Index(index), rest @ ..], write) => {
                return self
                    .get_mut(*index)
                    .ok_or_else(|| Misfit::new(format!("no index {index}")))?
                    .apply(rest, write);
            }
            _ => return Err(Misfit::new("the write does not fit a list")),
        }
        Ok(())
    }
}

/// `PyField::from_py` of a `str_enum!` set: an exact str of the set.
pub(crate) fn enum_from_py<T: StrEnum>(value: &Bound<'_, PyAny>) -> Result<T, Misfit> {
    let text = value.downcast_exact::<PyString>().map_err(|_| Misfit::new("not a str"))?;
    T::from_text(text.to_str().or_misfit()?).ok_or_else(|| Misfit::new(T::MISFIT))
}

/// `PyField::to_py` of a `str_enum!` set: the interned str.
pub(crate) fn enum_to_py<T: StrEnum>(value: T, py: Python<'_>) -> Bound<'_, PyAny> {
    PyString::intern(py, value.as_text()).into_any()
}

// ---- A kernel's inputs -------------------------------------------------------------------------

/// What a kernel returns for anything it does not decide: the Python rule then runs.
#[track_caller]
pub(crate) fn unsupported<T>(reason: &'static str) -> PyResult<T> {
    Err(Unsupported::new_err(reason))
}

/// An attribute of an object a kernel was given, which may not be the class it expects.
pub(crate) fn attribute<'py>(
    object: &Bound<'py, PyAny>,
    name: &Bound<'py, PyString>,
) -> PyResult<Bound<'py, PyAny>> {
    object.getattr(name).map_err(|_| Unsupported::new_err("an input has no such attribute"))
}

/// Whether `world.model_state` lacks this namespace: `state is None` in a Python rule, which then
/// has nothing to do, so its kernel doesn't either and no exception is paid for it each tick.
pub(crate) fn namespace_is_absent(world: &Bound<'_, PyAny>, name: &Bound<'_, PyString>) -> PyResult<bool> {
    let table = attribute(world, pyo3::intern!(world.py(), "model_state"))?;
    Ok(exact_dict(&table)?.get_item(name)?.is_none_or(|state| state.is_none()))
}

/// An int and nothing else: not a bool, and one that fits.
pub(crate) fn exact_int(value: &Bound<'_, PyAny>) -> PyResult<i64> {
    let Ok(number) = value.downcast_exact::<PyInt>() else {
        return unsupported("a value is not an int");
    };
    number.extract().map_err(|_| Unsupported::new_err("an int is too large"))
}

/// An int of any size, as `str(int)` writes it. A Scenario's seed is an int from 0, past 64 bits
/// too, and the Python rules write it as text (`CounterRNG` keys its draws with `str(seed)`), so it
/// is never read as an i64.
pub(crate) fn exact_int_text(value: &Bound<'_, PyAny>) -> PyResult<String> {
    let Ok(number) = value.downcast_exact::<PyInt>() else {
        return unsupported("a value is not an int");
    };
    Ok(number.str()?.to_str()?.to_owned())
}

/// A float and nothing else.
pub(crate) fn exact_float(value: &Bound<'_, PyAny>) -> PyResult<f64> {
    match value.downcast_exact::<PyFloat>() {
        Ok(number) => Ok(number.value()),
        Err(_) => unsupported("a value is not a float"),
    }
}

/// A str and nothing else, with UTF-8.
pub(crate) fn exact_str<'a>(value: &'a Bound<'_, PyAny>) -> PyResult<&'a str> {
    let Ok(text) = value.downcast_exact::<PyString>() else {
        return unsupported("a value is not a str");
    };
    text.to_str().map_err(|_| Unsupported::new_err("a str is not UTF-8"))
}

/// A str and nothing else, with UTF-8, for a rule that only asks: nothing is refused.
pub(crate) fn str_value<'a>(value: &'a Bound<'_, PyAny>) -> Option<&'a str> {
    value.downcast_exact::<PyString>().ok()?.to_str().ok()
}

/// A dict whose reads are the inherited C code, so reading it runs no Python.
pub(crate) fn exact_dict<'a, 'py>(value: &'a Bound<'py, PyAny>) -> PyResult<&'a Bound<'py, PyDict>> {
    as_readable_dict(value).ok_or_else(|| Unsupported::new_err("a value is not a plain dict"))
}

/// A list whose reads are the inherited C code.
pub(crate) fn exact_list<'a, 'py>(value: &'a Bound<'py, PyAny>) -> PyResult<&'a Bound<'py, PyList>> {
    as_readable_list(value).ok_or_else(|| Unsupported::new_err("a value is not a plain list"))
}

// ---- The macros --------------------------------------------------------------------------------

/// The Python side of a struct of the core's `state_row!`: its fields, by name, in the order the
/// struct lists them.
macro_rules! py_row {
    ($name:ty { $($field:ident),* $(,)? }) => {
        const _: () = assert!(
            $crate::core::row::same_names(<$name>::NAMES, &[$(stringify!($field)),*]),
            concat!("py_row! must list the fields of ", stringify!($name), " in their order"),
        );

        impl $crate::core::row::PyField for $name {
            fn from_py(
                value: &pyo3::Bound<'_, pyo3::PyAny>,
            ) -> Result<Self, $crate::core::world_core::Misfit> {
                use pyo3::types::PyDictMethods;
                use $crate::core::world_core::OrMisfit;
                let record = $crate::core::tracked::as_readable_dict(value)
                    .ok_or_else(|| $crate::core::world_core::Misfit::new("not a dict"))?;
                if record.len() > <$name>::NAMES.len() {
                    return Err($crate::core::world_core::Misfit::new("other fields than the row's"));
                }
                let mut present = 0;
                let row = Self {
                    $(
                        $field: match record.get_item(pyo3::intern!(value.py(), stringify!($field))).or_misfit()? {
                            Some(item) => {
                                present += 1;
                                $crate::core::row::PyField::from_py(&item)?
                            }
                            None => $crate::core::row::Field::when_missing().ok_or_else(|| {
                                $crate::core::world_core::Misfit::new(
                                    concat!("other fields than the row's: no ", stringify!($field)),
                                )
                            })?,
                        },
                    )*
                };
                if present != record.len() {
                    return Err($crate::core::world_core::Misfit::new("other fields than the row's"));
                }
                Ok(row)
            }

            fn to_py<'py>(
                &self,
                py: pyo3::Python<'py>,
            ) -> pyo3::PyResult<pyo3::Bound<'py, pyo3::PyAny>> {
                use pyo3::types::PyDictMethods;
                let record = pyo3::types::PyDict::new(py);
                $(
                    if !$crate::core::row::Field::is_absent(&self.$field) {
                        record.set_item(
                            pyo3::intern!(py, stringify!($field)),
                            $crate::core::row::PyField::to_py(&self.$field, py)?,
                        )?;
                    }
                )*
                Ok(record.into_any())
            }

            fn part_to_py<'py>(
                &self,
                py: pyo3::Python<'py>,
                key: &str,
            ) -> pyo3::PyResult<Option<pyo3::Bound<'py, pyo3::PyAny>>> {
                $(
                    if key == stringify!($field) {
                        if $crate::core::row::Field::is_absent(&self.$field) {
                            return Ok(None);
                        }
                        return $crate::core::row::PyField::to_py(&self.$field, py).map(Some);
                    }
                )*
                let _ = py;
                Ok(None)
            }

            fn part_py_items(&self, key: &str) -> Option<&dyn $crate::core::row::PyItems> {
                $(
                    if key == stringify!($field) {
                        if $crate::core::row::Field::is_absent(&self.$field) {
                            return None;
                        }
                        return $crate::core::row::PyField::py_items(&self.$field);
                    }
                )*
                None
            }

            fn to_py_emptied<'py>(
                &self,
                py: pyo3::Python<'py>,
                emptied: &[&str],
            ) -> pyo3::PyResult<pyo3::Bound<'py, pyo3::PyAny>> {
                use pyo3::types::PyDictMethods;
                let record = pyo3::types::PyDict::new(py);
                $(
                    if !$crate::core::row::Field::is_absent(&self.$field) {
                        let value = if emptied.contains(&stringify!($field))
                            && $crate::core::row::Field::items(&self.$field).is_some()
                        {
                            pyo3::types::PyList::empty(py).into_any()
                        } else {
                            $crate::core::row::PyField::to_py(&self.$field, py)?
                        };
                        record.set_item(pyo3::intern!(py, stringify!($field)), value)?;
                    }
                )*
                let _ = emptied;
                Ok(record.into_any())
            }

            fn apply(
                &mut self,
                path: &[$crate::core::world_core::Part<'_>],
                write: &$crate::core::world_core::Write<'_, '_>,
            ) -> Result<(), $crate::core::world_core::Misfit> {
                use $crate::core::world_core::{Misfit, Part, Write};
                match (path, write) {
                    ([], Write::Set { key: Part::Key(key), value }) => {
                        $(
                            if *key == stringify!($field) {
                                self.$field = $crate::core::row::PyField::from_py(value)?;
                                return Ok(());
                            }
                        )*
                        let _ = value;
                        Err(Misfit::new(format!("no field {key:?}")))
                    }
                    ([Part::Key(key), rest @ ..], write) => {
                        $(
                            if *key == stringify!($field) {
                                return $crate::core::row::PyField::apply(&mut self.$field, rest, write);
                            }
                        )*
                        let _ = (rest, write);
                        Err(Misfit::new(format!("no field {key:?}")))
                    }
                    ([], Write::Delete { key: Part::Key(key) }) => {
                        $(
                            if *key == stringify!($field)
                                && !$crate::core::row::Field::is_absent(&self.$field)
                            {
                                if let Some(gone) = $crate::core::row::Field::when_missing() {
                                    self.$field = gone;
                                    return Ok(());
                                }
                            }
                        )*
                        Err(Misfit::new("the write changes which fields a record has"))
                    }
                    _ => Err(Misfit::new("the write changes which fields a record has")),
                }
            }
        }
    };
}

pub(crate) use py_row;

/// The Python side of a set of the core's `str_enum!`.
macro_rules! py_str_enum {
    ($($name:ty),+ $(,)?) => {
        $(
            impl $crate::core::row::PyField for $name {
                fn from_py(
                    value: &pyo3::Bound<'_, pyo3::PyAny>,
                ) -> Result<Self, $crate::core::world_core::Misfit> {
                    $crate::core::row::enum_from_py(value)
                }

                fn to_py<'py>(
                    &self,
                    py: pyo3::Python<'py>,
                ) -> pyo3::PyResult<pyo3::Bound<'py, pyo3::PyAny>> {
                    Ok($crate::core::row::enum_to_py(*self, py))
                }
            }
        )+
    };
}

pub(crate) use py_str_enum;

/// A record of a row of this crate: the core's `state_row!` and `py_row!` together.
macro_rules! state_row {
    (
        $(#[$meta:meta])*
        $vis:vis struct $name:ident {
            $( $field:ident : $kind:ty $(=> $setter:ident)? ),* $(,)?
        }
    ) => {
        sheltersim_core::state_row! {
            $(#[$meta])*
            $vis struct $name {
                $( $field : $kind $(=> $setter)? ),*
            }
        }

        $crate::core::row::py_row! { $name { $($field),* } }
    };
}

pub(crate) use state_row;

/// A closed set of strings of a row of this crate: the core's `str_enum!` and `py_str_enum!`.
#[allow(unused_macros)]
macro_rules! str_enum {
    ($vis:vis enum $name:ident { $($member:ident = $text:literal),+ $(,)? }) => {
        sheltersim_core::str_enum! { $vis enum $name { $($member = $text),+ } }
        $crate::core::row::py_str_enum!($name);
    };
}

#[allow(unused_imports)]
pub(crate) use str_enum;
