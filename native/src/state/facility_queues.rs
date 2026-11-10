//! `facility_queues_v1`: the row is the core's (`sheltersim_core::state::facility_queues`); here
//! are its Python side and the check that the world holds it.

use pyo3::intern;
use pyo3::prelude::*;

pub(crate) use sheltersim_core::state::facility_queues::*;

use crate::core::row::{attribute, load_row, py_row};
use crate::core::tracked::as_readable_dict;
use crate::core::world_core::{mirror_of, raw_dict_get, NamespaceMirror, RowType};
use crate::Unsupported;

py_row! { QueueStall { minute, request_id, reason } }
py_row! { QueueRow { facility_id, service_point_id, queued_request_ids, active_request_ids, last_stall } }
py_row! {
    QueueRequest {
        request_id, queue_id, service_point_id, facility_id, agent_id, service_kind,
        requested_minute, status, queue_task_id, active_task_id,
    }
}
py_row! { QueuesRow { schema_version, policy_version, queues, requests } }

/// The mirror of the namespace, if the world holds it. A row left behind by a write past the table
/// answers for nothing.
pub(crate) fn held_mirror<'py>(world: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, NamespaceMirror>>> {
    let py = world.py();
    let table = attribute(world, intern!(py, "model_state"))?;
    if as_readable_dict(&table).is_none() {
        return Err(Unsupported::new_err("model_state is another kind of mapping"));
    }
    let Some(held) = raw_dict_get(&table, intern!(py, "facility_queues_v1").as_any())? else {
        return Ok(None);
    };
    let mirror = mirror_of(world, NAME)?;
    if !mirror.try_borrow().is_ok_and(|mirror| mirror.holds_state(&held)) {
        return Err(Unsupported::new_err("the world holds another queue state"));
    }
    Ok(Some(mirror))
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("ROW", Py::new(py, RowType::new("facility_queues", load_row::<QueuesRow>))?)?;
    Ok(())
}
