//! `movement_v1`: where people and staff are and the trips under way (`domain/movement.py`). The
//! rules that run every tick are `rules/movement.rs`; the others write one record at a time through
//! the forwarded writes (docs/rust-port/lanes/i-movement.md, m4-movement.md).
//!
//! A value of another type than the rules write taints the namespace, and the Python rules and
//! their errors run on the tree: an int `progress` or an int coordinate, which validation accepts,
//! only costs the row.
//!
//! The PyO3 crate's `state/movement.rs` reads it from Python.

use std::sync::Arc;

use crate::error::{Misfit, Result};
use crate::json::{write_json_string, Encoder};
use crate::row::{Field, IdMap};
use crate::state_row;
use crate::value::V;

state_row! {
    #[derive(Copy)]
    pub struct Point {
        x: f64,
        y: f64,
    }
}

state_row! {
    pub struct Position {
        zone_id: Arc<str>,
        point: Option<Point>,
        edge_id: Option<Arc<str>>,
        progress: f64,
        position_kind: Arc<str>,
    }
}

/// A trip. The last three fields are absent from the record when they are `None`, which `state_row!`
/// cannot say: only queue trips have `queue_id`, and only staff support trips the other two. A
/// null there fits no rule (`validate_movement_runtime_state`), so it is a `Misfit` here.
#[derive(Clone)]
pub struct Journey {
    pub event_id: Arc<str>,
    pub task_id: Arc<str>,
    pub purpose: Arc<str>,
    pub facility_id: Option<Arc<str>>,
    pub destination_zone_id: Arc<str>,
    pub route_zone_ids: Vec<Arc<str>>,
    pub route_edge_ids: Vec<Arc<str>>,
    pub route_points: Vec<Point>,
    pub started_minute: i64,
    pub due_minute: i64,
    pub queue_id: Option<Arc<str>>,
    pub support_request_id: Option<Arc<str>>,
    pub support_staff_id: Option<Arc<str>>,
}

impl Journey {
    fn write_optional(encoder: &mut Encoder, name: &str, value: &Option<Arc<str>>) -> Result<()> {
        if let Some(value) = value {
            encoder.out.push(b',');
            write_json_string(&mut encoder.out, name);
            encoder.out.push(b':');
            Field::write_json(value, encoder)?;
        }
        Ok(())
    }
}

/// The fields every journey has.
const JOURNEY_REQUIRED_FIELDS: usize = 10;

impl Field for Journey {
    /// No map inside: the route lists keep their order.
    fn canonicalize(&mut self) {}

    /// The ten fields every journey has, and the last three where present. `Arc<str>` is what
    /// refuses a null there.
    fn from_value(value: &V) -> std::result::Result<Self, Misfit> {
        let V::Map(record) = value else {
            return Err(Misfit::new("not a dict"));
        };
        fn required<T: Field>(record: &indexmap::IndexMap<Arc<str>, V>, key: &str) -> std::result::Result<T, Misfit> {
            T::from_value(record.get(key).ok_or_else(|| Misfit::new("a journey field is missing"))?)
        }
        let mut found = JOURNEY_REQUIRED_FIELDS;
        let mut optional = |key: &str| -> std::result::Result<Option<Arc<str>>, Misfit> {
            match record.get(key) {
                Some(value) => {
                    found += 1;
                    Ok(Some(Field::from_value(value)?))
                }
                None => Ok(None),
            }
        };
        let journey = Self {
            event_id: required(record, "event_id")?,
            task_id: required(record, "task_id")?,
            purpose: required(record, "purpose")?,
            facility_id: required(record, "facility_id")?,
            destination_zone_id: required(record, "destination_zone_id")?,
            route_zone_ids: required(record, "route_zone_ids")?,
            route_edge_ids: required(record, "route_edge_ids")?,
            route_points: required(record, "route_points")?,
            started_minute: required(record, "started_minute")?,
            due_minute: required(record, "due_minute")?,
            queue_id: optional("queue_id")?,
            support_request_id: optional("support_request_id")?,
            support_staff_id: optional("support_staff_id")?,
        };
        if record.len() != found {
            return Err(Misfit::new("other fields than the row's"));
        }
        Ok(journey)
    }

    fn write_json(&self, encoder: &mut Encoder) -> Result<()> {
        fn field<T: Field>(encoder: &mut Encoder, name: &str, value: &T) -> Result<()> {
            write_json_string(&mut encoder.out, name);
            encoder.out.push(b':');
            value.write_json(encoder)
        }
        // The keys in byte order, as canonical_json has them.
        encoder.out.push(b'{');
        field(encoder, "destination_zone_id", &self.destination_zone_id)?;
        encoder.out.push(b',');
        field(encoder, "due_minute", &self.due_minute)?;
        encoder.out.push(b',');
        field(encoder, "event_id", &self.event_id)?;
        encoder.out.push(b',');
        field(encoder, "facility_id", &self.facility_id)?;
        encoder.out.push(b',');
        field(encoder, "purpose", &self.purpose)?;
        Self::write_optional(encoder, "queue_id", &self.queue_id)?;
        encoder.out.push(b',');
        field(encoder, "route_edge_ids", &self.route_edge_ids)?;
        encoder.out.push(b',');
        field(encoder, "route_points", &self.route_points)?;
        encoder.out.push(b',');
        field(encoder, "route_zone_ids", &self.route_zone_ids)?;
        encoder.out.push(b',');
        field(encoder, "started_minute", &self.started_minute)?;
        Self::write_optional(encoder, "support_request_id", &self.support_request_id)?;
        Self::write_optional(encoder, "support_staff_id", &self.support_staff_id)?;
        encoder.out.push(b',');
        field(encoder, "task_id", &self.task_id)?;
        encoder.out.push(b'}');
        Ok(())
    }
}

state_row! {
    pub struct MovementRow {
        schema_version: i64,
        policy_version: Arc<str>,
        positions: IdMap<Position>,
        staff_positions: IdMap<Position>,
        journeys: IdMap<Journey>,
        edge_usage_minutes: IdMap<i64>,
        last_updated_minute: i64 => set_last_updated_minute,
    }
}
