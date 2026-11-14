//! `staff_operations_v1`: the sanitation of each zone, the routed task of each staff member and the
//! finished tasks (`domain/staff_operations.py`). The rules that run every tick are
//! `rules/staff.rs`. The rules of a task's events write one value or one record at a time through
//! the forwarded writes (docs/rust-port/lanes/m4-staff.md).
//!
//! A value of another type than the rules write taints the namespace, and the Python rules and
//! their errors run on the tree: an int `sanitation`, which validation accepts, only costs the row.
//!
//! The PyO3 crate's `state/staff_operations.rs` reads it from Python and holds its validator, whose
//! checks of an active record read the objects and the pending events.

use std::collections::HashSet;
use std::sync::Arc;

use crate::entities::Entities;
use crate::row::IdMap;
use crate::value::V;
use crate::{state_row, str_enum};

pub const NAME: &str = "staff_operations_v1";
const POLICY: &str = "autonomous-routed-staff-v1";
const SOURCE: &str = "ASM-STAFF-OPERATIONS-V1";
const MAX_HISTORY: usize = 20_000;
const TASK_KINDS: [&str; 5] = ["cleaning", "maintenance", "information_outreach", "intake", "patrol"];
const THRESHOLDS: [&str; 3] =
    ["cleaning_sanitation_below", "outreach_uncertainty_at_least", "patrol_stress_at_least"];
/// A global rate of the config and the upper bound of the same rate in a zone.
const EFFECTS: [(&str, f64); 4] = [
    ("sanitation_use_per_person_minute", 0.01),
    ("cleaning_sanitation_recovery", 1.0),
    ("outreach_uncertainty_relief", 1.0),
    ("patrol_reassurance", 1.0),
];
const MAX_TASK_MINUTES: i64 = 24 * 60;

str_enum! {
    pub enum Phase { Idle = "idle", Travel = "travel", Working = "working" }
}

str_enum! {
    pub enum Outcome { Completed = "completed", Blocked = "blocked", Interrupted = "interrupted" }
}

state_row! {
    #[derive(Copy)]
    pub struct ZoneRecord {
        sanitation: f64 => set_sanitation,
        updated_minute: i64 => set_updated_minute,
    }
}

state_row! {
    pub struct OperationRecord {
        phase: Phase,
        task_id: Option<Arc<str>>,
        task_kind: Option<Arc<str>>,
        target_id: Option<Arc<str>>,
        target_zone_id: Option<Arc<str>>,
        reason: Option<Arc<str>>,
        started_minute: Option<i64>,
        travel_due_minute: Option<i64>,
        arrival_minute: Option<i64>,
        due_minute: Option<i64>,
        route_zone_ids: Vec<Arc<str>>,
        route_edge_ids: Vec<Arc<str>>,
        travel_minutes: i64,
        causation_event_ids: Vec<Arc<str>>,
    }
}

state_row! {
    pub struct FinishedTask {
        task_id: Arc<str>,
        staff_id: Arc<str>,
        task_kind: Arc<str>,
        target_id: Option<Arc<str>>,
        target_zone_id: Arc<str>,
        reason: Arc<str>,
        status: Outcome,
        start_minute: i64,
        arrival_minute: Option<i64>,
        complete_minute: i64,
        travel_minutes: i64,
        causation_event_ids: Vec<Arc<str>>,
    }
}

state_row! {
    pub struct StaffOperationsRow {
        schema_version: i64,
        policy_version: Arc<str>,
        source_id: Arc<str>,
        config: V,
        zones: IdMap<ZoneRecord>,
        staff: IdMap<OperationRecord>,
        history: Vec<FinishedTask>,
    }
}

impl OperationRecord {
    /// `record == _idle_record()`, when `phase` is idle.
    fn is_the_idle_record(&self) -> bool {
        self.task_id.is_none()
            && self.task_kind.is_none()
            && self.target_id.is_none()
            && self.target_zone_id.is_none()
            && self.reason.is_none()
            && self.started_minute.is_none()
            && self.travel_due_minute.is_none()
            && self.arrival_minute.is_none()
            && self.due_minute.is_none()
            && self.route_zone_ids.is_empty()
            && self.route_edge_ids.is_empty()
            && self.travel_minutes == 0
            && self.causation_event_ids.is_empty()
    }
}

impl ZoneRecord {
    fn is_valid(&self) -> bool {
        (0.0..=1.0).contains(&self.sanitation) && self.updated_minute >= 0
    }
}

impl FinishedTask {
    fn is_valid(&self, rows: &Entities, zones: &HashSet<String>) -> bool {
        let times = self.start_minute >= 0
            && self.complete_minute >= self.start_minute
            && self.travel_minutes >= 0
            && match self.arrival_minute {
                None => self.status == Outcome::Interrupted && self.travel_minutes > 0,
                Some(arrival) => self.start_minute <= arrival && arrival <= self.complete_minute,
            };
        times
            && rows.staff.get(&self.staff_id).is_some()
            && TASK_KINDS.contains(&&*self.task_kind)
            && zones.contains(&*self.target_zone_id)
            && self.target_id.as_deref().is_none_or(|id| rows.facilities.get(id).is_some())
            && self.causation_event_ids.iter().all(|id| !id.is_empty())
    }
}

fn number_in(value: &V, lower: f64, upper: f64) -> bool {
    match value {
        // The bounds are small, so an int compares as its float does.
        V::Int(number) => lower <= *number as f64 && *number as f64 <= upper,
        V::Float(number) => lower <= *number && *number <= upper,
        _ => false,
    }
}

/// The per-zone table of the config: each key a non-empty zone with a non-empty entry that passes.
fn zone_table_is_valid(table: &V, zones: &HashSet<String>, entry_is_valid: impl Fn(&str, &V) -> bool) -> bool {
    let V::Map(table) = table else {
        return false;
    };
    table.iter().all(|(zone, entry)| {
        let V::Map(entry) = entry else {
            return false;
        };
        !zone.is_empty()
            && zones.contains(&**zone)
            && !entry.is_empty()
            && entry.iter().all(|(name, value)| entry_is_valid(name, value))
    })
}

/// Whether `canonical_staff_operations_config` passes this config and leaves it as it is, or
/// differs only by the additive tables an older config doesn't have, and its tables name known zones.
fn config_is_canonical(config: &V, zones: &HashSet<String>) -> bool {
    let V::Map(config) = config else {
        return false;
    };
    if config.len() > 10 {
        return false;
    }
    let text = |name: &str, expected: &str| matches!(config.get(name), Some(V::Str(text)) if &**text == expected);
    let durations_are_valid = matches!(config.get("task_duration_minutes"), Some(V::Map(durations))
        if durations.len() == TASK_KINDS.len()
            && TASK_KINDS.iter().all(|kind| {
                matches!(durations.get(*kind), Some(V::Int(minutes)) if (1..=MAX_TASK_MINUTES).contains(minutes))
            }));
    let mut known = 8;
    if !(matches!(config.get("schema_version"), Some(V::Int(1)))
        && text("policy_version", POLICY)
        && text("source_id", SOURCE)
        && durations_are_valid
        && EFFECTS.iter().all(|(name, _)| config.get(*name).is_some_and(|value| number_in(value, 0.0, 1.0))))
    {
        return false;
    }
    if let Some(table) = config.get("thresholds_by_zone") {
        known += 1;
        if !zone_table_is_valid(table, zones, |name, value| {
            THRESHOLDS.contains(&name) && number_in(value, 0.0, 1.0)
        }) {
            return false;
        }
    }
    if let Some(table) = config.get("effects_by_zone") {
        known += 1;
        if !zone_table_is_valid(table, zones, |name, value| {
            EFFECTS.iter().any(|(effect, upper)| *effect == name && number_in(value, 0.0, *upper))
        }) {
            return false;
        }
    }
    config.len() == known
}

/// What an active record says, read out of the row so that the objects can be read after it.
pub struct Active {
    pub staff_id: String,
    pub working: bool,
    pub task_id: String,
    pub task_kind: String,
    pub target_id: Option<String>,
    pub target_zone_id: String,
    pub started: i64,
    pub travel_due: Option<i64>,
    pub arrival: Option<i64>,
    pub due: i64,
    pub route_zones: Vec<String>,
    pub route_edges: Vec<String>,
    pub travel_minutes: i64,
    /// `config["task_duration_minutes"][task_kind]`.
    pub duration: i64,
}

/// The fields of an active record that `validate_staff_operations_state` reads of it alone, or None
/// if one fails, which is then the Python rule's to say.
fn active_of(staff_id: &str, record: &OperationRecord, config: &V) -> Option<Active> {
    let (Some(task_id), Some(task_kind), Some(target_zone_id), Some(reason), Some(started), Some(due)) = (
        record.task_id.as_deref(),
        record.task_kind.as_deref(),
        record.target_zone_id.as_deref(),
        record.reason.as_deref(),
        record.started_minute,
        record.due_minute,
    ) else {
        return None;
    };
    let V::Map(config) = config else { return None };
    let Some(V::Map(durations)) = config.get("task_duration_minutes") else { return None };
    let Some(V::Int(duration)) = durations.get(task_kind) else { return None };
    if !TASK_KINDS.contains(&task_kind) || due <= started || record.travel_minutes < 0 || started < 0 || reason.is_empty() {
        return None;
    }
    Some(Active {
        staff_id: staff_id.to_owned(),
        working: record.phase == Phase::Working,
        task_id: task_id.to_owned(),
        task_kind: task_kind.to_owned(),
        target_id: record.target_id.as_deref().map(str::to_owned),
        target_zone_id: target_zone_id.to_owned(),
        started,
        travel_due: record.travel_due_minute,
        arrival: record.arrival_minute,
        due,
        route_zones: record.route_zone_ids.iter().map(|id| id.to_string()).collect(),
        route_edges: record.route_edge_ids.iter().map(|id| id.to_string()).collect(),
        travel_minutes: record.travel_minutes,
        duration: *duration,
    })
}


impl StaffOperationsRow {
    /// The rules of `validate_staff_operations_state` that read the row alone, for a world of these
    /// zones: the active records if they pass, or None, which is then the Python rule's to say.
    pub fn active_records(&self, zones: &HashSet<String>) -> Option<Vec<Active>> {
        if self.schema_version != 1
            || &*self.policy_version != POLICY
            || &*self.source_id != SOURCE
            || self.history.len() > MAX_HISTORY
            || self.zones.len() != zones.len()
            || !self.zones.iter().all(|(id, zone)| zones.contains(&**id) && zone.is_valid())
            || !config_is_canonical(&self.config, zones)
        {
            return None;
        }
        let mut active = Vec::new();
        for (staff_id, record) in self.staff.iter() {
            if record.phase == Phase::Idle {
                if !record.is_the_idle_record() {
                    return None;
                }
                continue;
            }
            active.push(active_of(staff_id, record, &self.config)?);
        }
        Some(active)
    }

    /// The rules that read the entity rows: a record for each staff member and none else, and the
    /// staff, facilities and zones of the finished tasks.
    pub fn agrees_with(&self, rows: &Entities, zones: &HashSet<String>) -> bool {
        self.staff.len() == rows.staff.len()
            && self.staff.keys().all(|id| rows.staff.get(id).is_some())
            && self.history.iter().all(|item| item.is_valid(rows, zones))
    }

    /// `_validate_staff_operation_receipt` (`domain/common_operations.py`) of a receipt that passed
    /// its contract, of this task at this minute: whether the retained completions agree with it
    /// (#152; in the core since #250).
    pub fn completion_agrees(&self, task_id: &str, minute: i64, receipt: &serde_json::Value, history_truncated: bool) -> bool {
        let row = self;
        let text = |key: &str| receipt.get(key).and_then(serde_json::Value::as_str);
        let int = |key: &str| receipt.get(key).and_then(serde_json::Value::as_i64);
        let mut matches = row.history.iter().filter(|item| &*item.task_id == task_id && item.complete_minute == minute);
        let Some(found) = matches.next() else {
            return history_truncated && !row.history.is_empty() && row.history.iter().all(|item| item.complete_minute > minute);
        };
        if matches.next().is_some() {
            return false;
        }
        let target = receipt.get("target_id");
        let target_agrees = match (&found.target_id, target) {
            (None, Some(serde_json::Value::Null)) => true,
            (Some(id), Some(serde_json::Value::String(text))) => &**id == text,
            _ => false,
        };
        let arrival_agrees = match (found.arrival_minute, receipt.get("arrival_minute")) {
            (None, Some(serde_json::Value::Null)) => true,
            (Some(arrival), Some(value)) => value.as_i64() == Some(arrival),
            _ => false,
        };
        text("staff_id") == Some(&*found.staff_id)
            && text("task_kind") == Some(&*found.task_kind)
            && target_agrees
            && text("target_zone_id") == Some(&*found.target_zone_id)
            && text("reason") == Some(&*found.reason)
            && text("status") == Some(found.status.text())
            && int("start_minute") == Some(found.start_minute)
            && arrival_agrees
            && int("travel_minutes") == Some(found.travel_minutes)
            && text("causation_id").is_some_and(|cause| found.causation_event_ids.iter().any(|id| &**id == cause))
    }
}
