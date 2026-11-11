//! `domain/population.py`: who arrives, waits, is admitted and leaves (lane c5 of Issue #152; the
//! contracts are the docstrings of `domain/population.py`).
//!
//! The rules are functions on the `World`. `population_dynamics_v1`, `population_presence_v1` and
//! `geometry_overlay_v1` are `ValueRow` namespaces, the others typed rows: a rule reads and writes
//! their rows alone, and an attached tree follows the row when the PyO3 crate gives the world back
//! (lane P8-0c). An admitted person is a new row of `agents` (`Table::insert`), whose Python object
//! the PyO3 crate makes from the row (lane 6-1): `build_agent` is what the constructors
//! `AgentProfile`, `InitialNeedProfile`, `NeedState`, `AgentState` and `Mobility` make of a saved
//! profile, with their `ValueError`s.
//!
//! Each rule plans, refusing with `Unsupported` before its first write what no valid input holds
//! (a `KernelRefusal` in Python), and then writes. A rule that raises its text after some writes
//! (`_state` making its state, a text of an arrival's item, a completion without its presence)
//! makes the same writes first. The texts (`EventSuppressed`, `EventApplicationError`, the
//! validation's `ValueError`s) are saved data and are the Python rules', in their order of checks.
//!
//! What only Python has comes as an input, read by the PyO3 crate before the world is borrowed:
//! whether `model_state` holds each namespace (`kit::Held`), the sections of the configs
//! (`kit::Kept`), the floor plan (`Geo`, or none: `PLAN_UNREAD` where a rule reads it), the approach
//! routes of the facility geometry. The pydantic contracts of the inputs (`PersonTraits`,
//! `DynamicArrivalPersonContext`, `PopulationSpec`; policy E15) go through `Host::contract`. A new
//! `population_dynamics_v1` is made here and put into `model_state` by the PyO3 crate before the
//! first write (a namespace a Run doesn't have yet has no slot). The pending events a departure
//! cancels are settled by the PyO3 crate (`Departed::cancels`), which keeps the Python objects of
//! the queue.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use indexmap::IndexMap;
use sha2::{Digest, Sha256};

use crate::cell::ShardRef;
use crate::config::agent_dynamics::Awareness;
use crate::config::geometry::ApproachRoute;
use crate::entities::{Activity, AgentRow, Entities, Mobility, NeedRow, ProfileRow};
use crate::error::{Error, Result};
use crate::events::writer::WriteError;
use crate::events::{is_stable_id, EventKind, EventRow};
use crate::geometry::{Geo, Pt};
use crate::host::{Contract, ErrorClass, Host, HostFailure};
use crate::json::Encoder;
use crate::mathx::py_min;
use crate::row::{Row, StrEnum, ValueRow};
use crate::rules::kit::movement::{record as position_record, KEY as MOVEMENT, KINDS};
use crate::rules::kit::{self, Events, Held, Kept};
use crate::rules::{agent_dynamics, movement, overlay};
use crate::state::agent_dynamics::{AgentDynamicsRow, NAME as AGENT_DYNAMICS};
use crate::state::body_state::{BodyRecord, BodyRow, NAME as BODY};
use crate::state::effect_program::{EffectProgramRow, NAME as EFFECT_PROGRAM};
use crate::state::information::{InformationRow, NAME as INFORMATION};
use crate::state::movement::MovementRow;
use crate::state::resource_service::{ServiceRow, NAME as RESOURCE_SERVICE};
use crate::state::social_network::{Relationship, SocialNetworkRow, NAME as SOCIAL_NETWORK};
use crate::state::social_propagation::{SocialRow, NAME as SOCIAL_RUNTIME};
use crate::text::py_repr;
use crate::value::V;
use crate::world::{World, WorldCell};

pub use crate::rules::kit::{DYNAMICS, POPULATION_SOURCE_ID as SOURCE_ID, PRESENCE};
pub use crate::rules::overlay::{OVERLAY_KEY as OVERLAY, PLAN_UNREAD, SPACES_KEY as SPACES};

pub const POLICY_VERSION: &str = "capacity-aware-arrivals-departures-v1";
const ROOM_INTAKE_SOURCE: &str = "ASM-TEMPORARY-ROOM-INTAKE-V1";
pub const MAX_ARRIVALS_PER_EVENT: i64 = 1000;
pub const MAX_PENDING_ARRIVALS: i64 = 10_000;
pub const MAX_PRESENCE_HISTORY: usize = 20_000;
const MAX_RETRIES: i64 = 1440;
pub const SUPPORT_KINDS: [&str; 9] = [
    "medical_assessment",
    "medication_support",
    "caregiver_assistance",
    "privacy_accommodation",
    "mobility_assistance",
    "hygiene_support",
    "emotional_support",
    "language_access",
    "dependent_assistance",
];

/// What a rule says where a pending event it reads has no row: the PyO3 crate makes the rows of
/// the queue's Python objects and asks again.
pub const NO_ROW: &str = "a pending event has no row";

const WAITING_HAS_RECORD: &str = "waiting arrival already has a presence record";

type Doc = IndexMap<Arc<str>, V>;

// ---- Reading values ------------------------------------------------------------------------------

#[track_caller]
fn refuse<T>(reason: &'static str) -> Result<T> {
    Err(Error::unsupported(reason))
}

/// `raise EventApplicationError(text)`.
fn failed<T>(text: impl Into<String>) -> Result<T> {
    Err(Error::failed(text))
}

/// `raise ValueError(text)` of the validation, marked as made here.
fn invalid<T>(text: &str) -> Result<T> {
    Err(Error::MarkedValue(text.to_owned()))
}

#[track_caller]
fn map_of(value: &V) -> Result<&Doc> {
    match value {
        V::Map(map) => Ok(map),
        _ => refuse("a value is not a plain dict"),
    }
}

#[track_caller]
fn map_mut(value: &mut V) -> Result<&mut Doc> {
    match value {
        V::Map(map) => Ok(map),
        _ => refuse("a value is not a plain dict"),
    }
}

#[track_caller]
fn list_of(value: &V) -> Result<&Vec<V>> {
    match value {
        V::List(items) => Ok(items),
        _ => refuse("a value is not a plain list"),
    }
}

#[track_caller]
fn list_mut(value: &mut V) -> Result<&mut Vec<V>> {
    match value {
        V::List(items) => Ok(items),
        _ => refuse("a value is not a plain list"),
    }
}

/// `value[key]` of a plain dict: Unsupported for a key it doesn't have, which no valid state lacks.
#[track_caller]
fn field<'a>(value: &'a V, key: &str) -> Result<&'a V> {
    map_of(value)?.get(key).ok_or_else(|| Error::unsupported("a field is missing"))
}

#[track_caller]
fn field_mut<'a>(value: &'a mut V, key: &str) -> Result<&'a mut V> {
    map_mut(value)?.get_mut(key).ok_or_else(|| Error::unsupported("a field is missing"))
}

/// `key in value` of a plain dict.
fn has(value: &V, key: &str) -> Result<bool> {
    Ok(map_of(value)?.contains_key(key))
}

#[track_caller]
fn text(value: &V) -> Result<Arc<str>> {
    kit::exact_str(value).cloned()
}

fn v_str(value: &str) -> V {
    V::Str(Arc::from(value))
}

pub fn optional(value: &Option<Arc<str>>) -> V {
    value.as_ref().map_or(V::Null, |text| V::Str(text.clone()))
}

pub fn strings(items: &[Arc<str>]) -> V {
    V::List(items.iter().map(|item| V::Str(item.clone())).collect())
}

/// A dict of these pairs, in this order.
fn doc<const N: usize>(pairs: [(&str, V); N]) -> Doc {
    let mut map = IndexMap::with_capacity(N);
    for (key, value) in pairs {
        map.insert(Arc::from(key), value);
    }
    map
}

/// `sorted(set(items))`.
pub fn sorted_set(mut items: Vec<Arc<str>>) -> Vec<Arc<str>> {
    items.sort_unstable_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    items.dedup();
    items
}

fn hex(digest: &[u8]) -> String {
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(char::from_digit(u32::from(byte >> 4), 16).unwrap_or('0'));
        out.push(char::from_digit(u32::from(byte & 15), 16).unwrap_or('0'));
    }
    out
}

pub fn sha_hex(text: &str) -> String {
    hex(&Sha256::digest(text.as_bytes()))
}

/// `Identifier` of contracts/extensions.py.
#[track_caller]
pub fn identifier(text: &str) -> Result<()> {
    if is_stable_id(text) {
        Ok(())
    } else {
        refuse("a value is not an Identifier")
    }
}

#[track_caller]
fn optional_identifier(text: Option<&str>) -> Result<()> {
    text.map_or(Ok(()), identifier)
}

/// `value == number` in Python, of a value of a plain document.
fn equals_int(value: &V, number: i64) -> bool {
    match value {
        V::Int(value) => *value == number,
        V::Bool(flag) => i64::from(*flag) == number,
        // Exact for the minutes of a Run, which a float holds.
        V::Float(value) => (-(1_i64 << 53)..=1 << 53).contains(&number) && *value == number as f64,
        _ => false,
    }
}

/// `value == text` in Python, of a value of a plain document.
fn equals_text(value: &V, text: &str) -> bool {
    matches!(value, V::Str(value) if &**value == text)
}

/// `json.loads(key)` of a key of a pair of people, as `_pair_from_key` accepts it.
pub fn pair_of(key: &str) -> Result<[String; 2]> {
    let Ok(parts) = serde_json::from_str::<Vec<String>>(key) else {
        return refuse("a pair key is not a list of strs");
    };
    <[String; 2]>::try_from(parts).map_err(|_| Error::unsupported("a pair key is not two people"))
}

fn names_person(key: &str, agent_id: &str) -> Result<bool> {
    Ok(pair_of(key)?.iter().any(|part| part == agent_id))
}

// ---- What the PyO3 crate reads for the rules -------------------------------------------------------

/// Whether `model_state` holds each namespace the rules read.
#[derive(Clone, Debug)]
pub struct Namespaces {
    pub dynamics: Held,
    pub presence: Held,
    pub information: Held,
    pub body: Held,
    pub agent_dynamics: Held,
    pub movement: Held,
    pub overlay: Held,
    pub spaces: Held,
    pub social_network: Held,
    pub social_runtime: Held,
    pub resource_service: Held,
    pub effect_program: Held,
}

/// Why reading the approach routes of `facility_geometry` raised: `caught` for a `TypeError` or
/// `ValueError` the arrivals' `try` turns into their text, and the refusal the plan says
/// otherwise (`the plan read: ...`).
#[derive(Clone, Debug)]
pub struct RoutesFault {
    pub caught: bool,
    pub refusal: Error,
}

pub type Routes = std::result::Result<Arc<[ApproachRoute]>, RoutesFault>;

/// What the rules read beside the world's rows.
pub struct Inputs {
    pub namespaces: Namespaces,
    /// `geometry_overlay.schema_version` of the configs.
    pub schema: Kept<Option<i64>>,
    /// The agent dynamics' policy and facility awareness, for `add_agent`.
    pub policy: Kept<&'static str>,
    pub awareness: Kept<Option<Awareness>>,
    /// The floor plan, None where the PyO3 crate did not read it or reading it raised.
    pub geo: Option<Arc<Geo>>,
    /// The approach routes, where a rule reads them.
    pub routes: Option<Routes>,
}

impl Inputs {
    fn geo(&self) -> Result<&Geo> {
        self.geo.as_deref().ok_or_else(|| Error::unsupported(PLAN_UNREAD))
    }

    fn routes(&self) -> Result<&Arc<[ApproachRoute]>> {
        match &self.routes {
            Some(Ok(routes)) => Ok(routes),
            Some(Err(fault)) => Err(fault.refusal.clone()),
            None => refuse("the approach routes were not read"),
        }
    }

    fn places(&self) -> overlay::Places<'_> {
        overlay::Places { spaces: &self.namespaces.spaces, overlay: &self.namespaces.overlay, schema: &self.schema }
    }

    fn intake(&self) -> overlay::Intake<'_> {
        overlay::Intake { overlay: &self.namespaces.overlay, movement: &self.namespaces.movement, schema: &self.schema, geo: self.geo.as_deref() }
    }
}

fn is_held(held: &Held) -> bool {
    !matches!(held, Held::Absent)
}

/// A `ValueRow` namespace the world holds, as `model_state.get(name)`: None for an absent key
/// and for a None, the refusal of a key holding another value.
fn value_of<'w>(world: &'w World, name: &str, held: &Held) -> Result<Option<ShardRef<'w, ValueRow>>> {
    Ok(kit::held_row::<ValueRow>(world, name, held)?.filter(|row| !matches!(row.0, V::Null)))
}

/// Whether a `ValueRow` namespace is there as a dict (`namespace(...).filter(is_dict)`).
fn is_dict_namespace(world: &World, name: &str, held: &Held) -> Result<bool> {
    Ok(value_of(world, name, held)?.is_some_and(|row| matches!(row.0, V::Map(_))))
}

/// A typed namespace the world holds, or None.
fn typed_of<'w, T: Row>(world: &'w World, name: &str, held: &Held) -> Result<Option<ShardRef<'w, T>>> {
    kit::held_row::<T>(world, name, held)
}

fn zone_rows(world: &World) -> Result<ShardRef<'_, Entities>> {
    let rows = world.entities.try_borrow()?;
    if rows.zones_tainted().is_some() {
        return refuse("the zones are tainted");
    }
    Ok(rows)
}

fn zone_known(world: &World, zone_id: &str) -> Result<bool> {
    Ok(zone_rows(world)?.zones.contains_key(zone_id))
}

/// `world.metric_state[name] = metric_state.get(name, 0.0) + count`.
fn add_metric(world: &World, name: &str, count: usize) -> Result<()> {
    let mut metrics = kit::metric_entries_of(world, &[name])?;
    let value = metrics.get(name).copied().unwrap_or(0.0) + count as f64;
    metrics.insert(Arc::from(name), value);
    Ok(())
}

fn set_metric(world: &World, name: &str, value: f64) -> Result<()> {
    kit::metric_entries_of(world, &[name])?.insert(Arc::from(name), value);
    Ok(())
}

// ---- The presence ledger: contracts/presence.py as far as a plain document goes ------------------

const RECORD_FIELDS: [&str; 9] = [
    "agent_id",
    "household_id",
    "status",
    "since_minute",
    "transition_zone_id",
    "return_zone_id",
    "expected_return_minute",
    "archive_path",
    "source_event_ids",
];
const STATUSES: [&str; 6] = ["admitted", "approaching", "awaiting_admission", "temporarily_absent", "departed", "transferred"];
const TRANSITION_FIELDS: [&str; 14] = [
    "schema_version",
    "transition_id",
    "event_id",
    "agent_id",
    "minute",
    "action",
    "from_zone_id",
    "to_zone_id",
    "from_household_id",
    "to_household_id",
    "causation_id",
    "source_ids",
    "state_snapshot_hash",
    "time_semantics",
];
const ACTIONS: [&str; 11] = [
    "arrived",
    "approaching",
    "approach_completed",
    "waiting",
    "admitted",
    "departed",
    "temporary_exit",
    "returned",
    "transferred",
    "reunited",
    "relocated",
];

fn required_identifier(map: &Doc, key: &str) -> Result<Arc<str>> {
    match map.get(key) {
        Some(V::Str(value)) if is_stable_id(value) => Ok(value.clone()),
        _ => refuse("a presence field is not an Identifier"),
    }
}

fn maybe_identifier(map: &Doc, key: &str) -> Result<Option<Arc<str>>> {
    match map.get(key) {
        None | Some(V::Null) => Ok(None),
        Some(V::Str(value)) if is_stable_id(value) => Ok(Some(value.clone())),
        _ => refuse("a presence field is not an Identifier or None"),
    }
}

fn identifiers(value: Option<&V>) -> Result<Vec<Arc<str>>> {
    match value {
        None => Ok(Vec::new()),
        Some(V::List(items)) => items
            .iter()
            .map(|item| match item {
                V::Str(text) if is_stable_id(text) => Ok(text.clone()),
                _ => refuse("a list holds what is not an Identifier"),
            })
            .collect(),
        _ => refuse("a list of Identifiers is something else"),
    }
}

fn minute_field(value: Option<&V>) -> Result<i64> {
    match value {
        Some(V::Int(minute)) if *minute >= 0 => Ok(*minute),
        _ => refuse("a minute is not a plain non-negative int"),
    }
}

/// `PresenceRecord`, of a document whose result is certain.
#[derive(Clone, Debug, PartialEq)]
pub struct Person {
    pub agent_id: Arc<str>,
    pub household_id: Option<Arc<str>>,
    pub status: Arc<str>,
    pub since_minute: i64,
    pub transition_zone_id: Option<Arc<str>>,
    pub return_zone_id: Option<Arc<str>>,
    pub expected_return_minute: Option<i64>,
    pub archive_path: Option<Vec<Arc<str>>>,
    pub source_event_ids: Vec<Arc<str>>,
}

impl Person {
    pub fn parse(value: &V) -> Result<Self> {
        let V::Map(map) = value else {
            return refuse("a presence record is not a mapping");
        };
        if map.keys().any(|key| !RECORD_FIELDS.contains(&&**key)) {
            return refuse("a presence record has another field");
        }
        let status = match map.get("status") {
            Some(V::Str(status)) if STATUSES.contains(&&**status) => status.clone(),
            _ => return refuse("a presence status is another value"),
        };
        let expected_return_minute = match map.get("expected_return_minute") {
            None | Some(V::Null) => None,
            other => Some(minute_field(other)?),
        };
        let archive_path = match map.get("archive_path") {
            None | Some(V::Null) => None,
            other => {
                let path = identifiers(other)?;
                if path.is_empty() || path.len() > 5 {
                    return refuse("an archive path has another length");
                }
                Some(path)
            }
        };
        let person = Self {
            agent_id: required_identifier(map, "agent_id")?,
            household_id: maybe_identifier(map, "household_id")?,
            status,
            since_minute: minute_field(map.get("since_minute"))?,
            transition_zone_id: maybe_identifier(map, "transition_zone_id")?,
            return_zone_id: maybe_identifier(map, "return_zone_id")?,
            expected_return_minute,
            archive_path,
            source_event_ids: identifiers(map.get("source_event_ids"))?,
        };
        person.check()?;
        Ok(person)
    }

    /// `coherent_return`.
    pub fn check(&self) -> Result<()> {
        if &*self.status == "temporarily_absent" {
            if self.expected_return_minute.is_some_and(|minute| minute < self.since_minute) {
                return refuse("expected return precedes departure");
            }
            if self.archive_path.is_none() {
                return refuse("temporary absence requires a retained state reference");
            }
        }
        if &*self.status == "admitted" && self.transition_zone_id.is_none() {
            return refuse("admitted presence requires a zone");
        }
        Ok(())
    }

    /// `model_dump(mode="json")`.
    pub fn dump(&self) -> V {
        V::Map(doc([
            ("agent_id", V::Str(self.agent_id.clone())),
            ("household_id", optional(&self.household_id)),
            ("status", V::Str(self.status.clone())),
            ("since_minute", V::Int(self.since_minute)),
            ("transition_zone_id", optional(&self.transition_zone_id)),
            ("return_zone_id", optional(&self.return_zone_id)),
            ("expected_return_minute", self.expected_return_minute.map_or(V::Null, V::Int)),
            ("archive_path", self.archive_path.as_ref().map_or(V::Null, |path| strings(path))),
            ("source_event_ids", strings(&self.source_event_ids)),
        ]))
    }

    /// `sha256(json.dumps(record.model_dump(mode="json"), sort_keys=True, separators=(",", ":")))`:
    /// the values are Identifiers, ints and None, so the canonical bytes are those.
    pub fn hash(&self) -> Result<String> {
        let mut encoder = Encoder { out: Vec::new() };
        self.dump().write_json(&mut encoder)?;
        Ok(hex(&Sha256::digest(&encoder.out)))
    }
}

/// `PresenceTransition`, with values the plan has checked.
#[derive(Clone, Debug)]
pub struct Transition {
    pub transition_id: String,
    pub event_id: Arc<str>,
    pub agent_id: Arc<str>,
    pub minute: i64,
    pub action: &'static str,
    pub from_zone_id: Option<Arc<str>>,
    pub to_zone_id: Option<Arc<str>>,
    pub from_household_id: Option<Arc<str>>,
    pub to_household_id: Option<Arc<str>>,
    pub causation_id: Option<Arc<str>>,
    pub source_ids: Vec<Arc<str>>,
    pub state_snapshot_hash: Option<String>,
}

impl Transition {
    pub fn dump(&self) -> V {
        V::Map(doc([
            ("schema_version", V::Int(2)),
            ("transition_id", v_str(&self.transition_id)),
            ("event_id", V::Str(self.event_id.clone())),
            ("agent_id", V::Str(self.agent_id.clone())),
            ("minute", V::Int(self.minute)),
            ("action", v_str(self.action)),
            ("from_zone_id", optional(&self.from_zone_id)),
            ("to_zone_id", optional(&self.to_zone_id)),
            ("from_household_id", optional(&self.from_household_id)),
            ("to_household_id", optional(&self.to_household_id)),
            ("causation_id", optional(&self.causation_id)),
            ("source_ids", strings(&self.source_ids)),
            ("state_snapshot_hash", self.state_snapshot_hash.as_deref().map_or(V::Null, v_str)),
            ("time_semantics", v_str("recorded_event_minute")),
        ]))
    }
}

fn transition_is_plain(value: &V) -> Result<()> {
    let V::Map(map) = value else {
        return refuse("a presence transition is not a mapping");
    };
    if map.keys().any(|key| !TRANSITION_FIELDS.contains(&&**key)) {
        return refuse("a presence transition has another field");
    }
    if !matches!(map.get("schema_version"), None | Some(V::Int(2))) {
        return refuse("a presence transition has another schema");
    }
    required_identifier(map, "transition_id")?;
    required_identifier(map, "event_id")?;
    required_identifier(map, "agent_id")?;
    minute_field(map.get("minute"))?;
    if !matches!(map.get("action"), Some(V::Str(action)) if ACTIONS.contains(&&**action)) {
        return refuse("a presence transition has another action");
    }
    for key in ["from_zone_id", "to_zone_id", "from_household_id", "to_household_id", "causation_id"] {
        maybe_identifier(map, key)?;
    }
    identifiers(map.get("source_ids"))?;
    match map.get("state_snapshot_hash") {
        None | Some(V::Null) => {}
        Some(V::Str(hash)) if hash.len() == 64 && hash.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f')) => {}
        _ => return refuse("a presence transition has another hash"),
    }
    match map.get("time_semantics") {
        None => {}
        Some(V::Str(text)) if &**text == "recorded_event_minute" => {}
        _ => return refuse("a presence transition has other time semantics"),
    }
    Ok(())
}

/// `PresenceState.model_validate(raw)`, for a document it certainly accepts.
pub fn presence_is_plain(raw: &V) -> Result<()> {
    let V::Map(state) = raw else {
        return refuse("the presence state is not a mapping");
    };
    if state.keys().any(|key| !["schema_version", "people", "history"].contains(&&**key)) {
        return refuse("the presence state has another field");
    }
    if !matches!(state.get("schema_version"), None | Some(V::Int(1))) {
        return refuse("the presence state has another schema");
    }
    let Some(V::Map(people)) = state.get("people") else {
        return refuse("the presence state lacks its people");
    };
    for (key, record) in people {
        identifier(key)?;
        Person::parse(record)?;
    }
    match state.get("history") {
        None => {}
        Some(V::List(items)) if items.len() <= MAX_PRESENCE_HISTORY => {
            for item in items {
                transition_is_plain(item)?;
            }
        }
        _ => return refuse("the presence history is something else"),
    }
    Ok(())
}

/// `_presence_state`: whether the world holds the ledger as a dict, checked.
fn presence_state(world: &World, held: &Held) -> Result<bool> {
    match value_of(world, PRESENCE, held)? {
        Some(row) if matches!(row.0, V::Map(_)) => {
            presence_is_plain(&row.0)?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// The record of a person in the ledger, if any.
fn person_in(presence: &V, agent_id: &str) -> Result<Option<Person>> {
    match kit::get(field(presence, "people")?, agent_id)? {
        V::Null => Ok(None),
        found => Person::parse(found).map(Some),
    }
}

/// `raw["people"][agent_id] = record` and the append of `_append_presence_history`.
pub fn write_presence(world: &World, person: &Person, transition: &Transition) -> Result<()> {
    let mut row = world.row_mut::<ValueRow>(PRESENCE)?;
    let state = map_mut(&mut row.0)?;
    map_mut(state.get_mut("people").ok_or_else(|| Error::Key("people".to_owned()))?)?.insert(person.agent_id.clone(), person.dump());
    let history = list_mut(state.get_mut("history").ok_or_else(|| Error::Key("history".to_owned()))?)?;
    history.push(transition.dump());
    if history.len() > MAX_PRESENCE_HISTORY {
        let extra = history.len() - MAX_PRESENCE_HISTORY;
        history.drain(..extra);
    }
    Ok(())
}

/// What `_record_presence_transition` asks of an ID it will write, checked in a plan.
fn check_presence_ids(event_id: &str, agent_id: &str, zone: Option<&str>, household: Option<&str>, source_id: &str) -> Result<()> {
    identifier(event_id)?;
    identifier(agent_id)?;
    optional_identifier(zone)?;
    optional_identifier(household)?;
    identifier(source_id)
}

/// `_record_presence_transition`. The plan checked the ledger and the IDs.
#[allow(clippy::too_many_arguments)]
fn record_presence_transition(
    world: &World,
    inputs: &Inputs,
    agent_id: &Arc<str>,
    event_id: &Arc<str>,
    minute: i64,
    action: &'static str,
    zone_id: Option<&Arc<str>>,
    household_id: Option<Arc<str>>,
    source_id: &str,
) -> Result<()> {
    let previous = {
        let Some(raw) = value_of(world, PRESENCE, &inputs.namespaces.presence)?.filter(|row| matches!(row.0, V::Map(_))) else {
            return Ok(());
        };
        person_in(&raw.0, agent_id)?
    };
    let (admitted, group, here) = {
        let rows = world.rows()?;
        match rows.agents.get(agent_id) {
            Some(agent) => (true, agent.profile.group_id.clone(), Some(agent.zone_id.clone())),
            None => (false, None, None),
        }
    };
    let mut household = household_id;
    if household.is_none() {
        household = previous.as_ref().and_then(|person| person.household_id.clone());
    }
    if household.is_none() && admitted {
        household = group;
    }
    let zone: Option<Arc<str>> = match zone_id {
        Some(zone) => Some(zone.clone()),
        None if admitted => here,
        None => previous.as_ref().and_then(|person| person.transition_zone_id.clone()),
    };
    let mut events = previous.as_ref().map_or(Vec::new(), |person| person.source_event_ids.clone());
    events.push(event_id.clone());
    let person = Person {
        agent_id: agent_id.clone(),
        household_id: household.clone(),
        status: Arc::from(if admitted { "admitted" } else { "departed" }),
        since_minute: minute,
        transition_zone_id: zone.clone(),
        return_zone_id: None,
        expected_return_minute: None,
        archive_path: None,
        source_event_ids: sorted_set(events),
    };
    // A backslash and a zero, not a NUL, in the ID of this one kind of receipt.
    let digest = sha_hex(&format!("{event_id}\\0{agent_id}\\0{action}"));
    let transition = Transition {
        transition_id: format!("presence-{}", &digest[..24]),
        event_id: event_id.clone(),
        agent_id: agent_id.clone(),
        minute,
        action,
        from_zone_id: previous.as_ref().and_then(|person| person.transition_zone_id.clone()),
        to_zone_id: zone,
        from_household_id: previous.as_ref().and_then(|person| person.household_id.clone()),
        to_household_id: household,
        causation_id: None,
        source_ids: vec![Arc::from(source_id)],
        state_snapshot_hash: None,
    };
    write_presence(world, &person, &transition)
}

// ---- Room in a zone: `capacity_metrics` as far as admission reads it -----------------------------

/// What `effective_capacity` is made of, for one zone.
#[derive(Clone, Copy)]
struct Room {
    nominal: i64,
    active_people: i64,
    expired_people: i64,
}

impl Room {
    /// `nominal + active_people + retained`, for this many people.
    fn effective(self, current: i64) -> i64 {
        let retained = self.expired_people.min((current - self.nominal - self.active_people).max(0));
        self.nominal + self.active_people + retained
    }
}

/// The people of a world and the rooms given in this plan, as admission counts them.
pub struct Places {
    occupancy: HashMap<Arc<str>, i64>,
    incoming: HashMap<Arc<str>, i64>,
    capacity: HashMap<Arc<str>, Option<Room>>,
    planned_rooms: Vec<(Arc<str>, Option<Pt>)>,
}

impl Places {
    pub fn of(world: &World) -> Result<Self> {
        let rows = world.rows()?;
        let mut occupancy: HashMap<Arc<str>, i64> = HashMap::new();
        let mut incoming: HashMap<Arc<str>, i64> = HashMap::new();
        for (_, agent) in rows.agents.iter() {
            *occupancy.entry(agent.zone_id.clone()).or_default() += 1;
            if agent.activity == Activity::Moving {
                if let Some(target) = &agent.target_id {
                    // `incoming_travelers`: a facility's zone, or the target itself.
                    let zone = rows.facilities.get(target).map_or(target, |facility| &facility.zone_id);
                    *incoming.entry(zone.clone()).or_default() += 1;
                }
            }
        }
        Ok(Self { occupancy, incoming, capacity: HashMap::new(), planned_rooms: Vec::new() })
    }

    /// `capacity_metrics`' reads for this zone.
    fn capacity(&mut self, world: &World, inputs: &Inputs, zone_id: &str) -> Result<Option<Room>> {
        if let Some(known) = self.capacity.get(zone_id) {
            return Ok(*known);
        }
        let found = match overlay::nominal_capacity(world, zone_id)? {
            None => None,
            Some(nominal) => {
                let read = overlay::capacity(world, &inputs.places(), zone_id, nominal, Some(nominal))?;
                Some(Room { nominal, active_people: read.active_people, expired_people: read.expired_people })
            }
        };
        self.capacity.insert(Arc::from(zone_id), found);
        Ok(found)
    }

    fn occupancy(&self, zone_id: &str) -> i64 {
        self.occupancy.get(zone_id).copied().unwrap_or(0)
    }

    /// population.py's `_zone_has_room`.
    pub fn has_room(&mut self, world: &World, inputs: &Inputs, zone_id: &str) -> Result<bool> {
        let Some(capacity) = self.capacity(world, inputs, zone_id)? else {
            return Ok(true);
        };
        let occupancy = self.occupancy(zone_id);
        Ok(occupancy + self.incoming.get(zone_id).copied().unwrap_or(0) < capacity.effective(occupancy))
    }

    /// A person is planned into this zone.
    pub fn add(&mut self, zone_id: &Arc<str>) {
        *self.occupancy.entry(zone_id.clone()).or_default() += 1;
    }

    /// `available_temporary_room`, and the point `admit_to_temporary_room` then gives.
    pub fn available_room(&self, world: &World, inputs: &Inputs, zone_id: &str) -> Result<Option<(Arc<str>, Pt)>> {
        overlay::available_room_after(world, &inputs.intake(), zone_id, &self.planned_rooms)
    }

    /// The room this plan fills, for the admissions that follow it.
    pub fn take_room(&mut self, inputs: &Inputs, room_id: &Arc<str>, point: Pt) {
        self.planned_rooms.push((room_id.clone(), is_held(&inputs.namespaces.movement).then_some(point)));
    }
}

/// A target of an admission: the zone, and a room of the overlay with its point.
type Target = (Arc<str>, Option<(Arc<str>, Pt)>);

/// `_choose_admission_target`: the first zone with room, or with a room of the overlay.
fn choose_target(world: &World, inputs: &Inputs, places: &mut Places, item: &V) -> Result<Option<Target>> {
    let mut zones = vec![text(field(item, "zone_id")?)?];
    match map_of(item)?.get("admission_zone_ids") {
        None => {}
        Some(V::List(alternates)) => {
            for zone in alternates {
                zones.push(text(zone)?);
            }
        }
        Some(_) => return refuse("the admission zones are not a list"),
    }
    for zone in zones {
        if places.has_room(world, inputs, &zone)? {
            return Ok(Some((zone, None)));
        }
        if let Some(room) = places.available_room(world, inputs, &zone)? {
            return Ok(Some((zone, Some(room))));
        }
    }
    Ok(None)
}

/// `_zone_has_room`.
pub fn zone_has_room(world: &World, inputs: &Inputs, zone_id: &str) -> Result<bool> {
    Places::of(world)?.has_room(world, inputs, zone_id)
}

/// `_choose_admission_target`: the zone, and the room if one is taken.
pub fn choose_admission_target(world: &World, inputs: &Inputs, item: &V) -> Result<Option<(Arc<str>, Option<Arc<str>>)>> {
    let mut places = Places::of(world)?;
    Ok(choose_target(world, inputs, &mut places, item)?.map(|(zone, room)| (zone, room.map(|(room_id, _)| room_id))))
}

// ---- The agent the constructors make -------------------------------------------------------------

/// What the constructors make of a saved profile: the agent's row, or the `ValueError` they raise
/// (`Mobility(...)`, `InitialNeedProfile.__post_init__`, `AgentState.__post_init__`), which the
/// rule raises where it makes the agent.
#[derive(Clone)]
pub enum Built {
    Row(Box<AgentRow>),
    Raises(Error),
}

const NEED_NAMES: [&str; 4] = ["toilet_urgency", "hunger", "fatigue", "hydration_need"];

/// `AgentState(profile=AgentProfile(...), zone_id=zone_id, needs=NeedState(**initial),
/// next_decision_at=minute)` of a saved profile, as `_admit_profile` makes it (`schema_version` is
/// the world's: v5 has no hydration, which is then 0.0). Unsupported where the constructors raise
/// another error (a field missing, a key `InitialNeedProfile` does not take), or make an object
/// whose fields no row holds (an int need level, an ID that is not a str): the Python store would
/// then taint. A `ValueError` comes first where the constructors raise it first.
pub fn build_agent(profile: &V, zone_id: &Arc<str>, minute: i64, schema_version: i64) -> Result<Built> {
    let raw = map_of(profile)?;
    let missing = || Error::unsupported("a saved profile lacks a field");
    // `initial.update(raw["initial_needs"])`, then v5's hydration.
    let V::Map(given) = raw.get("initial_needs").ok_or_else(missing)? else {
        return refuse("saved initial needs are not a plain dict");
    };
    let mut initial: Doc = given.clone();
    if schema_version == 5 {
        initial.insert(Arc::from("hydration_need"), V::Float(0.0));
    }
    let V::Map(provenance) = raw.get("provenance").ok_or_else(missing)? else {
        return refuse("a saved provenance is not a plain dict");
    };
    let id = raw.get("id").ok_or_else(missing)?;
    let group_id = raw.get("group_id").ok_or_else(missing)?;
    let initial_zone_id = raw.get("initial_zone_id").ok_or_else(missing)?;
    let mobility = match raw.get("mobility").ok_or_else(missing)? {
        V::Str(value) => match Mobility::from_text(value) {
            Some(member) => member,
            None => return Ok(Built::Raises(Error::Value(format!("{} is not a valid Mobility", py_repr(value))))),
        },
        // The enum raises a ValueError with the value's repr, which no saved profile has.
        _ => return refuse("a saved mobility is not a str"),
    };
    let support_need = raw.get("support_need").ok_or_else(missing)?;
    let stress_sensitivity = raw.get("stress_sensitivity").ok_or_else(missing)?;
    let rng_key = raw.get("rng_key").ok_or_else(missing)?;
    // `InitialNeedProfile(**initial)`: its keys, then each level in the order it checks them.
    if initial.keys().any(|key| !NEED_NAMES.contains(&&**key)) {
        return refuse("saved initial needs have another key");
    }
    let mut levels = [0.0; 4];
    let mut fits = true;
    for (place, name) in NEED_NAMES.iter().enumerate() {
        match initial.get(*name) {
            None => {}
            Some(V::Float(value)) if value.is_finite() && (0.0..=1.0).contains(value) => levels[place] = *value,
            // A level the check takes that a float field of a row doesn't hold.
            Some(V::Int(0 | 1)) => fits = false,
            Some(_) => return Ok(Built::Raises(Error::Value(format!("{name} must be finite and between 0 and 1")))),
        }
    }
    if minute < 0 {
        return Ok(Built::Raises(Error::Value("next_decision_at must be a non-negative integer".to_owned())));
    }
    let (V::Str(id), V::Str(initial_zone_id), V::Str(rng_key), V::Float(support_need), V::Float(stress_sensitivity)) =
        (id, initial_zone_id, rng_key, support_need, stress_sensitivity)
    else {
        return refuse("a saved profile has a value no row holds");
    };
    let group_id = match group_id {
        V::Null => None,
        V::Str(group) => Some(group.clone()),
        _ => return refuse("a saved group is not a str"),
    };
    if !fits {
        return refuse("a saved need is an int");
    }
    let needs = NeedRow { toilet_urgency: levels[0], hunger: levels[1], fatigue: levels[2], hydration_need: levels[3] };
    Ok(Built::Row(Box::new(AgentRow {
        profile: ProfileRow {
            id: id.clone(),
            group_id,
            initial_zone_id: initial_zone_id.clone(),
            mobility,
            support_need: *support_need,
            stress_sensitivity: *stress_sensitivity,
            rng_key: rng_key.clone(),
            initial_needs: needs.clone(),
            provenance: V::Map(provenance.clone()),
        },
        zone_id: zone_id.clone(),
        activity: Activity::Idle,
        target_id: None,
        needs,
        stress: 0.0,
        trust: 0.5,
        next_decision_at: minute,
        support_request_id: None,
        support_cooldown_until: 0,
    })))
}

// ---- `_admit_profile` ----------------------------------------------------------------------------

/// `admit_to_temporary_room`, whose checks the plan made.
pub fn write_room_admission(world: &World, held: &Held, agent_id: &str, room_id: &str, minute: i64, event_id: &str) -> Result<()> {
    if value_of(world, OVERLAY, held)?.is_none() {
        return Err(Error::Runtime("the overlay left during an admission".to_owned()));
    }
    let mut row = world.row_mut::<ValueRow>(OVERLAY)?;
    let mut sources = vec![Arc::from(ROOM_INTAKE_SOURCE)];
    for source in list_of(field(field(field(&row.0, "temporary_rooms")?, room_id)?, "source_ids")?)? {
        sources.push(text(source)?);
    }
    let record = V::Map(doc([
        ("agent_id", v_str(agent_id)),
        ("room_id", v_str(room_id)),
        ("admitted_minute", V::Int(minute)),
        ("event_id", v_str(event_id)),
        ("source_ids", strings(&sorted_set(sources))),
    ]));
    map_mut(field_mut(&mut row.0, "room_admissions")?)?.insert(Arc::from(agent_id), record);
    Ok(())
}

/// `state["positions"][id] = {...}` of a person now standing in a zone, and `finish_movement`.
pub fn reset_position(world: &World, geo: &Geo, agent_id: &Arc<str>, zone_id: &Arc<str>) -> Result<()> {
    world
        .row_mut::<MovementRow>(MOVEMENT)?
        .positions
        .insert(agent_id.clone(), position_record(zone_id.clone(), None, KINDS.unavailable.clone(), None, 0.0));
    movement::finish(world, geo, agent_id)
}

/// `add_body_agent` for a person with no stress yet.
fn add_body_record(world: &World, agent_id: &Arc<str>, fatigue: f64) -> Result<()> {
    let record = BodyRecord {
        sleep_debt_minutes: py_min(1440.0, fatigue * 360.0),
        uncertainty: py_min(1.0, 0.25 + 0.0 * 0.5),
        reassurance: 0.0,
        discontent: 0.0,
        positive_affect: 0.5,
        negative_affect: 0.0,
        hygiene_need: 0.0,
    };
    world.row_mut::<BodyRow>(BODY)?.agents.insert(agent_id.clone(), record);
    Ok(())
}

/// What admission asks of the paths it writes through, in a plan: their writes must not refuse.
pub fn check_admission_paths(world: &World, inputs: &Inputs, zones: &[&str]) -> Result<()> {
    if is_held(&inputs.namespaces.agent_dynamics) {
        agent_dynamics::check_add_agent(world, &inputs.policy, &inputs.awareness)?;
    }
    if is_held(&inputs.namespaces.movement) {
        if let Held::Other(_) = inputs.namespaces.movement {
            return refuse("the world holds another movement state");
        }
        world.row::<MovementRow>(MOVEMENT)?;
        movement::check_idle_points(world, inputs.geo()?, zones)?;
    }
    Ok(())
}

/// A waiting arrival's household (its traits') and source (its profile's provenance).
fn household_and_source(item: &V) -> Result<(Option<Arc<str>>, Arc<str>)> {
    let household = match kit::get(item, "traits")? {
        traits @ V::Map(_) => kit::optional_text(kit::get(traits, "household_id")?)?.cloned(),
        _ => None,
    };
    let profile = field(item, "profile")?;
    let source = match map_of(profile)?.get("provenance") {
        None => Arc::from(SOURCE_ID),
        Some(V::Map(provenance)) => match provenance.get("source_id") {
            None => Arc::from(SOURCE_ID),
            Some(source) => text(source)?,
        },
        Some(_) => return refuse("a provenance is not a plain dict"),
    };
    Ok((household, source))
}

/// What the plan asks of a person about to be admitted.
fn check_item_for_admission(item: &V, zone: &str) -> Result<()> {
    let raw = field(item, "profile")?;
    let agent_id = text(field(raw, "id")?)?;
    let event_id = text(field(item, "event_id")?)?;
    let (household, source) = household_and_source(item)?;
    let group = kit::optional_text(field(raw, "group_id")?)?.cloned();
    check_presence_ids(&event_id, &agent_id, Some(zone), household.as_deref().or(group.as_deref()), &source)?;
    kit::exact_int(field(item, "created_minute")?)?;
    if let Some(route) = map_of(item)?.get("approach_route_id") {
        text(route)?;
    }
    Ok(())
}

/// One admission a plan has made.
#[derive(Clone)]
struct Admission {
    item: V,
    zone: Arc<str>,
    room: Option<(Arc<str>, Pt)>,
    agent: Built,
}

/// `_admit_profile`.
fn write_admission(world: &World, inputs: &Inputs, admission: &Admission, minute: i64) -> Result<Arc<str>> {
    let agent = match &admission.agent {
        Built::Row(agent) => agent,
        Built::Raises(error) => return Err(error.clone()),
    };
    let agent_id = agent.profile.id.clone();
    let fatigue = agent.needs.fatigue;
    let zone_id = &admission.zone;
    let item = &admission.item;
    world.rows_mut()?.agents.insert(agent_id.clone(), (**agent).clone());
    if is_held(&inputs.namespaces.information) {
        let mut info = world.row_mut::<InformationRow>(INFORMATION)?;
        info.beliefs.insert(agent_id.clone(), Default::default());
        info.query_pressure.insert(agent_id.clone(), 0.0);
    }
    agent_dynamics::add_agent(world, Some(&agent_id), &inputs.policy, &inputs.awareness)?;
    if is_held(&inputs.namespaces.body) {
        add_body_record(world, &agent_id, fatigue)?;
    }
    if value_of(world, DYNAMICS, &inputs.namespaces.dynamics)?.is_none() {
        return Err(Error::Runtime("the population state left during an admission".to_owned()));
    }
    let event_id = field(item, "event_id")?.clone();
    let mut record = doc([
        ("event_id", event_id.clone()),
        ("agent_id", V::Str(agent_id.clone())),
        ("zone_id", V::Str(zone_id.clone())),
        ("preferred_zone_id", field(item, "zone_id")?.clone()),
        ("room_id", admission.room.as_ref().map_or(V::Null, |(room_id, _)| V::Str(room_id.clone()))),
        ("created_minute", field(item, "created_minute")?.clone()),
        ("admitted_minute", V::Int(minute)),
    ]);
    let fields = map_of(item)?;
    if fields.contains_key("approach_route_id") {
        record.insert(Arc::from("approach_route_id"), field(item, "approach_route_id")?.clone());
        record.insert(Arc::from("approach_started_minute"), field(item, "approach_started_minute")?.clone());
        let completed = match fields.get("approach_completed_minute") {
            Some(completed) => completed.clone(),
            None => field(item, "approach_due_minute")?.clone(),
        };
        record.insert(Arc::from("approach_completed_minute"), completed);
    }
    {
        let mut row = world.row_mut::<ValueRow>(DYNAMICS)?;
        list_mut(field_mut(&mut row.0, "arrivals")?)?.push(V::Map(record));
    }
    let event_text = text(&event_id)?;
    if is_held(&inputs.namespaces.movement) {
        reset_position(world, inputs.geo()?, &agent_id, zone_id)?;
    }
    if let Some((room_id, point)) = &admission.room {
        write_room_admission(world, &inputs.namespaces.overlay, &agent_id, room_id, minute, &event_text)?;
        if is_held(&inputs.namespaces.movement) {
            let mut row = world.row_mut::<MovementRow>(MOVEMENT)?;
            if !row.positions.contains_key(&*agent_id) {
                return Err(Error::Key(agent_id.to_string()));
            }
            kit::place_in_bed(&mut row, &agent_id, zone_id, *point);
        }
    }
    let status = match value_of(world, PRESENCE, &inputs.namespaces.presence)? {
        Some(presence) if matches!(presence.0, V::Map(_)) => person_in(&presence.0, &agent_id)?.map(|person| person.status),
        _ => None,
    };
    let action = if status.as_deref() == Some("awaiting_admission") { "admitted" } else { "arrived" };
    let (household, source) = household_and_source(item)?;
    record_presence_transition(world, inputs, &agent_id, &event_text, minute, action, Some(zone_id), household, &source)?;
    Ok(agent_id)
}

/// What a plan with admissions asks after them: the paths, the ledger, the overlay's admissions.
fn check_admissions(world: &World, inputs: &Inputs, admissions: &[Admission]) -> Result<()> {
    if admissions.is_empty() {
        return Ok(());
    }
    let zones: Vec<&str> = admissions.iter().map(|admission| &*admission.zone).collect();
    check_admission_paths(world, inputs, &zones)?;
    presence_state(world, &inputs.namespaces.presence)?;
    if admissions.iter().any(|admission| admission.room.is_some()) {
        let overlay = value_of(world, OVERLAY, &inputs.namespaces.overlay)?.ok_or_else(|| Error::unsupported("no overlay"))?;
        if !matches!(kit::get(&overlay.0, "room_admissions")?, V::Map(_)) {
            return refuse("a room admission has no admissions to join");
        }
    }
    Ok(())
}

// ---- `admit_pending_arrivals` --------------------------------------------------------------------

/// Whether `population_dynamics_v1` surely has no one waiting (`nothing_in`): absent, None, or a
/// dict whose `pending_arrivals` is empty.
pub fn nothing_waiting(world: &World, held: &Held) -> Result<bool> {
    let Some(state) = value_of(world, DYNAMICS, held)? else {
        return Ok(true);
    };
    Ok(match &state.0 {
        V::Map(state) => state.get("pending_arrivals").is_some_and(|pending| !pending.is_truthy()),
        _ => false,
    })
}

struct AdmitPlan {
    admissions: Vec<Admission>,
    /// The indices of the arrivals that keep waiting, in admission order.
    remaining: Vec<usize>,
    held: usize,
    /// For each waiting arrival: its event and ID.
    waiting: Vec<(Arc<str>, Arc<str>)>,
    /// The arrival summaries: their index in `arrivals`, event and requested count.
    summaries: Vec<(usize, Arc<str>, i64)>,
}

fn plan_admit(world: &World, inputs: &Inputs, minute: i64) -> Result<Option<AdmitPlan>> {
    let Some(state) = value_of(world, DYNAMICS, &inputs.namespaces.dynamics)? else {
        return Ok(None);
    };
    let pending = field(&state.0, "pending_arrivals")?;
    if !pending.is_truthy() {
        return Ok(None);
    }
    let pending = list_of(pending)?;
    let mut waiting = Vec::with_capacity(pending.len());
    for (index, item) in pending.iter().enumerate() {
        let created = kit::exact_int(field(item, "created_minute")?)?;
        let event_id = text(field(item, "event_id")?)?;
        let agent_id = text(field(field(item, "profile")?, "id")?)?;
        waiting.push((created, event_id, agent_id, index));
    }
    // Stable, as sorted is; IDs compare as their bytes do.
    waiting.sort_by(|first, second| (first.0, first.1.as_bytes(), first.2.as_bytes()).cmp(&(second.0, second.1.as_bytes(), second.2.as_bytes())));
    let schema_version = world.schema_version()?;
    let mut places = Places::of(world)?;
    let mut admissions = Vec::new();
    let mut remaining = Vec::with_capacity(waiting.len());
    let mut still = Vec::with_capacity(waiting.len());
    for (_, event_id, agent_id, index) in waiting {
        let item = &pending[index];
        let approaching = has(item, "approach_route_id")? && matches!(kit::get(item, "approach_completed_minute")?, V::Null);
        if !approaching {
            if let Some((zone, room)) = choose_target(world, inputs, &mut places, item)? {
                check_item_for_admission(item, &zone)?;
                let agent = build_agent(field(item, "profile")?, &zone, minute, schema_version)?;
                places.add(&zone);
                if let Some((room_id, point)) = &room {
                    places.take_room(inputs, room_id, *point);
                }
                admissions.push(Admission { item: item.clone(), zone, room, agent });
                continue;
            }
        }
        remaining.push(index);
        still.push((event_id, agent_id));
    }
    check_admissions(world, inputs, &admissions)?;
    let mut summaries = Vec::new();
    for (index, record) in list_of(field(&state.0, "arrivals")?)?.iter().enumerate() {
        if !has(record, "requested_count")? {
            continue;
        }
        let requested = kit::exact_int(field(record, "requested_count")?)?;
        let event_id = text(field(record, "event_id")?)?;
        summaries.push((index, event_id, requested));
    }
    Ok(Some(AdmitPlan { admissions, remaining, held: pending.len(), waiting: still, summaries }))
}

/// A summary's counts to write: its index, and the waiting and admitted counts that changed.
type CountChange = (usize, Option<i64>, Option<i64>);

fn write_admit(world: &World, inputs: &Inputs, minute: i64, plan: AdmitPlan) -> Result<Vec<Arc<str>>> {
    let mut admitted = Vec::with_capacity(plan.admissions.len());
    for admission in &plan.admissions {
        admitted.push(write_admission(world, inputs, admission, minute)?);
    }
    // Everyone stays where they were on most ticks: the list is then not written.
    let same = plan.remaining.len() == plan.held && plan.remaining.iter().enumerate().all(|(place, index)| place == *index);
    let mut counts: HashMap<&str, HashSet<&str>> = HashMap::new();
    for (event_id, agent_id) in &plan.waiting {
        counts.entry(event_id).or_default().insert(agent_id);
    }
    let changes: Vec<CountChange> = {
        let state = value_of(world, DYNAMICS, &inputs.namespaces.dynamics)?.ok_or_else(|| Error::Runtime("the population state left".to_owned()))?;
        let arrivals = list_of(field(&state.0, "arrivals")?)?;
        let mut changes = Vec::new();
        for (index, event_id, requested) in &plan.summaries {
            let count = counts.get(&**event_id).map_or(0, HashSet::len) as i64;
            let record = arrivals.get(*index).ok_or_else(|| Error::Runtime("a summary left".to_owned()))?;
            // The same value again changes nothing but the stamp.
            let set_waiting = !matches!(kit::get(record, "waiting_count")?, V::Int(value) if *value == count);
            let set_admitted = !matches!(kit::get(record, "admitted_count")?, V::Int(value) if *value == requested - count);
            if set_waiting || set_admitted {
                changes.push((*index, set_waiting.then_some(count), set_admitted.then_some(requested - count)));
            }
        }
        changes
    };
    if !same || !changes.is_empty() {
        let mut row = world.row_mut::<ValueRow>(DYNAMICS)?;
        if !same {
            let pending = list_mut(field_mut(&mut row.0, "pending_arrivals")?)?;
            let mut kept: Vec<Option<V>> = std::mem::take(pending).into_iter().map(Some).collect();
            *pending = plan.remaining.iter().map(|index| kept[*index].take().unwrap_or(V::Null)).collect();
        }
        let arrivals = list_mut(field_mut(&mut row.0, "arrivals")?)?;
        for (index, waiting, admitted_count) in changes {
            let record = map_mut(&mut arrivals[index])?;
            if let Some(count) = waiting {
                record.insert(Arc::from("waiting_count"), V::Int(count));
            }
            if let Some(count) = admitted_count {
                record.insert(Arc::from("admitted_count"), V::Int(count));
            }
        }
    }
    if !admitted.is_empty() {
        add_metric(world, "population_arrival_count", admitted.len())?;
    }
    set_metric(world, "population_waiting_count", plan.remaining.len() as f64)?;
    Ok(admitted)
}

/// `admit_pending_arrivals`: the IDs admitted.
pub fn admit_pending(world: &World, inputs: &Inputs, minute: i64) -> Result<Vec<Arc<str>>> {
    if nothing_waiting(world, &inputs.namespaces.dynamics)? {
        return Ok(Vec::new());
    }
    match plan_admit(world, inputs, minute)? {
        None => Ok(Vec::new()),
        Some(plan) => kit::writing(write_admit(world, inputs, minute, plan)),
    }
}

/// What `admit_pending_arrivals` asks of the waiting, whoever gets a place: a rule that writes and
/// then admits asks this first (`kit::check_waiting_for_admission`).
pub fn check_waiting_for_admission(world: &World, inputs: &Inputs) -> Result<()> {
    if nothing_waiting(world, &inputs.namespaces.dynamics)? {
        return Ok(());
    }
    let state = value_of(world, DYNAMICS, &inputs.namespaces.dynamics)?;
    kit::check_waiting_for_admission(state.as_deref().map(|row| &row.0), &mut Checks { world, inputs, places: None })
}

/// What `check_waiting_for_admission` asks of the rooms, the paths and the ledger.
struct Checks<'a> {
    world: &'a World,
    inputs: &'a Inputs,
    places: Option<Places>,
}

impl kit::Admission for Checks<'_> {
    type Error = Error;

    fn error(error: Error) -> Error {
        error
    }

    fn begin(&mut self) -> Result<()> {
        self.places = Some(Places::of(self.world)?);
        Ok(())
    }

    fn room(&mut self, zone: &str) -> Result<()> {
        let places = self.places.as_mut().ok_or_else(|| Error::Runtime("the admission check did not begin".to_owned()))?;
        places.has_room(self.world, self.inputs, zone)?;
        places.available_room(self.world, self.inputs, zone)?;
        Ok(())
    }

    fn paths(&mut self, zones: &[&str]) -> Result<()> {
        check_admission_paths(self.world, self.inputs, zones)?;
        presence_state(self.world, &self.inputs.namespaces.presence)?;
        if let Some(overlay) = value_of(self.world, OVERLAY, &self.inputs.namespaces.overlay)? {
            if !matches!(kit::get(&overlay.0, "room_admissions")?, V::Map(_)) && has(&overlay.0, "temporary_rooms")? {
                return refuse("a room admission has no admissions to join");
            }
        }
        Ok(())
    }
}

// ---- `_state` -------------------------------------------------------------------------------------

/// `create_population_dynamics_state`. The contract of the Scenario's population is Python's
/// (`Host::contract`); `config` is `model_configs.get("population")`, None for None.
fn new_dynamics_state<C: WorldCell, H: Host>(cell: &C, host: &mut H, config: Option<&V>) -> std::result::Result<V, H::Error> {
    let mut traits = Doc::new();
    if let Some(raw) = config {
        let spec = host.contract(&Contract::PopulationSpec, raw)?;
        let people = map_of(field(&spec, "people")?)?;
        let mut ids: Vec<&Arc<str>> = people.keys().collect();
        ids.sort_unstable_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
        for id in ids {
            traits.insert(id.clone(), people[&**id].clone());
        }
    }
    let initial = cell.with(|world| -> Result<Vec<Arc<str>>> {
        let rows = world.rows()?;
        Ok(sorted_set(rows.agents.iter().map(|(id, _)| id.clone()).collect()))
    })??;
    Ok(V::Map(doc([
        ("schema_version", V::Int(1)),
        ("policy_version", v_str(POLICY_VERSION)),
        ("source_id", v_str(SOURCE_ID)),
        ("initial_agent_ids", strings(&initial)),
        ("profile_traits", V::Map(traits)),
        ("profile_context", V::Map(Doc::new())),
        ("pending_arrivals", V::List(Vec::new())),
        ("arrivals", V::List(Vec::new())),
        ("departures", V::List(Vec::new())),
    ])))
}

/// What `_state` gives: Some(a new state) where the world has none, which the first write puts
/// into `model_state`; None for the world's.
fn dynamics_or_new<C: WorldCell, H: Host>(cell: &C, host: &mut H, inputs: &Inputs, config: Option<&V>) -> std::result::Result<Option<V>, H::Error> {
    let present = cell.with(|world| -> Result<bool> {
        match &inputs.namespaces.dynamics {
            Held::Absent => Ok(false),
            Held::Other(reason) => Err(Error::unsupported(*reason)),
            // `_state` returns the None it finds, and the rule raises on it.
            Held::Held => match value_of(world, DYNAMICS, &inputs.namespaces.dynamics)? {
                Some(_) => Ok(true),
                None => refuse("the population state is None"),
            },
        }
    })??;
    if present {
        return Ok(None);
    }
    new_dynamics_state(cell, host, config).map(Some)
}

/// The state a plan reads: a new one, or the world's.
fn state_in<'a>(new_state: &'a Option<V>, held: &'a Option<ShardRef<'_, ValueRow>>) -> Result<&'a V> {
    match (new_state, held) {
        (Some(state), _) => Ok(state),
        (None, Some(row)) => Ok(&row.0),
        (None, None) => refuse("the population state is None"),
    }
}

// ---- `queue_arrivals` ----------------------------------------------------------------------------

/// `admission_zone_ids` as given: a list or tuple of strs, or something the rule says is invalid.
#[derive(Clone, Debug)]
pub enum Alternates {
    NotGiven,
    Invalid,
    Zones(Vec<Arc<str>>),
}

/// `language` as given: a valid one, or a value whose validity the contract decides.
#[derive(Clone, Debug)]
pub enum Language {
    Valid(Arc<str>),
    Ask(V),
}

/// `person_context_overrides` as given: its items, or a value whose iteration raised the
/// `TypeError` or `ValueError` the rule turns into its text.
#[derive(Clone, Debug)]
pub enum Overrides {
    Items(Vec<V>),
    Caught,
}

/// The arguments of `queue_arrivals`, as the PyO3 crate read them: each Err is a refusal raised
/// where the rule reads that argument.
pub struct ArrivalArgs {
    /// The zone: None for a value no zone has.
    pub zone: Result<Option<Arc<str>>>,
    pub alternates: Result<Alternates>,
    /// None for no route; Some(None) for a value that is no route's ID.
    pub route_id: Option<Option<Arc<str>>>,
    /// The count, None for a value that is not an int.
    pub count: Result<Option<i64>>,
    /// None where not given.
    pub household_size: Option<Result<Option<i64>>>,
    /// None where not given; Some(Ok(None)) for an invalid one.
    pub mobility: Option<Result<Option<&'static str>>>,
    pub language: Option<Result<Language>>,
    pub overrides: Option<Result<Overrides>>,
    /// `model_configs.get("geometry_overlay", {})`: whether it is a dict, and for a dict whether its
    /// schema is 3 or 4 (or the refusal of reading it).
    pub rooms: (bool, Result<bool>),
    /// `model_configs.get("population")`, for a new state.
    pub population: Option<V>,
}

/// A prepared arrival.
#[derive(Clone)]
struct Prepared {
    item: Doc,
    agent_id: Arc<str>,
    household: Arc<str>,
    role: &'static str,
    care_needs: Vec<Arc<str>>,
    source: Arc<str>,
    completion: Option<EventRow>,
    admission: Option<Admission>,
}

/// Where a rule that writes first stops with its text, in the item's writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stop {
    /// `_schedule_arrival_approach_completion`, before it takes a sequence.
    BeforeSequence,
    /// The same, after it took one.
    AfterSequence,
    /// `_record_waiting_presence`, after the item joined the waiting.
    WaitingPresence,
}

/// The plan of `queue_arrivals`.
pub struct ArrivalsPlan {
    /// A state `_state` made, which the PyO3 crate puts into `model_state` before the writes.
    pub new_state: Option<V>,
    /// The rule's text once `_state` ran, raised after that state is in place, before any write.
    pub fails: Option<String>,
    zone: Arc<str>,
    prepared: Vec<Prepared>,
    contexts: Vec<(Arc<str>, V)>,
    summary: V,
    event_id: Arc<str>,
    due_minute: i64,
    approaching: bool,
    route_sources: Vec<Arc<str>>,
    pub admitted: i64,
    /// The rule's text after some of its writes: at this item, at this point.
    failure: Option<(usize, Stop, &'static str)>,
}

/// The plan's end once `_state` has made its state: the state goes in, then the text.
fn fails_after_state(new_state: Option<V>, text: impl Into<String>) -> ArrivalsPlan {
    ArrivalsPlan {
        new_state,
        fails: Some(text.into()),
        zone: Arc::from(""),
        prepared: Vec::new(),
        contexts: Vec::new(),
        summary: V::Null,
        event_id: Arc::from(""),
        due_minute: 0,
        approaching: false,
        route_sources: Vec::new(),
        admitted: 0,
        failure: None,
    }
}

/// `_generated_arrival_context`: the same fields as `model_dump(exclude_defaults, exclude_none)`.
fn generated_context(items: &[Prepared]) -> Result<Vec<(Arc<str>, V)>> {
    let mut households: Vec<(&str, Vec<&Prepared>)> = Vec::new();
    for item in items {
        match households.iter_mut().find(|(name, _)| *name == &*item.household) {
            Some((_, members)) => members.push(item),
            None => households.push((&item.household, vec![item])),
        }
    }
    households.sort_by(|first, second| first.0.as_bytes().cmp(second.0.as_bytes()));
    let mut contexts = Vec::with_capacity(items.len());
    for (household, mut members) in households {
        members.sort_by(|first, second| first.agent_id.as_bytes().cmp(second.agent_id.as_bytes()));
        let caregiver = members.iter().find(|item| item.role == "adult").map(|item| item.agent_id.clone());
        let mut care_for: Vec<Arc<str>> = Vec::new();
        if caregiver.is_some() {
            care_for = members.iter().filter(|item| matches!(item.role, "child" | "dependent")).map(|item| item.agent_id.clone()).collect();
            care_for.sort_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
        }
        if care_for.len() > 16 {
            return refuse("a household has more dependents than a context holds");
        }
        for item in members {
            let dependent = matches!(item.role, "child" | "dependent");
            let caregiver_ids: Vec<Arc<str>> = if dependent { caregiver.iter().cloned().collect() } else { Vec::new() };
            let is_caregiver = caregiver.as_deref() == Some(&*item.agent_id);
            let requires_review = dependent || !care_for.is_empty();
            let role_name = if item.role == "adult" && is_caregiver && !care_for.is_empty() {
                Some("caregiver")
            } else {
                match item.role {
                    "child" => Some("child"),
                    "dependent" => Some("dependent"),
                    "alone" => Some("alone"),
                    _ => None,
                }
            };
            let mut context = doc([("household_id", v_str(household))]);
            if !caregiver_ids.is_empty() {
                context.insert(Arc::from("caregiver_ids"), strings(&caregiver_ids));
            }
            if is_caregiver && !care_for.is_empty() {
                context.insert(Arc::from("care_for_ids"), strings(&care_for));
            }
            if requires_review {
                context.insert(Arc::from("requires_review"), V::Bool(true));
                context.insert(Arc::from("review_reasons"), V::List(vec![v_str("synthetic_arrival_care_relationship_requires_review")]));
            }
            if let Some(name) = role_name {
                context.insert(Arc::from("roles"), V::List(vec![v_str(name)]));
            }
            if item.role == "child" {
                context.insert(Arc::from("guardian_status"), v_str(if caregiver_ids.is_empty() { "unaccompanied" } else { "accompanied" }));
            }
            if !item.care_needs.is_empty() {
                context.insert(Arc::from("care_support_needs"), strings(&item.care_needs));
            }
            let sources = sorted_set(vec![Arc::from(SOURCE_ID), item.source.clone()]);
            for source in &sources {
                identifier(source)?;
            }
            context.insert(Arc::from("source_ids"), strings(&sources));
            contexts.push((item.agent_id.clone(), V::Map(context)));
        }
    }
    Ok(contexts)
}

/// The route of an arrival on an approach: its ID, minutes and the parts a pending arrival keeps.
struct Approach {
    route_id: Arc<str>,
    travel_minutes: i64,
    source_ids: Vec<Arc<str>>,
    walking_speed: f64,
    distance: f64,
}

/// The parts of an approach route a pending arrival keeps; its minutes are Unsupported where reading
/// them raised.
fn read_route(route: &ApproachRoute) -> Result<Approach> {
    let travel_minutes = route.travel_minutes.clone().map_err(|fault| Error::unsupported(fault.text.to_string()))?;
    Ok(Approach {
        route_id: route.route_id.clone(),
        travel_minutes,
        source_ids: route.source_ids.clone(),
        walking_speed: route.walking_speed,
        distance: route.distance,
    })
}

/// What the arrivals have checked before they read the world again.
struct Checked {
    zone: Arc<str>,
    alternates: Vec<Arc<str>>,
    route: Option<Approach>,
    count: i64,
    household_size: i64,
    mobility: &'static str,
    language: Arc<str>,
    care_by_index: HashMap<i64, Vec<Arc<str>>>,
}

/// The plan of `queue_arrivals`, through the event's row. A host error is what a contract raised.
pub fn plan_arrivals<C: WorldCell, H: Host>(cell: &C, host: &mut H, inputs: &Inputs, row: &EventRow, args: ArrivalArgs) -> std::result::Result<ArrivalsPlan, H::Error> {
    // The rule's checks, in its order, each with its text.
    let zone = cell.with(|world| -> Result<Arc<str>> {
        match args.zone? {
            Some(zone) if zone_known(world, &zone)? => Ok(zone),
            _ => failed("population arrival references an unknown zone"),
        }
    })??;
    let alternates = cell.with(|world| -> Result<Vec<Arc<str>>> {
        let invalid = || failed("population arrival alternate admission zones are invalid");
        let zones = match args.alternates? {
            Alternates::NotGiven => return Ok(Vec::new()),
            Alternates::Invalid => return invalid(),
            Alternates::Zones(zones) => zones,
        };
        let distinct: HashSet<&str> = zones.iter().map(|item| &**item).collect();
        if distinct.len() != zones.len() {
            return invalid();
        }
        for item in &zones {
            if !zone_known(world, item)? {
                return invalid();
            }
        }
        if zones.contains(&zone) {
            return invalid();
        }
        Ok(zones)
    })??;
    let route = match &args.route_id {
        None => None,
        Some(route_id) => {
            if !alternates.is_empty() {
                return Err(Error::failed("one arrival approach route cannot combine with alternate admission zones").into());
            }
            let routes = match &inputs.routes {
                Some(Ok(routes)) => routes,
                Some(Err(fault)) if fault.caught => return Err(Error::failed("population arrival approach route is invalid").into()),
                Some(Err(fault)) => return Err(fault.refusal.clone().into()),
                None => return Err(Error::unsupported("the approach routes were not read").into()),
            };
            let found = route_id.as_ref().and_then(|route_id| routes.iter().find(|route| route.route_id == *route_id));
            let Some(found) = found else {
                return Err(Error::failed("population arrival approach route is invalid").into());
            };
            if found.destination_zone_id != zone {
                return Err(Error::failed("population arrival approach route targets a different zone").into());
            }
            Some(read_route(found)?)
        }
    };
    if !alternates.is_empty() {
        let (is_dict, schema) = args.rooms;
        if !is_dict || !schema? {
            return Err(Error::failed("alternate room admission requires geometry_overlay schema_version=3 or 4").into());
        }
    }
    let count = match args.count? {
        Some(count) if (1..=MAX_ARRIVALS_PER_EVENT).contains(&count) => count,
        _ => return Err(Error::failed("population arrival count is outside its supported range").into()),
    };
    let household_size = match args.household_size {
        None => 1,
        Some(size) => match size? {
            Some(size) if (1..=12).contains(&size) => size,
            _ => return Err(Error::failed("population arrival household_size is outside its supported range").into()),
        },
    };
    let mobility = match args.mobility {
        None => "independent",
        Some(mobility) => match mobility? {
            Some(mobility) => mobility,
            None => return Err(Error::failed("population arrival mobility is invalid").into()),
        },
    };
    let language: Arc<str> = match args.language {
        None => Arc::from("ja"),
        Some(language) => match language? {
            Language::Valid(language) => language,
            Language::Ask(value) => {
                // The contract decides what Rust doesn't: its ValueError is the rule's text.
                let fields = V::Map(doc([("age_years", V::Int(30)), ("household_role", v_str("adult")), ("languages", V::List(vec![value]))]));
                return match host.contract(&Contract::PersonTraits, &fields) {
                    Ok(_) => Err(Error::unsupported("a language the contract takes and Rust does not").into()),
                    Err(error) if error.class() == ErrorClass::Value => Err(Error::failed("population arrival language is invalid").into()),
                    Err(error) => Err(error),
                };
            }
        },
    };
    let mut care_by_index: HashMap<i64, Vec<Arc<str>>> = HashMap::new();
    if let Some(overrides) = args.overrides {
        const CONTEXT_INVALID: &str = "population arrival person context is invalid";
        let items = match overrides? {
            Overrides::Caught => return Err(Error::failed(CONTEXT_INVALID).into()),
            Overrides::Items(items) => items,
        };
        let mut parsed = Vec::with_capacity(items.len());
        for item in &items {
            match host.contract(&Contract::ArrivalContext, item) {
                Ok(dump) => parsed.push(dump),
                // `except (TypeError, ValueError)`, past a refusal.
                Err(error) if matches!(error.class(), ErrorClass::Value | ErrorClass::Type) => return Err(Error::failed(CONTEXT_INVALID).into()),
                Err(error) => return Err(error),
            }
        }
        let mut indices = Vec::with_capacity(parsed.len());
        for item in &parsed {
            indices.push(kit::exact_int(field(item, "member_index")?)?);
        }
        let distinct: HashSet<i64> = indices.iter().copied().collect();
        if distinct.len() != indices.len() || indices.iter().any(|index| *index >= count) {
            return Err(Error::failed("population arrival person context index is invalid").into());
        }
        for (item, index) in parsed.iter().zip(indices) {
            let mut needs = Vec::new();
            for kind in list_of(field(item, "care_support_needs")?)? {
                let kind = text(kind)?;
                if !SUPPORT_KINDS.contains(&&*kind) {
                    return Err(Error::unsupported("a support kind is not known").into());
                }
                needs.push(kind);
            }
            care_by_index.insert(index, needs);
        }
    }
    let new_state = dynamics_or_new(cell, host, inputs, args.population.as_ref())?;
    let checked = Checked { zone, alternates, route, count, household_size, mobility, language, care_by_index };
    Ok(cell.with(|world| plan_arrivals_in(world, inputs, row, new_state, checked))??)
}

fn plan_arrivals_in(world: &World, inputs: &Inputs, row: &EventRow, new_state: Option<V>, checked: Checked) -> Result<ArrivalsPlan> {
    let Checked { zone, alternates, route, count, household_size, mobility, language, care_by_index } = checked;
    let held = value_of(world, DYNAMICS, &inputs.namespaces.dynamics)?;
    let (reserved, fails) = {
        let state = state_in(&new_state, &held)?;
        let pending = list_of(field(state, "pending_arrivals")?)?;
        let pending_count = pending.len() as i64;
        let agents = world.rows()?.agents.len() as i64;
        let mut known: HashSet<Arc<str>> = HashSet::new();
        for id in list_of(field(state, "initial_agent_ids")?)? {
            known.insert(text(id)?);
        }
        let traits_map = map_of(field(state, "profile_traits")?)?;
        known.extend(traits_map.keys().cloned());
        let fails = if pending_count + count > MAX_PENDING_ARRIVALS {
            Some("population arrival exceeds the pending capacity bound")
        } else if agents + pending_count + count > 10_000 {
            Some("population arrival exceeds the 10000-resident runtime bound")
        } else if known.len() as i64 + count > 10_000 {
            Some("population arrival exceeds the 10000-profile history bound")
        } else {
            None
        };
        let mut reserved: HashSet<Arc<str>> = world.rows()?.agents.iter().map(|(id, _)| id.clone()).collect();
        if fails.is_none() {
            for item in pending {
                reserved.insert(text(field(field(item, "profile")?, "id")?)?);
            }
            reserved.extend(traits_map.keys().cloned());
        }
        (reserved, fails)
    };
    drop(held);
    if let Some(text) = fails {
        return Ok(fails_after_state(new_state, text));
    }
    let mut reserved = reserved;
    identifier(&row.event_id)?;
    // `f"{world.seed}"` and the int itself: a seed of any size, past 64 bits too.
    let seed = world.scalars()?.seed()?.clone();
    let seed_value = seed.small.map_or_else(|| V::Big(Box::from(&*seed.text)), V::Int);
    let schema_version = world.schema_version()?;
    let hydrated = schema_version == 6;
    let event_digest = sha_hex(&row.event_id);
    let event_digest = &event_digest[..20];
    let context_source = row.source_id.clone().unwrap_or_else(|| Arc::from(SOURCE_ID));
    let mut prepared = Vec::with_capacity(count as usize);
    for index in 0..count {
        let digest = sha_hex(&format!("{}\0{}\0{index}", seed.text, row.event_id));
        let byte = |start: usize| -> i64 { i64::from_str_radix(&digest[start..start + 2], 16).unwrap_or(0) };
        let agent_id: Arc<str> = Arc::from(format!("arrival-{}", &digest[..28]));
        if !reserved.insert(agent_id.clone()) {
            return Ok(fails_after_state(new_state, "population arrival generated a duplicate stable ID"));
        }
        let group_id: Arc<str> = Arc::from(format!("household-{event_digest}-{:03}", index / household_size));
        let age_years = 5 + byte(0) % 79;
        let role = if age_years < 18 {
            "child"
        } else if age_years >= 75 {
            "dependent"
        } else if household_size == 1 {
            "alone"
        } else {
            "adult"
        };
        let traits = V::Map(doc([
            ("age_years", V::Int(age_years)),
            ("household_id", V::Str(group_id.clone())),
            ("household_role", v_str(role)),
            ("languages", V::List(vec![V::Str(language.clone())])),
            ("accessibility_needs", V::List(Vec::new())),
            ("medical_support", V::Bool(false)),
            ("source_ids", V::List(Vec::new())),
        ]));
        let level = |start: usize| -> f64 { byte(start) as f64 / 255.0 * 0.35 };
        let mut initial = doc([("toilet_urgency", V::Float(level(2))), ("hunger", V::Float(level(4))), ("fatigue", V::Float(level(6)))]);
        if hydrated {
            initial.insert(Arc::from("hydration_need"), V::Float(level(8)));
        }
        let profile = V::Map(doc([
            ("id", V::Str(agent_id.clone())),
            ("group_id", V::Str(group_id.clone())),
            ("initial_zone_id", V::Str(zone.clone())),
            ("mobility", v_str(mobility)),
            ("support_need", V::Float(byte(10) as f64 / 255.0 * 0.5)),
            ("stress_sensitivity", V::Float(byte(12) as f64 / 255.0)),
            ("rng_key", V::Str(Arc::from(format!("arrival:{}", &digest[..24])))),
            ("initial_needs", V::Map(initial)),
            (
                "provenance",
                V::Map(doc([
                    ("source_type", v_str("assumption")),
                    ("source_id", v_str(SOURCE_ID)),
                    ("event_id", V::Str(row.event_id.clone())),
                    ("seed", seed_value.clone()),
                ])),
            ),
        ]));
        let care_needs = care_by_index.get(&index).cloned().unwrap_or_default();
        let mut item = doc([
            ("profile", profile),
            ("traits", traits),
            ("member_index", V::Int(index)),
            ("care_support_needs", strings(&care_needs)),
            ("context_source_id", V::Str(context_source.clone())),
            ("zone_id", V::Str(zone.clone())),
            ("admission_zone_ids", strings(&alternates)),
            ("created_minute", V::Int(row.due_minute)),
            ("event_id", V::Str(row.event_id.clone())),
        ]);
        if let Some(route) = &route {
            item.insert(Arc::from("approach_route_id"), V::Str(route.route_id.clone()));
            item.insert(Arc::from("approach_started_minute"), V::Int(row.due_minute));
            item.insert(Arc::from("approach_due_minute"), V::Int(row.due_minute + route.travel_minutes));
        }
        prepared.push(Prepared { item, agent_id, household: group_id, role, care_needs, source: context_source.clone(), completion: None, admission: None });
    }
    let contexts = generated_context(&prepared)?;
    let presence = presence_state(world, &inputs.namespaces.presence)?;
    let events = Events::of(world)?;
    let mut sequence = events.next_sequence()?;
    let mut places = Places::of(world)?;
    let mut admitted = 0;
    let route_sources = route.as_ref().map_or(Vec::new(), |route| route.source_ids.clone());
    let zone_list: Vec<Arc<str>> = std::iter::once(zone.clone()).chain(alternates.iter().cloned()).collect();
    let mut failure = None;
    // `agent_id in raw["people"]` of `_record_waiting_presence`, whose text stops the writes there.
    let waiting_has_record = |item: &Prepared| -> Result<bool> {
        if !presence {
            return Ok(false);
        }
        let ledger = value_of(world, PRESENCE, &inputs.namespaces.presence)?.ok_or_else(|| Error::unsupported("no presence"))?;
        Ok(map_of(field(&ledger.0, "people")?)?.contains_key(&*item.agent_id))
    };
    // What `_record_waiting_presence` validates of the IDs it will write.
    let check_waiting_ids = |item: &Prepared| -> Result<()> {
        if !presence {
            return Ok(());
        }
        identifier(&zone)?;
        identifier(&row.event_id)?;
        identifier(&item.household)?;
        for source in &route_sources {
            identifier(source)?;
        }
        Ok(())
    };
    for (index, item) in prepared.iter_mut().enumerate() {
        if let Some(route) = &route {
            let due = row.due_minute + route.travel_minutes;
            if due < events.duration() {
                match plan_completion(world, inputs, row, item, route, due, sequence)? {
                    Ok(completion) => {
                        sequence += 1;
                        item.item.insert(Arc::from("approach_completion_event_id"), V::Str(completion.event_id.clone()));
                        item.completion = Some(completion);
                    }
                    Err((stop, text)) => {
                        failure = Some((index, stop, text));
                        break;
                    }
                }
            }
            if waiting_has_record(item)? {
                failure = Some((index, Stop::WaitingPresence, WAITING_HAS_RECORD));
                break;
            }
            check_waiting_ids(item)?;
            continue;
        }
        let mut decision = None;
        for candidate in &zone_list {
            if places.has_room(world, inputs, candidate)? {
                decision = Some((candidate.clone(), None));
                break;
            }
            if let Some(found) = places.available_room(world, inputs, candidate)? {
                decision = Some((candidate.clone(), Some(found)));
                break;
            }
        }
        match decision {
            Some((target, room)) => {
                check_presence_ids(&row.event_id, &item.agent_id, Some(&target), Some(&item.household), SOURCE_ID)?;
                places.add(&target);
                if let Some((room_id, point)) = &room {
                    places.take_room(inputs, room_id, *point);
                    let overlay = value_of(world, OVERLAY, &inputs.namespaces.overlay)?.ok_or_else(|| Error::unsupported("no overlay"))?;
                    if !matches!(kit::get(&overlay.0, "room_admissions")?, V::Map(_)) {
                        return refuse("a room admission has no admissions to join");
                    }
                }
                admitted += 1;
                let document = V::Map(item.item.clone());
                let agent = build_agent(field(&document, "profile")?, &target, row.due_minute, schema_version)?;
                item.admission = Some(Admission { item: document, zone: target, room, agent });
            }
            None => {
                if waiting_has_record(item)? {
                    failure = Some((index, Stop::WaitingPresence, WAITING_HAS_RECORD));
                    break;
                }
                check_waiting_ids(item)?;
            }
        }
    }
    if prepared.iter().any(|item| item.admission.is_some()) {
        let zones: Vec<&str> = zone_list.iter().map(|zone| &**zone).collect();
        check_admission_paths(world, inputs, &zones)?;
    }
    let summary = V::Map(doc([
        ("event_id", V::Str(row.event_id.clone())),
        ("zone_id", V::Str(zone.clone())),
        ("requested_count", V::Int(count)),
        ("admitted_count", V::Int(admitted)),
        ("waiting_count", V::Int(count - admitted)),
        ("minute", V::Int(row.due_minute)),
    ]));
    Ok(ArrivalsPlan {
        new_state,
        fails: None,
        zone,
        prepared,
        contexts,
        summary,
        event_id: row.event_id.clone(),
        due_minute: row.due_minute,
        approaching: route.is_some(),
        route_sources,
        admitted,
        failure,
    })
}

/// A completion event, or the text the rule stops with and where.
type Completion = std::result::Result<EventRow, (Stop, &'static str)>;

/// Whether the approved program of `effect_program_v1` holds this effect as `add_evacuees`.
fn adds_evacuees(effects: Option<&EffectProgramRow>, program_id: &str, effect_id: &str) -> bool {
    effects.and_then(|effects| effects.programs.get(program_id)).is_some_and(|program| {
        program.effect_ids.iter().any(|id| &**id == effect_id) && program.operators.get(effect_id).is_some_and(|op| &**op == "add_evacuees")
    })
}

/// `_schedule_arrival_approach_completion`, as a row, or the text it stops with and where.
fn plan_completion(world: &World, inputs: &Inputs, arrival: &EventRow, item: &Prepared, route: &Approach, due: i64, sequence: i64) -> Result<Completion> {
    let program_id = kit::payload(arrival, "program_id").cloned().unwrap_or(V::Null);
    let effect_id = kit::payload(arrival, "effect_id").cloned().unwrap_or(V::Null);
    if !matches!(program_id, V::Str(_) | V::Null) || !matches!(effect_id, V::Str(_) | V::Null) {
        return refuse("a program's IDs are something else");
    }
    // `world.model_state.get("effect_program_v1", {}).get("programs", {}).get(program_id)`.
    let approved = match (&program_id, &effect_id) {
        (V::Str(program_id), V::Str(effect_id)) => {
            let effects = typed_of::<EffectProgramRow>(world, EFFECT_PROGRAM, &inputs.namespaces.effect_program)?;
            adds_evacuees(effects.as_deref(), program_id, effect_id)
        }
        _ => false,
    };
    if !approved {
        return Ok(Err((Stop::BeforeSequence, "arrival approach is not part of an approved EffectProgram")));
    }
    let Some(V::Map(marker)) = kit::provenance(arrival, "effect_program_v1") else {
        return Ok(Err((Stop::AfterSequence, "arrival event lacks its approved EffectProgram provenance")));
    };
    // `derived_effect_marker`: the arrival's guard is not the approach's (#89).
    let mut derived = marker.clone();
    derived.shift_remove("guard");
    derived.insert(Arc::from("phase"), v_str("derived"));
    let source = route.source_ids.first().cloned().ok_or_else(|| Error::unsupported("a route has no sources"))?;
    let completion = kit::new_event(kit::NewEvent {
        event_id: Arc::from(format!("arrival-approach-complete-{sequence}")),
        kind: EventKind::Completion,
        due_minute: due,
        priority: 0,
        sequence,
        event_type: Arc::from("population_approach_complete"),
        payload: doc([
            ("program_id", program_id),
            ("effect_id", effect_id),
            ("agent_id", V::Str(item.agent_id.clone())),
            ("arrival_event_id", V::Str(arrival.event_id.clone())),
            ("route_id", V::Str(route.route_id.clone())),
            ("zone_id", item.item.get("zone_id").cloned().unwrap_or(V::Null)),
        ]),
        source_id: Some(source),
        causation_id: Some(arrival.event_id.clone()),
        provenance: doc([
            ("effect_program_v1", V::Map(derived)),
            (
                "arrival_approach",
                V::Map(doc([
                    ("route_id", V::Str(route.route_id.clone())),
                    ("source_ids", strings(&route.source_ids)),
                    ("walking_speed_m_per_min", V::Float(route.walking_speed)),
                    ("distance_m", V::Float(route.distance)),
                ])),
            ),
        ]),
    })?;
    Ok(Ok(completion))
}

fn write_waiting_presence(world: &World, inputs: &Inputs, prepared: &Prepared, plan: &ArrivalsPlan) -> Result<()> {
    if !is_dict_namespace(world, PRESENCE, &inputs.namespaces.presence)? {
        return Ok(());
    }
    let (status, action) = if plan.approaching { ("approaching", "approaching") } else { ("awaiting_admission", "waiting") };
    let person = Person {
        agent_id: prepared.agent_id.clone(),
        household_id: Some(prepared.household.clone()),
        status: Arc::from(status),
        since_minute: plan.due_minute,
        transition_zone_id: Some(plan.zone.clone()),
        return_zone_id: None,
        expected_return_minute: None,
        archive_path: None,
        source_event_ids: vec![plan.event_id.clone()],
    };
    let digest = sha_hex(&format!("{}\0{}\0{action}", plan.event_id, prepared.agent_id));
    let mut sources = vec![Arc::from(SOURCE_ID)];
    sources.extend(plan.route_sources.iter().cloned());
    let transition = Transition {
        transition_id: format!("presence-{}", &digest[..24]),
        event_id: plan.event_id.clone(),
        agent_id: prepared.agent_id.clone(),
        minute: plan.due_minute,
        action,
        from_zone_id: None,
        to_zone_id: Some(plan.zone.clone()),
        from_household_id: None,
        to_household_id: Some(prepared.household.clone()),
        causation_id: None,
        source_ids: sorted_set(sources),
        state_snapshot_hash: None,
    };
    write_presence(world, &person, &transition)
}

/// A duplicate event as the `RuntimeError` it never is where the plan took a new sequence.
fn plain(error: WriteError) -> Error {
    match error {
        WriteError::Error(error) => error,
        WriteError::Duplicate(text) => Error::Runtime(text),
    }
}

fn append_pending(world: &World, item: &Doc) -> Result<()> {
    let mut row = world.row_mut::<ValueRow>(DYNAMICS)?;
    list_mut(field_mut(&mut row.0, "pending_arrivals")?)?.push(V::Map(item.clone()));
    Ok(())
}

/// The writes of `queue_arrivals`, once the PyO3 crate has put a new state into `model_state`:
/// how many were admitted, or the rule's text after the writes it makes first.
pub fn write_arrivals(world: &World, inputs: &Inputs, plan: &ArrivalsPlan) -> Result<i64> {
    if value_of(world, DYNAMICS, &inputs.namespaces.dynamics)?.is_none() {
        return Err(Error::Runtime("the population state left".to_owned()));
    }
    {
        let mut row = world.row_mut::<ValueRow>(DYNAMICS)?;
        let state = map_mut(&mut row.0)?;
        let contexts = map_mut(state.entry(Arc::from("profile_context")).or_insert_with(|| V::Map(Doc::new())))?;
        for (agent_id, context) in &plan.contexts {
            contexts.insert(agent_id.clone(), context.clone());
        }
    }
    let events = Events::of(world)?;
    for (index, prepared) in plan.prepared.iter().enumerate() {
        // The rule's text stops its writes at this item, at this point.
        let stops_at = |at: Stop| plan.failure.filter(|(place, stop, _)| *place == index && *stop == at).map(|(_, _, text)| text);
        {
            let traits = prepared.item.get("traits").ok_or_else(|| Error::Runtime("an item lost its traits".to_owned()))?;
            let mut row = world.row_mut::<ValueRow>(DYNAMICS)?;
            map_mut(field_mut(&mut row.0, "profile_traits")?)?.insert(prepared.agent_id.clone(), traits.clone());
        }
        if plan.approaching {
            if let Some(text) = stops_at(Stop::BeforeSequence) {
                return failed(text);
            }
            if let Some(text) = stops_at(Stop::AfterSequence) {
                events.allocate_sequence()?;
                return failed(text);
            }
            if let Some(completion) = &prepared.completion {
                events.allocate_sequence()?;
                events.schedule(completion.clone()).map_err(plain)?;
            }
            append_pending(world, &prepared.item)?;
            if let Some(text) = stops_at(Stop::WaitingPresence) {
                return failed(text);
            }
            write_waiting_presence(world, inputs, prepared, plan)?;
            continue;
        }
        match &prepared.admission {
            Some(admission) => {
                write_admission(world, inputs, admission, plan.due_minute)?;
            }
            None => {
                append_pending(world, &prepared.item)?;
                if let Some(text) = stops_at(Stop::WaitingPresence) {
                    return failed(text);
                }
                write_waiting_presence(world, inputs, prepared, plan)?;
            }
        }
    }
    let waiting = {
        let mut row = world.row_mut::<ValueRow>(DYNAMICS)?;
        list_mut(field_mut(&mut row.0, "arrivals")?)?.push(plan.summary.clone());
        list_of(field(&row.0, "pending_arrivals")?)?.len()
    };
    add_metric(world, "population_arrival_count", plan.admitted as usize)?;
    set_metric(world, "population_waiting_count", waiting as f64)?;
    Ok(plan.admitted)
}

// ---- `apply_arrival_approach_completion` ---------------------------------------------------------

/// `apply_arrival_approach_completion`.
pub fn apply_approach_completion(world: &World, inputs: &Inputs, row: &EventRow) -> Result<()> {
    let V::Map(fields) = &row.payload else {
        return refuse("a payload is not a mapping");
    };
    let expected = ["program_id", "effect_id", "agent_id", "arrival_event_id", "route_id", "zone_id"];
    if &*row.event_type != "population_approach_complete" || fields.len() != expected.len() || expected.iter().any(|key| !fields.contains_key(*key)) {
        return failed("arrival approach completion payload fields do not match");
    }
    let (Some(V::Str(program_id)), Some(V::Str(effect_id)), Some(V::Str(agent_id)), Some(V::Str(arrival_id)), Some(V::Str(route_id)), Some(V::Str(zone_id))) = (
        fields.get("program_id"),
        fields.get("effect_id"),
        fields.get("agent_id"),
        fields.get("arrival_event_id"),
        fields.get("route_id"),
        fields.get("zone_id"),
    ) else {
        return refuse("a payload value is not a str");
    };
    let approved = {
        let effects = typed_of::<EffectProgramRow>(world, EFFECT_PROGRAM, &inputs.namespaces.effect_program)?;
        adds_evacuees(effects.as_deref(), program_id, effect_id)
    };
    if !approved || row.causation_id.as_deref() != Some(&**arrival_id) {
        return failed("arrival approach completion is outside its approved program");
    }
    const MISMATCH: &str = "arrival approach completion does not match pending arrival";
    let index = {
        let Some(state) = value_of(world, DYNAMICS, &inputs.namespaces.dynamics)?.filter(|row| matches!(row.0, V::Map(_))) else {
            return failed(MISMATCH);
        };
        let pending: &[V] = match kit::get(&state.0, "pending_arrivals")? {
            V::Null => &[],
            pending => list_of(pending)?,
        };
        let mut found = None;
        for (index, item) in pending.iter().enumerate() {
            if equals_text(field(field(item, "profile")?, "id")?, agent_id) {
                found = Some(index);
                break;
            }
        }
        let Some(index) = found else {
            return failed(MISMATCH);
        };
        let item = &pending[index];
        if !equals_text(kit::get(item, "approach_completion_event_id")?, &row.event_id)
            || !equals_text(kit::get(item, "approach_route_id")?, route_id)
            || !equals_text(kit::get(item, "event_id")?, arrival_id)
            || !equals_text(kit::get(item, "zone_id")?, zone_id)
            || !equals_int(kit::get(item, "approach_due_minute")?, row.due_minute)
            || !matches!(kit::get(item, "approach_completed_minute")?, V::Null)
        {
            return failed(MISMATCH);
        }
        index
    };
    const PROVENANCE: &str = "arrival approach route provenance does not match";
    let routes = inputs.routes()?;
    let Some(route) = routes.iter().find(|candidate| candidate.route_id == *route_id) else {
        return failed(PROVENANCE);
    };
    if route.destination_zone_id != *zone_id {
        return failed(PROVENANCE);
    }
    if !row.source_id.as_ref().is_some_and(|source| route.source_ids.contains(source)) {
        return failed(PROVENANCE);
    }
    let route = read_route(route)?;
    // `_record_approach_completed`, which raises after the two writes to the pending arrival.
    let mut record = None;
    let mut no_presence = false;
    if let Some(raw) = value_of(world, PRESENCE, &inputs.namespaces.presence)?.filter(|row| matches!(row.0, V::Map(_))) {
        let previous = match kit::get(&raw.0, "people")? {
            V::Null => &V::Null,
            people => kit::get(people, agent_id)?,
        };
        if !matches!(previous, V::Map(_)) || !equals_text(kit::get(previous, "status")?, "approaching") {
            no_presence = true;
        } else {
            let person = Person::parse(previous)?;
            identifier(&row.event_id)?;
            identifier(agent_id)?;
            identifier(zone_id)?;
            optional_identifier(row.causation_id.as_deref())?;
            for source in &route.source_ids {
                identifier(source)?;
            }
            record = Some(person);
        }
    }
    kit::writing((|| {
        {
            let mut state = world.row_mut::<ValueRow>(DYNAMICS)?;
            let pending = list_mut(field_mut(&mut state.0, "pending_arrivals")?)?;
            let item = map_mut(&mut pending[index])?;
            if item.shift_remove("approach_completion_event_id").is_none() {
                return Err(Error::Key("approach_completion_event_id".to_owned()));
            }
            item.insert(Arc::from("approach_completed_minute"), V::Int(row.due_minute));
        }
        if no_presence {
            return failed("arrival approach has no matching presence state");
        }
        let Some(mut person) = record else {
            return Ok(());
        };
        person.status = Arc::from("awaiting_admission");
        person.since_minute = row.due_minute;
        let digest = sha_hex(&format!("{}\0{agent_id}\0approach-completed", row.event_id));
        let mut sources = vec![Arc::from(SOURCE_ID)];
        sources.extend(route.source_ids.iter().cloned());
        let transition = Transition {
            transition_id: format!("presence-{}", &digest[..24]),
            event_id: row.event_id.clone(),
            agent_id: agent_id.clone(),
            minute: row.due_minute,
            action: "approach_completed",
            from_zone_id: None,
            to_zone_id: Some(zone_id.clone()),
            from_household_id: None,
            to_household_id: person.household_id.clone(),
            causation_id: row.causation_id.clone(),
            source_ids: sorted_set(sources),
            state_snapshot_hash: None,
        };
        write_presence(world, &person, &transition)
    })())
}

// ---- `schedule_population_retry` -----------------------------------------------------------------

/// The plan of a retry: the row needs the sequence, which is the next one when it is written.
pub struct Retry {
    row: EventRow,
    program: Option<Arc<str>>,
}

/// The plan of `schedule_population_retry`: Err(text) is the rule's `EventSuppressed`.
pub fn plan_retry(world: &World, held: &Held, row: &EventRow, id_prefix: &str, subject: &str) -> Result<std::result::Result<Retry, String>> {
    let events = Events::of(world)?;
    let due = world.minute_index()? + 1;
    if due >= events.duration() {
        return Ok(Err(format!("{subject} could not complete before the scenario ended")));
    }
    let retry_count = match kit::provenance(row, "population_retry") {
        Some(V::Map(retry)) => match retry.get("count") {
            None => 0,
            Some(V::Int(count)) => *count,
            // `isinstance(True, int)`, which a retry count never is.
            Some(V::Bool(_)) => return refuse("a retry count is a bool"),
            Some(_) => MAX_RETRIES,
        },
        _ => 0,
    };
    if retry_count >= MAX_RETRIES {
        return Ok(Err(format!("{subject} reached the bounded deferral limit")));
    }
    let sequence = events.next_sequence()?;
    let digest = sha_hex(&format!("{}:{sequence}", row.event_id));
    let next_id: Arc<str> = Arc::from(format!("{id_prefix}-{}", &digest[..24]));
    let V::Map(mut record) = row.provenance.clone() else {
        return refuse("provenance is not a mapping");
    };
    record.insert(Arc::from("deferred_from"), V::Str(row.event_id.clone()));
    record.insert(Arc::from("population_retry"), V::Map(doc([("count", V::Int(retry_count + 1))])));
    let mut program = None;
    if let Some(V::Map(compiler)) = record.get("effect_program_v1") {
        if let Some(effects) = typed_of::<EffectProgramRow>(world, EFFECT_PROGRAM, held)? {
            let (Some(V::Str(program_id)), Some(V::Str(effect_id))) = (compiler.get("program_id"), compiler.get("effect_id")) else {
                return refuse("a program's marker is not two strs");
            };
            if effects.programs.get(&**program_id).is_some_and(|found| found.effect_ids.contains(effect_id)) {
                program = Some(program_id.clone());
            }
        }
    }
    let V::Map(fields) = row.payload.clone() else {
        return refuse("a payload is not a mapping");
    };
    let retry = kit::new_event(kit::NewEvent {
        event_id: next_id,
        kind: EventKind::Endogenous,
        due_minute: due,
        priority: row.priority,
        sequence,
        event_type: row.event_type.clone(),
        payload: fields,
        source_id: row.source_id.clone(),
        causation_id: Some(row.event_id.clone()),
        provenance: record,
    })?;
    Ok(Ok(Retry { row: retry, program }))
}

fn write_retry(world: &World, plan: Retry) -> Result<()> {
    let events = Events::of(world)?;
    events.allocate_sequence()?;
    if let Some(program_id) = &plan.program {
        let mut effects = world.row_mut::<EffectProgramRow>(EFFECT_PROGRAM)?;
        let program = effects.programs.get_mut(&**program_id).ok_or_else(|| Error::Runtime("the program left".to_owned()))?;
        program.scheduled_event_ids.push(plan.row.event_id.clone());
    }
    events.schedule(plan.row).map_err(plain)
}

/// `schedule_population_retry`: Err(EventSuppressed) where it can't.
pub fn schedule_retry(world: &World, held: &Held, row: &EventRow, id_prefix: &str, subject: &str) -> Result<()> {
    match plan_retry(world, held, row, id_prefix, subject)? {
        Ok(plan) => kit::writing(write_retry(world, plan)),
        Err(text) => Err(Error::suppressed(text)),
    }
}

// ---- `queue_departures` --------------------------------------------------------------------------

/// The arguments of `queue_departures`, as the PyO3 crate read them.
pub struct DepartureArgs {
    pub zone: Result<Option<Arc<str>>>,
    pub count: Result<Option<i64>>,
    /// None where not given; Some(None) for a value that is not a bool.
    pub include_initial: Option<Option<bool>>,
    pub population: Option<V>,
}

/// What the plan of `queue_departures` found. Each carries the state `_state` made, if it made
/// one, which goes into `model_state` first.
pub enum Departures {
    /// The rule's text: the state goes in, then the text.
    Fails { new_state: Option<V>, text: String },
    /// Someone selected is busy: the state goes in, the retry is scheduled, and the departure waits.
    Busy { new_state: Option<V> },
    /// These leave.
    Leave { new_state: Option<V>, selected: Vec<Arc<str>> },
}

/// `_arrival_minute`.
fn arrival_minute(state: &V, agent_id: &str) -> Result<i64> {
    for record in list_of(field(state, "arrivals")?)? {
        if equals_text(kit::get(record, "agent_id")?, agent_id) {
            let fields = map_of(record)?;
            let value = match fields.get("admitted_minute") {
                Some(value) => value,
                None => fields.get("created_minute").unwrap_or(&V::Int(0)),
            };
            return kit::exact_int(value);
        }
    }
    Ok(0)
}

/// What `_remove_information_state` and `_archive_person` ask of the information state.
fn check_information(world: &World, inputs: &Inputs) -> Result<()> {
    if let Some(info) = typed_of::<InformationRow>(world, INFORMATION, &inputs.namespaces.information)? {
        for key in info.contacts.keys() {
            pair_of(key)?;
        }
    }
    Ok(())
}

fn check_relationship_keys(world: &World, inputs: &Inputs) -> Result<()> {
    if let Some(network) = typed_of::<SocialNetworkRow>(world, SOCIAL_NETWORK, &inputs.namespaces.social_network)? {
        for key in network.relationships.map.keys() {
            pair_of(key)?;
        }
    }
    if let Some(body) = typed_of::<BodyRow>(world, BODY, &inputs.namespaces.body)? {
        for key in body.relationships.map.keys() {
            pair_of(key)?;
        }
    }
    if let Some(runtime) = typed_of::<SocialRow>(world, SOCIAL_RUNTIME, &inputs.namespaces.social_runtime)? {
        for key in runtime.contacts.keys() {
            pair_of(key)?;
        }
    }
    Ok(())
}

/// Each pending event has a row: the cancellations read rows.
fn check_queue_rows(world: &World) -> Result<()> {
    let queue = world.queue()?;
    for (_, serial) in queue.ordered() {
        if queue.slot(*serial).row.is_none() {
            return refuse(NO_ROW);
        }
    }
    Ok(())
}

/// The plan of `queue_departures` through the event's row. A host error is what a contract raised.
pub fn plan_departures<C: WorldCell, H: Host>(cell: &C, host: &mut H, inputs: &Inputs, row: &EventRow, args: DepartureArgs) -> std::result::Result<Departures, H::Error> {
    let (zone, count) = cell.with(|world| -> Result<(Arc<str>, i64)> {
        let zone = match args.zone? {
            Some(zone) if zone_known(world, &zone)? => zone,
            _ => return failed("population departure references an unknown zone"),
        };
        let count = match args.count? {
            Some(count) if (1..=MAX_ARRIVALS_PER_EVENT).contains(&count) => count,
            _ => return failed("population departure count is outside its supported range"),
        };
        Ok((zone, count))
    })??;
    let include_initial = match args.include_initial {
        None => false,
        Some(Some(flag)) => flag,
        Some(None) => return Err(Error::failed("population departure include_initial must be boolean").into()),
    };
    let new_state = dynamics_or_new(cell, host, inputs, args.population.as_ref())?;
    Ok(cell.with(|world| plan_departures_in(world, inputs, row, new_state, &zone, count, include_initial))??)
}

fn plan_departures_in(world: &World, inputs: &Inputs, row: &EventRow, new_state: Option<V>, zone: &str, count: i64, include_initial: bool) -> Result<Departures> {
    let held = value_of(world, DYNAMICS, &inputs.namespaces.dynamics)?;
    let (keyed, busy) = {
        let state = state_in(&new_state, &held)?;
        let mut initial: HashSet<Arc<str>> = HashSet::new();
        for id in list_of(field(state, "initial_agent_ids")?)? {
            initial.insert(text(id)?);
        }
        let rows = world.rows()?;
        let mut keyed = Vec::new();
        for (id, agent) in rows.agents.iter() {
            if (include_initial || !initial.contains(id)) && &*agent.zone_id == zone {
                let minute = if initial.contains(id) { -1 } else { arrival_minute(state, id)? };
                keyed.push((minute, id.clone()));
            }
        }
        keyed.sort_by(|first, second| (first.0, first.1.as_bytes()).cmp(&(second.0, second.1.as_bytes())));
        let busy = keyed.iter().take(count as usize).any(|(_, id)| {
            rows.agents.get(id).is_none_or(|agent| !matches!(agent.activity, Activity::Idle | Activity::Resting | Activity::Sleeping) || agent.support_request_id.is_some())
        });
        (keyed, busy)
    };
    drop(held);
    if (keyed.len() as i64) < count {
        return Ok(Departures::Fails { new_state, text: format!("only {} admitted dynamic residents are available to leave", keyed.len()) });
    }
    if busy {
        return Ok(Departures::Busy { new_state });
    }
    let selected: Vec<Arc<str>> = keyed.into_iter().take(count as usize).map(|(_, id)| id).collect();
    identifier(&row.event_id)?;
    if row.due_minute < 0 {
        return refuse("a minute before the Run");
    }
    presence_state(world, &inputs.namespaces.presence)?;
    for id in &selected {
        identifier(id)?;
    }
    check_information(world, inputs)?;
    check_relationship_keys(world, inputs)?;
    check_queue_rows(world)?;
    check_waiting_for_admission(world, inputs)?;
    Ok(Departures::Leave { new_state, selected })
}

/// An event to settle as SUPPRESSED, with its detail.
pub type Cancel = (Arc<EventRow>, String);

fn event_names_agent(row: &EventRow, agent_id: &str) -> bool {
    ["agent_id", "sender_agent_id", "recipient_agent_id", "participant_agent_id"]
        .iter()
        .any(|key| matches!(kit::payload(row, key), Some(V::Str(value)) if &**value == agent_id))
}

/// `_cancel_agent_events`, planned on the queue as it is: the events each person's departure
/// suppresses, which no one before took.
fn cancels_of(pending: &[Arc<EventRow>], taken: &mut HashSet<Arc<str>>, agent_id: &str, withdrawn: &HashSet<Arc<str>>, cancels: &mut Vec<Cancel>) {
    let detail = format!("Agent {agent_id} departed before the event was due");
    for row in pending {
        if !taken.contains(&row.event_id) && event_names_agent(row, agent_id) {
            taken.insert(row.event_id.clone());
            cancels.push((row.clone(), detail.clone()));
        }
    }
    let detail = format!("Claim withdrawn after Agent {agent_id} departed");
    for row in pending {
        if taken.contains(&row.event_id) {
            continue;
        }
        let from_agent = &*row.event_type == "information_rumor" && matches!(kit::payload(row, "origin_agent_id"), Some(V::Str(origin)) if &**origin == agent_id);
        let claim = matches!(kit::payload(row, "message_id"), Some(V::Str(message)) if withdrawn.contains(message));
        if from_agent || claim {
            taken.insert(row.event_id.clone());
            cancels.push((row.clone(), detail.clone()));
        }
    }
}

/// A tie of `social_network_v1` as its tree holds it.
fn relationship_value(record: &Relationship) -> V {
    V::Map(doc([
        ("contact_count", V::Int(record.contact_count)),
        ("conversation_count", V::Int(record.conversation_count)),
        ("cooperation_count", V::Int(record.cooperation_count)),
        ("trust", record.trust.map_or(V::Null, V::Float)),
        ("shared_message_ids", strings(&record.shared_message_ids)),
        ("last_contact_minute", record.last_contact_minute.map_or(V::Null, V::Int)),
        ("last_interaction", optional(&record.last_interaction)),
    ]))
}

/// The keys of a map of pairs that name this person, sorted.
fn pairs_naming<'a, T: 'a>(keys: impl Iterator<Item = (&'a Arc<str>, &'a T)>, agent_id: &str) -> Result<Vec<Arc<str>>> {
    let mut found = Vec::new();
    for (key, _) in keys {
        if names_person(key, agent_id)? {
            found.push(key.clone());
        }
    }
    found.sort_unstable_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    Ok(found)
}

/// `remove_agent` of social_network: the ties, popped in key order, as the departure keeps them.
fn remove_social_relationships(world: &World, inputs: &Inputs, agent_id: &str) -> Result<Vec<V>> {
    let keys = match typed_of::<SocialNetworkRow>(world, SOCIAL_NETWORK, &inputs.namespaces.social_network)? {
        None => return Ok(Vec::new()),
        Some(network) => pairs_naming(network.relationships.map.iter(), agent_id)?,
    };
    let mut archived = Vec::with_capacity(keys.len());
    if keys.is_empty() {
        return Ok(archived);
    }
    let mut network = world.row_mut::<SocialNetworkRow>(SOCIAL_NETWORK)?;
    for key in keys {
        let pair = pair_of(&key)?;
        let record = network.relationships.map.shift_remove(&*key).ok_or_else(|| Error::Key(key.to_string()))?;
        archived.push(V::Map(doc([
            ("pair", V::List(pair.iter().map(|part| v_str(part)).collect())),
            ("record", relationship_value(&record)),
        ])));
    }
    Ok(archived)
}

/// `_remove_information_state`: the messages the person started.
fn remove_information_state(world: &World, inputs: &Inputs, agent_id: &str) -> Result<HashSet<Arc<str>>> {
    let mut withdrawn = HashSet::new();
    if typed_of::<InformationRow>(world, INFORMATION, &inputs.namespaces.information)?.is_none() {
        return Ok(withdrawn);
    }
    let mut info = world.row_mut::<InformationRow>(INFORMATION)?;
    info.beliefs.shift_remove(agent_id);
    info.query_pressure.shift_remove(agent_id);
    for key in pairs_naming(info.contacts.iter(), agent_id)? {
        info.contacts.shift_remove(&*key);
    }
    for (message_id, message) in info.messages.iter() {
        if &*message.origin_agent_id == agent_id {
            withdrawn.insert(message_id.clone());
        }
    }
    #[allow(clippy::iter_over_hash_type, reason = "removes each id from a map; the map ends the same in any order")]
    for message_id in &withdrawn {
        info.messages.shift_remove(&**message_id);
    }
    if !withdrawn.is_empty() {
        for held in info.beliefs.values_mut() {
            #[allow(clippy::iter_over_hash_type, reason = "removes each id from a map; the map ends the same in any order")]
            for message_id in &withdrawn {
                held.shift_remove(&**message_id);
            }
        }
    }
    info.support_requests.retain(|item| &*item.agent_id != agent_id);
    Ok(withdrawn)
}

/// The departure record's profile: `_profile_record`.
fn profile_record(agent: &AgentRow) -> V {
    let profile = &agent.profile;
    let initial = &profile.initial_needs;
    V::Map(doc([
        ("id", V::Str(profile.id.clone())),
        ("group_id", optional(&profile.group_id)),
        ("initial_zone_id", V::Str(profile.initial_zone_id.clone())),
        ("mobility", v_str(profile.mobility.text())),
        ("support_need", V::Float(profile.support_need)),
        ("stress_sensitivity", V::Float(profile.stress_sensitivity)),
        ("rng_key", V::Str(profile.rng_key.clone())),
        ("initial_needs", needs_value(initial)),
        ("provenance", profile.provenance.clone()),
    ]))
}

fn needs_value(needs: &NeedRow) -> V {
    V::Map(doc([
        ("toilet_urgency", V::Float(needs.toilet_urgency)),
        ("hunger", V::Float(needs.hunger)),
        ("fatigue", V::Float(needs.fatigue)),
        ("hydration_need", V::Float(needs.hydration_need)),
    ]))
}

/// Where the cancellations of a departure are gathered.
struct Cancels<'a> {
    pending: &'a [Arc<EventRow>],
    taken: HashSet<Arc<str>>,
    found: Vec<Cancel>,
}

/// `_depart_agent`: the person's events to cancel are gathered.
fn write_departure(world: &World, inputs: &Inputs, agent_id: &Arc<str>, row: &EventRow, cancels: &mut Cancels<'_>) -> Result<()> {
    let agent = world.rows()?.agents.get(agent_id).cloned().ok_or_else(|| Error::Key(agent_id.to_string()))?;
    overlay::release(world, &inputs.namespaces.overlay, "bed_reservations", Some(agent_id))?;
    overlay::release(world, &inputs.namespaces.overlay, "room_admissions", Some(agent_id))?;
    let traits = {
        let state = value_of(world, DYNAMICS, &inputs.namespaces.dynamics)?.ok_or_else(|| Error::Runtime("the population state left".to_owned()))?;
        kit::get(field(&state.0, "profile_traits")?, agent_id)?.clone()
    };
    let relationships = remove_social_relationships(world, inputs, agent_id)?;
    let departure = V::Map(doc([
        ("event_id", V::Str(row.event_id.clone())),
        ("agent_id", V::Str(agent_id.clone())),
        ("zone_id", V::Str(agent.zone_id.clone())),
        ("minute", V::Int(row.due_minute)),
        ("profile", profile_record(&agent)),
        ("traits", traits),
        ("needs", needs_value(&agent.needs)),
        ("relationships", V::List(relationships)),
    ]));
    {
        let mut state = world.row_mut::<ValueRow>(DYNAMICS)?;
        list_mut(field_mut(&mut state.0, "departures")?)?.push(departure);
    }
    let withdrawn = remove_information_state(world, inputs, agent_id)?;
    cancels_of(cancels.pending, &mut cancels.taken, agent_id, &withdrawn, &mut cancels.found);
    if typed_of::<ServiceRow>(world, RESOURCE_SERVICE, &inputs.namespaces.resource_service)?.is_some() {
        let mut service = world.row_mut::<ServiceRow>(RESOURCE_SERVICE)?;
        service.request_history.retain(|item| item.agent_id != *agent_id);
        service.queue_stall_history.retain(|item| item.agent_id != *agent_id);
        service.stall_state.retain(|_, record| record.agent_id != *agent_id);
    }
    if typed_of::<SocialRow>(world, SOCIAL_RUNTIME, &inputs.namespaces.social_runtime)?.is_some() {
        let mut runtime = world.row_mut::<SocialRow>(SOCIAL_RUNTIME)?;
        for key in pairs_naming(runtime.contacts.iter(), agent_id)? {
            runtime.contacts.shift_remove(&*key);
        }
        runtime.agent_cooldown_until.shift_remove(&**agent_id);
    }
    {
        let mut rows = world.rows_mut()?;
        let requests: Vec<Arc<str>> = rows.support_requests.iter().filter(|(_, request)| request.agent_id == *agent_id).map(|(id, _)| id.clone()).collect();
        for id in requests {
            rows.support_requests.remove_row(&id);
        }
    }
    if typed_of::<AgentDynamicsRow>(world, AGENT_DYNAMICS, &inputs.namespaces.agent_dynamics)?.is_some() {
        world.row_mut::<AgentDynamicsRow>(AGENT_DYNAMICS)?.agents.shift_remove(&**agent_id);
    }
    if typed_of::<BodyRow>(world, BODY, &inputs.namespaces.body)?.is_some() {
        let mut body = world.row_mut::<BodyRow>(BODY)?;
        body.agents.shift_remove(&**agent_id);
        for key in pairs_naming(body.relationships.map.iter(), agent_id)? {
            body.relationships.map.shift_remove(&*key);
        }
    }
    if typed_of::<MovementRow>(world, MOVEMENT, &inputs.namespaces.movement)?.is_some() {
        let mut movement = world.row_mut::<MovementRow>(MOVEMENT)?;
        movement.journeys.shift_remove(&**agent_id);
        movement.positions.shift_remove(&**agent_id);
    }
    world.rows_mut()?.agents.remove_row(agent_id);
    record_presence_transition(world, inputs, agent_id, &row.event_id, row.due_minute, "departed", None, None, SOURCE_ID)
}

/// The writes of `queue_departures`, once the PyO3 crate has put a new state in: the people leave,
/// and the counts. What follows is the PyO3 crate's: it settles the cancelled events, in this
/// order, and then `admit_pending` runs.
pub fn write_departures(world: &World, inputs: &Inputs, row: &EventRow, selected: &[Arc<str>]) -> Result<Vec<Cancel>> {
    let pending: Vec<Arc<EventRow>> = {
        let queue = world.queue()?;
        queue.ordered().iter().filter_map(|(_, serial)| queue.slot(*serial).row.clone()).collect()
    };
    let mut cancels = Cancels { pending: &pending, taken: HashSet::new(), found: Vec::new() };
    for agent_id in selected {
        write_departure(world, inputs, agent_id, row, &mut cancels)?;
    }
    add_metric(world, "population_departure_count", selected.len())?;
    let waiting = {
        let state = value_of(world, DYNAMICS, &inputs.namespaces.dynamics)?.ok_or_else(|| Error::Runtime("the population state left".to_owned()))?;
        list_of(field(&state.0, "pending_arrivals")?)?.len()
    };
    set_metric(world, "population_waiting_count", waiting as f64)?;
    Ok(cancels.found)
}

// ---- `validate_population_dynamics_state` --------------------------------------------------------

/// `PersonTraits.model_validate`, of a document it certainly accepts.
fn traits_are_plain(value: &V) -> Result<()> {
    let V::Map(map) = value else {
        return refuse("traits are not a mapping");
    };
    const FIELDS: [&str; 7] = ["age_years", "household_id", "household_role", "languages", "accessibility_needs", "medical_support", "source_ids"];
    if map.keys().any(|key| !FIELDS.contains(&&**key)) {
        return refuse("traits have another field");
    }
    if !matches!(map.get("age_years"), Some(V::Int(age)) if (0..=120).contains(age)) {
        return refuse("an age is out of range");
    }
    maybe_identifier(map, "household_id")?;
    match map.get("household_role") {
        None => {}
        Some(V::Str(role)) if ["adult", "child", "dependent", "alone"].contains(&&**role) => {}
        _ => return refuse("a role is another value"),
    }
    identifiers(map.get("languages"))?;
    identifiers(map.get("accessibility_needs"))?;
    if !matches!(map.get("medical_support"), None | Some(V::Bool(_))) {
        return refuse("medical support is not a bool");
    }
    identifiers(map.get("source_ids"))?;
    Ok(())
}

/// A context's household, related people and sources.
type ContextParts = (Option<Arc<str>>, Vec<Arc<str>>, Vec<Arc<str>>);

/// `PersonContext.model_validate` of a document it certainly accepts: the related IDs and sources.
fn context_is_plain(value: &V) -> Result<ContextParts> {
    let V::Map(map) = value else {
        return refuse("a context is not a mapping");
    };
    const GENERATED: [&str; 8] = ["household_id", "caregiver_ids", "care_for_ids", "requires_review", "review_reasons", "roles", "guardian_status", "source_ids"];
    if map.keys().any(|key| !GENERATED.contains(&&**key) && &**key != "care_support_needs") {
        return refuse("a context has a field a generated one lacks");
    }
    let caregiver = identifiers(map.get("caregiver_ids"))?;
    let care_for = identifiers(map.get("care_for_ids"))?;
    if caregiver.len() > 16 || care_for.len() > 16 {
        return refuse("a context has too many people");
    }
    if !matches!(map.get("requires_review"), None | Some(V::Bool(_))) {
        return refuse("requires_review is not a bool");
    }
    match map.get("review_reasons") {
        None => {}
        Some(V::List(items)) if items.len() <= 32 && items.iter().all(|item| matches!(item, V::Str(_))) => {}
        _ => return refuse("the review reasons are something else"),
    }
    match map.get("roles") {
        None => {}
        Some(V::List(items)) if items.iter().all(|item| matches!(item, V::Str(role) if ["parent", "child", "caregiver", "dependent", "partner", "alone"].contains(&&**role))) => {}
        _ => return refuse("the roles are something else"),
    }
    match map.get("guardian_status") {
        None | Some(V::Null) => {}
        Some(V::Str(status)) if ["accompanied", "unaccompanied", "not_applicable"].contains(&&**status) => {}
        _ => return refuse("a guardian status is another value"),
    }
    match map.get("care_support_needs") {
        None => {}
        Some(V::List(items)) if items.len() <= 8 => {
            let mut seen = HashSet::new();
            for item in items {
                let V::Str(kind) = item else {
                    return refuse("a support kind is not a str");
                };
                if !SUPPORT_KINDS.contains(&&**kind) || !seen.insert(kind.clone()) {
                    return refuse("a support kind is unknown or repeats");
                }
            }
        }
        _ => return refuse("the support kinds are something else"),
    }
    let mut related = caregiver;
    related.extend(care_for);
    Ok((maybe_identifier(map, "household_id")?, related, identifiers(map.get("source_ids"))?))
}

/// `value != 1` is false: 1, True or 1.0.
pub fn is_one(value: Option<&V>) -> bool {
    matches!(value, Some(V::Int(1) | V::Bool(true))) || matches!(value, Some(V::Float(number)) if *number == 1.0)
}

/// A value Python hashes without calling anything: what `value in mapping` reads.
fn is_scalar(value: &V) -> bool {
    !matches!(value, V::List(_) | V::Map(_))
}

const SCHEMA: &str = "population_dynamics_v1 has an invalid exact schema";
const PENDING_INVALID: &str = "population_dynamics_v1 pending arrival is invalid";

/// Whether the validation reads the approach routes: an arrival on its approach is waiting.
pub fn validation_reads_routes(world: &World, held: &Held) -> Result<bool> {
    let Some(state) = value_of(world, DYNAMICS, held)? else {
        return Ok(false);
    };
    Ok(match kit::get(&state.0, "pending_arrivals") {
        Ok(V::List(pending)) => pending.iter().any(|item| matches!(item, V::Map(fields) if fields.contains_key("approach_route_id"))),
        _ => false,
    })
}

/// What the validation reads of a world: of the core's world (`validate`), or of the Python values
/// of a world the core does not hold as it is (the PyO3 crate's, for the whole-world validation of
/// a save or a world whose store is tainted, policy E6).
pub struct Facts<'a> {
    /// `population_dynamics_v1`, None for none.
    pub state: Option<&'a V>,
    /// `population_presence_v1`, None for none.
    pub presence: Option<&'a V>,
    /// The IDs of `world.agents`, in its order.
    pub agents: &'a [Arc<str>],
    /// `zone_id in world.zones` of a str.
    pub zone_known: &'a dyn Fn(&str) -> Result<bool>,
    pub minute_index: Result<i64>,
    pub duration: Result<i64>,
    pub routes: Option<&'a Routes>,
    /// The pending event of this ID, as a row; None for none pending.
    pub event: &'a dyn Fn(&str) -> Result<Option<Arc<EventRow>>>,
}

/// `validate_population_dynamics_state` of the core's world.
pub fn validate(world: &World, inputs: &Inputs) -> Result<()> {
    let state = value_of(world, DYNAMICS, &inputs.namespaces.dynamics)?;
    let Some(state) = state else {
        return Ok(());
    };
    let presence = value_of(world, PRESENCE, &inputs.namespaces.presence)?;
    let agents: Vec<Arc<str>> = world.rows()?.agents.iter().map(|(id, _)| id.clone()).collect();
    let zone_known = |zone: &str| zone_known(world, zone);
    let event = |event_id: &str| -> Result<Option<Arc<EventRow>>> {
        let queue = world.queue()?;
        match queue.serial_of(event_id) {
            None => Ok(None),
            Some(serial) => match &queue.slot(serial).row {
                Some(row) => Ok(Some(row.clone())),
                None => refuse(NO_ROW),
            },
        }
    };
    validate_facts(&Facts {
        state: Some(&state.0),
        presence: presence.as_deref().map(|row| &row.0),
        agents: &agents,
        zone_known: &zone_known,
        minute_index: world.minute_index(),
        duration: world.duration_minutes(),
        routes: inputs.routes.as_ref(),
        event: &event,
    })
}

/// `validate_population_dynamics_state`: its ValueErrors in its order of checks. Unsupported, a
/// `KernelRefusal` in Python, for what the Python rule's contracts decided (`PersonContext`,
/// `PersonTraits`: the plain forms a generated arrival has are read here) and for values of other
/// types than a run writes.
pub fn validate_facts(facts: &Facts<'_>) -> Result<()> {
    let state = match facts.state {
        None | Some(V::Null) => return Ok(()),
        Some(state) => state,
    };
    let V::Map(document) = state else {
        return invalid(SCHEMA);
    };
    const FIELDS: [&str; 8] = ["schema_version", "policy_version", "source_id", "initial_agent_ids", "profile_traits", "pending_arrivals", "arrivals", "departures"];
    let has_context = document.contains_key("profile_context");
    if document.len() != FIELDS.len() + usize::from(has_context) || FIELDS.iter().any(|key| !document.contains_key(*key)) {
        return invalid(SCHEMA);
    }
    let good_header = is_one(document.get("schema_version"))
        && matches!(document.get("policy_version"), Some(V::Str(text)) if &**text == POLICY_VERSION)
        && matches!(document.get("source_id"), Some(V::Str(text)) if &**text == SOURCE_ID);
    if !good_header {
        return invalid(SCHEMA);
    }
    let Some(V::List(initial)) = document.get("initial_agent_ids") else {
        return invalid(SCHEMA);
    };
    let mut historical: HashSet<&str> = HashSet::with_capacity(initial.len());
    for id in initial {
        let V::Str(id) = id else {
            return refuse("an initial ID is not a str");
        };
        if !historical.insert(id) {
            return invalid(SCHEMA);
        }
    }
    let (Some(V::Map(traits)), Some(V::List(pending)), Some(V::List(_)), Some(V::List(_))) =
        (document.get("profile_traits"), document.get("pending_arrivals"), document.get("arrivals"), document.get("departures"))
    else {
        return invalid(SCHEMA);
    };
    if pending.len() as i64 > MAX_PENDING_ARRIVALS {
        return invalid(SCHEMA);
    }
    historical.extend(traits.keys().map(|key| &**key));
    if facts.agents.iter().any(|id| !historical.contains(&**id)) {
        return invalid("population_dynamics_v1 is missing an active profile");
    }
    let empty = IndexMap::new();
    let contexts = match document.get("profile_context") {
        None => &empty,
        Some(V::Map(contexts)) if contexts.keys().all(|identity| historical.contains(&**identity)) => contexts,
        Some(_) => return invalid("population_dynamics_v1 has an invalid dynamic context index"),
    };
    for (identity, context) in contexts {
        let (household, related, sources) = context_is_plain(context)?;
        if related.iter().any(|id| id == identity || !historical.contains(&**id)) {
            return invalid("dynamic household context references an unknown person");
        }
        let household_of = |id: &str| {
            traits.get(id).and_then(|item| match item {
                V::Map(item) => item.get("household_id"),
                _ => None,
            })
        };
        let agrees = match &household {
            None => false,
            Some(household) => {
                sources.iter().any(|source| &**source == SOURCE_ID)
                    && matches!(household_of(identity), Some(V::Str(text)) if text == household)
                    && related.iter().all(|id| matches!(household_of(id), Some(V::Str(text)) if text == household))
            }
        };
        if !agrees {
            return invalid("dynamic household context disagrees with generated profile provenance");
        }
    }
    if pending.is_empty() {
        return Ok(());
    }
    check_pending_items(facts, pending, contexts)
}

fn check_pending_items(facts: &Facts<'_>, pending: &[V], contexts: &Doc) -> Result<()> {
    const BASE: [&str; 5] = ["profile", "traits", "zone_id", "created_minute", "event_id"];
    const CONTEXT: [&str; 3] = ["member_index", "care_support_needs", "context_source_id"];
    const APPROACH: [&str; 3] = ["approach_route_id", "approach_started_minute", "approach_due_minute"];
    let minute_index = facts.minute_index.clone()?;
    let duration = facts.duration.clone()?;
    // The people of the presence state.
    let no_people = V::Map(IndexMap::new());
    let people: Option<&V> = match facts.presence {
        Some(V::Map(presence)) => Some(presence.get("people").unwrap_or(&no_people)),
        _ => None,
    };
    let agents: HashSet<&str> = facts.agents.iter().map(|id| &**id).collect();
    let in_zones = |value: &V| -> Result<bool> {
        match value {
            V::Str(id) => (facts.zone_known)(id),
            value if is_scalar(value) => Ok(false),
            _ => refuse("a zone ID is not hashable"),
        }
    };
    let mut seen: HashSet<&str> = HashSet::new();
    for item in pending {
        let V::Map(fields) = item else {
            return invalid(PENDING_INVALID);
        };
        let has_base = BASE.iter().all(|key| fields.contains_key(*key));
        let has_context = CONTEXT.iter().filter(|key| fields.contains_key(**key)).count();
        let has_approach = APPROACH.iter().filter(|key| fields.contains_key(**key)).count();
        let completion = fields.contains_key("approach_completion_event_id");
        let completed = fields.contains_key("approach_completed_minute");
        let alternates = fields.contains_key("admission_zone_ids");
        let expected = BASE.len() + has_context + has_approach + usize::from(completion) + usize::from(completed) + usize::from(alternates);
        if !has_base
            || fields.len() != expected
            || !(has_context == 0 || has_context == CONTEXT.len())
            || !(has_approach == 0 || has_approach == APPROACH.len())
            || (completion || completed) && has_approach == 0
            || completion && completed
        {
            return invalid(PENDING_INVALID);
        }
        let zone_value = fields.get("zone_id").ok_or_else(|| Error::unsupported("no zone"))?;
        let Some(V::Str(agent_id)) = fields.get("profile").and_then(|profile| match profile {
            V::Map(profile) => profile.get("id"),
            _ => None,
        }) else {
            return refuse("a pending arrival's ID is not a str");
        };
        if !in_zones(zone_value)? || seen.contains(&**agent_id) || agents.contains(&**agent_id) {
            return invalid(PENDING_INVALID);
        }
        if let Some(value) = fields.get("admission_zone_ids") {
            let V::List(ids) = value else {
                return invalid(PENDING_INVALID);
            };
            let mut unique = HashSet::new();
            for id in ids {
                let V::Str(id) = id else {
                    return invalid(PENDING_INVALID);
                };
                unique.insert(id);
            }
            if unique.len() != ids.len() {
                return invalid(PENDING_INVALID);
            }
            #[allow(clippy::iter_over_hash_type, reason = "only checks membership, with one text for any id")]
            for id in unique {
                if !(facts.zone_known)(id)? {
                    return invalid(PENDING_INVALID);
                }
            }
        }
        if has_context > 0 {
            let good = matches!(fields.get("member_index"), Some(V::Int(index)) if (0..MAX_ARRIVALS_PER_EVENT).contains(index))
                && matches!(fields.get("context_source_id"), Some(V::Str(_)));
            let Some(V::List(needs)) = fields.get("care_support_needs") else {
                return invalid("population_dynamics_v1 pending context is invalid");
            };
            let mut unique = HashSet::new();
            if !good || needs.iter().any(|need| !matches!(need, V::Str(_)) || !unique.insert(format!("{need:?}"))) {
                return invalid("population_dynamics_v1 pending context is invalid");
            }
            let saved = contexts.get(agent_id).and_then(|context| match context {
                V::Map(context) => context.get("care_support_needs"),
                _ => None,
            });
            let same = match saved {
                None => needs.is_empty(),
                Some(V::List(saved)) => saved == needs,
                Some(_) => return refuse("saved care needs are not a list"),
            };
            if !same {
                return invalid("pending care context disagrees with its persisted profile context");
            }
        }
        if has_approach > 0 {
            const TIMING: &str = "pending arrival approach route/timing is invalid";
            let routes = match facts.routes {
                Some(Ok(routes)) => routes,
                Some(Err(fault)) => return Err(fault.refusal.clone()),
                None => return refuse("the approach routes were not read"),
            };
            let route_value = fields.get("approach_route_id").ok_or_else(|| Error::unsupported("no route"))?;
            let mut found = None;
            if let V::Str(route_id) = route_value {
                for route in routes.iter() {
                    if route.route_id == *route_id {
                        found = Some(route);
                    }
                }
            } else if !is_scalar(route_value) {
                return refuse("a route ID is not hashable");
            }
            let (Some(route), Some(V::Int(started)), Some(V::Int(due))) = (found, fields.get("approach_started_minute"), fields.get("approach_due_minute")) else {
                return invalid(TIMING);
            };
            match fields.get("created_minute") {
                Some(V::Int(created)) if created == started => {}
                Some(V::Int(_) | V::Null | V::Str(_) | V::List(_) | V::Map(_)) => return invalid(TIMING),
                _ => return refuse("a created minute is not an int"),
            }
            if due <= started {
                return invalid(TIMING);
            }
            let zone = match zone_value {
                V::Str(zone) => zone,
                _ => return refuse("a zone ID is not a str"),
            };
            let travel = route.travel_minutes.clone().map_err(|fault| Error::unsupported(fault.text.to_string()))?;
            if route.destination_zone_id != *zone || *due != started + travel {
                return invalid("pending arrival approach disagrees with its route contract");
            }
            let finished = match fields.get("approach_completed_minute") {
                None | Some(V::Null) => None,
                Some(V::Int(minute)) if minute >= due && *minute <= minute_index => Some(*minute),
                Some(_) => return invalid("pending arrival approach completion minute is invalid"),
            };
            let completion_id = match fields.get("approach_completion_event_id") {
                None | Some(V::Null) => None,
                Some(value) => Some(value),
            };
            if finished.is_none() && *due < duration {
                const MISSING: &str = "pending arrival approach completion event is missing";
                let Some(completion_id) = completion_id else {
                    return invalid(MISSING);
                };
                // `event.event_id == completion_event_id` of a value of another type is false.
                let V::Str(completion_id) = completion_id else {
                    return invalid(MISSING);
                };
                let Some(event) = (facts.event)(completion_id)? else {
                    return invalid(MISSING);
                };
                if &*event.event_type != "population_approach_complete" {
                    return invalid(MISSING);
                }
                if event.due_minute != *due || !matches!(kit::payload(&event, "agent_id"), Some(V::Str(id)) if id == agent_id) || kit::payload(&event, "route_id") != Some(route_value) {
                    return invalid("pending arrival approach completion event disagrees");
                }
            } else if finished.is_none() && completion_id.is_some() {
                return invalid("out-of-horizon arrival approach has a completion event");
            } else if finished.is_some() && completion_id.is_some() {
                return invalid("completed arrival approach retains a pending event");
            }
            if let Some(people) = people {
                let V::Map(people) = people else {
                    return refuse("the presence's people are not a dict");
                };
                let expected = if finished.is_none() { "approaching" } else { "awaiting_admission" };
                match people.get(&**agent_id).filter(|person| person.is_truthy()) {
                    None => {}
                    Some(V::Map(person)) => {
                        if !matches!(person.get("status"), Some(V::Str(status)) if &**status == expected) {
                            return invalid("arrival approach disagrees with population presence");
                        }
                    }
                    Some(_) => return refuse("a presence record is not a dict"),
                }
            }
        }
        seen.insert(agent_id);
        traits_are_plain(fields.get("traits").ok_or_else(|| Error::unsupported("no traits"))?)?;
    }
    Ok(())
}
