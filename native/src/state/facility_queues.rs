//! `facility_queues_v1`: the row is the core's (`sheltersim_core::state::facility_queues`); here
//! are its Python side and the check that the world holds it.

use pyo3::intern;
use pyo3::prelude::*;

pub(crate) use sheltersim_core::state::facility_queues::*;

use crate::core::row::{attribute, py_row};
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
    m.add("ROW", Py::new(py, RowType::of::<QueuesRow>("facility_queues"))?)?;
    Ok(())
}

// ---- The Python side of the row, for the rules that read it (lane c1) ------------------------------
//
// The kernels of `facility_queues_v1` are the core's (`sheltersim_core::rules::facility_queues`).
// What is here reads the row for a rule that holds other borrows (`with_queues`), or makes Python
// values of it.

/// Whether the world holds the namespace, as a kernel of the core takes it.
pub(crate) fn held(world: &Bound<'_, PyAny>) -> PyResult<sheltersim_core::rules::kit::Held> {
    crate::core::world_core::namespace_held(world, intern!(world.py(), NAME), "the world holds another queue state")
}

/// Run f on the row, or give None for a world without the namespace.
pub(crate) fn with_queues<R>(world: &Bound<'_, PyAny>, f: impl FnOnce(&mut QueuesRow) -> PyResult<R>) -> PyResult<Option<R>> {
    match held_mirror(world)? {
        Some(mirror) => Ok(Some(NamespaceMirror::with_row::<QueuesRow, _>(&mirror, f)?)),
        None => Ok(None),
    }
}

/// `facility_has_independent_queues`.
pub(crate) fn has_independent(world: &Bound<'_, PyAny>, facility_id: &str) -> PyResult<bool> {
    let rules = sheltersim_core::rules::facility_queues::has_independent;
    Ok(with_queues(world, |row| Ok(rules(row, facility_id)))?.unwrap_or(false))
}

/// The heads of the facility's independent queues, or None for a facility without any.
pub(crate) fn independent_heads(world: &Bound<'_, PyAny>, facility_id: &str) -> PyResult<Option<Vec<std::sync::Arc<str>>>> {
    use crate::core::errors::OrPy;
    let rules = sheltersim_core::rules::facility_queues::independent_heads;
    Ok(with_queues(world, |row| rules(row, facility_id).or_py())?.flatten())
}

/// The mirror of the namespace while the world holds it detached, its row alone holding it. None
/// while it has a tree.
fn detached_mirror<'py>(world: &Bound<'py, PyAny>) -> PyResult<Option<Bound<'py, NamespaceMirror>>> {
    let table = attribute(world, intern!(world.py(), "model_state"))?;
    let held = crate::core::row::exact_dict(&table)?.get_item(NAME)?;
    match held {
        Some(held) if crate::core::tracked::is_detached(&held) => held_mirror(world),
        _ => Ok(None),
    }
}

/// The tree's node of a request, for a rule that hands it to Python. A detached namespace gives a
/// new document of the row's record: read it, never write.
pub(crate) fn request_node<'py>(world: &Bound<'py, PyAny>, request_id: &str) -> PyResult<Option<Bound<'py, PyAny>>> {
    use crate::core::row::{exact_dict, unsupported, PyField};
    let py = world.py();
    if let Some(mirror) = detached_mirror(world)? {
        return NamespaceMirror::with_row::<QueuesRow, _>(&mirror, |row| row.requests.get(request_id).map(|request| PyField::to_py(request, py)).transpose());
    }
    let table = attribute(world, intern!(py, "model_state"))?;
    let Some(state) = exact_dict(&table)?.get_item(NAME)? else {
        return Ok(None);
    };
    let Some(requests) = exact_dict(&state)?.get_item(intern!(py, "requests"))? else {
        return unsupported("the queues have no requests");
    };
    exact_dict(&requests)?.get_item(request_id)
}
