//! `body_state_v1`: the row is the core's (`sheltersim_core::state::body_state`); here are its
//! Python side and its validator.

use std::collections::VecDeque;
use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyList;

pub(crate) use sheltersim_core::state::body_state::*;

use crate::core::row::{load_row, namespace_is_absent, py_row, PyField, PyItems};
use crate::core::tracked::as_readable_list;
use crate::core::world_core::{loads_items, mirror_of, Misfit, NamespaceMirror, OrMisfit, Part, RowType, Write};
use crate::encode;
use crate::state::entities::{entities_of, EntityStore};

py_row! {
    BodyRecord {
        sleep_debt_minutes, uncertainty, reassurance, discontent, positive_affect, negative_affect,
        hygiene_need,
    }
}
py_row! { Relationship { contact_count, trust, contact_minutes, last_contact_minute, last_interaction, topic_ids } }
py_row! { BodyRow { schema_version, policy_version, source_id, config, agents, relationships, history } }

impl PyField for Relationships {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        Ok(Self::from_map(PyField::from_py(value)?))
    }

    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.map.to_py(py)
    }

    fn apply(&mut self, path: &[Part<'_>], write: &Write<'_, '_>) -> Result<(), Misfit> {
        let key = match (path, write) {
            ([Part::Key(key), ..], _) | ([], Write::Set { key: Part::Key(key), .. }) => Some(*key),
            _ => None,
        };
        self.map.apply(path, write)?;
        if let Some(key) = key {
            self.written.push(Arc::from(key));
        }
        Ok(())
    }
}

fn item_bytes(value: &Bound<'_, PyAny>) -> Result<Box<[u8]>, Misfit> {
    Ok(encode(value).or_misfit()?.into_boxed_slice())
}

impl PyItems for History {
    fn slice_to_py<'py>(&self, py: Python<'py>, start: usize, stop: usize) -> PyResult<Bound<'py, PyList>> {
        loads_items(py, self.items.range(start..stop).map(|item| &**item))
    }
}

impl PyField for History {
    fn py_items(&self) -> Option<&dyn PyItems> {
        Some(self)
    }

    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        let list = as_readable_list(value).ok_or_else(|| Misfit::new("not a list"))?;
        let mut items = VecDeque::with_capacity(list.len());
        for item in list.iter() {
            items.push_back(item_bytes(&item)?);
        }
        Ok(Self { items })
    }

    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let loads = py.import(intern!(py, "json"))?.getattr(intern!(py, "loads"))?;
        let list = PyList::empty(py);
        for item in &self.items {
            list.append(loads.call1((std::str::from_utf8(item)?,))?)?;
        }
        Ok(list.into_any())
    }

    fn apply(&mut self, path: &[Part<'_>], write: &Write<'_, '_>) -> Result<(), Misfit> {
        match (path, write) {
            ([], Write::Append { value }) => self.items.push_back(item_bytes(value)?),
            ([], Write::Extend { values }) => {
                for value in values.iter() {
                    self.items.push_back(item_bytes(&value)?);
                }
            }
            ([], Write::DeleteRange { start, stop }) if start <= stop && *stop <= self.items.len() => {
                self.items.drain(*start..*stop);
            }
            ([], Write::Delete { key: Part::Index(index) }) if *index < self.items.len() => {
                self.items.remove(*index);
            }
            ([], Write::Set { key: Part::Index(index), value }) if *index < self.items.len() => {
                self.items[*index] = item_bytes(value)?;
            }
            ([], Write::Insert { index, value }) if *index <= self.items.len() => {
                self.items.insert(*index, item_bytes(value)?);
            }
            ([], Write::Clear) => self.items.clear(),
            ([], Write::Reverse) => self.items.make_contiguous().reverse(),
            ([], Write::ReplaceAll { items }) => {
                self.items = items.iter().map(|item| item_bytes(&item)).collect::<Result<_, _>>()?;
            }
            _ => return Err(Misfit::new("the write does not fit the history")),
        }
        Ok(())
    }
}

/// True only if the header, the Agent records and the relationships pass every rule
/// `validate_body_state` has for them. Its checks of the config and the history read what the row
/// doesn't hold, and stay in Python. Anything else, a false or Unsupported, runs the Python rules,
/// so their errors keep their text. What is accepted is not read again until it is written.
#[pyfunction]
fn rows_valid(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    if namespace_is_absent(world, intern!(world.py(), NAME))? {
        return Ok(false);
    }
    let mirror = mirror_of(world, NAME)?;
    let store = entities_of(world)?;
    NamespaceMirror::with_row::<BodyRow, _>(&mirror, |row| {
        let accepted = EntityStore::with_rows(&store, |rows| {
            let people = &rows.agents;
            if row.schema_version != 1
                || &*row.policy_version != POLICY_VERSION
                || &*row.source_id != SOURCE_ID
                || row.agents.len() != people.len()
                || row.relationships.map.len() > MAX_SOCIAL_RELATIONSHIPS
                || row.history.items.len() > MAX_SOCIAL_HISTORY
                || !row.agents.iter().all(|(id, record)| people.get(id).is_some() && record_is_valid(record))
            {
                return Ok(false);
            }
            let relationships = &mut row.relationships;
            let same_people = relationships.people.len() == row.agents.len()
                && relationships.people.iter().zip(row.agents.keys()).all(|(known, id)| known == id);
            if relationships.read_all || !same_people {
                let valid = relationships
                    .map
                    .iter()
                    .all(|(key, record)| key_is_valid(key, &row.agents) && relationship_is_valid(record));
                if !valid {
                    return Ok(false);
                }
                relationships.read_all = false;
                relationships.people = row.agents.keys().cloned().collect();
            } else {
                // A written key that left the map has nothing to check any more.
                let valid = relationships.written.iter().all(|key| {
                    relationships.map.get(key).is_none_or(|record| {
                        key_is_valid(key, &row.agents) && relationship_is_valid(record)
                    })
                });
                if !valid {
                    return Ok(false);
                }
            }
            relationships.written.clear();
            Ok(true)
        })?;
        if !accepted {
            // The Python rules decide now. If they pass it, the next check starts over, and the
            // list of written keys does not grow at each tick of such a world.
            row.relationships.read_all = true;
            row.relationships.written.clear();
        }
        Ok(accepted)
    })
}

/// The canonical JSON of the row's records of these people and these relationships, and of its
/// last history items, for benchmarks/rule_shadow: a whole `verify` per event would be too slow.
#[pyfunction]
fn row_parts<'py>(
    world: &Bound<'py, PyAny>,
    people: Vec<String>,
    relationships: Vec<String>,
    history_tail: usize,
) -> PyResult<(Bound<'py, PyAny>, Bound<'py, PyAny>, Vec<String>, usize)> {
    let py = world.py();
    let mirror = mirror_of(world, NAME)?;
    NamespaceMirror::with_row::<BodyRow, _>(&mirror, |row| {
        let found_people = pyo3::types::PyDict::new(py);
        for id in &people {
            if let Some(record) = row.agents.get(id.as_str()) {
                found_people.set_item(id, PyField::to_py(record, py)?)?;
            }
        }
        let found = pyo3::types::PyDict::new(py);
        for key in &relationships {
            if let Some(record) = row.relationships.map.get(key.as_str()) {
                found.set_item(key, PyField::to_py(record, py)?)?;
            }
        }
        let count = row.history.items.len();
        let mut tail = Vec::with_capacity(history_tail.min(count));
        for item in row.history.items.iter().skip(count.saturating_sub(history_tail)) {
            tail.push(String::from_utf8_lossy(item).into_owned());
        }
        Ok((found_people.into_any(), found.into_any(), tail, count))
    })
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("ROW", Py::new(py, RowType::new("body_state", load_row::<BodyRow>))?)?;
    m.add_function(wrap_pyfunction!(rows_valid, m)?)?;
    m.add_function(wrap_pyfunction!(row_parts, m)?)?;
    Ok(())
}
