//! What the rules ask of each other's files without being rules themselves (Issue #152, P8-0a;
//! design-p8.md, section 3.1): making events as rows and writing them to the world's queue and
//! log, the metric counts, a person's activity, the readers of payloads and of a few namespaces,
//! and the checks a write stage asks first. A kernel moved to the core calls these, never the
//! PyO3 crate, whose files of the same helpers are now glue over these.
//!
//! The helpers that read a namespace take its value or its row: while a namespace is attached its
//! tree is the truth (until P8-0c), so the PyO3 crate reads it and hands it in. The helpers that
//! write a slot's row take a `TreeSink`, through which the PyO3 crate keeps the tree in step; a
//! kernel of the core passes `NoTree`. `Events` and `add_metric` write the rows of the world's
//! slots alone: their trees follow the rows once P8-0c marks the slots a kernel wrote.
//!
//! Errors are the core's, with the texts of the helpers they replace.

use std::collections::BTreeMap;
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use indexmap::IndexMap;

use crate::cell::ShardMut;
use crate::entities::{Activity, AgentRow, Entities};
use crate::error::{Error, Result};
use crate::events::log::EventLog;
use crate::events::writer::{self, Reserved, WriteError, WriteResult, MAX_RULE_COUNTER};
use crate::events::{is_stable_id, EventKind, EventRow, EventStatus};
use crate::geometry::Pt;
use crate::row::{map_set, Atom, TreeSink, ValueRow};
use crate::state::effect_program::EffectProgramRow;
use crate::state::metric_state::MetricRow;
use crate::state::movement::{MovementRow, Point, Position};
use crate::text::casefold;
use crate::value::V;
use crate::world::{set_id_counter, World, ID_COUNTERS, METRIC_STATE};

/// What the queue rules ask of the movement rules (lane c1).
pub mod movement;

/// `population_presence_v1`, the ledger of who is where.
pub const PRESENCE: &str = "population_presence_v1";
/// `population_dynamics_v1`, the arrivals and departures.
pub const DYNAMICS: &str = "population_dynamics_v1";
/// The source of a population record whose profile names none.
pub const POPULATION_SOURCE_ID: &str = "ASM-POPULATION-DYNAMICS-V1";

// ---- What ran and what was refused --------------------------------------------------------------

/// The counts a group of kernels keeps of what ran and what it refused, by label, for the shadow
/// benchmarks (`stats` of the PyO3 crate's `handlers` and `effects`).
pub struct Counts {
    ran: Mutex<BTreeMap<&'static str, u64>>,
    refusals: Mutex<BTreeMap<String, u64>>,
}

impl Counts {
    pub const fn new() -> Self {
        Self { ran: Mutex::new(BTreeMap::new()), refusals: Mutex::new(BTreeMap::new()) }
    }

    /// The kernel of this label ran to its end.
    pub fn ran(&self, label: &'static str) {
        if let Ok(mut ran) = self.ran.lock() {
            *ran.entry(label).or_insert(0) += 1;
        }
    }

    /// The kernel of this label refused, for this reason.
    pub fn refused(&self, label: &'static str, reason: &str) {
        if let Ok(mut refusals) = self.refusals.lock() {
            *refusals.entry(format!("{label}: {reason}")).or_insert(0) += 1;
        }
    }

    /// Count a kernel's result under its label: a refusal by its reason, any other error not at all.
    pub fn counted<T>(&self, label: &'static str, result: Result<T>) -> Result<T> {
        match &result {
            Ok(_) => self.ran(label),
            Err(Error::Unsupported(reason)) => self.refused(label, reason),
            Err(_) => {}
        }
        result
    }

    /// `(label, times)` of what ran, in label order.
    pub fn ran_counts(&self) -> Vec<(&'static str, u64)> {
        self.ran.lock().map(|ran| ran.iter().map(|(label, times)| (*label, *times)).collect()).unwrap_or_default()
    }

    /// `("label: reason", times)` of what was refused, in that text's order.
    pub fn refusal_counts(&self) -> Vec<(String, u64)> {
        self.refusals
            .lock()
            .map(|refusals| refusals.iter().map(|(reason, times)| (reason.clone(), *times)).collect())
            .unwrap_or_default()
    }
}

impl Default for Counts {
    fn default() -> Self {
        Self::new()
    }
}

// ---- Reading values -----------------------------------------------------------------------------

static NULL: V = V::Null;

/// `map.get(key)`: None (`V::Null`) for a key it does not have. Unsupported for a value that is not
/// a mapping.
#[track_caller]
pub fn get<'a>(map: &'a V, key: &str) -> Result<&'a V> {
    match map {
        V::Map(map) => Ok(map.get(key).unwrap_or(&NULL)),
        _ => Err(Error::unsupported("a value is not a plain dict")),
    }
}

/// `mapping[key]`, or None for a key it does not have. Unsupported for a value that is not a
/// mapping, as a payload whose read raises.
#[track_caller]
pub fn mapping_get<'a>(mapping: &'a V, key: &str) -> Result<Option<&'a V>> {
    match mapping {
        V::Map(map) => Ok(map.get(key)),
        _ => Err(Error::unsupported("a payload cannot be read")),
    }
}

/// `mapping[key]`: `KeyError(key)` for a key it does not have.
#[track_caller]
fn item<'a>(mapping: &'a V, key: &str) -> Result<&'a V> {
    match mapping {
        V::Map(map) => map.get(key).ok_or_else(|| Error::Key(key.to_owned())),
        _ => Err(Error::unsupported("a value is not a plain dict")),
    }
}

/// An int and nothing else, as the PyO3 crate's `exact_int` reads one.
#[track_caller]
pub fn exact_int(value: &V) -> Result<i64> {
    match value {
        V::Int(number) => Ok(*number),
        V::Big(_) => Err(Error::unsupported("an int is too large")),
        _ => Err(Error::unsupported("a value is not an int")),
    }
}

/// A str and nothing else, as the PyO3 crate's `exact_str` reads one.
#[track_caller]
pub fn exact_str(value: &V) -> Result<&Arc<str>> {
    match value {
        V::Str(text) => Ok(text),
        _ => Err(Error::unsupported("a value is not a str")),
    }
}

/// A str, or None for None.
#[track_caller]
pub fn optional_text(value: &V) -> Result<Option<&Arc<str>>> {
    match value {
        V::Null => Ok(None),
        other => exact_str(other).map(Some),
    }
}

/// `event.payload.get(key)` of a row whose payload is a mapping.
pub fn payload<'a>(row: &'a EventRow, key: &str) -> Option<&'a V> {
    let V::Map(map) = &row.payload else {
        return None;
    };
    map.get(key)
}

/// `event.provenance.get(key)` of a row whose provenance is a mapping.
pub fn provenance<'a>(row: &'a EventRow, key: &str) -> Option<&'a V> {
    let V::Map(map) = &row.provenance else {
        return None;
    };
    map.get(key)
}

/// `event.kind is EventKind.<kind>`.
pub fn kind_is(row: &EventRow, kind: EventKind) -> bool {
    row.kind == kind
}

/// The status of `EventStatus.<member>`, by the member's name.
pub fn status_member(member: &str) -> Option<EventStatus> {
    match member {
        "APPLIED" => Some(EventStatus::Applied),
        "FAILED" => Some(EventStatus::Failed),
        "SUPPRESSED" => Some(EventStatus::Suppressed),
        _ => None,
    }
}

// ---- Namespaces and configs (lane P8 a2) ----------------------------------------------------------

/// `model_state.get(name) is None` as a kernel asks it, of the world's slots: true for a name no
/// slot owns, and for a slot that has neither a row nor a taint (the key is not in `model_state`).
/// A tainted namespace is present: its readers refuse it.
pub fn namespace_absent(world: &World, name: &str) -> Result<bool> {
    match world.slots.get(name) {
        None => Ok(true),
        Some(slot) => {
            let slot = slot.try_borrow()?;
            Ok(slot.row.is_none() && slot.taint.is_none())
        }
    }
}

/// A section of the configs as the PyO3 crate read it for a kernel, before the kernel borrowed the
/// world: its value, or the fault the kernel raises where the rule reads the section, so that a
/// bad config raises at the same point as before, and not at all where the rule never reads it.
pub type Kept<T> = std::result::Result<Arc<T>, crate::config::Fault>;

/// The value of a `Kept` section, or its error: a refusal its own `Unsupported`, any other fault a
/// corrupt `model_configs` (the PyO3 crate's `configs::read`).
#[track_caller]
pub fn kept<T>(section: &Kept<T>) -> Result<&T> {
    match section {
        Ok(value) => Ok(value),
        Err(fault) if fault.is_refusal() => Err(Error::unsupported(fault.text.to_string())),
        Err(_) => Err(Error::corrupt(crate::world::MODEL_CONFIGS, "a value of a section holds an error")),
    }
}

// ---- Making events and writing them ---------------------------------------------------------------

/// The arguments of a new `ScheduledEvent`.
pub struct NewEvent {
    pub event_id: Arc<str>,
    pub kind: EventKind,
    pub due_minute: i64,
    pub priority: i64,
    pub sequence: i64,
    pub event_type: Arc<str>,
    pub payload: IndexMap<Arc<str>, V>,
    pub source_id: Option<Arc<str>>,
    pub causation_id: Option<Arc<str>>,
    pub provenance: IndexMap<Arc<str>, V>,
}

/// `ScheduledEvent(...)` as a row: Unsupported for values the event refuses, whose error is the
/// Python rule's to raise.
#[track_caller]
pub fn new_event(event: NewEvent) -> Result<EventRow> {
    EventRow::new(
        event.event_id,
        event.kind,
        event.due_minute,
        event.priority,
        event.sequence,
        event.event_type,
        event.payload,
        event.source_id,
        event.causation_id,
        event.provenance,
    )
    .ok_or_else(|| Error::unsupported("the event's values are not what an event takes"))
}

/// `{"source_type": "assumption", "source_id": source, "policy_version": policy}`.
pub fn assumption(source: V, policy: &str) -> IndexMap<Arc<str>, V> {
    let mut provenance = IndexMap::with_capacity(3);
    provenance.insert(Arc::from("source_type"), V::Str(Arc::from("assumption")));
    provenance.insert(Arc::from("source_id"), source);
    provenance.insert(Arc::from("policy_version"), V::Str(Arc::from(policy)));
    provenance
}

/// `EventLogRecord(...)` and `EventLog.append` of an outcome, into the world's log.
pub fn log_record(world: &World, event: &EventRow, minute: i64, status: EventStatus, detail: Option<&str>) -> WriteResult<()> {
    writer::record(&mut *world.log_mut()?, event, minute, status, detail)
}

/// A world's queue, log and event counter, for one step of a rule: what the PyO3 crate's
/// `EventWriter` does over the stores, done over the `World`. `allocate_sequence` and `schedule`
/// write the row of `id_counters`.
pub struct Events<'w> {
    world: &'w World,
    duration: i64,
    reserved: Reserved,
}

impl<'w> Events<'w> {
    /// Unsupported, with nothing changed, unless the world holds its queue and log and its event
    /// counter is an int a rule can add to: a rule asks in its planning stage.
    #[track_caller]
    pub fn of(world: &'w World) -> Result<Self> {
        if !world.queue_held {
            return Err(Error::unsupported("the event queue is not in Rust"));
        }
        if !world.log_held {
            return Err(Error::unsupported("the event log is not in Rust"));
        }
        match world.id_counter("event") {
            Ok(None) => {}
            Ok(Some(number)) if number <= MAX_RULE_COUNTER => {}
            _ => return Err(Error::unsupported("the event counter is not an int a rule can add to")),
        }
        let duration = world.duration_minutes().map_err(|_| Error::unsupported("the duration is not an int"))?;
        Ok(Self { world, duration, reserved: Reserved::default() })
    }

    pub fn world(&self) -> &'w World {
        self.world
    }

    pub fn duration(&self) -> i64 {
        self.duration
    }

    fn counter(&self) -> Result<i64> {
        Ok(self.world.id_counter("event")?.unwrap_or(0))
    }

    fn set_counter(&self, value: i64) -> Result<()> {
        set_id_counter(&mut *self.world.row_mut::<ValueRow>(ID_COUNTERS)?, "event", value)
    }

    /// What `allocate_sequence` would return, changing nothing, past the sequences reserved.
    pub fn next_sequence(&self) -> Result<i64> {
        Ok(self.reserved.after(self.counter()?))
    }

    /// Sets how many sequences `allocate_reserved` will take before what is planned from now on.
    pub fn reserve(&self, count: usize) {
        self.reserved.set(count);
    }

    /// `allocate_sequence`, of one the plan reserved.
    pub fn allocate_reserved(&self) -> Result<i64> {
        self.reserved.take_one();
        self.allocate_sequence()
    }

    /// `WorldState.allocate_event_sequence`.
    pub fn allocate_sequence(&self) -> Result<i64> {
        let sequence = self.counter()?;
        self.set_counter(sequence + 1)?;
        Ok(sequence)
    }

    /// `WorldState.schedule_event`, with its errors.
    pub fn schedule(&self, row: EventRow) -> WriteResult<()> {
        let after = writer::schedule(&mut *self.world.queue_mut()?, &*self.world.log()?, self.duration, row)?;
        let next = self.counter()?.max(after);
        self.set_counter(next).map_err(WriteError::from)
    }

    /// `EventLogRecord(...)` and `EventLog.append` of an outcome, with their errors.
    pub fn record(&self, event: &EventRow, realized_minute: i64, status: EventStatus, detail: Option<&str>) -> WriteResult<()> {
        log_record(self.world, event, realized_minute, status, detail)
    }

    /// Run f on the log, borrowed shared.
    pub fn with_log<R>(&self, f: impl FnOnce(&EventLog) -> R) -> Result<R> {
        Ok(f(&*self.world.log()?))
    }

    /// Whether this event is pending.
    pub fn is_pending(&self, event_id: &str) -> Result<bool> {
        Ok(self.world.queue()?.has_event(event_id))
    }

    /// Whether the log holds a record of this event ID, or of this sequence if one is given.
    pub fn is_realized(&self, event_id: &str, sequence: Option<i64>) -> Result<bool> {
        self.with_log(|log| log.has_event_id(event_id) || sequence.is_some_and(|sequence| log.has_sequence(sequence)))
    }

    /// Whether a new event of this ID (and of this sequence, if one is given) would be refused as a
    /// repeat: realized, or pending.
    pub fn is_taken(&self, event_id: &str, sequence: Option<i64>) -> Result<bool> {
        Ok(self.is_realized(event_id, sequence)? || self.is_pending(event_id)?)
    }
}

// ---- Metrics --------------------------------------------------------------------------------------

/// The tree of a detached slot, or of a kernel of the core: nothing to keep in step.
pub struct NoTree;

impl TreeSink for NoTree {
    type Error = Infallible;

    fn set_field(&self, _key: &'static str, _value: Atom<'_>) -> std::result::Result<(), Infallible> {
        Ok(())
    }

    fn set_entry(&self, _key: &str, _value: Atom<'_>) -> std::result::Result<(), Infallible> {
        Ok(())
    }
}

/// `metric_state[name] = metric_state.get(name, 0.0) + 1.0`, on the row and the tree.
pub fn add_one<S: TreeSink + ?Sized>(metrics: &mut MetricRow, tree: &S, name: &str) -> std::result::Result<(), S::Error> {
    let count = metrics.get(name).copied().unwrap_or(0.0) + 1.0;
    map_set(metrics, tree, name, count)
}

/// `world.metric_state` as its row, to write: Unsupported while the field holds no row.
#[track_caller]
pub fn metrics_of(world: &World) -> Result<ShardMut<'_, MetricRow>> {
    world.row_mut::<MetricRow>(METRIC_STATE)
}

/// `set_event(metric, get + 1.0)` of `world.metric_state`, on its row.
pub fn add_metric(world: &World, name: &str) -> Result<()> {
    let Ok(()) = add_one(&mut *metrics_of(world)?, &NoTree, name);
    Ok(())
}

// ---- People ---------------------------------------------------------------------------------------

/// A person's activity and target, on the row.
#[track_caller]
pub fn set_agent(entities: &mut Entities, agent_id: &str, activity: Activity, target: Option<&str>) -> Result<()> {
    let Some(row) = entities.agents.get_mut(agent_id) else {
        return Err(Error::unsupported("no such agent"));
    };
    row.activity = activity;
    row.target_id = target.map(Arc::from);
    Ok(())
}

/// `agent.profile.mobility.value`.
pub fn mobility_text(agent: &AgentRow) -> &'static str {
    agent.profile.mobility.text()
}

/// `current_household_id` as far as the presence ledger answers: Some(household) when it does,
/// None when the profile's group answers. `presence` is `model_state.get(PRESENCE)`.
#[track_caller]
pub fn presence_household(presence: Option<&V>, agent_id: &str) -> Result<Option<Option<Arc<str>>>> {
    let Some(V::Map(presence)) = presence else {
        return Ok(None);
    };
    let Some(people) = presence.get("people") else {
        return Ok(Some(None));
    };
    let V::Map(people) = people else {
        return Err(Error::unsupported("the presence state's people are for the Python rule"));
    };
    let Some(person) = people.get(agent_id) else {
        return Ok(Some(None));
    };
    let V::Map(person) = person else {
        return Err(Error::unsupported("a presence record is for the Python rule"));
    };
    match person.get("household_id") {
        None | Some(V::Null) => Ok(Some(None)),
        Some(V::Str(household)) => Ok(Some(Some(household.clone()))),
        Some(_) => Err(Error::unsupported("a value is not a str")),
    }
}

/// The profile's group of a person, None for no such person.
pub fn group_of(entities: &Entities, agent_id: &str) -> Option<Arc<str>> {
    entities.agents.get(agent_id).and_then(|agent| agent.profile.group_id.clone())
}

/// `current_household_id`: the presence ledger's household, or else the profile's group.
pub fn household_of(presence: Option<&V>, entities: &Entities, agent_id: &str) -> Result<Option<Arc<str>>> {
    match presence_household(presence, agent_id)? {
        Some(found) => Ok(found),
        None => Ok(group_of(entities, agent_id)),
    }
}

/// `current_household_id` of a world whose namespaces are rows (P8-0c on).
pub fn household_in(world: &World, agent_id: &str) -> Result<Option<Arc<str>>> {
    let presence = world.slots.try_value(PRESENCE)?;
    household_of(presence.as_deref(), &*world.rows()?, agent_id)
}

// ---- Facilities -----------------------------------------------------------------------------------

/// `"toilet" in kind.casefold()`.
pub fn is_toilet(kind: &str) -> bool {
    casefold(kind).contains("toilet")
}

/// `active_facility_outage`: `bool(infrastructure_event_context_ids(world, facility_id))`, of
/// `model_state.get("infrastructure_v1")`. A state that is not a dict has no outage; one whose
/// records lack a key raises that `KeyError`, as Python's reads do.
#[track_caller]
pub fn active_outage(infrastructure: Option<&V>, facility_id: &str) -> Result<bool> {
    let Some(state @ V::Map(_)) = infrastructure else {
        return Ok(false);
    };
    let V::Map(outages) = item(state, "outages")? else {
        return Err(Error::unsupported("the outages are not a plain dict"));
    };
    for record in outages.values() {
        if !item(record, "active")?.is_truthy() {
            continue;
        }
        let V::List(affected) = item(record, "affected_facility_ids")? else {
            return Err(Error::unsupported("the affected facilities are not a list"));
        };
        if affected.iter().any(|id| matches!(id, V::Str(id) if &**id == facility_id)) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `has_active_facility_modifier`, of the row of `effect_program_v1` if the world holds one.
pub fn has_active_modifier(program: Option<&EffectProgramRow>, facility_id: &str) -> bool {
    program
        .and_then(|program| program.active_facility_modifiers.get(facility_id))
        .is_some_and(|record| !record.modifiers.is_empty())
}

// ---- Places ---------------------------------------------------------------------------------------

/// Whether two position records have the same bits.
pub fn same_position(first: &Position, second: &Position) -> bool {
    let same_point = match (&first.point, &second.point) {
        (None, None) => true,
        (Some(a), Some(b)) => a.x.to_bits() == b.x.to_bits() && a.y.to_bits() == b.y.to_bits(),
        _ => false,
    };
    same_point
        && first.zone_id == second.zone_id
        && first.edge_id == second.edge_id
        && first.progress.to_bits() == second.progress.to_bits()
        && first.position_kind == second.position_kind
}

/// What `place_in_bed` did to the row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Placed {
    /// The row has no position record of the person: nothing written.
    Absent,
    /// The record has these bits already.
    Same,
    /// The record of this key is new: a tree must take it.
    Changed(Arc<str>),
}

/// `position.update({"zone_id": zone, "point": {"x", "y"}, "edge_id": None, "progress": 1.0,
/// "position_kind": "physical"})` of a person at their bed, on the row of `movement_v1`.
pub fn place_in_bed(movement: &mut MovementRow, agent_id: &str, zone_id: &Arc<str>, point: Pt) -> Placed {
    let Some((key, current)) = movement.positions.get_key_value(agent_id) else {
        return Placed::Absent;
    };
    let placed = Position {
        zone_id: zone_id.clone(),
        point: Some(Point { x: point.x, y: point.y }),
        edge_id: None,
        progress: 1.0,
        position_kind: Arc::from("physical"),
    };
    if same_position(current, &placed) {
        return Placed::Same;
    }
    let key = key.clone();
    movement.positions.insert(key.clone(), placed);
    Placed::Changed(key)
}

// ---- Population -----------------------------------------------------------------------------------

/// The start of the text of a refusal after a write stage began.
pub const WRITING_REFUSED: &str = "a kernel refused after it began writing";

/// A write stage's result: nothing may fall back after a write, so a refusal here is a bug to
/// see (`RuntimeError`). Every other error is what the same operation raises in the Python rule.
pub fn writing<T>(result: Result<T>) -> Result<T> {
    result.map_err(|error| match error {
        // As the PyO3 crate's `writing` writes `PyErr`'s `Unsupported: reason`.
        Error::Unsupported(reason) => Error::Runtime(format!("{WRITING_REFUSED}: Unsupported: {reason}")),
        other => other,
    })
}

/// What `check_waiting_for_admission` asks of the rules that are not in the core yet (the rooms of
/// the overlay, the movement's scene, the presence ledger), in its order.
pub trait Admission {
    type Error;
    /// A core error as the caller's.
    fn error(error: Error) -> Self::Error;
    /// Before the waiting are read: who is where (`Places::of`).
    fn begin(&mut self) -> std::result::Result<(), Self::Error>;
    /// `has_room` and then `available_room` of a zone an arrival may go to.
    fn room(&mut self, zone: &str) -> std::result::Result<(), Self::Error>;
    /// After every zone: `check_admission_paths` of them, `presence_state`, and the overlay's room
    /// admissions.
    fn paths(&mut self, zones: &[&str]) -> std::result::Result<(), Self::Error>;
}

/// What `admit_inner` asks of the waiting, whoever gets a place: a rule that writes and then admits
/// asks this first, as the admission can't refuse after the writes. `dynamics` is
/// `model_state.get(DYNAMICS)`, whose shapes the population validator checked: a shape it never
/// lets through is a refusal here.
pub fn check_waiting_for_admission<A: Admission>(dynamics: Option<&V>, admission: &mut A) -> std::result::Result<(), A::Error> {
    let state = match dynamics {
        None | Some(V::Null) => return Ok(()),
        Some(state) => state,
    };
    let pending = item(state, "pending_arrivals").map_err(A::error)?;
    if !pending.is_truthy() {
        return Ok(());
    }
    admission.begin()?;
    let V::List(pending) = pending else {
        return Err(A::error(Error::unsupported("the pending arrivals are not a list")));
    };
    let mut zones: Vec<&Arc<str>> = Vec::new();
    for waiting in pending {
        let targets = targets_of(waiting).map_err(A::error)?;
        for zone in targets {
            check_item_for_admission(waiting, zone).map_err(A::error)?;
            admission.room(zone)?;
            zones.push(zone);
        }
    }
    let zones: Vec<&str> = zones.iter().map(|zone| &***zone).collect();
    admission.paths(&zones)?;
    let V::List(arrivals) = item(state, "arrivals").map_err(A::error)? else {
        return Err(A::error(Error::unsupported("the arrivals are not a list")));
    };
    for record in arrivals {
        if let V::Map(fields) = record {
            if fields.contains_key("requested_count") {
                exact_int(item(record, "requested_count").map_err(A::error)?).map_err(A::error)?;
                exact_str(item(record, "event_id").map_err(A::error)?).map_err(A::error)?;
            }
        } else {
            return Err(A::error(Error::unsupported("an arrival is not a plain dict")));
        }
    }
    Ok(())
}

/// The checks of a waiting arrival's own fields, and the zones it may go to: its zone, then
/// `admission_zone_ids`.
fn targets_of(waiting: &V) -> Result<Vec<&Arc<str>>> {
    exact_int(item(waiting, "created_minute")?)?;
    exact_str(item(waiting, "event_id")?)?;
    exact_str(item(item(waiting, "profile")?, "id")?)?;
    if let V::Map(fields) = waiting {
        if let Some(route) = fields.get("approach_route_id") {
            exact_str(route)?;
        }
    }
    let mut targets = vec![exact_str(item(waiting, "zone_id")?)?];
    match get(waiting, "admission_zone_ids")? {
        V::Null if !matches!(waiting, V::Map(fields) if fields.contains_key("admission_zone_ids")) => {}
        V::List(zones) => {
            for zone in zones {
                targets.push(exact_str(zone)?);
            }
        }
        _ => return Err(Error::unsupported("the admission zones are not a list")),
    }
    Ok(targets)
}

/// `Identifier` of contracts/extensions.py.
#[track_caller]
fn identifier(text: &str) -> Result<()> {
    if is_stable_id(text) {
        Ok(())
    } else {
        Err(Error::unsupported("a value is not an Identifier"))
    }
}

/// What the plan asks of a person about to be admitted to this zone.
fn check_item_for_admission(waiting: &V, zone: &str) -> Result<()> {
    let raw = item(waiting, "profile")?;
    let agent_id = exact_str(item(raw, "id")?)?;
    let event_id = item(waiting, "event_id")?;
    let traits = get(waiting, "traits")?;
    let household = match traits {
        V::Map(_) => optional_text(get(traits, "household_id")?)?,
        _ => None,
    };
    let group = optional_text(item(raw, "group_id")?)?;
    // `raw.get("provenance", {}).get("source_id", POPULATION_SOURCE_ID)`.
    let source = match get(raw, "provenance")? {
        V::Null if !matches!(raw, V::Map(fields) if fields.contains_key("provenance")) => None,
        V::Map(fields) => fields.get("source_id"),
        _ => return Err(Error::unsupported("a provenance is not a plain dict")),
    };
    let event_id = exact_str(event_id)?;
    let source: &str = match source {
        None => POPULATION_SOURCE_ID,
        Some(source) => exact_str(source)?,
    };
    identifier(event_id)?;
    identifier(agent_id)?;
    identifier(zone)?;
    if let Some(household) = household.or(group) {
        identifier(household)?;
    }
    identifier(source)?;
    exact_int(item(waiting, "created_minute")?)?;
    if let V::Map(fields) = waiting {
        if let Some(route) = fields.get("approach_route_id") {
            exact_str(route)?;
        }
    }
    Ok(())
}

// ---- Namespaces the Python world holds (lane c1) -------------------------------------------------

/// Whether the Python world holds a namespace's slot, as the PyO3 crate's `held_mirror` of it
/// answers: a row left behind by a write past `model_state` answers for nothing. The PyO3 crate
/// reads it before it borrows the world, and a kernel says `Other`'s refusal where it read the
/// namespace before.
#[derive(Clone, Debug, PartialEq)]
pub enum Held {
    /// `model_state` has no such key.
    Absent,
    /// The key holds the slot's tree, or the placeholder of the detached slot.
    Held,
    /// Another value, or no slot: the refusal the read says.
    Other(&'static str),
}

/// The row of a namespace the world holds, borrowed shared: None for an absent key, the refusal of
/// `Other`, or the slot's own (a tainted namespace has no row).
#[track_caller]
pub fn held_row<'w, T: crate::row::Row>(world: &'w World, name: &str, held: &Held) -> Result<Option<crate::cell::ShardRef<'w, T>>> {
    match held {
        Held::Absent => Ok(None),
        Held::Other(reason) => Err(Error::unsupported(*reason)),
        Held::Held => world.row::<T>(name).map(Some),
    }
}
