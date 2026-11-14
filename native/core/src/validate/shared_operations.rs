//! `validate_shared_operations_state(world, validate_event_history=...)` of
//! `domain/common_operations.py`, its history included, with Python's errors and texts (lane E of
//! #250, #350). Raw errors: `validate_simulator_extensions` passes them on. The check before it
//! (the namespace without its signed contract) is `validate::extensions`'s.
//!
//! Without the history (`View::history` false) the receipts checked are those since the previous
//! tick (`records_since_previous_tick`), and the care lifecycle is not replayed, as in Python.
//! There is no history memo (`_history_memo` of a save): the whole history is replayed each time.
//!
//! `SharedOperationsState` and the receipts are pydantic models; only whether they validate
//! matters (Python wraps their errors), and their dumps, so they are the core's pydantic
//! (`contracts::pydantic`) here. The one error of theirs Python lets through is that of a
//! `CareRequestState` the replay makes (`care_state`).

use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::Arc;

use indexmap::IndexMap;

use super::movement::py::{cmp_i64, contains, eq, int_cmp, is_exact_int, is_text, num_cmp, type_name, Held};
use super::View;
use crate::contracts::pydantic::{self as pd, Ctx, Field, Fields, FloatS, In, IntS, Len, Lit, Model, Pattern, StrS, S};
use crate::error::{Error, Result};
use crate::events::log::EventLog;
use crate::events::EventStatus;
use crate::value::V;

const KEY: &str = "shared_operations_v1";
const POLICY: &str = "shared-operations-v1";
const MAX_HISTORY: usize = 20_000;
const MAX_REQUESTS: usize = 100_000;
const ACTIVE: [&str; 3] = ["requested", "assigned", "started"];

type Map = IndexMap<Arc<str>, V>;

// ---- The models (contracts/operations.py) ---------------------------------------------------------

const IDENT: S = S::Str(StrS { strip: false, min: None, max: None, pattern: Some(Pattern::Identifier) });
const OPT_IDENT: S = S::Opt(&IDENT);
const IDS: S = S::Tuple(&IDENT, Len { min: None, max: None });
const MINUTE: S = S::Int(IntS { strict: true, ge: Some(0), gt: None, le: None });
const FINITE: S = S::Float(FloatS { strict: false, finite: true, ge: None, gt: None, le: None });
const SUPPORT_KIND: S = S::Lit(&[
    Lit::Str("medical_assessment"),
    Lit::Str("medication_support"),
    Lit::Str("caregiver_assistance"),
    Lit::Str("privacy_accommodation"),
    Lit::Str("mobility_assistance"),
    Lit::Str("hygiene_support"),
    Lit::Str("emotional_support"),
    Lit::Str("language_access"),
    Lit::Str("dependent_assistance"),
]);
const CARE_STATUS: S =
    S::Lit(&[Lit::Str("requested"), Lit::Str("assigned"), Lit::Str("started"), Lit::Str("completed"), Lit::Str("interrupted"), Lit::Str("declined")]);

const fn required(name: &'static str, s: S) -> Field {
    Field { name, s, default: None }
}

const fn with(name: &'static str, s: S, default: fn() -> V) -> Field {
    Field { name, s, default: Some(default) }
}

const fn model(name: &'static str, fields: &'static [Field], after: Option<pd::After>) -> Model {
    Model { name, fields, before: None, after, recursive: false }
}

fn null() -> V {
    V::Null
}
fn no_items() -> V {
    V::List(Vec::new())
}
fn no_map() -> V {
    V::Map(Map::new())
}
fn int_one() -> V {
    V::Int(1)
}
fn no() -> V {
    V::Bool(false)
}
fn care() -> V {
    V::Str(Arc::from("care"))
}
fn operations() -> V {
    V::Str(Arc::from("operations"))
}
fn not_applicable() -> V {
    V::Str(Arc::from("not_applicable"))
}
fn staff_unavailable() -> V {
    V::Str(Arc::from("staff_unavailable"))
}
fn care_transition() -> V {
    V::Str(Arc::from("care_support_transition"))
}
fn continuity() -> V {
    V::Str(Arc::from("staff_support_continuity"))
}
fn handoff() -> V {
    V::Str(Arc::from("staff_handoff"))
}
fn recovery() -> V {
    V::Str(Arc::from("facility_recovery_transition"))
}
fn staff_operation() -> V {
    V::Str(Arc::from("staff_operation_transition"))
}

fn field<'a>(fields: &'a Fields, name: &str) -> &'a V {
    fields.get(name).unwrap_or(&V::Null)
}

fn text<'a>(fields: &'a Map, name: &str) -> &'a str {
    match fields.get(name) {
        Some(V::Str(text)) => text,
        _ => "",
    }
}

fn items<'a>(fields: &'a Fields, name: &str) -> &'a [V] {
    match fields.get(name) {
        Some(V::List(items)) => items,
        _ => &[],
    }
}

fn changed(row: &V) -> bool {
    match row {
        V::Map(row) => !eq(row.get("before").unwrap_or(&V::Null), row.get("after").unwrap_or(&V::Null)),
        _ => false,
    }
}

fn fail(text: &str) -> std::result::Result<(), String> {
    Err(text.to_owned())
}

/// `field_path`'s pattern, `^[A-Za-z0-9_]+(?:\.[A-Za-z0-9_]+){0,5}$`, is asked here: the model's
/// pass or fail is the same.
fn numeric_change_is_valid(fields: &mut Fields) -> std::result::Result<(), String> {
    let path = text(fields, "field_path");
    let parts: Vec<&str> = path.split('.').collect();
    if parts.len() > 6 || parts.iter().any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')) {
        return fail("field_path does not match its pattern");
    }
    Ok(())
}

static NUMERIC_CHANGE: Model = model(
    "NumericStateChange",
    &[
        required("entity_id", IDENT),
        required(
            "entity_kind",
            S::Lit(&[Lit::Str("agent"), Lit::Str("staff"), Lit::Str("facility"), Lit::Str("zone"), Lit::Str("resource"), Lit::Str("model_state")]),
        ),
        required("field_path", S::Str(StrS { strip: false, min: None, max: Some(128), pattern: None })),
        required("before", FINITE),
        required("after", FINITE),
        required("unit", S::Str(StrS { strip: false, min: Some(1), max: Some(80), pattern: None })),
    ],
    Some(numeric_change_is_valid),
);

fn care_evidence_is_valid(fields: &mut Fields) -> std::result::Result<(), String> {
    let changes = items(fields, "state_changes");
    let observation = text(fields, "outcome_observation");
    if text(fields, "status") == "completed" {
        let bad = match observation {
            "observed_change" => changes.is_empty() || !changes.iter().any(changed),
            "observed_no_change" => changes.is_empty() || changes.iter().any(changed),
            _ => !changes.is_empty(),
        };
        if bad {
            return fail("the outcome does not match its evidence");
        }
    } else if observation != "not_applicable" || !changes.is_empty() {
        return fail("only a completed support operation may report an outcome");
    }
    Ok(())
}

static CARE_RECEIPT: Model = model(
    "CareTransitionReceipt",
    &[
        with("record_type", S::Lit(&[Lit::Str("care")]), care),
        with("schema_version", S::Lit(&[Lit::Int(1)]), int_one),
        with("event_type", S::Lit(&[Lit::Str("care_support_transition")]), care_transition),
        required("event_id", IDENT),
        required("request_id", IDENT),
        required("agent_id", IDENT),
        with("household_id", OPT_IDENT, null),
        required("support_kind", SUPPORT_KIND),
        required("status", CARE_STATUS),
        required("minute", MINUTE),
        with("staff_id", OPT_IDENT, null),
        with(
            "outcome_observation",
            S::Lit(&[Lit::Str("not_applicable"), Lit::Str("observed_change"), Lit::Str("observed_no_change"), Lit::Str("not_measured")]),
            not_applicable,
        ),
        with("state_changes", S::Tuple(&S::Model(&NUMERIC_CHANGE), Len { min: None, max: Some(32) }), no_items),
        with("causation_id", OPT_IDENT, null),
        with("source_ids", IDS, no_items),
    ],
    Some(care_evidence_is_valid),
);

fn continuity_is_valid(fields: &mut Fields) -> std::result::Result<(), String> {
    if eq(field(fields, "event_id"), field(fields, "causation_id")) {
        return fail("support continuity event cannot cause itself");
    }
    Ok(())
}

static CONTINUITY_RECEIPT: Model = model(
    "StaffSupportContinuityReceipt",
    &[
        with("record_type", S::Lit(&[Lit::Str("care")]), care),
        with("schema_version", S::Lit(&[Lit::Int(1)]), int_one),
        with("event_type", S::Lit(&[Lit::Str("staff_support_continuity")]), continuity),
        required("event_id", IDENT),
        required("request_id", IDENT),
        required("agent_id", IDENT),
        required("task_id", IDENT),
        required("previous_staff_id", IDENT),
        required("status", S::Lit(&[Lit::Str("requeued"), Lit::Str("unresolved")])),
        with("cause_type", S::Lit(&[Lit::Str("staff_unavailable"), Lit::Str("staff_shift_change"), Lit::Str("route_unreachable")]), staff_unavailable),
        required("minute", MINUTE),
        required("causation_id", IDENT),
        with("source_ids", IDS, no_items),
    ],
    Some(continuity_is_valid),
);

fn handoff_is_valid(fields: &mut Fields) -> std::result::Result<(), String> {
    let replacement = field(fields, "replacement_staff_id");
    if (text(fields, "status") == "reassigned") == matches!(replacement, V::Null) || eq(replacement, field(fields, "previous_staff_id")) {
        return fail("a staff handoff must change the assigned person");
    }
    Ok(())
}

static HANDOFF_RECEIPT: Model = model(
    "StaffHandoffReceipt",
    &[
        with("record_type", S::Lit(&[Lit::Str("operations")]), operations),
        with("schema_version", S::Lit(&[Lit::Int(1)]), int_one),
        with("event_type", S::Lit(&[Lit::Str("staff_handoff")]), handoff),
        required("event_id", IDENT),
        required("task_id", IDENT),
        required("previous_staff_id", IDENT),
        with("replacement_staff_id", OPT_IDENT, null),
        required("status", S::Lit(&[Lit::Str("reassigned"), Lit::Str("unfilled"), Lit::Str("rejected")])),
        required("minute", MINUTE),
        required("reason_code", IDENT),
        with("causation_id", OPT_IDENT, null),
        with("source_ids", IDS, no_items),
    ],
    Some(handoff_is_valid),
);

fn recovery_is_valid(fields: &mut Fields) -> std::result::Result<(), String> {
    if matches!(text(fields, "status"), "degraded" | "outage" | "restored") && eq(field(fields, "working_units"), field(fields, "previous_working_units")) {
        return fail("capacity transition must record different before/after working units");
    }
    Ok(())
}

static RECOVERY_RECEIPT: Model = model(
    "FacilityRecoveryReceipt",
    &[
        with("record_type", S::Lit(&[Lit::Str("operations")]), operations),
        with("schema_version", S::Lit(&[Lit::Int(1)]), int_one),
        with("event_type", S::Lit(&[Lit::Str("facility_recovery_transition")]), recovery),
        required("event_id", IDENT),
        required("facility_id", IDENT),
        required("component_id", IDENT),
        required("status", S::Lit(&[Lit::Str("degraded"), Lit::Str("outage"), Lit::Str("repair_started"), Lit::Str("repaired"), Lit::Str("restored")])),
        required("minute", MINUTE),
        required("previous_working_units", MINUTE),
        required("working_units", MINUTE),
        with("source_ids", IDS, no_items),
        with("causation_id", OPT_IDENT, null),
    ],
    Some(recovery_is_valid),
);

fn staff_operation_is_valid(fields: &mut Fields) -> std::result::Result<(), String> {
    let at = |name: &str| field(fields, name).clone();
    let (start, arrival, minute, travel, total) = (at("start_minute"), at("arrival_minute"), at("minute"), at("travel_minutes"), at("total_state_change_count"));
    let le = |a: &V, b: &V| int_cmp(a, b).is_some_and(Ordering::is_le);
    if !le(&start, &arrival) || !le(&arrival, &minute) {
        return fail("staff operation completion times are inconsistent");
    }
    // travel > arrival - start
    let elapsed = match (&arrival, &start) {
        (V::Int(a), V::Int(b)) => V::Int(a - b),
        _ => return fail("staff operation times are too large"),
    };
    if int_cmp(&travel, &elapsed) == Some(Ordering::Greater) {
        return fail("staff operation travel exceeds elapsed assignment time");
    }
    let changes = items(fields, "state_changes");
    let count = changes.len() as i64;
    if cmp_i64(&total, count) == Some(Ordering::Less) {
        return fail("staff operation change count is smaller than its sample");
    }
    let truncated = matches!(field(fields, "state_changes_truncated"), V::Bool(true));
    if truncated != (cmp_i64(&total, count) == Some(Ordering::Greater)) {
        return fail("staff operation change truncation flag is inconsistent");
    }
    let total_zero = cmp_i64(&total, 0) == Some(Ordering::Equal);
    let observation = text(fields, "outcome_observation");
    let bad = if text(fields, "status") == "blocked" {
        !changes.is_empty() || !total_zero || observation != "observed_no_change"
    } else {
        match observation {
            "observed_change" => total_zero || !changes.iter().any(changed),
            "observed_no_change" => !total_zero || changes.iter().any(changed),
            _ => !changes.is_empty() || !total_zero,
        }
    };
    if bad {
        return fail("staff operation evidence is inconsistent");
    }
    Ok(())
}

static STAFF_OPERATION_RECEIPT: Model = model(
    "StaffOperationReceipt",
    &[
        with("record_type", S::Lit(&[Lit::Str("operations")]), operations),
        with("schema_version", S::Lit(&[Lit::Int(1)]), int_one),
        with("event_type", S::Lit(&[Lit::Str("staff_operation_transition")]), staff_operation),
        required("event_id", IDENT),
        required("task_id", IDENT),
        required("staff_id", IDENT),
        required("task_kind", S::Lit(&[Lit::Str("cleaning"), Lit::Str("maintenance"), Lit::Str("information_outreach"), Lit::Str("intake"), Lit::Str("patrol")])),
        with("target_id", OPT_IDENT, null),
        required("target_zone_id", IDENT),
        required("reason", S::Str(StrS { strip: false, min: Some(1), max: Some(240), pattern: None })),
        required("status", S::Lit(&[Lit::Str("completed"), Lit::Str("blocked")])),
        required("minute", MINUTE),
        required("start_minute", MINUTE),
        required("arrival_minute", MINUTE),
        required("travel_minutes", MINUTE),
        required("outcome_observation", S::Lit(&[Lit::Str("observed_change"), Lit::Str("observed_no_change"), Lit::Str("not_measured")])),
        with("state_changes", S::Tuple(&S::Model(&NUMERIC_CHANGE), Len { min: None, max: Some(256) }), no_items),
        required("total_state_change_count", MINUTE),
        with("state_changes_truncated", S::Bool { strict: false }, no),
        required("causation_id", IDENT),
        with("source_ids", IDS, no_items),
    ],
    Some(staff_operation_is_valid),
);

fn request_time_is_monotonic(fields: &mut Fields) -> std::result::Result<(), String> {
    if int_cmp(field(fields, "updated_minute"), field(fields, "created_minute")) == Some(Ordering::Less) {
        return fail("care request update cannot precede its creation");
    }
    Ok(())
}

static CARE_REQUEST: Model = model(
    "CareRequestState",
    &[
        required("request_id", IDENT),
        required("agent_id", IDENT),
        with("household_id", OPT_IDENT, null),
        required("support_kind", SUPPORT_KIND),
        required("status", CARE_STATUS),
        required("created_minute", MINUTE),
        required("updated_minute", MINUTE),
        required("latest_event_id", IDENT),
    ],
    Some(request_time_is_monotonic),
);

/// The fields of `SharedOperationsState` but its history, which `parse` reads itself (a union by
/// `event_type`, which the core's pydantic has no type for).
static STATE_PARTS: Model = model(
    "SharedOperationsState",
    &[
        with("schema_version", S::Lit(&[Lit::Int(1)]), int_one),
        required("policy_version", S::Lit(&[Lit::Str(POLICY)])),
        with("support_requests", S::Dict(&IDENT, &S::Model(&CARE_REQUEST)), no_map),
        with("history_truncated", S::Bool { strict: false }, no),
    ],
    None,
);

/// `OperationHistoryRecord`: the model its `event_type` names, validated; None where it fails.
fn receipt(value: &V) -> Option<Fields> {
    let V::Map(map) = value else {
        return None;
    };
    let model = match map.get("event_type") {
        Some(V::Str(tag)) => match &**tag {
            "care_support_transition" => &CARE_RECEIPT,
            "staff_support_continuity" => &CONTINUITY_RECEIPT,
            "staff_handoff" => &HANDOFF_RECEIPT,
            "facility_recovery_transition" => &RECOVERY_RECEIPT,
            "staff_operation_transition" => &STAFF_OPERATION_RECEIPT,
            _ => return None,
        },
        _ => return None,
    };
    pd::model(model, In::plain(value), &mut Ctx::default()).ok()
}

/// What `SharedOperationsState.model_validate` makes of the state: its support requests (dumps),
/// its history (dumps) and `history_truncated`; None where it fails.
struct Parsed {
    support_requests: Map,
    history: Vec<Fields>,
    truncated: bool,
}

fn parse(state: &V) -> Option<Parsed> {
    let V::Map(map) = state else {
        return None;
    };
    let mut parts = map.clone();
    let history = parts.shift_remove("history");
    let fields = pd::model(&STATE_PARTS, In::plain(&V::Map(parts)), &mut Ctx::default()).ok()?;
    let records = match history {
        None => Vec::new(),
        Some(V::List(items)) if items.len() <= MAX_HISTORY => items.iter().map(receipt).collect::<Option<Vec<_>>>()?,
        Some(_) => return None,
    };
    let support_requests = match field(&fields, "support_requests") {
        V::Map(requests) if requests.len() <= MAX_REQUESTS => requests.clone(),
        _ => return None,
    };
    // The model's after-validator.
    for (key, row) in &support_requests {
        let V::Map(row) = row else { return None };
        if text(row, "request_id") != &**key || !ACTIVE.contains(&text(row, "status")) {
            return None;
        }
    }
    Some(Parsed { support_requests, history: records, truncated: matches!(field(&fields, "history_truncated"), V::Bool(true)) })
}

// ---- The rule --------------------------------------------------------------------------------------

/// `records_since_previous_tick(history, minute_index)`.
fn since_previous_tick(history: &[V], minute_index: i64) -> &[V] {
    let mut start = history.len();
    while start > 0 {
        let minute = match &history[start - 1] {
            V::Map(item) => item.get("minute"),
            _ => None,
        };
        if minute.is_some_and(|minute| is_exact_int(minute) && cmp_i64(minute, minute_index - 1) == Some(Ordering::Less)) {
            break;
        }
        start -= 1;
    }
    &history[start.saturating_sub(1)..]
}

/// A realized record of the log: its event, minute and status.
struct Found {
    event: crate::events::EventRow,
    realized_minute: i64,
    status: EventStatus,
}

/// `world.event_log.find(event_id)`.
fn find(log: &EventLog, event_id: &str) -> Result<Option<Found>> {
    let Some(index) = log.index_of(event_id) else {
        return Ok(None);
    };
    let record = log.record(index)?.ok_or_else(|| Error::unsupported("a record of the log has no bytes"))?;
    Ok(Some(Found { event: record.event, realized_minute: record.realized_minute, status: record.status }))
}

fn option_eq(found: Option<&Arc<str>>, value: &V) -> bool {
    match (found, value) {
        (None, V::Null) => true,
        (Some(found), V::Str(text)) => found == text,
        _ => false,
    }
}

/// `_event_payload_matches(event_record, record)`.
fn payload_matches(found: &Found, record: &Fields) -> bool {
    let payload = &found.event.payload;
    let mut expected = record.clone();
    let has_cause = matches!(payload, V::Map(map) if map.contains_key("cause_type"));
    if text(record, "event_type") == "staff_support_continuity" && !has_cause && text(record, "cause_type") == "staff_unavailable" {
        expected.shift_remove("cause_type");
    }
    let sources_hold = match &found.event.source_id {
        None => true,
        Some(source) => items(record, "source_ids").iter().any(|id| matches!(id, V::Str(id) if id == source)),
    };
    eq(payload, &V::Map(expected)) && option_eq(found.event.causation_id.as_ref(), field(record, "causation_id")) && sources_hold
}

/// `a > b` of two values of a retained completion, or Python's `TypeError`.
fn greater(left: &V, right: &V) -> Result<bool> {
    num_cmp(left, right).map(|order| order == Ordering::Greater).ok_or_else(|| {
        Error::Type(format!("'>' not supported between instances of '{}' and '{}'", type_name(left), type_name(right)))
    })
}

/// `_validate_staff_operation_receipt`.
fn staff_operation_receipt(view: &View<'_>, record: &Fields, truncated: bool) -> Result<()> {
    let operations = view.state("staff_operations_v1")?;
    let history: Vec<V> = match operations.as_deref() {
        Some(V::Map(operations)) => match operations.get("history") {
            None => Vec::new(),
            Some(V::List(items)) => items.clone(),
            Some(V::Map(_) | V::Str(_)) => Vec::new(),
            Some(other) => return Err(Error::Type(format!("'{}' object is not iterable", type_name(other)))),
        },
        _ => Vec::new(),
    };
    let rows: Vec<&Map> = history.iter().filter_map(|row| if let V::Map(row) = row { Some(row) } else { None }).collect();
    let get = |row: &Map, name: &str| row.get(name).cloned().unwrap_or(V::Null);
    let matches: Vec<&Map> = rows
        .iter()
        .copied()
        .filter(|row| eq(&get(row, "task_id"), field(record, "task_id")) && eq(&get(row, "complete_minute"), field(record, "minute")))
        .collect();
    if matches.is_empty() {
        if truncated && !history.is_empty() {
            let mut all_later = true;
            for row in &rows {
                if !greater(&row.get("complete_minute").cloned().unwrap_or(V::Int(0)), field(record, "minute"))? {
                    all_later = false;
                    break;
                }
            }
            if all_later {
                return Ok(());
            }
        }
        return Err(Error::Value("staff operation receipt has no matching retained completion".to_owned()));
    }
    if matches.len() != 1 {
        return Err(Error::Value("staff operation receipt matches duplicate completion history".to_owned()));
    }
    let row = matches[0];
    let same = ["staff_id", "task_kind", "target_id", "target_zone_id", "reason", "status", "start_minute", "arrival_minute", "travel_minutes"]
        .iter()
        .all(|name| eq(&get(row, name), field(record, name)));
    if !same || !contains(&row.get("causation_event_ids").cloned().unwrap_or(V::List(Vec::new())), field(record, "causation_id"), Held::Tracked)? {
        return Err(Error::Value("staff operation receipt disagrees with retained completion history".to_owned()));
    }
    Ok(())
}

/// `CareRequestState(...)` as the replay makes it, or its `ValidationError`.
#[allow(clippy::too_many_arguments)]
fn care_state(request_id: &V, agent_id: &V, household_id: &V, support_kind: &V, status: &str, created: &V, updated: &V, latest: &V) -> Result<Map> {
    let mut input = Map::new();
    for (name, value) in [
        ("request_id", request_id.clone()),
        ("agent_id", agent_id.clone()),
        ("household_id", household_id.clone()),
        ("support_kind", support_kind.clone()),
        ("status", V::Str(Arc::from(status))),
        ("created_minute", created.clone()),
        ("updated_minute", updated.clone()),
        ("latest_event_id", latest.clone()),
    ] {
        input.insert(Arc::from(name), value);
    }
    pd::model(&CARE_REQUEST, In::plain(&V::Map(input)), &mut Ctx::default()).map_err(|errors| Error::Validation(pd::text(CARE_REQUEST.name, &errors)))
}

fn transitions(status: &str) -> &'static [&'static str] {
    match status {
        "requested" => &["assigned", "started", "declined", "interrupted"],
        "assigned" => &["started", "declined", "interrupted"],
        "started" => &["completed", "interrupted"],
        _ => &[],
    }
}

fn value_error<T>(text: &str) -> Result<T> {
    Err(Error::Value(text.to_owned()))
}

/// `_care_interruption_due`.
fn interruption_due(log: &EventLog, minute: i64, request_id: &str, head_status: &str, request_status: &str) -> Result<bool> {
    if head_status != "requested" || request_status != "unresolved" {
        return Ok(false);
    }
    for &index in log.at_minute(minute) {
        let record = log.record(index)?.ok_or_else(|| Error::unsupported("a record of the log has no bytes"))?;
        let payload_request = match &record.event.payload {
            V::Map(payload) => payload.get("request_id").cloned().unwrap_or(V::Null),
            _ => V::Null,
        };
        if record.status == EventStatus::Applied && &*record.event.event_type == "support_request_timeout" && is_text(&payload_request, request_id) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(view: &View<'_>) -> Result<()> {
    let Some(state) = view.state(KEY)? else {
        return Ok(());
    };
    if matches!(*state, V::Null) {
        return Ok(());
    }
    let minute = view.minute_index()?;
    let checked = match &*state {
        V::Map(map) if !view.history => match map.get("history") {
            Some(V::List(history)) => {
                let mut checked = map.clone();
                checked.insert(Arc::from("history"), V::List(since_previous_tick(history, minute).to_vec()));
                V::Map(checked)
            }
            _ => state.clone(),
        },
        other => other.clone(),
    };
    let Some(parsed) = parse(&checked) else {
        return value_error("shared_operations_v1 has an invalid exact schema");
    };
    let full_length = match &*state {
        V::Map(map) => match map.get("history") {
            Some(V::List(history)) => history.len(),
            Some(_) => 0,
            None => return Err(Error::Key("history".to_owned())),
        },
        _ => 0,
    };
    drop(state);
    let minutes: Vec<&V> = parsed.history.iter().map(|item| field(item, "minute")).collect();
    if full_length > MAX_HISTORY || minutes.windows(2).any(|pair| int_cmp(pair[0], pair[1]) == Some(Ordering::Greater)) {
        return value_error("shared_operations_v1 history is over limit or out of time order");
    }
    let log = view.log()?;
    let mut seen: HashSet<Arc<str>> = HashSet::new();
    let mut care: IndexMap<Arc<str>, Map> = IndexMap::new();
    for item in &parsed.history {
        let event_id: Arc<str> = Arc::from(text(item, "event_id"));
        if cmp_i64(field(item, "minute"), minute) == Some(Ordering::Greater) || seen.contains(&event_id) {
            return value_error("shared_operations_v1 history has future, duplicate, or unordered records");
        }
        seen.insert(event_id.clone());
        let found = find(&log, &event_id)?;
        let linked = found.as_ref().is_some_and(|found| {
            found.status == EventStatus::Applied
                && cmp_i64(field(item, "minute"), found.realized_minute) == Some(Ordering::Equal)
                && &*found.event.event_type == text(item, "event_type")
                && payload_matches(found, item)
        });
        if !linked {
            return value_error("shared_operations_v1 record must link to a realized applied event");
        }
        let event_type = text(item, "event_type");
        if event_type == "staff_operation_transition" {
            staff_operation_receipt(view, item, parsed.truncated)?;
        }
        if !view.history {
            continue;
        }
        let request_id: Arc<str> = Arc::from(text(item, "request_id"));
        if event_type == "care_support_transition" {
            let status = text(item, "status");
            let created = match care.get(&request_id) {
                None => {
                    if status != "requested" && !parsed.truncated {
                        return value_error("a support request must begin with a requested transition");
                    }
                    let saved = match parsed.support_requests.get(&request_id) {
                        Some(V::Map(saved)) if parsed.truncated => Some(saved),
                        _ => None,
                    };
                    let created = saved.map_or_else(|| field(item, "minute").clone(), |saved| field(saved, "created_minute").clone());
                    if int_cmp(&created, field(item, "minute")) == Some(Ordering::Greater) {
                        return value_error("truncated care history has an invalid creation minute");
                    }
                    created
                }
                Some(prior) => {
                    if ["agent_id", "household_id", "support_kind"].iter().any(|name| !eq(field(prior, name), field(item, name))) {
                        return value_error("support request identity changed during its lifecycle");
                    }
                    if !transitions(text(prior, "status")).contains(&status) {
                        return value_error("support lifecycle transition is out of order");
                    }
                    field(prior, "created_minute").clone()
                }
            };
            let state = care_state(
                field(item, "request_id"),
                field(item, "agent_id"),
                field(item, "household_id"),
                field(item, "support_kind"),
                status,
                &created,
                field(item, "minute"),
                field(item, "event_id"),
            )?;
            care.insert(request_id, state);
        } else if event_type == "staff_support_continuity" {
            let prior = care.get(&request_id).cloned();
            let saved = match parsed.support_requests.get(&request_id) {
                Some(V::Map(saved)) => Some(saved),
                _ => None,
            };
            let status = text(item, "status");
            let created = match &prior {
                None => {
                    if !parsed.truncated || (status == "requeued" && saved.is_none()) {
                        return value_error("support continuity must follow an active care transition");
                    }
                    if let Some(saved) = saved {
                        if !eq(field(saved, "agent_id"), field(item, "agent_id"))
                            || !eq(field(saved, "latest_event_id"), field(item, "event_id"))
                            || text(saved, "status") != "requested"
                        {
                            return value_error("truncated support continuity does not match the active request");
                        }
                    }
                    saved.map_or_else(|| field(item, "minute").clone(), |saved| field(saved, "created_minute").clone())
                }
                Some(prior) => {
                    if !matches!(text(prior, "status"), "assigned" | "started") || !eq(field(prior, "agent_id"), field(item, "agent_id")) {
                        return value_error("support continuity does not follow assigned resident care");
                    }
                    field(prior, "created_minute").clone()
                }
            };
            if status == "requeued" {
                let source = prior.as_ref().or(saved).expect("one of them is there, as checked above");
                let state = care_state(
                    field(item, "request_id"),
                    field(item, "agent_id"),
                    field(source, "household_id"),
                    field(source, "support_kind"),
                    "requested",
                    &created,
                    field(item, "minute"),
                    field(item, "event_id"),
                )?;
                care.insert(request_id, state);
            } else {
                care.shift_remove(&request_id);
            }
        }
    }
    let active_care: IndexMap<&Arc<str>, &Map> = care.iter().filter(|(_, state)| ACTIVE.contains(&text(state, "status"))).collect();
    for (request_id, replayed) in &active_care {
        let same = parsed.support_requests.get(*request_id).is_some_and(|saved| eq(saved, &V::Map((*replayed).clone())));
        if !same {
            return value_error("shared_operations_v1 care request head does not match its retained history");
        }
    }
    let rows = view.rows()?;
    for (request_id, head) in &parsed.support_requests {
        let V::Map(head) = head else {
            return value_error("shared_operations_v1 stores a terminal request as active");
        };
        let head_status = text(head, "status");
        if !ACTIVE.contains(&head_status) {
            return value_error("shared_operations_v1 stores a terminal request as active");
        }
        let Some(request) = rows.support_requests.get(request_id).filter(|request| is_text(field(head, "agent_id"), &request.agent_id)) else {
            return value_error("shared_operations_v1 care head has no matching persisted support request");
        };
        let expected_status = if head_status == "requested" { "pending" } else { "assigned" };
        if request.status.text() != expected_status && !interruption_due(&log, minute, request_id, head_status, request.status.text())? {
            return value_error("shared_operations_v1 care status disagrees with the persisted support request");
        }
        if active_care.contains_key(request_id) || !view.history {
            if matches!(head_status, "assigned" | "started") {
                let evidence = find(&log, text(head, "latest_event_id"))?;
                let staff_id = match evidence.as_ref().map(|found| &found.event.payload) {
                    Some(V::Map(payload)) => payload.get("staff_id").cloned().unwrap_or(V::Null),
                    _ => V::Null,
                };
                if !option_eq(request.assigned_staff_id.as_ref(), &staff_id) {
                    return value_error("shared_operations_v1 assigned staff disagrees with the support task");
                }
            }
            continue;
        }
        if !parsed.truncated {
            return value_error("shared_operations_v1 care request head has no matching history");
        }
        let record = find(&log, text(head, "latest_event_id"))?;
        let Some(record) = record.filter(|record| {
            record.status == EventStatus::Applied
                && cmp_i64(field(head, "updated_minute"), record.realized_minute) == Some(Ordering::Equal)
                && matches!(&*record.event.event_type, "care_support_transition" | "staff_support_continuity")
        }) else {
            return value_error("truncated care request head has no applied event evidence");
        };
        let Some(latest) = receipt(&record.event.payload) else {
            return value_error("truncated care request head event has an invalid receipt");
        };
        let same = |pairs: &[(&str, &str)]| pairs.iter().all(|(mine, theirs)| eq(field(&latest, mine), field(head, theirs)));
        let matches = match text(&latest, "event_type") {
            "care_support_transition" => {
                is_text(field(&latest, "request_id"), request_id)
                    && same(&[("agent_id", "agent_id"), ("household_id", "household_id"), ("support_kind", "support_kind"), ("status", "status"), ("event_id", "latest_event_id")])
            }
            "staff_support_continuity" => {
                text(&latest, "status") == "requeued"
                    && is_text(field(&latest, "request_id"), request_id)
                    && same(&[("agent_id", "agent_id"), ("event_id", "latest_event_id")])
                    && head_status == "requested"
            }
            _ => false,
        };
        if !matches || !payload_matches(&record, &latest) {
            return value_error("truncated care request head disagrees with its applied event");
        }
    }
    Ok(())
}
