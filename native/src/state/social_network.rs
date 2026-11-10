//! `social_network_v1`: the row is the core's (`sheltersim_core::state::social_network`); here are
//! its Python side and its validator.

use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;

pub(crate) use sheltersim_core::state::social_network::*;

use crate::core::row::{namespace_is_absent, py_row, PyField};
use crate::core::world_core::{mirror_of, Misfit, NamespaceMirror, Part, RowType, Write};

py_row! {
    Relationship {
        contact_count, conversation_count, cooperation_count, trust, shared_message_ids,
        last_contact_minute, last_interaction,
    }
}
py_row! {
    SocialNetworkRow {
        schema_version, policy_version, relationships, interaction_count, information_transfer_count,
    }
}

impl PyField for Relationships {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        Ok(Self::from_map(PyField::from_py(value)?))
    }

    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        PyField::to_py(&self.map, py)
    }

    fn apply(&mut self, path: &[Part<'_>], write: &Write<'_, '_>) -> Result<(), Misfit> {
        match (path, write) {
            ([], Write::Set { key: Part::Key(key), .. }) | ([Part::Key(key), ..], _) => {
                let key = Arc::<str>::from(*key);
                PyField::apply(&mut self.map, path, write)?;
                self.note_write(&key);
                Ok(())
            }
            _ => PyField::apply(&mut self.map, path, write),
        }
    }
}

/// The namespace's validator: accepts, or leaves it to the Python rules and their error text.
#[pyfunction]
fn validate(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    if namespace_is_absent(world, intern!(world.py(), NAME))? {
        return Ok(true);
    }
    let mirror = mirror_of(world, NAME)?;
    NamespaceMirror::with_row::<SocialNetworkRow, _>(&mirror, |row| Ok(row.is_valid()))
}

/// For the comparison with `_pair_key_problem` in the tests.
#[pyfunction]
fn key_is_good(key: &str) -> bool {
    pair_key_is_good(key)
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("ROW", Py::new(py, RowType::of::<SocialNetworkRow>("social_network"))?)?;
    m.add_function(wrap_pyfunction!(validate, m)?)?;
    m.add_function(wrap_pyfunction!(key_is_good, m)?)?;
    Ok(())
}
