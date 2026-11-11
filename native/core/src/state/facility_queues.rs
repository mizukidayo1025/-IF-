//! `facility_queues_v1`: the independent FIFO of each service point (`domain/facility_queues.py`).
//! `rules/facility_queues.rs` reads the row and writes through the tree, whose writes reach it, or
//! the row itself while the namespace is detached. A value of another type than the rules write
//! taints the namespace, and the Python rules and their errors run on the tree.

use std::sync::Arc;

use crate::row::IdMap;
use crate::state_row;

pub const NAME: &str = "facility_queues_v1";

state_row! {
    pub struct QueueStall {
        minute: i64,
        request_id: Arc<str>,
        reason: Arc<str>,
    }
}

state_row! {
    pub struct QueueRow {
        facility_id: Arc<str>,
        service_point_id: Arc<str>,
        queued_request_ids: Vec<Arc<str>>,
        active_request_ids: Vec<Arc<str>>,
        last_stall: Option<QueueStall>,
    }
}

state_row! {
    pub struct QueueRequest {
        request_id: Arc<str>,
        queue_id: Arc<str>,
        service_point_id: Arc<str>,
        facility_id: Arc<str>,
        agent_id: Arc<str>,
        service_kind: Arc<str>,
        requested_minute: i64,
        status: Arc<str>,
        queue_task_id: Arc<str>,
        active_task_id: Arc<str>,
    }
}

state_row! {
    pub struct QueuesRow {
        schema_version: i64,
        policy_version: Arc<str>,
        queues: IdMap<QueueRow>,
        requests: IdMap<QueueRequest>,
    }
}
