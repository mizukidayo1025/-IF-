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

use crate::error::Result;
use crate::json::{write_json_string, Encoder};
use crate::row::{Field, IdMap};
use crate::state_row;

state_row! {
    #[derive(Clone, Copy)]
    pub struct Point {
        x: f64,
        y: f64,
    }
}

state_row! {
    #[derive(Clone)]
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

impl Field for Journey {
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
