//! `information_spread_v1`: the row is the core's (`sheltersim_core::state::information`); here are
//! its Python side and its validator.

use std::collections::HashSet;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyString;

pub(crate) use sheltersim_core::state::information::*;

use crate::contacts::exact_int;
use crate::core::row::{load_row, py_row};
use crate::core::tracked::as_readable_dict;
use crate::core::world_core::{mirror_of, raw_dict_get, NamespaceMirror, RowType};
use crate::state::entities::{entities_of, EntityStore};

py_row! { MessageRow { id, truth_status, source_id, zone_id, time, origin_event_id, origin_agent_id } }
py_row! {
    BeliefRow {
        message_id, belief, source, last_contact, last_event_id, exposed, belief_strength,
        share_propensity, correction_exposed, correction_accepted, ever_misbelieved, ever_forgotten,
        last_correction_accepted,
    }
}
py_row! { ContactRow { last_seen_minute, consecutive_minutes } }
py_row! { SupportRequestRow { request_id, agent_id, minute, message_ids } }
py_row! { CountersRow { contact_attempts, contact_deliveries, correction_deliveries, support_requests } }
py_row! {
    InformationRow {
        schema_version, messages, beliefs, contacts, query_pressure, support_requests, counters,
    }
}

/// The namespace's validator: accepts, or leaves it to the Python rules and their error text.
#[pyfunction]
fn validate(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = world.py();
    let duration = exact_int(&world.getattr(intern!(py, "duration_minutes"))?)?;
    let zones = world.getattr(intern!(py, "zones"))?;
    if as_readable_dict(&zones).is_none() {
        return Ok(false);
    }
    let mirror = mirror_of(world, NAME)?;
    EntityStore::with_rows(&entities_of(world)?, |rows| {
        let agents: HashSet<&str> = rows.agents.iter().map(|(id, _)| &**id).collect();
        NamespaceMirror::with_row::<InformationRow, _>(&mirror, |row| {
            row.is_valid(&agents, duration, |zone| {
                Ok(raw_dict_get(&zones, PyString::new(py, zone).as_any())?.is_some())
            })
        })
    })
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("ROW", Py::new(py, RowType::new("information", load_row::<InformationRow>))?)?;
    m.add_function(wrap_pyfunction!(validate, m)?)?;
    Ok(())
}
