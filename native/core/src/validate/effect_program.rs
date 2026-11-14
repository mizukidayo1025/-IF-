//! `validate_effect_program_state(world, validate_event_history=...)` of `events/effects.py`, its
//! history included, with Python's errors and texts. `validate::world` makes a `ValueError` (a
//! pydantic `ValidationError` too) `invalid EffectProgram state: {exc}`.
//!
//! The programs' records go through their pydantic contracts (`contracts::checked`) as Python holds
//! them: the state as its tracked tree (`TrackedDict`), an event's marker frozen (mapping proxies
//! and tuples), whose class names the texts show. The events are the queue's and the log's records
//! that carry a marker (`EventLog::with_provenance_object`), each read once, and the lookups by ID
//! go through the log's index (`EventsById`).

use std::collections::HashSet;
use std::sync::Arc;

use indexmap::IndexMap;
use sha2::{Digest, Sha256};

use super::View;
use crate::cell::ShardRef;
use crate::contracts::{self, Checked, Holder};
use crate::error::{Error, Result};
use crate::events::queue::Queue;
use crate::events::{is_stable_id, EventRow};
use crate::rules::dispatch::py_eq;
use crate::rules::effects::{expected_event_type, OPERATORS};
use crate::value::V;

pub const MARKER: &str = "effect_program_v1";
const STATE_FIELDS: [&str; 3] = ["schema_version", "programs", "active_facility_modifiers"];
const PROGRAM_FIELDS: [&str; 9] = [
    "program_hash",
    "source_text_hash",
    "narrative",
    "compiler_provenance",
    "assumptions",
    "support_mode",
    "effect_ids",
    "operators",
    "scheduled_event_ids",
];
const SUPPORT_MODES: [&str; 5] = ["native", "composed", "approximate", "clarification_required", "not_simulatable"];
const MARKER_FIELDS: [&str; 9] = ["program_id", "effect_id", "op", "source_text_hash", "compiler_provenance", "assumption_ids", "policy_dependencies", "units", "phase"];
const STACK_FIELDS: [&str; 2] = ["baseline_working_units", "modifiers"];
const ENTRY_FIELDS: [&str; 6] = ["modifier_id", "program_id", "effect_id", "working_units", "start_minute", "sequence"];

type Map = IndexMap<Arc<str>, V>;

fn value_error(text: &str) -> Error {
    Error::Value(text.to_owned())
}

fn has_fields(map: &Map, fields: &[&str]) -> bool {
    map.len() == fields.len() && fields.iter().all(|field| map.contains_key(*field))
}

/// The `TypeError` of hashing a list or a dict of the state, which Python holds as its tracked tree
/// (`core/state_mirror.py`), whose classes the text names.
pub(super) fn unhashable(value: &V) -> Result<()> {
    match value {
        V::List(_) => Err(Error::Type("unhashable type: 'TrackedList'".to_owned())),
        V::Map(_) => Err(Error::Type("unhashable type: 'TrackedDict'".to_owned())),
        _ => Ok(()),
    }
}

/// `re.fullmatch(r"[0-9a-f]{64}", value)`.
fn is_lower_hex_64(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// `_modifier_id(program_id, effect_id)`.
fn modifier_id(program_id: &str, effect_id: &str) -> String {
    let digest = Sha256::digest(format!("{program_id}:{effect_id}").as_bytes());
    let mut text = String::with_capacity(41);
    text.push_str("modifier-");
    for byte in &digest[..16] {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

/// The marker of an event, where its provenance holds a mapping under `effect_program_v1`.
fn marker_of(event: &EventRow) -> Option<&Map> {
    match &event.provenance {
        V::Map(provenance) => match provenance.get(MARKER) {
            Some(V::Map(marker)) => Some(marker),
            _ => None,
        },
        _ => None,
    }
}

/// `EventsById(world.event_log, world.event_queue.pending())`: an event by ID, the log's record
/// winning over a pending event.
pub(super) struct EventsById<'w> {
    world: &'w crate::world::World,
    queue: ShardRef<'w, Queue>,
}

impl<'w> EventsById<'w> {
    pub fn new(view: &View<'w>) -> Result<Self> {
        Ok(Self { world: view.world, queue: view.queue()? })
    }

    /// `by_id.get(event_id)`.
    pub fn get(&self, event_id: &str) -> Result<Option<Arc<EventRow>>> {
        if let Some(logged) = crate::load::find_record(self.world, event_id)? {
            return Ok(Some(Arc::new(logged.event)));
        }
        match self.queue.serial_of(event_id) {
            Some(serial) => self.queue.slot(serial).row.clone().map(Some).ok_or_else(|| Error::unsupported("a pending event has no row")),
            None => Ok(None),
        }
    }

    /// `event_id in by_id`.
    pub fn contains(&self, event_id: &str) -> Result<bool> {
        Ok(self.world.log()?.index_of(event_id).is_some() || self.queue.serial_of(event_id).is_some())
    }

    /// `world.event_queue.pending()`, in queue order.
    pub fn pending(&self) -> Result<Vec<Arc<EventRow>>> {
        self.queue
            .ordered()
            .iter()
            .map(|(_, serial)| self.queue.slot(*serial).row.clone().ok_or_else(|| Error::unsupported("a pending event has no row")))
            .collect()
    }
}

/// `world.event_log.with_provenance_object("effect_program_v1")`: the events of those records.
fn logged_with_marker(view: &View<'_>) -> Result<Vec<EventRow>> {
    let log = view.log()?;
    let mut events = Vec::new();
    for index in log.with_provenance_object(MARKER) {
        let record = log.record(*index)?.ok_or_else(|| Error::unsupported("the log does not hold a record's bytes"))?;
        events.push(record.event);
    }
    Ok(events)
}

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(view: &View<'_>) -> Result<()> {
    let by_id = EventsById::new(view)?;
    let state = view.state(MARKER)?;
    let state = state.as_deref().filter(|state| !matches!(state, V::Null));
    let Some(state) = state else {
        let missing = || value_error("EffectProgram events require effect_program_v1 state");
        if view.history && !view.log()?.with_provenance_object(MARKER).is_empty() {
            return Err(missing());
        }
        if by_id.pending()?.iter().any(|event| marker_of(event).is_some()) {
            return Err(missing());
        }
        return Ok(());
    };
    let (programs, active) = match state {
        V::Map(state)
            if has_fields(state, &STATE_FIELDS)
                && py_eq(&state["schema_version"], &V::Int(1))
                && matches!(state["programs"], V::Map(_))
                && matches!(state["active_facility_modifiers"], V::Map(_)) =>
        {
            match (&state["programs"], &state["active_facility_modifiers"]) {
                (V::Map(programs), V::Map(active)) => (programs, active),
                _ => unreachable!("checked above"),
            }
        }
        _ => return Err(value_error("effect_program_v1 has an invalid exact schema")),
    };
    if !view.history {
        return Ok(());
    }
    // known_effects: (program, effect) -> the program's record; scheduled_events: event -> program.
    let mut known: IndexMap<(Arc<str>, Arc<str>), &Map> = IndexMap::new();
    let mut scheduled: IndexMap<Arc<str>, Arc<str>> = IndexMap::new();
    for (program_id, program) in programs {
        let program = match program {
            V::Map(program) if is_stable_id(program_id) && has_fields(program, &PROGRAM_FIELDS) => program,
            _ => return Err(value_error("effect program record fields are invalid")),
        };
        for name in ["program_hash", "source_text_hash"] {
            if !matches!(&program[name], V::Str(hash) if is_lower_hex_64(hash)) {
                return Err(Error::Value(format!("effect program {name} is invalid")));
            }
        }
        let texts = |value: &V| match value {
            V::List(items) if items.iter().all(|item| matches!(item, V::Str(_))) => {
                Some(items.iter().filter_map(|item| if let V::Str(text) = item { Some(text.clone()) } else { None }).collect::<Vec<_>>())
            }
            _ => None,
        };
        let narrative_fits = matches!(&program["narrative"], V::Str(text) if (1..=1000).contains(&text.chars().count()));
        let support_fits = matches!(&program["support_mode"], V::Str(mode) if SUPPORT_MODES.contains(&&**mode));
        let effect_ids = texts(&program["effect_ids"]);
        let scheduled_ids = texts(&program["scheduled_event_ids"]);
        let (true, V::List(assumptions), true, Some(effect_ids), V::Map(operators), Some(scheduled_ids)) =
            (narrative_fits, &program["assumptions"], support_fits, effect_ids, &program["operators"], scheduled_ids)
        else {
            return Err(value_error("effect program metadata values are invalid"));
        };
        let unique: HashSet<&str> = effect_ids.iter().map(|id| &**id).collect();
        if unique.len() != effect_ids.len() {
            return Err(value_error("effect program effect IDs must be unique"));
        }
        if operators.len() != unique.len()
            || operators.keys().any(|effect| !unique.contains(&**effect))
            || operators.values().any(|op| !matches!(op, V::Str(op) if OPERATORS.contains(&&**op)))
        {
            return Err(value_error("effect program operator registry references are invalid"));
        }
        if scheduled_ids.iter().map(|id| &**id).collect::<HashSet<_>>().len() != scheduled_ids.len() {
            return Err(value_error("effect program scheduled event IDs must be unique"));
        }
        contracts::checked(&Checked::CompilerProvenance, &program["compiler_provenance"], Holder::Tracked)?;
        let mut assumption_ids: HashSet<Arc<str>> = HashSet::new();
        for assumption in assumptions {
            let parsed = contracts::checked(&Checked::PlanAssumption, assumption, Holder::Tracked)?;
            let id = match &parsed {
                V::Map(parsed) => match parsed.get("assumption_id") {
                    Some(V::Str(id)) => id.clone(),
                    _ => return Err(Error::unsupported("an assumption's dump without its ID")),
                },
                _ => return Err(Error::unsupported("an assumption's dump is no dict")),
            };
            if !assumption_ids.insert(id) {
                return Err(value_error("effect program assumption IDs must be unique"));
            }
        }
        for effect_id in &effect_ids {
            if !is_stable_id(effect_id) {
                return Err(value_error("effect program effect ID is invalid"));
            }
            known.insert((program_id.clone(), effect_id.clone()), program);
        }
        for event_id in &scheduled_ids {
            if !is_stable_id(event_id) || scheduled.contains_key(event_id) {
                return Err(value_error("effect program event IDs must be unique stable IDs"));
            }
            scheduled.insert(event_id.clone(), program_id.clone());
        }
    }
    // Events without a marker pass the loop below unread.
    let mut events: Vec<Arc<EventRow>> = by_id.pending()?;
    events.extend(logged_with_marker(view)?.into_iter().map(Arc::new));
    for (event_id, program_id) in &scheduled {
        let Some(event) = by_id.get(event_id)? else {
            return Err(value_error("effect program references an unknown scheduled or realized event"));
        };
        if !marker_of(&event).is_some_and(|marker| py_eq(marker.get("program_id").unwrap_or(&V::Null), &V::Str(program_id.clone()))) {
            return Err(value_error("effect program provenance does not match its event"));
        }
    }
    for event in &events {
        let Some(marker) = marker_of(event) else {
            continue;
        };
        let has_guard = marker.contains_key("guard");
        if marker.len() != MARKER_FIELDS.len() + usize::from(has_guard) || !MARKER_FIELDS.iter().all(|name| marker.contains_key(*name)) {
            return Err(value_error("EffectProgram event provenance fields are invalid"));
        }
        let (V::Str(program_id), V::Str(effect_id)) = (&marker["program_id"], &marker["effect_id"]) else {
            return Err(value_error("EffectProgram event provenance IDs are invalid"));
        };
        let Some(program) = known.get(&(program_id.clone(), effect_id.clone())) else {
            return Err(value_error("event provenance references an unknown EffectProgram effect"));
        };
        let operator = match &program["operators"] {
            V::Map(operators) => operators.get(&**effect_id).unwrap_or(&V::Null),
            _ => &V::Null,
        };
        if !py_eq(&marker["op"], operator) {
            return Err(value_error("effect event operator does not match the scheduled program"));
        }
        if !py_eq(&marker["source_text_hash"], &program["source_text_hash"]) {
            return Err(value_error("event source hash does not match its EffectProgram"));
        }
        let phase = match &marker["phase"] {
            V::Str(phase) if matches!(&**phase, "start" | "expiry" | "derived") => phase.clone(),
            _ => return Err(value_error("effect event phase is invalid")),
        };
        contracts::checked(&Checked::CompilerProvenance, &marker["compiler_provenance"], Holder::Frozen)?;
        let strs = |value: &V| matches!(value, V::List(items) if items.iter().all(|item| matches!(item, V::Str(_))));
        if !strs(&marker["assumption_ids"])
            || !strs(&marker["policy_dependencies"])
            || !matches!(&marker["units"], V::Str(units) if is_stable_id(units))
        {
            return Err(value_error("EffectProgram event provenance values are invalid"));
        }
        if has_guard {
            if &*phase != "start" {
                return Err(value_error("only a start event may store a guard"));
            }
            contracts::checked(&Checked::Guard, &marker["guard"], Holder::Frozen)?;
        }
        if !scheduled.contains_key(&*event.event_id) && &*phase != "derived" {
            return Err(value_error("unscheduled EffectProgram event lacks derived-event marking"));
        }
        if &*phase == "derived" {
            let known_parent = match &event.causation_id {
                Some(cause) => by_id.contains(cause)?,
                None => false,
            };
            if !known_parent {
                return Err(value_error("derived EffectProgram event lacks a known causal parent"));
            }
        }
        // The op equals its program's operator, a str, by now.
        let op = match &marker["op"] {
            V::Str(op) => op.clone(),
            _ => return Err(Error::unsupported("a marker's operator is no str")),
        };
        if expected_event_type(&phase, &op) != Some(&*event.event_type) {
            return Err(value_error("EffectProgram event type does not match its registered operator"));
        }
    }
    let rows = view.rows()?;
    for (facility_id, record) in active {
        let (Some(facility), V::Map(record)) = (rows.facilities.get(facility_id), record) else {
            return Err(value_error("active facility modifier record is invalid"));
        };
        if !has_fields(record, &STACK_FIELDS) {
            return Err(value_error("active facility modifier record is invalid"));
        }
        let (V::Int(baseline), V::List(modifiers)) = (&record["baseline_working_units"], &record["modifiers"]) else {
            return Err(value_error("active facility modifier baseline or stack is invalid"));
        };
        let baseline = *baseline;
        if !(0 <= baseline && baseline <= facility.capacity) || modifiers.is_empty() {
            return Err(value_error("active facility modifier baseline or stack is invalid"));
        }
        let mut modifier_ids: HashSet<&str> = HashSet::new();
        let mut lowest = baseline;
        for item in modifiers {
            let item = match item {
                V::Map(item) if has_fields(item, &ENTRY_FIELDS) => item,
                _ => return Err(value_error("facility modifier stack entry is invalid")),
            };
            let identity = || value_error("facility modifier stack identity or numeric values are invalid");
            let (V::Str(id), V::Str(program_id), V::Str(effect_id)) = (&item["modifier_id"], &item["program_id"], &item["effect_id"]) else {
                return Err(identity());
            };
            // An int of any size, not negative (a `Big` is past i64).
            let numbers_fit = ["working_units", "start_minute", "sequence"]
                .iter()
                .all(|name| matches!(&item[*name], V::Int(number) if *number >= 0) || matches!(&item[*name], V::Big(digits) if !digits.starts_with('-')));
            if **id != *modifier_id(program_id, effect_id)
                || modifier_ids.contains(&**id)
                || !known.contains_key(&(program_id.clone(), effect_id.clone()))
                || !numbers_fit
            {
                return Err(identity());
            }
            let working_units = match item["working_units"] {
                V::Int(working_units) => working_units,
                // Past i64: more than any baseline.
                _ => i64::MAX,
            };
            if working_units > baseline {
                return Err(value_error("facility modifier exceeds its captured baseline"));
            }
            modifier_ids.insert(id);
            lowest = lowest.min(working_units);
        }
        if facility.working_units != lowest {
            return Err(value_error("facility working availability does not match its modifier stack"));
        }
    }
    Ok(())
}
