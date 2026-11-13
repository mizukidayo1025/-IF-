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
//!
//! A transaction marks the log at its start (`mark`) and, if it rolls back, takes out the records
//! appended since with everything they added to the indexes (`rollback`), last first (lane 6-2).

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use super::packed::{Blocks, Packer, Unpacked};
use super::{is_stable_id, EventRow, EventStatus};
use crate::error::{Error, Result};
use crate::value::V;

/// What the indexes and a Rust rule read of one record.
#[derive(Clone, Debug, PartialEq)]
struct Entry {
    event_id: Arc<str>,
    /// For `rollback`, which takes it out of `sequences`.
    sequence: i64,
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

#[derive(Clone, Debug, PartialEq)]
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
    // The records in the binary form, once `pack_records` was called (the browser's log).
    packed: Option<Packed>,
}

/// Where `Packed::starts` says a record is not in the binary form: its bytes are the entry's.
const NOT_PACKED: u64 = u64::MAX;

/// The records of a log in the binary form (`events::packed`, #252): each record's packed bytes, in
/// blocks (`Blocks`), and where each starts. A record whose packed form would not write its bytes
/// again keeps its bytes in its entry instead.
#[derive(Clone, Debug, PartialEq)]
struct Packed {
    packer: Packer,
    blocks: Blocks,
    /// Where each record starts in `blocks`, or `NOT_PACKED`.
    starts: Vec<u64>,
}

impl Packed {
    fn new(records: usize) -> Self {
        Self { packer: Packer::new(), blocks: Blocks::default(), starts: Vec::with_capacity(records) }
    }

    /// Packs a record into the blocks: where it starts, or the packer's error with nothing held.
    fn push(&mut self, event: &EventRow, realized_minute: i64, status: EventStatus, detail: Option<&str>) -> Result<u64> {
        let mut record = Vec::with_capacity(256);
        self.packer.pack(&mut record, event, realized_minute, status, detail)?;
        Ok(self.blocks.push(&record))
    }

    fn at(&self, start: u64) -> &[u8] {
        self.blocks.at(start)
    }

    fn truncate(&mut self, start: u64) {
        self.blocks.truncate(start);
    }
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
            packed: None,
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
        if let Some(packed) = &mut self.packed {
            packed.starts.push(NOT_PACKED);
        }
        self.entries.push(Entry {
            event_id: event_id.clone(),
            sequence: fields.sequence,
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
        if self.packed.is_none() {
            let written = event.record_json(realized_minute, status, detail)?;
            return Ok(self.add(event.index_fields(realized_minute, status), false, Some(written)));
        }
        let index = self.entries.len();
        let refused = self.add(event.index_fields(realized_minute, status), true, None);
        if refused != 0 {
            return Ok(refused);
        }
        if let Some(packed) = &mut self.packed {
            if let Ok(start) = packed.push(event, realized_minute, status, detail) {
                packed.starts[index] = start;
                return Ok(0);
            }
        }
        // A record the form cannot hold (a negative minute) keeps its bytes.
        self.entries[index].written = Some(event.record_json(realized_minute, status, detail)?);
        Ok(0)
    }

    /// Holds the records in the binary form from now on (#252): the ones the log holds bytes of
    /// now, and every record `append_row` adds. A record is packed only if its packed form writes
    /// its bytes again, byte for byte; any other keeps its bytes. Its bytes are then read through
    /// `write_record` and `record`, and `written` has none. Returns how many records were packed.
    /// Call it with no transaction open: a `rollback` to a mark made before it would leave records
    /// packed that the mark did not know of.
    pub fn pack_records(&mut self) -> Result<usize> {
        let mut packed = self.packed.take().unwrap_or_else(|| Packed::new(self.entries.len()));
        let mut count = 0;
        for index in packed.starts.len()..self.entries.len() {
            packed.starts.push(NOT_PACKED);
            let Some(raw) = self.entries[index].written.as_deref() else { continue };
            let Some((event, minute, status, detail)) = EventRow::from_saved_record(raw) else { continue };
            let Ok(start) = packed.push(&event, minute, status, detail.as_deref()) else { continue };
            let same = packed.packer.unpack(packed.at(start)).ok().map(|(record, _)| record.record_json()).transpose()?;
            if same.as_deref() != Some(raw) {
                packed.truncate(start);
                continue;
            }
            packed.starts[index] = start;
            self.entries[index].written = None;
            count += 1;
        }
        self.packed = Some(packed);
        Ok(count)
    }

    /// The packer and the blocks of the records in the binary form, or None when the log is not
    /// packed: what a Run in the browser stores (`archive`, #252 PR 6-5). A record kept as bytes
    /// (`pack_records`) is in neither.
    pub fn packed_store(&self) -> Option<(&Packer, &Blocks)> {
        self.packed.as_ref().map(|packed| (&packed.packer, &packed.blocks))
    }

    /// Whether every record is in the blocks: none kept its bytes instead.
    pub fn all_packed(&self) -> bool {
        self.packed.as_ref().is_some_and(|packed| packed.starts.iter().all(|start| *start != NOT_PACKED))
    }

    /// Whether the records are held in the binary form (`pack_records`).
    pub fn is_packed(&self) -> bool {
        self.packed.is_some()
    }

    /// The bytes of the records in the binary form, of its string table's texts and of its shapes'
    /// keys, and their counts: (bytes, strings, shapes), or None when the log is not packed.
    pub fn packed_sizes(&self) -> Option<(usize, usize, usize)> {
        let packed = self.packed.as_ref()?;
        Some((packed.blocks.size(), packed.packer.strings().len(), packed.packer.shapes().len()))
    }

    /// The record at a position as it was realized, read from its packed form or its bytes; None
    /// for a record the log holds neither of (Python has its bytes) or for no record there.
    pub fn record(&self, index: usize) -> Result<Option<Unpacked>> {
        if let Some(packed) = &self.packed {
            if let Some(&start) = packed.starts.get(index).filter(|start| **start != NOT_PACKED) {
                return Ok(Some(packed.packer.unpack(packed.at(start))?.0));
            }
        }
        let Some(raw) = self.written(index) else {
            return Ok(None);
        };
        let (event, realized_minute, status, detail) =
            EventRow::from_saved_record(raw).ok_or_else(|| Error::unsupported("the record's bytes are not a record"))?;
        Ok(Some(Unpacked { event, realized_minute, status, detail }))
    }

    /// Appends the canonical JSON of the record at a position to `out`, from its bytes or its
    /// packed form: false, with nothing appended, for a record the log holds neither of.
    pub fn write_record(&self, index: usize, out: &mut Vec<u8>) -> Result<bool> {
        if let Some(raw) = self.written(index) {
            out.extend_from_slice(raw);
            return Ok(true);
        }
        match self.record(index)? {
            Some(record) => {
                out.extend_from_slice(&record.record_json()?);
                Ok(true)
            }
            None => Ok(false),
        }
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

    /// Where the log is now, for a `rollback` to come back to (a transaction's start).
    pub fn mark(&self) -> LogMark {
        LogMark {
            len: self.entries.len(),
            unencoded: self.unencoded,
            type_count: self.type_names.len(),
            max_sequence: self.max_sequence,
            lifecycle_len: self.lifecycle.len(),
            lifecycle_agent_count: self.lifecycle_agents.len(),
            completed_support_kinds: self.completed_support_kinds.clone(),
            applied_facility_events: self.applied_facility_events.clone(),
            audited: self.audited,
            audit_bounds: self.audit_bounds,
            scanned_types: self.scanned_types.clone(),
        }
    }

    /// Take out the records appended since the mark, last first, with what each added to the
    /// indexes: the log is then what it was at the mark. The bytes of an earlier record that
    /// Python asked for since stay with Python's bytes object (`take_written`), as they would.
    pub fn rollback(&mut self, mark: LogMark) {
        let len = mark.len.min(self.entries.len());
        let mut removed: HashSet<Arc<str>> = HashSet::with_capacity(self.entries.len() - len);
        for index in (len..self.entries.len()).rev() {
            let entry = &self.entries[index];
            self.by_id.remove(&entry.event_id);
            self.sequences.remove(&entry.sequence);
            self.by_minute.pop_position(entry.minute, index);
            self.by_type.pop_position(entry.event_type, index);
            removed.insert(entry.event_id.clone());
        }
        if let Some(packed) = &mut self.packed {
            if let Some(&start) = packed.starts.get(len..).unwrap_or_default().iter().find(|start| **start != NOT_PACKED) {
                packed.truncate(start);
            }
            packed.starts.truncate(len);
        }
        self.entries.truncate(len);
        if !removed.is_empty() {
            // The appended records' keys, which point at them: an earlier record's key points at
            // an earlier record, and `or_insert` kept it.
            self.first_care_request.retain(|_, event_id| !removed.contains(event_id.as_str()));
            self.by_provenance.retain(|_, positions| {
                positions.truncate(positions.partition_point(|index| *index < len));
                !positions.is_empty()
            });
        }
        for name in self.type_names.drain(mark.type_count..) {
            self.type_ids.remove(&name);
        }
        self.lifecycle.truncate(mark.lifecycle_len);
        for agent in self.lifecycle_agents.drain(mark.lifecycle_agent_count..) {
            self.lifecycle_agent_ids.remove(&agent);
        }
        self.unencoded = mark.unencoded;
        self.max_sequence = mark.max_sequence;
        self.completed_support_kinds = mark.completed_support_kinds;
        self.applied_facility_events = mark.applied_facility_events;
        self.audited = mark.audited;
        self.audit_bounds = mark.audit_bounds;
        self.scanned_types = mark.scanned_types;
    }

    /// The sizes of the record list and of the ID and sequence indexes.
    pub fn index_sizes(&self) -> (usize, usize, usize) {
        (self.entries.len(), self.by_id.len(), self.sequences.len())
    }
}

/// Where an `EventLog` was (`EventLog::mark`): its counts, and the small indexes a record may add
/// to without leaving a trace of which record did (the support kinds of a person, the facility
/// events), whole. The scanned types are a reader's: a rollback forgets the reads since.
#[derive(Clone, Debug)]
pub struct LogMark {
    len: usize,
    unencoded: usize,
    type_count: usize,
    max_sequence: i64,
    lifecycle_len: usize,
    lifecycle_agent_count: usize,
    completed_support_kinds: HashMap<String, Vec<String>>,
    applied_facility_events: HashMap<String, Vec<String>>,
    audited: usize,
    audit_bounds: (Option<i64>, Option<i64>),
    scanned_types: HashSet<String>,
}

impl LogMark {
    /// How many records the log had.
    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// An index of positions by key, for `rollback`.
trait PositionIndex<K> {
    /// Take `position`, the last one pushed under `key`, out; a key left with no position leaves
    /// the index, as it was before its first record.
    fn pop_position(&mut self, key: K, position: usize);
}

impl PositionIndex<i64> for BTreeMap<i64, Vec<usize>> {
    fn pop_position(&mut self, key: i64, position: usize) {
        if let Some(positions) = self.get_mut(&key) {
            debug_assert_eq!(positions.last(), Some(&position));
            positions.pop();
            if positions.is_empty() {
                self.remove(&key);
            }
        }
    }
}

impl PositionIndex<u32> for HashMap<u32, Vec<usize>> {
    fn pop_position(&mut self, key: u32, position: usize) {
        if let Some(positions) = self.get_mut(&key) {
            debug_assert_eq!(positions.last(), Some(&position));
            positions.pop();
            if positions.is_empty() {
                self.remove(&key);
            }
        }
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
