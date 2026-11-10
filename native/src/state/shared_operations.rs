//! `shared_operations_v1`: the row is the core's (`sheltersim_core::state::shared_operations`); here
//! are its Python side and the check of a tick, which reads the event log, the support requests and
//! `staff_operations_v1`.

use std::collections::VecDeque;
use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyList, PyString};

pub(crate) use sheltersim_core::state::shared_operations::*;

use crate::core::row::{exact_int, load_row, namespace_is_absent, py_row, py_str_enum, PyField, PyItems};
use crate::core::tracked::{as_readable_dict, as_readable_list};
use crate::core::world_core::{loads_items, mirror_of, Misfit, NamespaceMirror, OrMisfit, Part, RowType, Write};
use crate::encode;
use crate::state::entities::{entities_of, EntityStore, RequestStatus};
use crate::state::events::EventWriter;

py_str_enum!(CareStatus);
py_row! {
    CareHead {
        request_id, agent_id, household_id, support_kind, status, created_minute, updated_minute,
        latest_event_id,
    }
}
py_row! { SharedOperationsRow { schema_version, policy_version, support_requests, history, history_truncated } }

impl PyField for Heads {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        Ok(Self::unchecked(PyField::from_py(value)?))
    }

    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        self.map.to_py(py)
    }

    fn apply(&mut self, path: &[Part<'_>], write: &Write<'_, '_>) -> Result<(), Misfit> {
        self.mark_unchecked();
        self.map.apply(path, write)
    }
}

fn receipt_from(value: &Bound<'_, PyAny>) -> Result<Receipt, Misfit> {
    let py = value.py();
    let mut receipt = Receipt {
        minute: None,
        event_id: None,
        verified: false,
        reads_staff_history: true,
        bytes: encode(value).or_misfit()?.into_boxed_slice(),
    };
    if let Some(record) = as_readable_dict(value) {
        if let Some(minute) = record.get_item(intern!(py, "minute")).or_misfit()? {
            receipt.minute = exact_int(&minute).ok();
        }
        if let Some(event_id) = record.get_item(intern!(py, "event_id")).or_misfit()? {
            if let Ok(event_id) = event_id.downcast_exact::<PyString>() {
                receipt.event_id = Some(Arc::from(event_id.to_str().or_misfit()?));
            }
        }
        if let Some(event_type) = record.get_item(intern!(py, "event_type")).or_misfit()? {
            receipt.reads_staff_history = !event_type
                .downcast_exact::<PyString>()
                .is_ok_and(|text| text.to_str().is_ok_and(|text| text != "staff_operation_transition"));
        }
    }
    Ok(receipt)
}

impl PyItems for Receipts {
    fn slice_to_py<'py>(&self, py: Python<'py>, start: usize, stop: usize) -> PyResult<Bound<'py, PyList>> {
        loads_items(py, self.items.range(start..stop).map(|item| &*item.bytes))
    }
}

impl PyField for Receipts {
    fn py_items(&self) -> Option<&dyn PyItems> {
        Some(self)
    }

    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        let list = as_readable_list(value).ok_or_else(|| Misfit::new("not a list"))?;
        let mut items = VecDeque::with_capacity(list.len());
        for item in list.iter() {
            items.push_back(receipt_from(&item)?);
        }
        Ok(Self { items })
    }

    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        let loads = py.import(intern!(py, "json"))?.getattr(intern!(py, "loads"))?;
        let list = PyList::empty(py);
        for item in &self.items {
            list.append(loads.call1((std::str::from_utf8(&item.bytes)?,))?)?;
        }
        Ok(list.into_any())
    }

    fn apply(&mut self, path: &[Part<'_>], write: &Write<'_, '_>) -> Result<(), Misfit> {
        match (path, write) {
            ([], Write::Append { value }) => self.items.push_back(receipt_from(value)?),
            ([], Write::Extend { values }) => {
                for value in values.iter() {
                    self.items.push_back(receipt_from(&value)?);
                }
            }
            ([], Write::DeleteRange { start, stop }) if start <= stop && *stop <= self.items.len() => {
                self.items.drain(*start..*stop);
            }
            ([], Write::Delete { key: Part::Index(index) }) if *index < self.items.len() => {
                self.items.remove(*index);
            }
            ([], Write::Set { key: Part::Index(index), value }) if *index < self.items.len() => {
                self.items[*index] = receipt_from(value)?;
            }
            ([], Write::Insert { index, value }) if *index <= self.items.len() => {
                self.items.insert(*index, receipt_from(value)?);
            }
            ([], Write::Clear) => self.items.clear(),
            ([], Write::Reverse) => self.items.make_contiguous().reverse(),
            ([], Write::ReplaceAll { items }) => {
                self.items = items.iter().map(|item| receipt_from(&item)).collect::<Result<_, _>>()?;
            }
            _ => return Err(Misfit::new("the write does not fit the history")),
        }
        Ok(())
    }
}

/// `validate_shared_operations_state` of a tick (no replay of the history): accepts, or leaves it
/// to the Python rules and their error text.
#[pyfunction]
fn validate_tick(world: &Bound<'_, PyAny>) -> PyResult<bool> {
    let py = world.py();
    if namespace_is_absent(world, intern!(py, NAME))? {
        return Ok(true);
    }
    let minute = exact_int(&world.getattr(intern!(py, "minute_index"))?)?;
    let writer = EventWriter::of(world)?;
    let store = entities_of(world)?;
    let mirror = mirror_of(world, NAME)?;
    // (the request, the event that says who has it) of each head with a staff member.
    let mut assigned = Vec::new();
    // The recent staff operation receipts, which `_validate_staff_operation_receipt` reads again.
    let mut staff_receipts: Vec<Box<[u8]>> = Vec::new();
    let mut truncated = false;
    let accepted = NamespaceMirror::with_row::<SharedOperationsRow, _>(&mirror, |row| {
        if !row.header_is_valid() || row.history.items.len() > MAX_HISTORY {
            return Ok(false);
        }
        let Some(recent) = row.history.recent_verified(minute) else {
            return Ok(false);
        };
        let log = writer.log().borrow();
        let mut seen: Vec<&str> = Vec::new();
        for item in recent {
            let Some(event_id) = item.event_id.as_deref() else {
                return Ok(false);
            };
            if seen.contains(&event_id) || !log.has_event_id(event_id) {
                return Ok(false);
            }
            seen.push(event_id);
            if item.reads_staff_history {
                staff_receipts.push(item.bytes.clone());
            }
        }
        truncated = row.history_truncated;
        EntityStore::with_rows(&store, |rows| {
            for (id, head) in &row.support_requests.map {
                let Some(request) = rows.support_requests.get(id) else {
                    return Ok(false);
                };
                let expected = if head.status == CareStatus::Requested {
                    RequestStatus::Pending
                } else {
                    RequestStatus::Assigned
                };
                if request.agent_id != head.agent_id || request.status != expected {
                    return Ok(false);
                }
                if head.status != CareStatus::Requested {
                    assigned.push((request.assigned_staff_id.clone(), head.latest_event_id.clone()));
                }
            }
            Ok(true)
        })
    })?;
    if !accepted {
        return Ok(false);
    }
    for bytes in &staff_receipts {
        let Ok(receipt) = serde_json::from_slice::<serde_json::Value>(bytes) else {
            return Ok(false);
        };
        if !crate::state::staff_operations::completion_agrees(world, &receipt, truncated)? {
            return Ok(false);
        }
    }
    if !assigned.is_empty() {
        let log = world.getattr(intern!(py, "event_log"))?;
        let (find, event_name, payload_name, staff_name) =
            (intern!(py, "find"), intern!(py, "event"), intern!(py, "payload"), intern!(py, "staff_id"));
        for (staff, event_id) in assigned {
            let record = log.call_method1(find, (&*event_id,))?;
            let evidence = if record.is_none() {
                None
            } else {
                let payload = record.getattr(event_name)?.getattr(payload_name)?;
                let value = payload.get_item(staff_name).ok();
                match value {
                    Some(value) if !value.is_none() => Some(value),
                    _ => None,
                }
            };
            let same = match (&evidence, &staff) {
                (None, None) => true,
                (Some(value), Some(staff)) => {
                    value.downcast_exact::<PyString>().is_ok_and(|text| text.to_str().is_ok_and(|text| text == &**staff))
                }
                _ => false,
            };
            if !same {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// The Python rules have just accepted this world's state at its minute, for a tick.
#[pyfunction]
fn recent_passed(world: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = world.py();
    if namespace_is_absent(world, intern!(py, NAME))? {
        return Ok(());
    }
    let minute = exact_int(&world.getattr(intern!(py, "minute_index"))?)?;
    NamespaceMirror::with_row::<SharedOperationsRow, _>(&mirror_of(world, NAME)?, |row| {
        row.history.recent_passed(minute);
        Ok(())
    })
}

/// The Python rules have just replayed this world's whole history and passed it.
#[pyfunction]
fn history_passed(world: &Bound<'_, PyAny>) -> PyResult<()> {
    let py = world.py();
    if namespace_is_absent(world, intern!(py, NAME))? {
        return Ok(());
    }
    NamespaceMirror::with_row::<SharedOperationsRow, _>(&mirror_of(world, NAME)?, |row| {
        row.history.all_passed();
        Ok(())
    })
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("ROW", Py::new(py, RowType::new("shared_operations", load_row::<SharedOperationsRow>))?)?;
    m.add_function(wrap_pyfunction!(validate_tick, m)?)?;
    m.add_function(wrap_pyfunction!(recent_passed, m)?)?;
    m.add_function(wrap_pyfunction!(history_passed, m)?)?;
    Ok(())
}
