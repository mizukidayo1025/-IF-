//! `resource_service_v1`: the row is the core's (`sheltersim_core::state::resource_service`); here
//! are its Python side and its validator, which reads the entities and the zones.

use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyString;

pub(crate) use sheltersim_core::state::resource_service::*;

use crate::core::row::{attribute, namespace_is_absent, py_row};
use crate::core::tracked::as_readable_dict;
use crate::core::value::V;
use crate::core::world_core::{mirror_of, raw_dict_get, with_state_value, NamespaceMirror, RowType};
use crate::state::entities::{entities_of, EntityStore};
use crate::Unsupported;

const QUEUES_KEY: &str = "facility_queues_v1";
const LIFECYCLE_KEY: &str = "population_lifecycle_v1";

py_row! { RequestRecord { minute, agent_id, action, outcome, reason, facility_id, origin_zone_id, detail } }
py_row! { StallRecord { minute, facility_id, agent_id, reason } }
py_row! { Stall { agent_id, reason } }
py_row! {
    ServiceRow {
        schema_version, cancelled_task_ids, active_reservation_ids, request_history,
        queue_stall_history, stall_state,
    }
}

/// The mirror of the namespace the world really holds: a row left behind by a write past the table
/// answers for nothing.
pub(crate) fn held_mirror<'py>(world: &Bound<'py, PyAny>) -> PyResult<Bound<'py, NamespaceMirror>> {
    let py = world.py();
    let mirror = mirror_of(world, NAME)?;
    let table = attribute(world, intern!(py, "model_state"))?;
    if as_readable_dict(&table).is_none() {
        return Err(Unsupported::new_err("model_state is another kind of mapping"));
    }
    let held = raw_dict_get(&table, intern!(py, "resource_service_v1").as_any())?;
    if !held.is_some_and(|held| mirror.try_borrow().is_ok_and(|mirror| mirror.holds_state(&held))) {
        return Err(Unsupported::new_err("the world holds another service state"));
    }
    Ok(mirror)
}

struct Known<'py> {
    agents: Bound<'py, PyAny>,
    zones: Bound<'py, PyAny>,
    /// The keys of `population_lifecycle_v1.people`, read from its row when it is detached.
    archived: Option<rustc_hash::FxHashSet<Arc<str>>>,
}

impl ServiceWorld for Known<'_> {
    type Error = PyErr;

    fn person(&self, id: &str) -> PyResult<bool> {
        let key = PyString::new(self.agents.py(), id);
        if raw_dict_get(&self.agents, key.as_any())?.is_some() {
            return Ok(true);
        }
        Ok(match &self.archived {
            Some(people) => people.contains(id),
            None => false,
        })
    }

    fn zone(&self, id: &str) -> PyResult<bool> {
        Ok(raw_dict_get(&self.zones, PyString::new(self.zones.py(), id).as_any())?.is_some())
    }
}

/// The part of `WorldState.validate` a tick runs: accepts, or leaves it to the Python rules.
#[pyfunction]
fn validate(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = world.py();
    if namespace_is_absent(world, intern!(py, NAME))? {
        return Ok(false);
    }
    let table = attribute(world, intern!(py, "model_state"))?;
    let table = as_readable_dict(&table).ok_or_else(|| Unsupported::new_err("model_state is another kind of mapping"))?;
    // Where a facility has independent queues, each queue's head is a head of the facility.
    let mut heads: std::collections::HashMap<Arc<str>, Vec<Arc<str>>> = std::collections::HashMap::new();
    if table.contains(QUEUES_KEY)? {
        let stalls = NamespaceMirror::with_row::<ServiceRow, _>(&held_mirror(world)?, |row| Ok(row.stall_state.keys().cloned().collect::<Vec<_>>()))?;
        for facility_id in stalls {
            if let Some(found) = crate::rules::facility_queues::independent_heads(world, &facility_id)? {
                heads.insert(facility_id, found);
            }
        }
    }
    let archived = with_state_value(world, LIFECYCLE_KEY, |lifecycle| {
        Ok(match lifecycle {
            Some(V::Map(lifecycle)) => match lifecycle.get("people") {
                Some(V::Map(people)) => Some(people.keys().cloned().collect()),
                _ => None,
            },
            _ => None,
        })
    })?;
    let known = Known { agents: attribute(world, intern!(py, "agents"))?, zones: attribute(world, intern!(py, "zones"))?, archived };
    if as_readable_dict(&known.agents).is_none() || as_readable_dict(&known.zones).is_none() {
        return Ok(false);
    }
    let minute_index = attribute(world, intern!(py, "minute_index"))?.extract::<i64>().map_err(|_| Unsupported::new_err("the minute is not an int"))?;
    let mirror = held_mirror(world)?;
    let store = entities_of(world)?;
    NamespaceMirror::with_row::<ServiceRow, _>(&mirror, |row| {
        EntityStore::with_rows(&store, |rows| row.passes_rules(rows, &known, minute_index, &heads))
    })
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("ROW", Py::new(py, RowType::of::<ServiceRow>("resource_service"))?)?;
    m.add_function(wrap_pyfunction!(validate, m)?)?;
    Ok(())
}

/// The `reservation_id` of each pending `resource_reserve` event, in the queue's order: read through
/// the queue's tuple, the same objects the Python rule reads (H6: the Python objects of the pending
/// events; lane P10-1c moved it here from the service's glue).
pub(crate) fn pending_reserves(world: &Bound<'_, PyAny>) -> PyResult<Vec<sheltersim_core::rules::service::Pending>> {
    use pyo3::exceptions::PyKeyError;
    use pyo3::types::PyTuple;
    use sheltersim_core::rules::service::Pending;
    let writer = crate::state::events::EventWriter::of(world)?;
    let py = world.py();
    let pending = writer.queue().call_method0(intern!(py, "pending"))?;
    let pending = pending.downcast_exact::<PyTuple>().map_err(|_| Unsupported::new_err("the pending events are not a tuple"))?;
    let mut found = Vec::new();
    for event in pending.iter() {
        if crate::core::row::exact_str(&attribute(&event, intern!(py, "event_type"))?)? != "resource_reserve" {
            continue;
        }
        let payload = attribute(&event, intern!(py, "payload"))?;
        // `payload["reservation_id"]`: a KeyError is no ID; another error, a payload not read here.
        let id = match payload.get_item(intern!(py, "reservation_id")) {
            Ok(id) => Some(id),
            Err(error) if error.is_instance_of::<PyKeyError>(py) => None,
            Err(_) => return Err(Unsupported::new_err("a payload cannot be read")),
        };
        found.push(match id {
            Some(id) => match crate::core::row::str_value(&id) {
                Some(id) => Pending::Id(Arc::from(id)),
                None => Pending::Other,
            },
            None => Pending::Missing,
        });
    }
    Ok(found)
}
