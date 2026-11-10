//! The realized EventLog without Python (docs/rust-port.md, stage B1): each record's index
//! fields, its canonical JSON bytes while the log holds them, and the indexes the EventLog keeps
//! (ID, sequence, minute, event type, provenance object keys, care requests, completed support
//! kinds, applied facility events), so lookups and audits need no record objects.
//!
//! A record a Rust rule appends (`append_row`) has its bytes written here. The PyO3 crate's
//! `EventStore` (`native/src/eventlog.rs`) keeps the Python objects of each position beside this:
//! a record appended from Python has its object and the bytes written from its frozen fields, and
//! a record read back from a saved Checkpoint has the Python bytes it was read from. When Python
//! asks for a record's bytes, `take_written` gives them up to the Python bytes object made of them,
//! which the save's caches then know by identity; that object is the record's bytes from then on.
//!
//! The bytes are what restoring the record reads, so a log whose every record has bytes written
//! or checked here (`all_encoded`) is its own restored log: a save's restore check neither parses
//! those bytes nor compares them with the objects (`core/checkpoint_cache.py`). A record with a
//! value the writer refuses, such as a str subclass in a payload, has only its object; the Python
//! encoder makes its bytes when asked, and the restore check then runs as it always did.
//!
//! Adding checks duplicate IDs and sequences and returns a code; the Python EventLog raises the
//! same DuplicateEventError as before.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use super::{is_stable_id, EventRow, EventStatus};
use crate::error::Result;
use crate::value::V;

/// What the indexes and a Rust rule read of one record.
#[derive(Clone, Debug)]
struct Entry {
    event_id: Arc<str>,
    /// The bytes until Python asks for them (`take_written`).
    written: Option<Box<[u8]>>,
    event_type: u32,
    applied: bool,
    minute: i64,
    /// Whether the bytes were written or read back here, so the record made from them is the
    /// same and its object may be let go (`releasable`).
    exact: bool,
}

/// What the indexes read from one record.
#[derive(Clone, Debug, Default)]
pub struct Fields {
    pub event_id: String,
    pub sequence: i64,
    pub event_type: String,
    pub minute: i64,
    pub applied: bool,
    pub request_id: Option<String>,
    pub agent_id: Option<String>,
    pub support_kind: Option<String>,
    pub payload_status_completed: bool,
    pub facility_id: Option<String>,
    pub provenance_objects: Vec<String>,
    // For agent_action_lifecycle records only.
    pub transition: Option<String>,
}

/// What one agent_action_lifecycle record does to its agent's active action.
#[derive(Clone, Copy, Debug, PartialEq)]
enum LifecycleStep {
    Set,
    End,
    Other,
}

pub const ACTION_LIFECYCLE_EVENT_TYPE: &str = "agent_action_lifecycle";

#[derive(Clone, Debug)]
pub struct EventLog {
    entries: Vec<Entry>,
    // Records with bytes only the Python encoder can make.
    unencoded: usize,
    type_names: Vec<String>,
    type_ids: HashMap<String, u32>,
    by_id: HashMap<Arc<str>, usize>,
    sequences: HashSet<i64>,
    max_sequence: i64,
    by_minute: BTreeMap<i64, Vec<usize>>,
    by_type: HashMap<u32, Vec<usize>>,
    by_provenance: HashMap<String, Vec<usize>>,
    first_care_request: HashMap<String, String>,
    completed_support_kinds: HashMap<String, Vec<String>>,
    applied_facility_events: HashMap<String, Vec<String>>,
    // In append order: each agent_action_lifecycle record with a str agent_id.
    lifecycle: Vec<(u32, LifecycleStep, usize)>,
    lifecycle_agents: Vec<String>,
    lifecycle_agent_ids: HashMap<String, u32>,
    // Records an audit checked and the bounds it checked them against.
    audited: usize,
    audit_bounds: (Option<i64>, Option<i64>),
    // The event types a reader asked for all records of (`scan_type`): it is likely to ask
    // again, so `releasable` keeps their objects.
    scanned_types: HashSet<String>,
}

impl Default for EventLog {
    fn default() -> Self {
        Self::new()
    }
}

impl EventLog {
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
            unencoded: 0,
            type_names: Vec::new(),
            type_ids: HashMap::new(),
            by_id: HashMap::new(),
            sequences: HashSet::new(),
            max_sequence: -1,
            by_minute: BTreeMap::new(),
            by_type: HashMap::new(),
            by_provenance: HashMap::new(),
            first_care_request: HashMap::new(),
            completed_support_kinds: HashMap::new(),
            applied_facility_events: HashMap::new(),
            lifecycle: Vec::new(),
            lifecycle_agents: Vec::new(),
            lifecycle_agent_ids: HashMap::new(),
            audited: 0,
            audit_bounds: (None, None),
            scanned_types: HashSet::new(),
        }
    }

    /// 0, or 1 for a realized ID, 2 for a realized sequence: what `add` would say, changing
    /// nothing.
    pub fn refusal(&self, event_id: &str, sequence: i64) -> u8 {
        if self.by_id.contains_key(event_id) {
            1
        } else if self.sequences.contains(&sequence) {
            2
        } else {
            0
        }
    }

    /// Adds a record at position `len()`: 0, or 1 for a realized ID, 2 for a realized sequence,
    /// with nothing changed. `written` is its bytes made here; `has_bytes` says that the caller
    /// holds bytes read back and checked here instead. A record with neither has bytes only the
    /// Python encoder can make.
    pub fn add(&mut self, fields: Fields, has_bytes: bool, written: Option<Box<[u8]>>) -> u8 {
        if self.by_id.contains_key(fields.event_id.as_str()) {
            return 1;
        }
        if self.sequences.contains(&fields.sequence) {
            return 2;
        }
        let index = self.entries.len();
        if fields.event_type == "care_support_transition" {
            if let Some(request_id) = &fields.request_id {
                self.first_care_request
                    .entry(request_id.clone())
                    .or_insert_with(|| fields.event_id.clone());
            }
            if let (true, true, Some(agent), Some(kind)) = (
                fields.applied,
                fields.payload_status_completed,
                &fields.agent_id,
                &fields.support_kind,
            ) {
                let kinds = self.completed_support_kinds.entry(agent.clone()).or_default();
                if !kinds.contains(kind) {
                    kinds.push(kind.clone());
                }
            }
        }
        if fields.applied
            && (fields.event_type == "facility_outage" || fields.event_type == "facility_recovery")
        {
            if let Some(facility_id) = &fields.facility_id {
                let events = self
                    .applied_facility_events
                    .entry(facility_id.clone())
                    .or_default();
                if !events.contains(&fields.event_id) {
                    events.push(fields.event_id.clone());
                }
            }
        }
        if fields.event_type == ACTION_LIFECYCLE_EVENT_TYPE {
            if let Some(agent) = &fields.agent_id {
                let agent = match self.lifecycle_agent_ids.get(agent) {
                    Some(id) => *id,
                    None => {
                        let id = self.lifecycle_agents.len() as u32;
                        self.lifecycle_agents.push(agent.clone());
                        self.lifecycle_agent_ids.insert(agent.clone(), id);
                        id
                    }
                };
                let step = match fields.transition.as_deref() {
                    Some("started" | "continued") => LifecycleStep::Set,
                    Some("completed" | "interrupted" | "abandoned" | "censored") => {
                        LifecycleStep::End
                    }
                    _ => LifecycleStep::Other,
                };
                self.lifecycle.push((agent, step, index));
            }
        }
        for key in fields.provenance_objects {
            self.by_provenance.entry(key).or_default().push(index);
        }
        let type_id = self.intern_type(&fields.event_type);
        let exact = written.is_some() || has_bytes;
        if !exact {
            self.unencoded += 1;
        }
        let event_id: Arc<str> = Arc::from(fields.event_id);
        self.entries.push(Entry {
            event_id: event_id.clone(),
            written,
            event_type: type_id,
            applied: fields.applied,
            minute: fields.minute,
            exact,
        });
        self.by_id.insert(event_id, index);
        self.sequences.insert(fields.sequence);
        self.max_sequence = self.max_sequence.max(fields.sequence);
        self.by_minute.entry(fields.minute).or_default().push(index);
        self.by_type.entry(type_id).or_default().push(index);
        0
    }

    fn intern_type(&mut self, name: &str) -> u32 {
        if let Some(id) = self.type_ids.get(name) {
            return *id;
        }
        let id = self.type_names.len() as u32;
        self.type_names.push(name.to_owned());
        self.type_ids.insert(name.to_owned(), id);
        id
    }

    /// `EventLog.append` of an outcome of `event`: 0, or 1 for a realized ID, 2 for a realized
    /// sequence, with nothing changed. `writer::record` raises the errors `EventLog.append`
    /// raises.
    pub fn append_row(
        &mut self,
        event: &EventRow,
        realized_minute: i64,
        status: EventStatus,
        detail: Option<&str>,
    ) -> Result<u8> {
        let refused = self.refusal(&event.event_id, event.sequence);
        if refused != 0 {
            return Ok(refused);
        }
        let written = event.record_json(realized_minute, status, detail)?;
        Ok(self.add(event.index_fields(realized_minute, status), false, Some(written)))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether every record has bytes written or checked here, so restoring this log gives the
    /// log itself.
    pub fn all_encoded(&self) -> bool {
        self.unencoded == 0
    }

    /// The bytes written here of the record at a position, which Python has not asked for yet.
    pub fn written(&self, index: usize) -> Option<&[u8]> {
        self.entries.get(index)?.written.as_deref()
    }

    /// The bytes written here, given up to the Python bytes object made of them.
    pub fn take_written(&mut self, index: usize) -> Option<Box<[u8]>> {
        self.entries[index].written.take()
    }

    /// The positions before `end` whose objects may be let go: their bytes were written or read
    /// back here, and they are not of a type `scan_type` was asked for.
    pub fn releasable(&self, end: usize) -> impl Iterator<Item = usize> + '_ {
        let end = end.min(self.entries.len());
        let kept: HashSet<u32> = self
            .scanned_types
            .iter()
            .filter_map(|name| self.type_ids.get(name).copied())
            .collect();
        (0..end).filter(move |index| {
            let entry = &self.entries[*index];
            entry.exact && !kept.contains(&entry.event_type)
        })
    }

    pub fn index_of(&self, event_id: &str) -> Option<usize> {
        self.by_id.get(event_id).copied()
    }

    pub fn is_applied(&self, index: usize) -> bool {
        self.entries[index].applied
    }

    pub fn has_event_id(&self, event_id: &str) -> bool {
        self.by_id.contains_key(event_id)
    }

    pub fn has_sequence(&self, sequence: i64) -> bool {
        self.sequences.contains(&sequence)
    }

    pub fn max_sequence(&self) -> i64 {
        self.max_sequence
    }

    /// The event of a request's first care record, if it has one.
    pub fn first_care_request_event_of(&self, request_id: &str) -> Option<&str> {
        self.first_care_request.get(request_id).map(String::as_str)
    }

    /// The support kinds with applied completion records for one person.
    pub fn completed_kinds(&self, agent_id: &str) -> &[String] {
        self.completed_support_kinds.get(agent_id).map_or(&[], Vec::as_slice)
    }

    pub fn applied_facility_event_ids(&self, facility_id: &str) -> &[String] {
        self.applied_facility_events.get(facility_id).map_or(&[], Vec::as_slice)
    }

    /// The positions of the records realized at a minute, in append order.
    pub fn at_minute(&self, minute: i64) -> &[usize] {
        self.by_minute.get(&minute).map_or(&[], Vec::as_slice)
    }

    /// The event IDs of `at_minute(minute)`.
    pub fn event_ids_at(&self, minute: i64) -> impl ExactSizeIterator<Item = &str> {
        self.at_minute(minute).iter().map(|index| &*self.entries[*index].event_id)
    }

    /// Every event ID in append order.
    pub fn event_ids(&self) -> impl ExactSizeIterator<Item = &str> {
        self.entries.iter().map(|entry| &*entry.event_id)
    }

    /// The event types with records, in no set order.
    pub fn event_types(&self) -> Vec<String> {
        self.by_type
            .keys()
            .map(|id| self.type_names[*id as usize].clone())
            .collect()
    }

    pub fn with_provenance_object(&self, key: &str) -> &[usize] {
        self.by_provenance.get(key).map_or(&[], Vec::as_slice)
    }

    /// The id of an event type with records.
    pub fn type_id(&self, name: &str) -> Option<u32> {
        self.type_ids.get(name).copied()
    }

    /// `type_id` of a type a reader asks for all records of, noted for `releasable`.
    pub fn scan_type(&mut self, name: &str) -> Option<u32> {
        if !self.scanned_types.contains(name) {
            self.scanned_types.insert(name.to_owned());
        }
        self.type_id(name)
    }

    /// The positions of a type's records in append order; empty for a type with none.
    pub fn positions_of_type(&self, event_type: &str) -> &[usize] {
        self.type_ids
            .get(event_type)
            .and_then(|id| self.by_type.get(id))
            .map_or(&[], Vec::as_slice)
    }

    /// The positions of the records of these distinct types at `start` or later, in append order.
    pub fn of_type_ids_from(&self, ids: &[u32], start: usize) -> Vec<usize> {
        let mut indices: Vec<usize> = Vec::new();
        for id in ids {
            if let Some(list) = self.by_type.get(id) {
                let first = list.partition_point(|index| *index < start);
                indices.extend(list[first..].iter().copied());
            }
        }
        if ids.len() > 1 {
            indices.sort_unstable();
        }
        indices
    }

    /// The agent_action_lifecycle records that decide each agent's active action, in the order a
    /// pass over all of them inserts the agents into a dict: a pass over just these gives the
    /// same dict.
    pub fn active_lifecycle_positions(&self) -> Vec<usize> {
        // Agent -> (position of its dict entry, its last record that set the entry).
        let mut active: HashMap<u32, (usize, usize)> = HashMap::new();
        for (order, (agent, step, index)) in self.lifecycle.iter().enumerate() {
            match step {
                LifecycleStep::Set => {
                    active
                        .entry(*agent)
                        .and_modify(|entry| entry.1 = *index)
                        .or_insert((order, *index));
                }
                LifecycleStep::End => {
                    active.remove(agent);
                }
                LifecycleStep::Other => {}
            }
        }
        let mut entries: Vec<(usize, usize)> = active.into_values().collect();
        entries.sort_unstable();
        entries.into_iter().map(|(_, index)| index).collect()
    }

    /// Up to limit applied records of the types `wanted` from the end, scanning at most
    /// scan_limit records, in append order; and whether records were left unscanned.
    pub fn recent_matching(&self, wanted: &HashSet<u32>, limit: usize, scan_limit: usize) -> (Vec<usize>, bool) {
        let mut found: Vec<usize> = Vec::new();
        let mut scanned = 0;
        for index in (0..self.entries.len()).rev() {
            if scanned == scan_limit {
                break;
            }
            scanned += 1;
            let entry = &self.entries[index];
            if entry.applied && wanted.contains(&entry.event_type) {
                found.push(index);
                if found.len() == limit {
                    break;
                }
            }
        }
        found.reverse();
        (found, scanned < self.entries.len())
    }

    /// Audit realization times: 0, 3 for a future realization, 4 for one outside the duration.
    /// IDs and sequences are unique by construction. Records an earlier audit checked at an
    /// earlier or the same minute and duration are not checked again.
    pub fn audit(&mut self, minute_index: Option<i64>, duration_minutes: Option<i64>) -> u8 {
        let (audited_minute, audited_duration) = self.audit_bounds;
        let mut start = self.audited.min(self.entries.len());
        if let Some(minute) = minute_index {
            if audited_minute.is_none_or(|previous| previous > minute) {
                start = 0;
            }
        }
        if duration_minutes.is_some() && audited_duration != duration_minutes {
            start = 0;
        }
        for entry in &self.entries[start..] {
            if minute_index.is_some_and(|minute| entry.minute > minute) {
                return 3;
            }
            if duration_minutes.is_some_and(|duration| entry.minute >= duration) {
                return 4;
            }
        }
        self.audited = self.entries.len();
        self.audit_bounds = (minute_index, duration_minutes);
        0
    }

    /// The sizes of the record list and of the ID and sequence indexes.
    pub fn index_sizes(&self) -> (usize, usize, usize) {
        (self.entries.len(), self.by_id.len(), self.sequences.len())
    }
}

/// The index fields of a record made from values, not read from a Python object.
pub struct NewFields<'a> {
    pub event_id: &'a str,
    pub sequence: i64,
    pub event_type: &'a str,
    pub minute: i64,
    pub applied: bool,
    pub payload: Option<&'a V>,
    pub provenance: Option<&'a V>,
}

pub fn fields_of_row(new: NewFields<'_>) -> Fields {
    let text = |key: &str| match new.payload {
        Some(V::Map(payload)) => match payload.get(key) {
            Some(V::Str(value)) => Some(String::from(&**value)),
            _ => None,
        },
        _ => None,
    };
    let provenance_objects = match new.provenance {
        Some(V::Map(provenance)) => provenance
            .iter()
            .filter(|(_, value)| matches!(value, V::Map(_)))
            .map(|(key, _)| String::from(&**key))
            .collect(),
        _ => Vec::new(),
    };
    Fields {
        event_id: new.event_id.to_owned(),
        sequence: new.sequence,
        event_type: new.event_type.to_owned(),
        minute: new.minute,
        applied: new.applied,
        request_id: text("request_id"),
        agent_id: text("agent_id"),
        support_kind: text("support_kind"),
        payload_status_completed: text("status").as_deref() == Some("completed"),
        facility_id: text("facility_id"),
        provenance_objects,
        transition: text("transition"),
    }
}

impl EventRow {
    /// The index fields of a record of this event's outcome.
    pub fn index_fields(&self, minute: i64, status: EventStatus) -> Fields {
        fields_of_row(NewFields {
            event_id: &self.event_id,
            sequence: self.sequence,
            event_type: &self.event_type,
            minute,
            applied: status == EventStatus::Applied,
            payload: Some(&self.payload),
            provenance: Some(&self.provenance),
        })
    }
}

fn bad_document(what: &str) -> String {
    format!("saved EventLog record: {what}")
}

/// The index fields of one saved record, after the checks EventLogRecord makes and a
/// check that the bytes are its canonical JSON. Runs without the GIL.
pub fn fields_of_bytes(bytes: &[u8]) -> std::result::Result<Fields, String> {
    use serde_json::Value;
    let document: Value =
        serde_json::from_slice(bytes).map_err(|error| bad_document(&error.to_string()))?;
    check_saved_record(&document).map_err(|what| bad_document(what))?;
    let mut canonical = Vec::with_capacity(bytes.len());
    write_canonical(&mut canonical, &document).map_err(|what| bad_document(what))?;
    if canonical != bytes {
        return Err(bad_document("record is not canonical JSON"));
    }
    let event = document.get("event").ok_or_else(|| bad_document("no event"))?;
    let text = |value: Option<&Value>| value.and_then(Value::as_str).map(str::to_owned);
    let payload = event.get("payload");
    let payload_text = |key: &str| text(payload.and_then(|value| value.get(key)));
    let mut provenance_objects = Vec::new();
    if let Some(Value::Object(provenance)) = event.get("provenance") {
        for (key, value) in provenance {
            if value.is_object() {
                provenance_objects.push(key.clone());
            }
        }
    }
    Ok(Fields {
        event_id: text(event.get("event_id")).ok_or_else(|| bad_document("event_id"))?,
        sequence: event
            .get("sequence")
            .and_then(Value::as_i64)
            .ok_or_else(|| bad_document("sequence"))?,
        event_type: text(event.get("event_type")).ok_or_else(|| bad_document("event_type"))?,
        minute: document
            .get("realized_minute")
            .and_then(Value::as_i64)
            .ok_or_else(|| bad_document("realized_minute"))?,
        applied: document.get("status").and_then(Value::as_str) == Some("applied"),
        request_id: payload_text("request_id"),
        agent_id: payload_text("agent_id"),
        support_kind: payload_text("support_kind"),
        payload_status_completed: payload
            .and_then(|value| value.get("status"))
            .and_then(Value::as_str)
            == Some("completed"),
        facility_id: payload_text("facility_id"),
        provenance_objects,
        transition: payload_text("transition"),
    })
}

pub const RECORD_FIELDS: [&str; 4] = ["detail", "event", "realized_minute", "status"];
pub const EVENT_FIELDS: [&str; 10] = [
    "causation_id",
    "due_minute",
    "event_id",
    "event_type",
    "kind",
    "payload",
    "priority",
    "provenance",
    "sequence",
    "source_id",
];
const EVENT_KINDS: [&str; 4] = ["exogenous", "completion", "endogenous", "intervention"];
const EVENT_STATUSES: [&str; 3] = ["applied", "failed", "suppressed"];

fn stable_id(value: &serde_json::Value) -> bool {
    value.as_str().is_some_and(is_stable_id)
}

fn exact_int(value: &serde_json::Value, minimum: Option<i64>) -> bool {
    // A JSON number with a fraction or exponent reads back as a float in Python.
    let integral = matches!(value, serde_json::Value::Number(number)
        if !number.to_string().contains(['.', 'e', 'E']));
    integral && value.as_i64().is_some_and(|number| minimum.is_none_or(|low| number >= low))
}

/// The checks EventLogRecord and ScheduledEvent.from_saved make of a saved record.
pub fn check_saved_record(document: &serde_json::Value) -> std::result::Result<(), &'static str> {
    use serde_json::Value;
    let has_fields = |value: &Value, names: &[&str]| {
        value
            .as_object()
            .is_some_and(|object| object.len() == names.len() && names.iter().all(|name| object.contains_key(*name)))
    };
    if !has_fields(document, &RECORD_FIELDS) {
        return Err("record fields do not match EventLogRecord");
    }
    if !exact_int(&document["realized_minute"], Some(0)) {
        return Err("realized_minute must be a non-negative integer");
    }
    if !document["status"].as_str().is_some_and(|status| EVENT_STATUSES.contains(&status)) {
        return Err("unknown event status");
    }
    let event = &document["event"];
    if !has_fields(event, &EVENT_FIELDS) {
        return Err("event fields do not match ScheduledEvent");
    }
    if !stable_id(&event["event_id"]) || !stable_id(&event["event_type"]) {
        return Err("event_id and event_type must be stable IDs");
    }
    for name in ["source_id", "causation_id"] {
        if !event[name].is_null() && !stable_id(&event[name]) {
            return Err("source_id and causation_id must be stable IDs");
        }
    }
    if !event["kind"].as_str().is_some_and(|kind| EVENT_KINDS.contains(&kind)) {
        return Err("unknown event kind");
    }
    if !exact_int(&event["due_minute"], Some(0)) || !exact_int(&event["sequence"], Some(0)) {
        return Err("event due_minute and sequence must be non-negative integers");
    }
    if !exact_int(&event["priority"], None) {
        return Err("event priority must be an integer");
    }
    if !event["payload"].is_object() || !event["provenance"].is_object() {
        return Err("event payload and provenance must be objects");
    }
    Ok(())
}

/// json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False) of
/// a value read from JSON, numbers written as Python reads and writes them back.
pub fn write_canonical(out: &mut Vec<u8>, value: &serde_json::Value) -> std::result::Result<(), &'static str> {
    use serde_json::Value;
    match value {
        Value::Null => out.extend_from_slice(b"null"),
        Value::Bool(flag) => out.extend_from_slice(if *flag { b"true" } else { b"false" }),
        Value::Number(number) => {
            let text = number.to_string();
            if text.contains(['.', 'e', 'E']) {
                let parsed: f64 = text.parse().map_err(|_| "invalid number")?;
                if !parsed.is_finite() {
                    return Err("number is not finite");
                }
                out.extend_from_slice(crate::json::python_float_repr(parsed).as_bytes());
            } else if text == "-0" {
                out.push(b'0');
            } else {
                out.extend_from_slice(text.as_bytes());
            }
        }
        Value::String(text) => crate::json::write_json_string(out, text),
        Value::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                write_canonical(out, item)?;
            }
            out.push(b']');
        }
        Value::Object(object) => {
            // serde_json's map is ordered by key bytes, as Python orders str.
            out.push(b'{');
            for (index, (key, item)) in object.iter().enumerate() {
                if index > 0 {
                    out.push(b',');
                }
                crate::json::write_json_string(out, key);
                out.push(b':');
                write_canonical(out, item)?;
            }
            out.push(b'}');
        }
    }
    Ok(())
}
