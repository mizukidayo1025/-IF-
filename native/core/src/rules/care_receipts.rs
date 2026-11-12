//! `domain/care_receipts.py`: a transition that has happened, as its event and its receipt, on the
//! rows of a `World` (moved to the core in P8b-1; docs/rust-port/lanes/m6-handlers.md).
//!
//! A care transition plans with nothing changed and then writes: its applied record goes to the
//! world's EventLog and its receipt to the row of `shared_operations_v1`. The other receipts (a
//! facility's recovery, a staff handoff, a support continuity, a staff operation) are made here as
//! the event and the detail of their applied record, which the PyO3 crate hands to the dispatcher
//! as Python's record. No pydantic model is made: a receipt a kernel builds is checked against its
//! contract's fields here, and one the contract would turn down is refused (policy E15; the Python
//! `make_*_record` that took it before are gone, as no Run reached them).
//!
//! `ingest_applied_shared_operation_records` reads each receipt from its event's payload with
//! `Receipt::of` (the Rust check of `OperationHistoryRecord`) and records it with
//! `common_operations::record_shared_operation`; its loop is the PyO3 crate's, which looks up the
//! records each receipt needs.

use std::sync::Arc;

use indexmap::IndexMap;

use crate::error::{Error, Result};
use crate::events::writer::{WriteError, WriteResult};
use crate::events::{EventKind, EventRow, EventStatus};
use crate::row::ValueRow;
use crate::host::{Hold, Holding, Host, TABLE_NOT_PLAIN};
use crate::rules::common_operations::{self, plan_record, write_record, CareReceipt, Change, PlannedRecord, Receipt, RECEIPT_EVENTS};
use crate::rules::kit;
use crate::state::shared_operations::NAME;
use crate::rules::kit::{assumption, log_record, DYNAMICS};
use crate::state::shared_operations::{is_identifier, is_support_kind, CareStatus};
use crate::value::V;
use crate::world::{set_id_counter, World, WorldCell, ID_COUNTERS, MODEL_CONFIGS};

const SOURCE: &str = "ASM-CARE-TRANSITION-PRODUCER-V1";
const POLICY: &str = "applied-care-transition-v1";
const LIFECYCLE_SOURCE: &str = "ASM-STAFF-SUPPORT-LIFECYCLE-V1";
const FACILITY_SOURCE: &str = "ASM-FACILITY-RECOVERY-PRODUCER-V1";
const FACILITY_POLICY: &str = "applied-facility-recovery-v1";
const HANDOFF_SOURCE: &str = "ASM-STAFF-HANDOFF-PRODUCER-V1";
const HANDOFF_POLICY: &str = "applied-staff-handoff-v1";
const OPERATION_SOURCE: &str = "ASM-STAFF-OPERATION-RECEIPT-V1";
const OPERATION_POLICY: &str = "applied-staff-operation-receipt-v1";
const CONTINUITY_POLICY: &str = "staff-support-continuity-v1";
/// `population_context_v1`, the people's synthetic context.
pub const POPULATION_CONTEXT: &str = "population_context_v1";

/// `tuple(sorted({...}))`: bytes of UTF-8 sort as code points do.
fn sorted_unique(mut items: Vec<Arc<str>>) -> Vec<Arc<str>> {
    items.sort_unstable();
    items.dedup();
    items
}

/// `WorldState.allocate_event_sequence`: the `event` counter of `id_counters`, and the counter one
/// past it.
pub fn allocate_sequence(world: &World) -> Result<i64> {
    let sequence = world.id_counter("event")?.unwrap_or(0);
    let next = sequence.checked_add(1).ok_or_else(|| Error::unsupported("the event counter is past 64 bits"))?;
    set_id_counter(&mut *world.row_mut::<ValueRow>(ID_COUNTERS)?, "event", next)?;
    Ok(sequence)
}

/// Whether the world's EventLog has a record of this event ID or of this sequence.
fn is_realized(world: &World, event_id: &str, sequence: i64) -> Result<bool> {
    if !world.log_held {
        return Err(Error::unsupported("the event log is not in Rust"));
    }
    let log = world.log()?;
    Ok(log.has_event_id(event_id) || log.has_sequence(sequence))
}

// ---- A care transition ------------------------------------------------------------------------------

/// What a transition says, as the arguments of `append_applied_care_transition`.
pub struct Transition {
    pub request_id: Arc<str>,
    pub agent_id: Arc<str>,
    pub household_id: Option<Arc<str>>,
    pub support_kind: Arc<str>,
    pub status: &'static str,
    pub staff_id: Option<Arc<str>>,
    pub causation_id: Option<Arc<str>>,
    pub source_ids: Vec<Arc<str>>,
    /// `outcome_observation` and `state_changes` (a list of `NumericStateChange` documents), of a
    /// completed one.
    pub outcome: Option<(&'static str, V)>,
}

/// The status of a transition: one a care receipt holds.
#[track_caller]
pub fn status_of(text: &str) -> Result<(&'static str, CareStatus)> {
    let name = match text {
        "requested" => "requested",
        "assigned" => "assigned",
        "started" => "started",
        "completed" => "completed",
        "interrupted" => "interrupted",
        "declined" => "declined",
        _ => return Err(Error::unsupported("an unknown care status")),
    };
    let status = CareStatus::parse(name).ok_or_else(|| Error::unsupported("an unknown care status"))?;
    Ok((name, status))
}

/// A transition planned: everything after it changes the world and cannot refuse.
pub struct PlannedTransition {
    care: CareReceipt,
    document: V,
    row: EventRow,
    plan: PlannedRecord,
    detail: String,
}

impl PlannedTransition {
    /// The event of the transition's applied record.
    pub fn row(&self) -> &EventRow {
        &self.row
    }

    /// The minute of the record.
    pub fn minute(&self) -> i64 {
        self.care.minute
    }

    /// The detail of the record.
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

/// `make_care_transition_record` and the checks of `record_shared_operation`, with `sequence` for
/// what `allocate_event_sequence` will give then.
pub fn plan_transition(world: &World, transition: Transition, sequence: i64) -> Result<PlannedTransition> {
    let Transition { request_id, agent_id, household_id, support_kind, status, staff_id, causation_id, source_ids, outcome } = transition;
    let (status_text, care_status) = status_of(status)?;
    let minute = world.minute_index()?;
    let optional_ok = |value: &Option<Arc<str>>| value.as_deref().is_none_or(is_identifier);
    if minute < 0
        || !is_identifier(&request_id)
        || !is_identifier(&agent_id)
        || !optional_ok(&household_id)
        || !optional_ok(&staff_id)
        || !optional_ok(&causation_id)
        || !is_support_kind(&support_kind)
    {
        return Err(Error::unsupported("a value the receipt's contract is for the Python rule to judge"));
    }
    let mut sources = source_ids;
    if !sources.iter().all(|source| is_identifier(source)) {
        return Err(Error::unsupported("a source the receipt's contract is for the Python rule to judge"));
    }
    sources.push(Arc::from(SOURCE));
    let sources = sorted_unique(sources);
    let event_id: Arc<str> = Arc::from(format!("care-support-transition-{sequence}"));
    if is_realized(world, &event_id, sequence)? {
        return Err(Error::unsupported("the event's identity is taken"));
    }
    let (observation, changes) = match outcome {
        Some((observation, V::List(changes))) => {
            let changes = changes.iter().map(|change| Change::of(change)).collect::<Result<Vec<_>>>()?;
            (Arc::from(observation), changes)
        }
        Some(_) => return Err(Error::unsupported("the state changes are not a list")),
        None => (Arc::from("not_applicable"), Vec::new()),
    };
    let care = CareReceipt {
        event_id: event_id.clone(),
        request_id,
        agent_id,
        household_id,
        support_kind,
        status: care_status,
        minute,
        staff_id,
        outcome_observation: observation,
        state_changes: changes,
        causation_id: causation_id.clone(),
        source_ids: sources,
    };
    let document = Receipt::Care(care.clone()).document();
    let V::Map(payload) = document.clone() else {
        return Err(Error::unsupported("a receipt is a document"));
    };
    let Some(row) = EventRow::new(
        event_id,
        EventKind::Endogenous,
        minute,
        0,
        sequence,
        Arc::from(crate::rules::common_operations::CARE_EVENT),
        payload,
        Some(Arc::from(SOURCE)),
        causation_id,
        assumption(V::Str(Arc::from(SOURCE)), POLICY),
    ) else {
        return Err(Error::unsupported("the event's values are not what an event takes"));
    };
    let plan = plan_record(world, &care)?;
    let detail = format!("care transition {status_text} recorded after state application");
    Ok(PlannedTransition { care, document, row, plan, detail })
}

/// `allocate_event_sequence` for what `plan_transition` planned.
pub fn allocate_transition(world: &World, planned: &PlannedTransition) -> Result<()> {
    let sequence = allocate_sequence(world)?;
    if sequence != planned.row.sequence {
        return Err(Error::Runtime("the event counter moved after the plan".to_owned()));
    }
    Ok(())
}

/// The record of an allocated transition, and its receipt.
pub fn record_transition(world: &mut World, planned: PlannedTransition) -> WriteResult<()> {
    let PlannedTransition { care, document, row, plan, detail } = planned;
    log_record(world, &row, care.minute, EventStatus::Applied, Some(&detail))?;
    write_record(world, &care, &document, plan).map_err(WriteError::from)
}

/// What `plan_transition` planned, with its sequence allocated now.
pub fn write_transition(world: &mut World, planned: PlannedTransition) -> WriteResult<()> {
    allocate_transition(world, &planned)?;
    record_transition(world, planned)
}

/// `append_applied_care_transition` once the namespace is known to be there.
pub fn append(world: &mut World, transition: Transition) -> WriteResult<()> {
    let sequence = world.id_counter("event")?.unwrap_or(0);
    let planned = plan_transition(world, transition, sequence)?;
    write_transition(world, planned)
}

// ---- The person's context ---------------------------------------------------------------------------

/// `population_context_people(world).get(agent_id)`, without merging the three: the last that has
/// the person wins. A copy of the person's document.
pub fn person_context(world: &World, agent_id: &str) -> Result<Option<V>> {
    // `part.get(name).get(agent)`, where each must be a dict to be read.
    let in_part = |part: Option<&V>, name: &str| -> Option<V> {
        let V::Map(part) = part? else {
            return None;
        };
        let V::Map(people) = part.get(name)? else {
            return None;
        };
        people.get(agent_id).cloned()
    };
    {
        let dynamic = world.slots.try_value(DYNAMICS)?;
        if let Some(V::Map(state)) = dynamic.as_deref() {
            if let Some(V::Map(people)) = state.get("profile_context") {
                if let Some(person) = people.get(agent_id) {
                    return Ok(Some(person.clone()));
                }
            }
        }
    }
    {
        let context = world.slots.try_value(POPULATION_CONTEXT)?;
        if let Some(person) = in_part(context.as_deref(), "people") {
            return Ok(Some(person));
        }
    }
    let configs = world.row::<ValueRow>(MODEL_CONFIGS)?;
    let V::Map(configs) = &configs.0 else {
        return Err(Error::unsupported("the configs are not a dict"));
    };
    Ok(in_part(configs.get("population_context"), "people"))
}

/// `care_source_ids_for_agent`.
pub fn source_ids_of(world: &World, agent_id: &str) -> Result<Vec<Arc<str>>> {
    let mut sources = vec![Arc::from(LIFECYCLE_SOURCE)];
    let Some(V::Map(person)) = person_context(world, agent_id)? else {
        return Ok(sources);
    };
    let Some(listed) = person.get("source_ids") else {
        return Ok(sources);
    };
    let V::List(items) = listed else {
        return Err(Error::unsupported("the person's sources are for the Python rule to read"));
    };
    for item in items {
        if let V::Str(source) = item {
            sources.push(source.clone());
        }
    }
    Ok(sorted_unique(sources))
}

// ---- The other receipts -----------------------------------------------------------------------------
//
// Each is an applied record of its own event, which goes to the dispatcher. What the receipt's
// contract would refuse is refused: a kernel built these values. The checks here are of the
// contract's fields only.

/// The event and detail of an applied record of a receipt, at this minute.
pub struct Made {
    pub event: EventRow,
    pub minute: i64,
    pub detail: String,
}

fn header(record_type: &str, event_type: &str, event_id: &str) -> IndexMap<Arc<str>, V> {
    let mut payload = IndexMap::with_capacity(21);
    payload.insert(Arc::from("record_type"), V::Str(Arc::from(record_type)));
    payload.insert(Arc::from("schema_version"), V::Int(1));
    payload.insert(Arc::from("event_type"), V::Str(Arc::from(event_type)));
    payload.insert(Arc::from("event_id"), V::Str(Arc::from(event_id)));
    payload
}

fn put(payload: &mut IndexMap<Arc<str>, V>, name: &str, value: V) {
    payload.insert(Arc::from(name), value);
}

fn str_value(text: &str) -> V {
    V::Str(Arc::from(text))
}

fn str_list(items: &[Arc<str>]) -> V {
    V::List(items.iter().map(|item| V::Str(item.clone())).collect())
}

#[track_caller]
fn refused<T>() -> Result<T> {
    Err(Error::unsupported("a receipt its contract turns down"))
}

struct Event<'a> {
    event_id: String,
    sequence: i64,
    minute: i64,
    event_type: &'a str,
    payload: IndexMap<Arc<str>, V>,
    source: &'a str,
    causation: &'a str,
    policy: &'a str,
    detail: String,
}

fn made(event: Event<'_>) -> Result<Made> {
    let source = Arc::from(event.source);
    let row = EventRow::new(
        Arc::from(event.event_id),
        EventKind::Endogenous,
        event.minute,
        0,
        event.sequence,
        Arc::from(event.event_type),
        event.payload,
        Some(Arc::clone(&source)),
        Some(Arc::from(event.causation)),
        assumption(V::Str(source), event.policy),
    )
    .ok_or_else(|| Error::unsupported("the event's values are not what an event takes"))?;
    Ok(Made { event: row, minute: event.minute, detail: event.detail })
}

/// The sequence and the minute of a new receipt, in the order the Python rule read them.
fn minute_and_sequence(world: &World) -> Result<(i64, i64)> {
    let sequence = allocate_sequence(world)?;
    Ok((world.minute_index()?, sequence))
}

/// `make_facility_recovery_record(...)[0]` of a world that has the shared contract.
#[allow(clippy::too_many_arguments)]
pub fn facility_recovery_record(
    world: &World,
    facility_id: &str,
    component_id: &str,
    status: &str,
    previous: i64,
    units: i64,
    causation: &str,
    source_ids: Vec<Arc<str>>,
) -> Result<Made> {
    let fits = is_identifier(facility_id)
        && is_identifier(component_id)
        && is_identifier(causation)
        && source_ids.iter().all(|source| is_identifier(source))
        && match status {
            "degraded" | "outage" | "restored" => units != previous,
            "repair_started" | "repaired" => true,
            _ => false,
        }
        && previous >= 0
        && units >= 0
        && world.minute_index()? >= 0;
    if !fits {
        return refused();
    }
    let (minute, sequence) = minute_and_sequence(world)?;
    let event_id = format!("facility-recovery-transition-{sequence}");
    let mut sources = source_ids;
    sources.push(Arc::from(FACILITY_SOURCE));
    let sources = sorted_unique(sources);
    let mut payload = header("operations", "facility_recovery_transition", &event_id);
    put(&mut payload, "facility_id", str_value(facility_id));
    put(&mut payload, "component_id", str_value(component_id));
    put(&mut payload, "status", str_value(status));
    put(&mut payload, "minute", V::Int(minute));
    put(&mut payload, "previous_working_units", V::Int(previous));
    put(&mut payload, "working_units", V::Int(units));
    put(&mut payload, "source_ids", str_list(&sources));
    put(&mut payload, "causation_id", str_value(causation));
    made(Event {
        event_id,
        sequence,
        minute,
        event_type: "facility_recovery_transition",
        payload,
        source: FACILITY_SOURCE,
        causation,
        policy: FACILITY_POLICY,
        detail: format!("facility {status} receipt recorded after working-unit update"),
    })
}

/// `make_staff_handoff_record(...)[0]` of a world that has the shared contract.
pub fn handoff_record(
    world: &World,
    task_id: &str,
    previous_staff: &str,
    replacement: &str,
    reason_code: &str,
    causation: &str,
    source_ids: Vec<Arc<str>>,
) -> Result<Made> {
    let fits = [task_id, previous_staff, replacement, reason_code, causation].iter().all(|id| is_identifier(id))
        && source_ids.iter().all(|source| is_identifier(source))
        && replacement != previous_staff
        && world.minute_index()? >= 0;
    if !fits {
        return refused();
    }
    let (minute, sequence) = minute_and_sequence(world)?;
    let event_id = format!("staff-handoff-{sequence}");
    let mut sources = source_ids;
    sources.push(Arc::from(HANDOFF_SOURCE));
    let sources = sorted_unique(sources);
    let mut payload = header("operations", "staff_handoff", &event_id);
    put(&mut payload, "task_id", str_value(task_id));
    put(&mut payload, "previous_staff_id", str_value(previous_staff));
    put(&mut payload, "replacement_staff_id", str_value(replacement));
    put(&mut payload, "status", str_value("reassigned"));
    put(&mut payload, "minute", V::Int(minute));
    put(&mut payload, "reason_code", str_value(reason_code));
    put(&mut payload, "causation_id", str_value(causation));
    put(&mut payload, "source_ids", str_list(&sources));
    made(Event {
        event_id,
        sequence,
        minute,
        event_type: "staff_handoff",
        payload,
        source: HANDOFF_SOURCE,
        causation,
        policy: HANDOFF_POLICY,
        detail: "staff task assignment updated before handoff receipt".to_owned(),
    })
}

/// `make_staff_support_continuity_record`.
#[allow(clippy::too_many_arguments)]
pub fn continuity_record(
    world: &World,
    request_id: &str,
    agent_id: &str,
    task_id: &str,
    previous_staff: &str,
    status: &str,
    causation: &str,
    cause_type: &str,
) -> Result<Made> {
    let fits = [request_id, agent_id, task_id, previous_staff, causation].iter().all(|id| is_identifier(id))
        && matches!(status, "requeued" | "unresolved")
        && matches!(cause_type, "staff_unavailable" | "staff_shift_change" | "route_unreachable")
        && world.minute_index()? >= 0;
    if !fits {
        return refused();
    }
    let (minute, sequence) = minute_and_sequence(world)?;
    let event_id = format!("staff-support-continuity-{sequence}");
    if event_id == causation {
        return refused();
    }
    let mut payload = header("care", "staff_support_continuity", &event_id);
    put(&mut payload, "request_id", str_value(request_id));
    put(&mut payload, "agent_id", str_value(agent_id));
    put(&mut payload, "task_id", str_value(task_id));
    put(&mut payload, "previous_staff_id", str_value(previous_staff));
    put(&mut payload, "status", str_value(status));
    put(&mut payload, "cause_type", str_value(cause_type));
    put(&mut payload, "minute", V::Int(minute));
    put(&mut payload, "causation_id", str_value(causation));
    put(&mut payload, "source_ids", V::List(vec![str_value(LIFECYCLE_SOURCE)]));
    made(Event {
        event_id,
        sequence,
        minute,
        event_type: "staff_support_continuity",
        payload,
        source: LIFECYCLE_SOURCE,
        causation,
        policy: CONTINUITY_POLICY,
        detail: format!("staff support request was {status} after assigned staff became unavailable"),
    })
}

/// What a completed staff operation says, for `make_staff_operation_record`: the values of the
/// operation's record that the receipt copies, as the record holds them.
pub struct OperationOutcome {
    pub task_id: V,
    pub staff_id: V,
    pub task_kind: V,
    pub target_id: V,
    pub target_zone_id: V,
    pub reason: V,
    pub status: V,
    pub start_minute: V,
    pub arrival_minute: V,
    pub travel_minutes: V,
    /// `NumericStateChange` documents, sorted by kind, entity and path.
    pub changes: Vec<V>,
    pub measured: bool,
    pub causation: Arc<str>,
}

fn identifier_value(value: &V) -> bool {
    matches!(value, V::Str(text) if is_identifier(text))
}

/// What `make_staff_operation_record` asks of one change: an identifier and two finite numbers.
fn change_fits(change: &V) -> bool {
    let V::Map(change) = change else {
        return false;
    };
    let number = |value: Option<&V>| match value {
        Some(V::Float(number)) => number.is_finite(),
        Some(V::Int(_) | V::Bool(_)) => true,
        _ => false,
    };
    change.get("entity_id").is_some_and(identifier_value) && number(change.get("before")) && number(change.get("after"))
}

/// `make_staff_operation_record(...)[0]` of a world that has the shared contract.
pub fn staff_operation_record(world: &World, outcome: OperationOutcome) -> Result<Made> {
    let minute = world.minute_index()?;
    let observation = if !outcome.changes.is_empty() {
        "observed_change"
    } else if outcome.measured {
        "observed_no_change"
    } else {
        "not_measured"
    };
    let total = outcome.changes.len();
    let sampled = &outcome.changes[..total.min(256)];
    let times_fit = match (&outcome.start_minute, &outcome.arrival_minute, &outcome.travel_minutes) {
        (V::Int(start), V::Int(arrival), V::Int(travel)) => {
            *start >= 0 && *travel >= 0 && start <= arrival && *arrival <= minute && *travel <= arrival - start
        }
        _ => false,
    };
    let reason_fits = matches!(&outcome.reason, V::Str(text) if (1..=240).contains(&text.chars().count()));
    let fits = times_fit
        && sampled.iter().all(change_fits)
        && reason_fits
        && identifier_value(&outcome.task_id)
        && identifier_value(&outcome.staff_id)
        && (outcome.target_id == V::Null || identifier_value(&outcome.target_id))
        && identifier_value(&outcome.target_zone_id)
        && is_identifier(&outcome.causation)
        && minute >= 0
        && matches!(&outcome.task_kind, V::Str(kind) if matches!(&**kind, "cleaning" | "maintenance" | "information_outreach" | "intake" | "patrol"))
        && match &outcome.status {
            V::Str(status) if &**status == "completed" => true,
            V::Str(status) if &**status == "blocked" => total == 0 && outcome.measured,
            _ => false,
        };
    if !fits {
        return refused();
    }
    let V::Str(status) = &outcome.status else {
        return refused();
    };
    let detail = format!("staff operation {status} outcome recorded after state application");
    let (minute, sequence) = minute_and_sequence(world)?;
    let event_id = format!("staff-operation-transition-{sequence}");
    let mut payload = header("operations", "staff_operation_transition", &event_id);
    put(&mut payload, "task_id", outcome.task_id.clone());
    put(&mut payload, "staff_id", outcome.staff_id.clone());
    put(&mut payload, "task_kind", outcome.task_kind.clone());
    put(&mut payload, "target_id", outcome.target_id.clone());
    put(&mut payload, "target_zone_id", outcome.target_zone_id.clone());
    put(&mut payload, "reason", outcome.reason.clone());
    put(&mut payload, "status", outcome.status.clone());
    put(&mut payload, "minute", V::Int(minute));
    put(&mut payload, "start_minute", outcome.start_minute.clone());
    put(&mut payload, "arrival_minute", outcome.arrival_minute.clone());
    put(&mut payload, "travel_minutes", outcome.travel_minutes.clone());
    put(&mut payload, "outcome_observation", str_value(observation));
    put(&mut payload, "state_changes", V::List(sampled.to_vec()));
    put(&mut payload, "total_state_change_count", V::Int(total as i64));
    put(&mut payload, "state_changes_truncated", V::Bool(total > sampled.len()));
    put(&mut payload, "causation_id", V::Str(outcome.causation.clone()));
    put(&mut payload, "source_ids", V::List(vec![str_value(OPERATION_SOURCE)]));
    made(Event {
        event_id,
        sequence,
        minute,
        event_type: "staff_operation_transition",
        payload,
        source: OPERATION_SOURCE,
        causation: &outcome.causation,
        policy: OPERATION_POLICY,
        detail,
    })
}

// ---- What the receipts read of the namespace, through the host (lane P9-1a) -------------------

/// The refusal of a namespace that is None in `model_state`.
pub const NAMESPACE_IS_NONE: &str = "the namespace is None";

/// Whether `append_applied_care_transition` has nothing to append to, as the host's `Holding`
/// answers: no namespace. One that is None is refused, as is a table that is not a plain dict.
pub fn nothing_to_append(holding: &Holding) -> Result<bool> {
    if !holding.is_absent(NAME).map_err(Error::unsupported)? {
        return Ok(false);
    }
    if holding.contains(NAME) {
        return Err(Error::unsupported(NAMESPACE_IS_NONE));
    }
    Ok(true)
}

/// The world's `model_state` holds the tree (or the detached row) of `shared_operations_v1`, as the
/// host's `Holding` answers.
pub fn check_tree(holding: &Holding) -> Result<()> {
    if !holding.table {
        return Err(Error::unsupported(TABLE_NOT_PLAIN));
    }
    match holding.keys.get(NAME).map(|key| key.hold) {
        Some(Hold::Held) => Ok(()),
        Some(Hold::NotOwned) => Err(Error::unsupported(crate::host::NOT_OWNED)),
        _ => Err(Error::unsupported(common_operations::OTHER_DOCUMENT)),
    }
}

// ---- ingest_applied_shared_operation_records, through the host (lane P9-1a) ---------------------

/// The refusal of a namespace that is neither a dict nor the detached row's placeholder.
pub const ANOTHER_VALUE: &str = "the namespace is another value";

/// Whether `model_state` has the namespace for the ingest to project into, as the host's `Holding`
/// answers: a value that is neither a dict nor the detached row's placeholder is refused, as is a
/// table that is not a plain dict.
pub fn has_namespace(holding: &Holding) -> Result<bool> {
    if !holding.table {
        return Err(Error::unsupported(TABLE_NOT_PLAIN));
    }
    match holding.keys.get(NAME) {
        None => Ok(false),
        Some(key) if key.hold != Hold::Held && !key.dict => Err(Error::unsupported(ANOTHER_VALUE)),
        Some(_) => Ok(true),
    }
}

/// `ingest_applied_shared_operation_records(world, (record,))` of one APPLIED record a kernel of the
/// core wrote at `realized_minute`, of an event it made (lane c6: the continuity receipt of a
/// replanned support): whether `model_state` has the namespace is the host's `Holding` (H1), and the
/// EventLog's records the receipt reads are looked up through the host (`Host::find_record`).
pub fn ingest_applied<C: WorldCell, H: Host>(cell: &C, host: &mut H, event: &EventRow, realized_minute: i64) -> std::result::Result<(), H::Error> {
    let holding = kit::holding(cell, host)?;
    if !has_namespace(&holding)? {
        return Ok(());
    }
    if !RECEIPT_EVENTS.contains(&&*event.event_type) {
        return Ok(());
    }
    let id = &*event.event_id;
    let recorded = cell.with(|world| -> Result<bool> {
        let minute = world.minute_index()?;
        common_operations::is_recorded(world, id, realized_minute == minute, minute)
    })??;
    if recorded {
        return Ok(());
    }
    let receipt = Receipt::of(&event.payload)?;
    common_operations::record_through(cell, host, &receipt)
}
