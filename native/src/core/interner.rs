//! IDs and enum members as small numbers in rows, and one Python object for each, so that
//! `agent.activity is Activity.IDLE` holds and a read allocates nothing. `Interner` holds no
//! Python objects, so rows stay `Send + Sync`; the objects live in side tables. `Interner` and `Sym`
//! live in `sheltersim_core::interner`.

use pyo3::prelude::*;
use pyo3::types::PyString;

pub(crate) use sheltersim_core::interner::{Interner, Sym};

/// The one Python str of each symbol, made when first asked for.
#[derive(Default)]
pub(crate) struct PyStrings {
    objects: Vec<Option<Py<PyString>>>,
}

impl PyStrings {
    pub(crate) fn get<'py>(
        &mut self,
        py: Python<'py>,
        interner: &Interner,
        symbol: Sym,
    ) -> Bound<'py, PyString> {
        let index = symbol.index();
        if index >= self.objects.len() {
            self.objects.resize_with(index + 1, || None);
        }
        self.objects[index]
            .get_or_insert_with(|| PyString::intern(py, interner.text(symbol)).unbind())
            .bind(py)
            .clone()
    }
}

/// The members of a Python enum, in definition order. Rows hold the index.
pub(crate) struct EnumTable {
    members: Vec<Py<PyAny>>,
}

impl EnumTable {
    pub(crate) fn from_class(class: &Bound<'_, PyAny>) -> PyResult<Self> {
        let members = class
            .try_iter()?
            .map(|item| Ok(item?.unbind()))
            .collect::<PyResult<_>>()?;
        Ok(Self { members })
    }

    pub(crate) fn member<'py>(&self, py: Python<'py>, index: u8) -> Bound<'py, PyAny> {
        self.members[index as usize].bind(py).clone()
    }

    /// The index of this very object; a value equal to a member but not it is `None`, and the
    /// caller leaves it to Python.
    pub(crate) fn index_of(&self, value: &Bound<'_, PyAny>) -> Option<u8> {
        self.members
            .iter()
            .position(|member| member.as_ptr() == value.as_ptr())
            .map(|index| index as u8)
    }
}
