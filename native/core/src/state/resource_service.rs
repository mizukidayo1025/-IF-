//! `resource_service_v1`: what the staffed water and meal service holds (`domain/resource_service.py`).
//! `rules/service.rs` writes the row alone, and an attached tree follows it (lane c3 of #152); a
//! Python write reaches the row through the forwarded writes (docs/rust-port/lanes/m4-service.md).
//!
//! A value of another type than the rules write taints the namespace, and the rules refuse it.
//!
//! The PyO3 crate's `state/resource_service.rs` reads it from Python and holds its validator.

use std::sync::Arc;

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
