//! `staff_operations_v1`: the row is the core's (`sheltersim_core::state::staff_operations`); here
//! are its Python side and its validator, whose checks of an active record read the objects of the
//! world and the pending events.

use std::collections::HashSet;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::PyString;

pub(crate) use sheltersim_core::state::staff_operations::*;

use crate::core::row::{attribute, exact_dict, exact_int, namespace_is_absent, py_row, py_str_enum, unsupported};
use crate::core::tracked::as_readable_list;
use crate::core::value::V;
use crate::core::world_core::{mirror_of, NamespaceMirror, RowType};
use crate::state::entities::{entities_of, EntityStore};
use crate::state::events::{EventWriter, PendingEvent, PendingSource};
use crate::Unsupported;

py_str_enum!(Phase, Outcome);
py_row! { ZoneRecord { sanitation, updated_minute } }
py_row! {
    OperationRecord {
        phase, task_id, task_kind, target_id, target_zone_id, reason, started_minute, travel_due_minute,
        arrival_minute, due_minute, route_zone_ids, route_edge_ids, travel_minutes, causation_event_ids,
    }
}
py_row! {
    FinishedTask {
        task_id, staff_id, task_kind, target_id, target_zone_id, reason, status, start_minute,
        arrival_minute, complete_minute, travel_minutes, causation_event_ids,
    }
}
py_row! { StaffOperationsRow { schema_version, policy_version, source_id, config, zones, staff, history } }

/// `event.payload.get(key)` as the rule compares it with a str or None.
enum Payload {
    Null,
    Text(String),
    Other,
}

impl Payload {
    fn same(&self, expected: Option<&str>) -> bool {
        match (self, expected) {
            (Payload::Null, None) => true,
            (Payload::Text(text), Some(expected)) => text == expected,
            _ => false,
        }
    }
}

fn payload_of(event: &PendingEvent<'_>, key: &str) -> PyResult<Payload> {
    match &event.source {
        PendingSource::Row(row) => {
            let V::Map(fields) = &row.payload else {
                return unsupported("a payload is not a mapping");
            };
            Ok(match fields.get(key) {
                None | Some(V::Null) => Payload::Null,
                Some(V::Str(text)) => Payload::Text(text.to_string()),
                Some(_) => Payload::Other,
            })
        }
        PendingSource::Object(object) => {
            let py = object.py();
            let value = object.getattr(intern!(py, "payload"))?.call_method1(intern!(py, "get"), (key,))?;
            if value.is_none() {
                return Ok(Payload::Null);
            }
            if let Ok(text) = value.downcast_exact::<PyString>() {
                return Ok(Payload::Text(text.to_str()?.to_owned()));
            }
            if value.is_instance_of::<PyString>() {
                // A str subclass may be equal to a str; the Python rule says which.
                return unsupported("a payload value is a str subclass");
            }
            Ok(Payload::Other)
        }
    }
}

fn event_type_of(event: &PendingEvent<'_>) -> PyResult<String> {
    match &event.source {
        PendingSource::Row(row) => Ok(row.event_type.to_string()),
        PendingSource::Object(object) => {
            let value = object.getattr(intern!(object.py(), "event_type"))?;
            Ok(text_of(&value)?.to_owned())
        }
    }
}

/// An exact str, or Unsupported: a subclass may be equal to a str, and the Python rule says which.
fn text_of<'a>(value: &'a Bound<'_, PyAny>) -> PyResult<&'a str> {
    match value.downcast_exact::<PyString>() {
        Ok(text) => text.to_str(),
        Err(_) => unsupported("a value the rule compares is not a plain str"),
    }
}

fn optional_text_of<'a>(value: &'a Bound<'_, PyAny>) -> PyResult<Option<&'a str>> {
    if value.is_none() {
        return Ok(None);
    }
    text_of(value).map(Some)
}

/// The rules of an active record that read the world: its task, its staff member, the graph and the
/// pending events. The objects are read, not the entity rows, as `rules/tick_validation.rs` does:
/// `staff.__dict__[...] = ...` reaches no row (docs/rust-port/lanes/t-tick-validation.md).
fn active_records_pass(world: &Bound<'_, PyAny>, active: &[Active]) -> PyResult<bool> {
    let py = world.py();
    let tasks = attribute(world, intern!(py, "active_tasks"))?;
    let tasks = exact_dict(&tasks)?;
    let staff = attribute(world, intern!(py, "staff"))?;
    let staff = exact_dict(&staff)?;
    let zones = attribute(world, intern!(py, "zones"))?;
    let zones = exact_dict(&zones)?;
    let edges = attribute(world, intern!(py, "zone_edges"))?;
    let edges = exact_dict(&edges)?;
    let facilities = attribute(world, intern!(py, "facilities"))?;
    let facilities = exact_dict(&facilities)?;
    let writer = EventWriter::of(world)?;
    let queue = writer.queue().borrow();
    let Some(pending) = queue.pending_events(py)? else {
        return unsupported("a pending event is not a plain ScheduledEvent");
    };
    for record in active {
        let Some(task) = tasks.get_item(&record.task_id)? else {
            return Ok(false);
        };
        let Some(member) = staff.get_item(&record.staff_id)? else {
            return Ok(false);
        };
        let task_staff = attribute(&task, intern!(py, "staff_id"))?;
        let active_task = attribute(&member, intern!(py, "active_task_id"))?;
        let work_queue = attribute(&member, intern!(py, "work_queue"))?;
        let Some(work_queue) = as_readable_list(&work_queue) else {
            return unsupported("a work queue is another kind of list");
        };
        let busy_until = attribute(&member, intern!(py, "busy_until"))?;
        let member_zone = attribute(&member, intern!(py, "zone_id"))?;
        let member_zone = text_of(&member_zone)?;
        if optional_text_of(&task_staff)? != Some(&*record.staff_id)
            || optional_text_of(&active_task)? != Some(&*record.task_id)
            || work_queue.len() != 1
            || text_of(&work_queue.get_item(0)?)? != record.task_id
            || busy_until.is_none()
            || exact_int(&busy_until)? != record.due
            || !zones.contains(&record.target_zone_id)?
            || record.route_zones.iter().map(|zone| zones.contains(zone)).any(|found| !matches!(found, Ok(true)))
            || record.route_edges.iter().map(|edge| edges.contains(edge)).any(|found| !matches!(found, Ok(true)))
            || record.target_id.as_ref().is_some_and(|target| !matches!(facilities.contains(target), Ok(true)))
        {
            return Ok(false);
        }
        if record.working {
            if !record.route_zones.is_empty()
                || !record.route_edges.is_empty()
                || record.travel_due.is_some()
                || record.arrival != Some(record.started + record.travel_minutes)
                || member_zone != record.target_zone_id
            {
                return Ok(false);
            }
        } else {
            // route_is_valid
            if record.route_zones.len() != record.route_edges.len() + 1
                || record.route_edges.is_empty()
                || record.route_zones[0] != member_zone
                || record.route_zones.last().map(String::as_str) != Some(&*record.target_zone_id)
            {
                return Ok(false);
            }
            let mut total: i64 = 0;
            for (index, edge_id) in record.route_edges.iter().enumerate() {
                let Some(edge) = edges.get_item(edge_id)? else {
                    return Ok(false);
                };
                let from = attribute(&edge, intern!(py, "from_zone_id"))?;
                let to = attribute(&edge, intern!(py, "to_zone_id"))?;
                if text_of(&from)? != record.route_zones[index] || text_of(&to)? != record.route_zones[index + 1] {
                    return Ok(false);
                }
                total = total
                    .checked_add(exact_int(&attribute(&edge, intern!(py, "travel_minutes"))?)?)
                    .ok_or_else(|| Unsupported::new_err("a route's minutes out of range"))?;
            }
            if total != record.travel_minutes
                || record.travel_due != Some(record.started + record.travel_minutes)
                || record.arrival.is_some()
            {
                return Ok(false);
            }
        }
        if record.due != record.started + record.travel_minutes + record.duration {
            return Ok(false);
        }
        let (mut completions, mut arrivals) = (Vec::new(), Vec::new());
        for event in &pending {
            let kind = event_type_of(event)?;
            let is_completion = kind == "staff_operation_complete";
            if !is_completion && kind != "staff_operation_arrive" {
                continue;
            }
            if !payload_of(event, "task_id")?.same(Some(&record.task_id))
                || !payload_of(event, "staff_id")?.same(Some(&record.staff_id))
            {
                continue;
            }
            if is_completion {
                if event.due_minute == record.due {
                    completions.push(event);
                }
            } else {
                arrivals.push(event);
            }
        }
        let expected_arrivals = usize::from(!record.working);
        if completions.len() != 1
            || !payload_of(completions[0], "task_kind")?.same(Some(&record.task_kind))
            || !payload_of(completions[0], "target_id")?.same(record.target_id.as_deref())
            || arrivals.len() != expected_arrivals
        {
            return Ok(false);
        }
        if let Some(arrival) = arrivals.first() {
            if Some(arrival.due_minute) != record.travel_due
                || !payload_of(arrival, "target_zone_id")?.same(Some(&record.target_zone_id))
            {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// `validate_staff_operations_state`: accepts, or leaves it to the Python rules and their error text.
/// An active record's rules read its task, its staff member, the graph and the pending events.
#[pyfunction]
fn validate(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = world.py();
    if namespace_is_absent(world, intern!(py, NAME))? {
        return Ok(true);
    }
    let zones_of_world = world.getattr(intern!(py, "zones"))?;
    let zones_of_world = exact_dict(&zones_of_world)?;
    let mut zones = HashSet::with_capacity(zones_of_world.len());
    for key in zones_of_world.keys() {
        let Ok(key) = key.downcast_exact::<PyString>().map(|key| key.to_string()) else {
            return Ok(false);
        };
        zones.insert(key);
    }
    let mirror = mirror_of(world, NAME)?;
    let store = entities_of(world)?;
    let mut active = Vec::new();
    let accepted = NamespaceMirror::with_row::<StaffOperationsRow, _>(&mirror, |row| {
        let Some(found) = row.active_records(&zones) else {
            return Ok(false);
        };
        active = found;
        EntityStore::with_rows(&store, |rows| Ok(row.agrees_with(rows, &zones)))
    })?;
    if !accepted {
        return Ok(false);
    }
    active_records_pass(world, &active)
}

/// `_validate_staff_operation_receipt` (`domain/common_operations.py`) of a receipt that passed its
/// contract: whether the retained completions of this namespace agree with it. Unsupported where the
/// tree is not the row's.
pub(crate) fn completion_agrees(world: &Bound<'_, PyAny>, receipt: &serde_json::Value, history_truncated: bool) -> PyResult<bool> {
    let py = world.py();
    let task_id = receipt.get("task_id").and_then(serde_json::Value::as_str);
    let minute = receipt.get("minute").and_then(serde_json::Value::as_i64);
    let (Some(task_id), Some(minute)) = (task_id, minute) else {
        return unsupported("a staff operation receipt lacks its task or minute");
    };
    let table = attribute(world, intern!(py, "model_state"))?;
    let table = exact_dict(&table)?;
    let Some(tree) = table.get_item(NAME)?.filter(|tree| !tree.is_none()) else {
        // No retained completions: the rule finds no match.
        return Ok(false);
    };
    let mirror = mirror_of(world, NAME)?;
    if !mirror.try_borrow()?.holds_state(&tree) {
        return unsupported("the staff operations are another tree than the row's");
    }
    NamespaceMirror::with_row::<StaffOperationsRow, _>(&mirror, |row| Ok(row.completion_agrees(task_id, minute, receipt, history_truncated)))
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("ROW", Py::new(py, RowType::of::<StaffOperationsRow>("staff_operations"))?)?;
    m.add_function(wrap_pyfunction!(validate, m)?)?;
    Ok(())
}
