//! `resource_service_v1`: what the staffed water and meal service holds (`domain/resource_service.py`).
//! `rules/service.rs` writes the row alone, and an attached tree follows it (lane c3 of #152); a
//! Python write reaches the row through the forwarded writes (docs/rust-port/lanes/m4-service.md).
//!
//! A value of another type than the rules write taints the namespace, and the rules refuse it.
//!
//! The PyO3 crate's `state/resource_service.rs` reads it from Python and holds its validator.

use std::collections::HashMap;
use std::sync::Arc;

use crate::entities::Entities;
use crate::row::IdMap;
use crate::state_row;

pub const NAME: &str = "resource_service_v1";
pub const SCHEMA_VERSION: i64 = 1;

state_row! {
    pub struct RequestRecord {
        minute: i64,
        agent_id: Arc<str>,
        action: Arc<str>,
        outcome: Arc<str>,
        reason: Arc<str>,
        facility_id: Option<Arc<str>>,
        origin_zone_id: Arc<str>,
        detail: Option<Arc<str>>,
    }
}

state_row! {
    pub struct StallRecord {
        minute: i64,
        facility_id: Arc<str>,
        agent_id: Arc<str>,
        reason: Arc<str>,
    }
}

state_row! {
    pub struct Stall {
        agent_id: Arc<str>,
        reason: Arc<str>,
    }
}

state_row! {
    pub struct ServiceRow {
        schema_version: i64,
        cancelled_task_ids: Vec<Arc<str>>,
        active_reservation_ids: Vec<Arc<str>>,
        request_history: Vec<RequestRecord>,
        queue_stall_history: Vec<StallRecord>,
        stall_state: IdMap<Stall>,
    }
}

pub fn listed_once(ids: &[Arc<str>]) -> bool {
    // A handful of IDs at a time for the active ones. The cancelled ones grow only with cancellations.
    let mut sorted: Vec<&str> = ids.iter().map(|id| &**id).collect();
    sorted.sort_unstable();
    sorted.iter().all(|id| !id.is_empty()) && sorted.windows(2).all(|pair| pair[0] != pair[1])
}

pub fn request_outcome_is_known(record: &RequestRecord) -> bool {
    matches!(
        (&*record.outcome, &*record.reason),
        ("queued", "arrived_at_facility")
            | ("travel_scheduled", "route_available" | "in_room_route_available")
            | (
                "rejected",
                "room_route_blocked" | "no_facility" | "no_route" | "request_rejected" | "travel_rejected" | "service_disabled"
            )
    )
}

pub fn stall_reason_is_known(reason: &str) -> bool {
    matches!(reason, "capacity" | "staff" | "stock" | "run_end" | "water_outage")
}

/// `records_since_previous_tick`: the newest records, from the last one before the previous tick's.
pub fn window_start<T>(history: &[T], minute_of: impl Fn(&T) -> i64, minute_index: i64) -> usize {
    let mut start = history.len();
    while start > 0 && minute_of(&history[start - 1]) >= minute_index - 1 {
        start -= 1;
    }
    start.saturating_sub(1)
}

/// What `ServiceRow::passes_rules` reads of the world beside the entity rows: whether a person is
/// here or archived (`population_lifecycle_v1.people`), and whether a zone is the world's. A reader
/// that cannot answer fails, and the check fails with its error.
pub trait ServiceWorld {
    type Error;
    fn person(&self, id: &str) -> std::result::Result<bool, Self::Error>;
    fn zone(&self, id: &str) -> std::result::Result<bool, Self::Error>;
}

impl ServiceRow {
    /// The rules of a tick's check in `WorldState.validate` (#152; in the core since #250), for a
    /// world of these rows, people and zones, at this minute; `heads` are the heads of the
    /// independent queues of each facility that has them. True says the Python rules accept; false
    /// decides nothing.
    pub fn passes_rules<W: ServiceWorld>(&self, rows: &Entities, known: &W, minute_index: i64, heads: &HashMap<Arc<str>, Vec<Arc<str>>>) -> std::result::Result<bool, W::Error> {
        let row = self;
        if row.schema_version != SCHEMA_VERSION
            || !listed_once(&row.cancelled_task_ids)
            || !listed_once(&row.active_reservation_ids)
        {
            return Ok(false);
        }
        let from = window_start(&row.request_history, |record| record.minute, minute_index);
        for record in &row.request_history[from..] {
            let expected_kind = match &*record.action {
                "fetch_water" => "water_station",
                "collect_meal" => "meal_distribution",
                _ => return Ok(false),
            };
            if !(0..=minute_index).contains(&record.minute)
                || !request_outcome_is_known(record)
                || !known.person(&record.agent_id)?
                || !known.zone(&record.origin_zone_id)?
            {
                return Ok(false);
            }
            if let Some(facility_id) = &record.facility_id {
                if rows.facilities.get(facility_id).is_none_or(|facility| &*facility.kind != expected_kind) {
                    return Ok(false);
                }
            }
        }
        let from = window_start(&row.queue_stall_history, |record| record.minute, minute_index);
        let mut previous = -1;
        for record in &row.queue_stall_history[from..] {
            if record.minute < 0
                || record.minute < previous
                || record.minute > minute_index
                || rows.facilities.get(&record.facility_id).is_none()
                || !known.person(&record.agent_id)?
                || !stall_reason_is_known(&record.reason)
            {
                return Ok(false);
            }
            previous = record.minute;
        }
        for (facility_id, record) in &row.stall_state {
            let Some(facility) = rows.facilities.get(facility_id) else {
                return Ok(false);
            };
            if !matches!(&*facility.kind, "water_station" | "meal_distribution")
                || rows.agents.get(&record.agent_id).is_none()
                || !stall_reason_is_known(&record.reason)
                || !(match heads.get(facility_id) {
                    Some(heads) => heads.contains(&record.agent_id),
                    None => facility.resource_queue_agent_ids.first() == Some(&record.agent_id),
                })
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
