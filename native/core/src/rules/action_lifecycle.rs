//! `domain/action_lifecycle.py`: lifecycle receipts of autonomous actions (docs/rust-port/lanes/
//! f-action-lifecycle.md). Its rules are these kernels alone (`kernel_only`). Moved to the core in
//! lane P8 a2.
//!
//! `ActiveActions` holds the open action of each agent as rows. The controller keeps it, and it is
//! not saved: a restored Run makes it again from the event log (`active_action_index`).
//!
//! Every kernel plans first: it reads the rows and what it was given and builds each receipt as an
//! `EventRow`, with the sequence it will get. A receipt the receipts' model would refuse, or decide
//! with its own text, is Unsupported with nothing changed. Then `commit` allocates the sequences and
//! appends the records to the world's log (`kit::Events`).
//!
//! A receipt is of schema 2 (#255): it writes each needs snapshot once. "started" has the needs at
//! the start as `start_needs` and `current_needs` empty; a closing receipt ("completed",
//! "interrupted", "abandoned", "censored") has the needs now as `current_needs` and `start_needs`
//! empty, as the "started" receipt of the same action holds them; "continued" has both. Schema 1
//! receipts, in the logs of older Runs, had both on every receipt.

use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};

use indexmap::IndexMap;

use crate::error::{Error, Result};
use crate::events::log::EventLog;
use crate::events::writer::WriteError;
use crate::events::{is_stable_id, EventKind, EventRow, EventStatus};
use crate::row::IdMap;
use crate::rules::kit::{self, Events};
use crate::state::body_state::{BodyRow, NAME as BODY_STATE};
use crate::state_row;
use crate::value::V;
use crate::world::World;

const SOURCE_ID: &str = "ASM-ACTION-LIFECYCLE-V1";
const POLICY_VERSION: &str = "applied-action-lifecycle-v2";
const EVENT_TYPE: &str = "agent_action_lifecycle";
/// The receipts' schema (`ActionLifecycleReceipt`): 2 since #255, each needs snapshot written once.
const RECEIPT_SCHEMA_VERSION: i64 = 2;
const MAX_REASON_CHARS: usize = 120;
const MAX_ACTIVITY_CHARS: usize = 80;
const POPULATION_LIFECYCLE: &str = "population_lifecycle_v1";
const POPULATION_DYNAMICS: &str = "population_dynamics_v1";

/// The strings every receipt holds, so that a receipt allocates none of them.
struct Words {
    schema_version: Arc<str>,
    action_id: Arc<str>,
    agent_id: Arc<str>,
    action: Arc<str>,
    target_id: Arc<str>,
    transition: Arc<str>,
    start_minute: Arc<str>,
    transition_minute: Arc<str>,
    duration_minutes: Arc<str>,
    reason_code: Arc<str>,
    activity: Arc<str>,
    start_activity: Arc<str>,
    start_needs: Arc<str>,
    current_needs: Arc<str>,
    model_id: Arc<str>,
    model_source_id: Arc<str>,
    source_type: Arc<str>,
    source_id: Arc<str>,
    policy_version: Arc<str>,
    assumption: Arc<str>,
    event_type: Arc<str>,
    model: Arc<str>,
    model_source: Arc<str>,
    needs: [Arc<str>; 6],
}

static WORDS: LazyLock<Words> = LazyLock::new(|| Words {
    schema_version: Arc::from("schema_version"),
    action_id: Arc::from("action_id"),
    agent_id: Arc::from("agent_id"),
    action: Arc::from("action"),
    target_id: Arc::from("target_id"),
    transition: Arc::from("transition"),
    start_minute: Arc::from("start_minute"),
    transition_minute: Arc::from("transition_minute"),
    duration_minutes: Arc::from("duration_minutes"),
    reason_code: Arc::from("reason_code"),
    activity: Arc::from("activity"),
    start_activity: Arc::from("start_activity"),
    start_needs: Arc::from("start_needs"),
    current_needs: Arc::from("current_needs"),
    model_id: Arc::from("model_id"),
    model_source_id: Arc::from("model_source_id"),
    source_type: Arc::from("source_type"),
    source_id: Arc::from("source_id"),
    policy_version: Arc::from("policy_version"),
    assumption: Arc::from("assumption"),
    event_type: Arc::from(EVENT_TYPE),
    model: Arc::from(POLICY_VERSION),
    model_source: Arc::from(SOURCE_ID),
    needs: NEED_NAMES.map(Arc::from),
});

const NEED_NAMES: [&str; 6] =
    ["toilet_urgency", "hunger", "hydration_need", "fatigue", "sleep_debt_minutes", "hygiene_need"];
const FATIGUE: usize = 3;
const SLEEP_DEBT: usize = 4;

state_row! {
    /// One open action, as `active_actions[agent_id]` holds it.
    pub struct ActiveAction {
        action_id: Arc<str>,
        agent_id: Arc<str>,
        action: Arc<str>,
        target_id: Option<Arc<str>>,
        start_minute: i64,
        reason_code: Arc<str>,
        start_activity: Option<Arc<str>>,
        start_needs: IdMap<f64>,
        model_id: Arc<str>,
        model_source_id: Arc<str>,
        continued: bool,
    }
}

/// The open actions of a controller, by agent, in the order a dict of them would have: a closed
/// action leaves, and a new one is the last key.
#[derive(Clone, Default)]
pub struct ActiveActions {
    pub rows: IdMap<ActiveAction>,
}

impl ActiveActions {
    /// The rows of `active_action_index`, each under its own agent's key; Unsupported otherwise.
    pub fn from_rows(rows: IdMap<ActiveAction>) -> Result<Self> {
        if rows.iter().any(|(key, action)| *key != action.agent_id) {
            return Err(Error::unsupported("an active action is under another agent's key"));
        }
        Ok(Self { rows })
    }

    pub fn len(&self) -> usize {
        self.rows.len()
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    pub fn contains(&self, agent_id: &str) -> bool {
        self.rows.contains_key(agent_id)
    }

    /// `active_action_index`: the open actions the log's agent_action_lifecycle records leave, made
    /// again after a restore (#251), in the order of the dict that function builds. An error for a
    /// deciding record the log cannot read back or whose payload is not an action's.
    pub fn of_log(log: &EventLog) -> Result<Self> {
        let mut rows = IdMap::default();
        for index in log.active_lifecycle_positions() {
            let record = log.record(index)?.ok_or_else(|| Error::unsupported("the log does not hold an action's record"))?;
            let V::Map(payload) = &record.event.payload else { continue };
            let Some(V::Str(agent_id)) = payload.get("agent_id") else { continue };
            let continued = match payload.get("transition") {
                Some(V::Str(transition)) if matches!(&**transition, "completed" | "interrupted" | "abandoned" | "censored") => {
                    rows.shift_remove(agent_id);
                    continue;
                }
                Some(V::Str(transition)) if matches!(&**transition, "started" | "continued") => &**transition == "continued",
                _ => continue,
            };
            let mut fields = IndexMap::with_capacity(ActiveAction::NAMES.len());
            for key in ["action_id", "agent_id", "action", "target_id", "start_minute", "reason_code", "start_activity", "start_needs", "model_id", "model_source_id"] {
                let value = payload.get(key).ok_or_else(|| Error::unsupported(format!("an action's record has no {key}")))?;
                fields.insert(Arc::from(key), value.clone());
            }
            fields.insert(Arc::from("continued"), V::Bool(continued));
            let row = <ActiveAction as crate::row::Field>::from_value(&V::Map(fields)).map_err(|misfit| Error::unsupported(format!("an action's record does not fit its row: {}", misfit.0)))?;
            rows.insert(agent_id.clone(), row);
        }
        Self::from_rows(rows)
    }
}

// ---- What the rule reads ---------------------------------------------------------------------------

/// `_need_snapshot`, in `NEED_NAMES` order.
type Needs = [f64; 6];

/// `sleep_debt_minutes` and `hygiene_need` of `body_state_v1.agents[id]`, 0.0 without them.
fn body_needs(world: &World, id: &str) -> Result<(f64, f64)> {
    if kit::namespace_absent(world, BODY_STATE)? {
        return Ok((0.0, 0.0));
    }
    let row = world.row::<BodyRow>(BODY_STATE)?;
    Ok(row.agents.get(id).map_or((0.0, 0.0), |record| (record.sleep_debt_minutes, record.hygiene_need)))
}

/// What the rule reads of an agent who is in the world.
struct Agent {
    activity: &'static str,
    needs: Needs,
}

fn agent_of(world: &World, id: &str) -> Result<Option<Agent>> {
    let found = {
        let rows = world.rows()?;
        let Some(agent) = rows.agents.get(id) else {
            return Ok(None);
        };
        let needs = &agent.needs;
        (agent.activity, [needs.toilet_urgency, needs.hunger, needs.hydration_need, needs.fatigue], agent.profile.id.clone())
    };
    let (activity, [toilet, hunger, hydration, fatigue], profile) = found;
    let (debt, hygiene) = body_needs(world, &profile)?;
    Ok(Some(Agent { activity: activity.text(), needs: [toilet, hunger, hydration, fatigue, debt, hygiene] }))
}

/// `_completion_status`, whose cause is always None.
fn completion_status(world: &World, action: &ActiveAction, needs: &Needs) -> Result<&'static str> {
    Ok(match &*action.action {
        "wait" => {
            let now = world.minute_index().map_err(|_| Error::unsupported("a value is not an int"))?;
            if now > action.start_minute {
                "completed"
            } else {
                "interrupted"
            }
        }
        "rest" | "sleep" => {
            let lower = |index: usize| {
                action.start_needs.get(NEED_NAMES[index]).is_some_and(|started| needs[index] < *started)
            };
            if lower(FATIGUE) || (&*action.action == "sleep" && lower(SLEEP_DEBT)) {
                "completed"
            } else {
                "interrupted"
            }
        }
        _ => "interrupted",
    })
}

// ---- A receipt ---------------------------------------------------------------------------------------

/// A receipt to append, and what it does at `commit`.
struct Receipt {
    row: EventRow,
    minute: i64,
    detail: String,
}

fn needs_value(needs: &Needs) -> V {
    let mut map = IndexMap::with_capacity(6);
    for (name, value) in WORDS.needs.iter().zip(needs) {
        map.insert(name.clone(), V::Float(*value));
    }
    V::Map(map)
}

fn optional_text(value: Option<&Arc<str>>) -> V {
    value.map_or(V::Null, |text| V::Str(text.clone()))
}

fn fits_chars(value: Option<&str>, limit: usize) -> bool {
    value.is_none_or(|text| text.chars().nth(limit).is_none())
}

/// What the event of `_append_transition` is; Unsupported where `_is_plain_receipt` is false, or
/// where the event or the log would refuse it.
#[allow(clippy::too_many_arguments)]
fn receipt(
    events: &Events<'_>,
    action: &ActiveAction,
    transition: &str,
    minute: i64,
    reason_code: &str,
    activity: Option<&str>,
    causation_id: Option<&str>,
    sequence: i64,
    current: Option<&Needs>,
) -> Result<Receipt> {
    let plain = 0 <= action.start_minute
        && action.start_minute <= minute
        && !reason_code.is_empty()
        && fits_chars(Some(reason_code), MAX_REASON_CHARS)
        && fits_chars(activity, MAX_ACTIVITY_CHARS)
        && fits_chars(action.start_activity.as_deref(), MAX_ACTIVITY_CHARS)
        && action.target_id.as_deref().is_none_or(is_stable_id)
        && is_stable_id(&action.action_id)
        && is_stable_id(&action.agent_id)
        && is_stable_id(&action.action)
        && is_stable_id(&action.model_id)
        && is_stable_id(&action.model_source_id)
        && action.start_needs.values().all(|value| value.is_finite())
        && current.is_none_or(|needs| needs.iter().all(|value| value.is_finite()));
    if !plain {
        return Err(Error::unsupported("a receipt the model has to decide"));
    }
    let words = &*WORDS;
    // A receipt of schema 2 writes each needs snapshot once (#255): "started" its needs as
    // `start_needs` only, a closing receipt the needs now as `current_needs` only (its start's are
    // the "started" receipt's of the same action), "continued" both, as the open action is made
    // again from it on restore (`active_action_index`).
    let (start_needs, current_needs) = match transition {
        "started" => (true, false),
        "continued" => (true, true),
        _ => (false, true),
    };
    let start_needs = if start_needs {
        action.start_needs.iter().map(|(name, value)| (name.clone(), V::Float(*value))).collect()
    } else {
        IndexMap::new()
    };
    let current_needs = match current {
        Some(needs) if current_needs => needs_value(needs),
        _ => V::Map(IndexMap::new()),
    };
    let mut payload = IndexMap::with_capacity(16);
    payload.insert(words.schema_version.clone(), V::Int(RECEIPT_SCHEMA_VERSION));
    payload.insert(words.action_id.clone(), V::Str(action.action_id.clone()));
    payload.insert(words.agent_id.clone(), V::Str(action.agent_id.clone()));
    payload.insert(words.action.clone(), V::Str(action.action.clone()));
    payload.insert(words.target_id.clone(), optional_text(action.target_id.as_ref()));
    payload.insert(words.transition.clone(), V::Str(Arc::from(transition)));
    payload.insert(words.start_minute.clone(), V::Int(action.start_minute));
    payload.insert(words.transition_minute.clone(), V::Int(minute));
    payload.insert(words.duration_minutes.clone(), V::Int(minute - action.start_minute));
    payload.insert(words.reason_code.clone(), V::Str(Arc::from(reason_code)));
    payload.insert(words.activity.clone(), activity.map_or(V::Null, |text| V::Str(Arc::from(text))));
    payload.insert(words.start_activity.clone(), optional_text(action.start_activity.as_ref()));
    payload.insert(words.start_needs.clone(), V::Map(start_needs));
    payload.insert(words.current_needs.clone(), current_needs);
    payload.insert(words.model_id.clone(), V::Str(action.model_id.clone()));
    payload.insert(words.model_source_id.clone(), V::Str(action.model_source_id.clone()));
    let mut provenance = IndexMap::with_capacity(3);
    provenance.insert(words.source_type.clone(), V::Str(words.assumption.clone()));
    provenance.insert(words.source_id.clone(), V::Str(words.model_source.clone()));
    provenance.insert(words.policy_version.clone(), V::Str(words.model.clone()));
    let event_id: Arc<str> = Arc::from(format!("action-lifecycle-{sequence}"));
    if events.is_realized(&event_id, Some(sequence))? {
        return Err(Error::unsupported("the log already has this receipt's identity"));
    }
    let row = EventRow::new(
        event_id,
        EventKind::Endogenous,
        minute,
        0,
        sequence,
        words.event_type.clone(),
        payload,
        Some(words.model.clone()),
        causation_id.map(Arc::from),
        provenance,
    );
    let Some(row) = row else {
        return Err(Error::unsupported("an event the Python class refuses"));
    };
    Ok(Receipt { row, minute, detail: format!("autonomous action {transition}") })
}

/// A write error of the log, as a core error: `receipt` refused every repeat, so a duplicate here
/// is a bug.
fn written(result: std::result::Result<(), WriteError>) -> Result<()> {
    match result {
        Ok(()) => Ok(()),
        Err(WriteError::Error(error)) => Err(error),
        Err(WriteError::Duplicate(text)) => Err(Error::Runtime(text)),
    }
}

/// Allocate each receipt's sequence and append it: `allocate_event_sequence` and `EventLog.append`.
///
/// Nothing a world holds can fail here, so receipts are never half written because of data: `receipt`
/// has refused a repeated ID or sequence, a minute out of range and a value that is not finite, and
/// `Events::of` a counter a rule could overflow. What is left is a bug, which raises with the earlier
/// receipts of this call appended and the open actions as they were, and that stops the Run.
fn commit(events: &Events<'_>, receipts: Vec<Receipt>) -> Result<()> {
    for receipt in receipts {
        let sequence = events.allocate_sequence()?;
        debug_assert_eq!(sequence, receipt.row.sequence);
        written(events.record(&receipt.row, receipt.minute, EventStatus::Applied, Some(&receipt.detail)))?;
    }
    Ok(())
}

// ---- record_action_proposal --------------------------------------------------------------------------

/// What `record_action_proposal` reads of an applied proposal.
pub struct Proposal<'a> {
    pub agent_id: &'a str,
    pub action: &'a str,
    pub target_id: Option<&'a str>,
    pub minute: i64,
    pub trigger: &'a str,
}

/// `record_action_proposal` of a committed proposal. Unsupported, with nothing changed, for what
/// the receipts' model would decide.
pub fn commit_action(world: &World, active: &mut ActiveActions, proposal: &Proposal<'_>) -> Result<()> {
    let Some(agent) = agent_of(world, proposal.agent_id)? else {
        return Ok(());
    };
    let events = Events::of(world)?;
    let (minute, activity) = (proposal.minute, Some(agent.activity));
    let first = events.next_sequence()?;
    let mut receipts = Vec::with_capacity(2);
    if let Some(current) = active.rows.get(proposal.agent_id) {
        let same_target = current.target_id.as_deref() == proposal.target_id;
        if &*current.action == proposal.action && same_target {
            if current.continued {
                return Ok(());
            }
            receipts.push(receipt(&events, current, "continued", minute, proposal.trigger, activity, None, first, Some(&agent.needs))?);
            commit(&events, receipts)?;
            if let Some(current) = active.rows.get_mut(proposal.agent_id) {
                current.continued = true;
            }
            return Ok(());
        }
        let status = completion_status(world, current, &agent.needs)?;
        receipts.push(receipt(
            &events,
            current,
            status,
            minute,
            "superseded_by_new_applied_action",
            activity,
            None,
            first,
            Some(&agent.needs),
        )?);
    }
    let sequence = first + receipts.len() as i64;
    let mut start_needs = IdMap::with_capacity_and_hasher(6, Default::default());
    for (name, value) in WORDS.needs.iter().zip(agent.needs) {
        start_needs.insert(name.clone(), value);
    }
    let started = ActiveAction {
        action_id: Arc::from(format!("action-{sequence}")),
        agent_id: Arc::from(proposal.agent_id),
        action: Arc::from(proposal.action),
        target_id: proposal.target_id.map(Arc::from),
        start_minute: minute,
        reason_code: Arc::from(proposal.trigger),
        start_activity: Some(Arc::from(agent.activity)),
        start_needs,
        model_id: WORDS.model.clone(),
        model_source_id: WORDS.model_source.clone(),
        continued: false,
    };
    receipts.push(receipt(&events, &started, "started", minute, proposal.trigger, activity, None, sequence, Some(&agent.needs))?);
    commit(&events, receipts)?;
    // The old action goes and the new one is the dict's last key, as pop and insert leave it.
    active.rows.shift_remove(proposal.agent_id);
    active.rows.insert(started.agent_id.clone(), started);
    Ok(())
}

// ---- The applied events ------------------------------------------------------------------------------

/// `_COMPLETION_ACTIONS`, `_ABANDONED_ACTIONS` and `_INTERRUPTED_ACTIONS`: what an event of this type
/// does to an action, and None for an action it does not end.
fn event_ends(event_type: &str, action: &str) -> Option<&'static str> {
    match (event_type, action) {
        ("toilet_service_complete", "request_toilet")
        | ("resource_service_complete", "fetch_water" | "collect_meal")
        | ("staff_support_complete", "seek_help" | "seek_information")
        | ("social_interaction", "socialize" | "care_family" | "help_others") => Some("completed"),
        ("facility_queue_abandon", "request_toilet" | "fetch_water" | "collect_meal") => Some("abandoned"),
        ("resource_service_cancel", "fetch_water" | "collect_meal")
        | ("support_request_timeout", "seek_help" | "seek_information") => Some("interrupted"),
        _ => None,
    }
}

/// Whether an event of this type can end an action: nothing else changes anything here.
pub fn ends_any_action(event_type: &str) -> bool {
    matches!(
        event_type,
        "toilet_service_complete"
            | "resource_service_complete"
            | "staff_support_complete"
            | "social_interaction"
            | "facility_queue_abandon"
            | "resource_service_cancel"
            | "support_request_timeout"
    )
}

/// What `record_applied_action_completion` reads of an applied event: its row's fields, or those of
/// an object Python made.
pub struct Ending<'a> {
    pub event_id: &'a str,
    pub event_type: &'a str,
    pub due_minute: i64,
    /// A mapping (`V::Map`) with at least the keys of `ENDING_KEYS` the event has.
    pub payload: &'a V,
}

/// The payload's keys the rule reads.
pub const ENDING_KEYS: [&str; 6] = ["agent_id", "sender_agent_id", "facility_id", "participant_agent_id", "staff_id", "request_id"];

impl<'a> Ending<'a> {
    pub fn of(row: &'a EventRow) -> Self {
        Self { event_id: &row.event_id, event_type: &row.event_type, due_minute: row.due_minute, payload: &row.payload }
    }
}

/// `payload.get(key)` when it is a str.
fn text<'a>(event: &Ending<'a>, key: &str) -> Option<&'a Arc<str>> {
    match event.payload {
        V::Map(map) => match map.get(key) {
            Some(V::Str(value)) => Some(value),
            _ => None,
        },
        _ => None,
    }
}

/// The agent whose open action the event ends, and how: None where the event ends none. It reads
/// only the open actions, so a caller asks it before it borrows the world.
pub fn closes(active: &ActiveActions, event: &Ending<'_>) -> Option<(Arc<str>, &'static str)> {
    let event_type = event.event_type;
    if !ends_any_action(event_type) {
        return None;
    }
    let agent_id = text(event, "agent_id").or_else(|| text(event, "sender_agent_id"))?;
    let current = active.rows.get(&**agent_id)?;
    let support = matches!(event_type, "staff_support_complete" | "support_request_timeout");
    let keys: &[&str] =
        if support { &["request_id", "staff_id"] } else { &["facility_id", "participant_agent_id", "staff_id", "request_id"] };
    let observed = keys.iter().find_map(|key| text(event, key));
    if let (Some(expected), Some(observed)) = (&current.target_id, observed) {
        if expected != observed {
            return None;
        }
    }
    if event.due_minute < current.start_minute {
        return None;
    }
    Some((agent_id.clone(), event_ends(event_type, &current.action)?))
}

/// `record_applied_action_completion` of an event the caller knows the log holds applied. Says
/// Unsupported before any change.
pub fn complete_for_event(world: &World, active: &mut ActiveActions, event: &Ending<'_>, minute: i64) -> Result<()> {
    let Some((agent_id, status)) = closes(active, event) else {
        return Ok(());
    };
    let event_type = event.event_type;
    let Some(current) = active.rows.get(&*agent_id) else {
        return Ok(());
    };
    let events = Events::of(world)?;
    let agent = agent_of(world, &agent_id)?;
    let sequence = events.next_sequence()?;
    let planned = receipt(
        &events,
        current,
        status,
        minute,
        event_type,
        agent.as_ref().map(|agent| agent.activity),
        Some(event.event_id),
        sequence,
        agent.as_ref().map(|agent| &agent.needs),
    )?;
    commit(&events, vec![planned])?;
    active.rows.shift_remove(&*agent_id);
    Ok(())
}

/// `state.get(field, ())` of a namespace held as a value, for a list: f gets each item.
fn items_of(world: &World, name: &str, field: &str, mut f: impl FnMut(&V)) -> Result<()> {
    let Some(state) = world.slots.try_value(name)? else {
        return Ok(());
    };
    let V::Map(state) = &*state else {
        return Ok(());
    };
    match state.get(field) {
        Some(V::List(rows)) => {
            rows.iter().for_each(&mut f);
            Ok(())
        }
        Some(_) => Err(Error::unsupported("a value is not a plain list")),
        None => Ok(()),
    }
}

/// The agents an applied population event took away, and why: the middle of
/// `record_applied_population_exit`.
fn population_causes(world: &World, event_id: &str) -> Result<BTreeMap<String, &'static str>> {
    let mut causes = BTreeMap::new();
    let matching = |row: &V| -> Option<String> {
        let V::Map(row) = row else {
            return None;
        };
        if !matches!(row.get("event_id"), Some(V::Str(id)) if &**id == event_id) {
            return None;
        }
        match row.get("agent_id") {
            Some(V::Str(agent_id)) => Some(agent_id.to_string()),
            _ => None,
        }
    };
    items_of(world, POPULATION_LIFECYCLE, "history", |row| {
        let Some(agent_id) = matching(row) else {
            return;
        };
        let V::Map(fields) = row else {
            return;
        };
        match fields.get("action") {
            Some(V::Str(action)) if &**action == "temporary_exit" => {
                causes.insert(agent_id, "population_temporary_exit");
            }
            Some(V::Str(action)) if &**action == "transferred" => {
                causes.insert(agent_id, "population_transfer");
            }
            _ => {}
        }
    })?;
    items_of(world, POPULATION_DYNAMICS, "departures", |row| {
        if let Some(agent_id) = matching(row) {
            causes.insert(agent_id, "population_departure");
        }
    })?;
    Ok(causes)
}

/// `record_applied_population_exit` of an applied `effect_program_population` event: the open
/// actions of the residents it took away close "interrupted", in agent ID order.
pub fn population_exit(world: &World, active: &mut ActiveActions, event_id: &str, minute: i64) -> Result<()> {
    let causes = population_causes(world, event_id)?;
    let events = Events::of(world)?;
    let mut sequence = events.next_sequence()?;
    let mut receipts = Vec::new();
    let mut closed = Vec::new();
    for (agent_id, reason) in &causes {
        let Some(current) = active.rows.get(agent_id.as_str()) else {
            continue;
        };
        let agent = agent_of(world, agent_id)?;
        receipts.push(receipt(
            &events,
            current,
            "interrupted",
            minute,
            reason,
            None,
            Some(event_id),
            sequence,
            agent.as_ref().map(|agent| &agent.needs),
        )?);
        closed.push(agent_id);
        sequence += 1;
    }
    commit(&events, receipts)?;
    for agent_id in closed {
        active.rows.shift_remove(agent_id.as_str());
    }
    Ok(())
}

/// `finalize_active_actions`: every action still open closes, in agent ID order.
pub fn finalize(world: &World, active: &mut ActiveActions, minute: i64) -> Result<()> {
    let events = Events::of(world)?;
    let mut ordered: Vec<&ActiveAction> = active.rows.values().collect();
    ordered.sort_unstable_by(|first, second| first.agent_id.cmp(&second.agent_id));
    let first = events.next_sequence()?;
    let mut receipts = Vec::with_capacity(ordered.len());
    for (index, current) in ordered.into_iter().enumerate() {
        let agent = agent_of(world, &current.agent_id)?;
        let status = match &agent {
            None => "censored",
            Some(agent) if completion_status(world, current, &agent.needs)? == "completed" => "completed",
            Some(_) => "censored",
        };
        receipts.push(receipt(
            &events,
            current,
            status,
            minute,
            "simulation_end_right_censor",
            agent.as_ref().map(|agent| agent.activity),
            None,
            first + index as i64,
            agent.as_ref().map(|agent| &agent.needs),
        )?);
    }
    commit(&events, receipts)?;
    active.rows.clear();
    Ok(())
}
