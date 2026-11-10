//! `movement_v1`: the row is the core's (`sheltersim_core::state::movement`); here is its Python
//! side. A journey's last three fields are absent from the record when they are `None`, which
//! `py_row!` cannot say, so its `PyField` is written out.

use std::sync::Arc;

use pyo3::intern;
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyString};

pub(crate) use sheltersim_core::state::movement::*;

use crate::core::row::{load_row, py_row, PyField};
use crate::core::tracked::as_readable_dict;
use crate::core::world_core::{Misfit, OrMisfit, Part, RowType, Write};

py_row! { Point { x, y } }
py_row! { Position { zone_id, point, edge_id, progress, position_kind } }
py_row! {
    MovementRow {
        schema_version, policy_version, positions, staff_positions, journeys, edge_usage_minutes,
        last_updated_minute,
    }
}

const JOURNEY_REQUIRED_FIELDS: usize = 10;

/// A present field of the record that may be absent. `Arc<str>` is what refuses a null.
fn optional(record: &Bound<'_, PyDict>, key: &Bound<'_, PyString>, found: &mut usize) -> Result<Option<Arc<str>>, Misfit> {
    match record.get_item(key).or_misfit()? {
        Some(value) => {
            *found += 1;
            Ok(Some(PyField::from_py(&value)?))
        }
        None => Ok(None),
    }
}

fn required<T: PyField>(record: &Bound<'_, PyDict>, key: &Bound<'_, PyString>) -> Result<T, Misfit> {
    T::from_py(&record.get_item(key).or_misfit()?.ok_or_else(|| Misfit::new("a journey field is missing"))?)
}

impl PyField for Journey {
    fn from_py(value: &Bound<'_, PyAny>) -> Result<Self, Misfit> {
        let py = value.py();
        let record = as_readable_dict(value).ok_or_else(|| Misfit::new("not a dict"))?;
        let mut found = JOURNEY_REQUIRED_FIELDS;
        let journey = Self {
            event_id: required(&record, intern!(py, "event_id"))?,
            task_id: required(&record, intern!(py, "task_id"))?,
            purpose: required(&record, intern!(py, "purpose"))?,
            facility_id: required(&record, intern!(py, "facility_id"))?,
            destination_zone_id: required(&record, intern!(py, "destination_zone_id"))?,
            route_zone_ids: required(&record, intern!(py, "route_zone_ids"))?,
            route_edge_ids: required(&record, intern!(py, "route_edge_ids"))?,
            route_points: required(&record, intern!(py, "route_points"))?,
            started_minute: required(&record, intern!(py, "started_minute"))?,
            due_minute: required(&record, intern!(py, "due_minute"))?,
            queue_id: optional(&record, intern!(py, "queue_id"), &mut found)?,
            support_request_id: optional(&record, intern!(py, "support_request_id"), &mut found)?,
            support_staff_id: optional(&record, intern!(py, "support_staff_id"), &mut found)?,
        };
        if record.len() != found {
            return Err(Misfit::new("other fields than the row's"));
        }
        Ok(journey)
    }

    fn to_py<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        // The order of the record `start_movement` writes.
        let record = PyDict::new(py);
        record.set_item(intern!(py, "event_id"), PyField::to_py(&self.event_id, py)?)?;
        record.set_item(intern!(py, "task_id"), PyField::to_py(&self.task_id, py)?)?;
        record.set_item(intern!(py, "purpose"), PyField::to_py(&self.purpose, py)?)?;
        record.set_item(intern!(py, "facility_id"), PyField::to_py(&self.facility_id, py)?)?;
        record.set_item(intern!(py, "destination_zone_id"), PyField::to_py(&self.destination_zone_id, py)?)?;
        record.set_item(intern!(py, "route_zone_ids"), PyField::to_py(&self.route_zone_ids, py)?)?;
        record.set_item(intern!(py, "route_edge_ids"), PyField::to_py(&self.route_edge_ids, py)?)?;
        record.set_item(intern!(py, "route_points"), PyField::to_py(&self.route_points, py)?)?;
        record.set_item(intern!(py, "started_minute"), PyField::to_py(&self.started_minute, py)?)?;
        record.set_item(intern!(py, "due_minute"), PyField::to_py(&self.due_minute, py)?)?;
        if let Some(value) = &self.queue_id {
            record.set_item(intern!(py, "queue_id"), PyField::to_py(value, py)?)?;
        }
        if let Some(value) = &self.support_request_id {
            record.set_item(intern!(py, "support_request_id"), PyField::to_py(value, py)?)?;
        }
        if let Some(value) = &self.support_staff_id {
            record.set_item(intern!(py, "support_staff_id"), PyField::to_py(value, py)?)?;
        }
        Ok(record.into_any())
    }

    fn apply(&mut self, path: &[Part<'_>], write: &Write<'_, '_>) -> Result<(), Misfit> {
        match (path, write) {
            ([], Write::Set { key: Part::Key(key), value }) => {
                match *key {
                    "event_id" => self.event_id = PyField::from_py(value)?,
                    "task_id" => self.task_id = PyField::from_py(value)?,
                    "purpose" => self.purpose = PyField::from_py(value)?,
                    "facility_id" => self.facility_id = PyField::from_py(value)?,
                    "destination_zone_id" => self.destination_zone_id = PyField::from_py(value)?,
                    "route_zone_ids" => self.route_zone_ids = PyField::from_py(value)?,
                    "route_edge_ids" => self.route_edge_ids = PyField::from_py(value)?,
                    "route_points" => self.route_points = PyField::from_py(value)?,
                    "started_minute" => self.started_minute = PyField::from_py(value)?,
                    "due_minute" => self.due_minute = PyField::from_py(value)?,
                    "queue_id" => self.queue_id = Some(PyField::from_py(value)?),
                    "support_request_id" => self.support_request_id = Some(PyField::from_py(value)?),
                    "support_staff_id" => self.support_staff_id = Some(PyField::from_py(value)?),
                    _ => return Err(Misfit::new(format!("no field {key:?}"))),
                }
                Ok(())
            }
            ([], Write::Delete { key: Part::Key(key) }) => {
                match *key {
                    "queue_id" if self.queue_id.is_some() => self.queue_id = None,
                    "support_request_id" if self.support_request_id.is_some() => self.support_request_id = None,
                    "support_staff_id" if self.support_staff_id.is_some() => self.support_staff_id = None,
                    _ => return Err(Misfit::new("the write changes which fields a record has")),
                }
                Ok(())
            }
            ([Part::Key("route_zone_ids"), rest @ ..], write) => self.route_zone_ids.apply(rest, write),
            ([Part::Key("route_edge_ids"), rest @ ..], write) => self.route_edge_ids.apply(rest, write),
            ([Part::Key("route_points"), rest @ ..], write) => self.route_points.apply(rest, write),
            _ => Err(Misfit::new("the write does not fit a journey")),
        }
    }
}

pub fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    let py = m.py();
    m.add("ROW", Py::new(py, RowType::new("movement", load_row::<MovementRow>))?)?;
    Ok(())
}
