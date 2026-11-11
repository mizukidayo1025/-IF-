//! `domain/temporary_spaces.py` and `domain/geometry_overlay.py`: capacity added during a Run, beds
//! and intake in a physical temporary room, the floor-plan point of a facility installed during a
//! Run, and the approved changes of the floor plan (lane c4 of Issue #152).
//!
//! The rules are functions on the `World`. `temporary_spaces_v1` and `geometry_overlay_v1` are
//! `ValueRow` namespaces: a rule reads and writes their rows alone, and an attached tree follows the
//! row when the PyO3 crate gives the world back (lane P8-0c). A transfer writes the facility's
//! `zone_id` and a route-access change the edge's `closed` on the entity rows, which the Python
//! objects follow (lane 6-1).
//!
//! What only Python has comes as an input, read by the PyO3 crate before the world is borrowed:
//! whether `model_state` holds each namespace (`kit::Held`), the sections of the configs
//! (`kit::Kept`), and the floor plan (`Geo`). A plan the PyO3 crate could not read is None here, and
//! a rule that reads it says `PLAN_UNREAD`, for the PyO3 crate to raise what the read raised, at the
//! point the rule read the plan before. The pydantic contracts of a change's parameters (policy E15)
//! go through `Host::contract`, which gives back `model_dump(mode="json")`: a check reads that, and
//! the change writes it.
//!
//! A rule raises the texts its callers save or show (a `ValueError`), and says Unsupported, a
//! `KernelRefusal` in Python, only for an input outside its contract (a state of another shape).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use indexmap::IndexMap;
use sha2::{Digest, Sha256};

use crate::cell::ShardRef;
use crate::config::movement::Movement;
use crate::config::service::ResourceService;
use crate::entities::{Activity, Entities};
use crate::error::{Error, Result};
use crate::events::EventRow;
use crate::geometry::{
    is_identifier, point_in_polygon, point_in_value, point_segment_distance, point_walkable, points_in_value, polyline_length,
    segment_clear_of_polygon, segment_inside_polygon, usable, Constraints, Geo, Layout, Pt,
};
use crate::host::{Contract, Host};
use crate::mathx::{hypot, pow, py_max, py_min, py_round, py_sum};
use crate::row::ValueRow;
use crate::rules::kit::movement::{self as walking, KEY as MOVEMENT_KEY};
use crate::rules::kit::{self, Held, Kept};
use crate::state::movement::MovementRow;
use crate::state::staff_operations::{StaffOperationsRow, NAME as STAFF_OPERATIONS};
use crate::text::py_repr;
use crate::value::V;
use crate::world::{World, WorldCell};

pub const OVERLAY_KEY: &str = "geometry_overlay_v1";
pub const SPACES_KEY: &str = "temporary_spaces_v1";
const MAX_SPACES: usize = 256;
const MAX_SPACE_HISTORY: usize = 20_000;
const MAX_OVERLAY_HISTORY: usize = 20_000;
const POLICY_SPACES: &str = "typed-temporary-capacity-v1";
const POLICY_OVERLAY: &str = "bounded-runtime-geometry-v1";
const PLACEMENT_SOURCE: &str = "ASM-INSTALLED-FACILITY-PLACEMENT-V1";
const INTAKE_SOURCE: &str = "ASM-TEMPORARY-ROOM-INTAKE-V1";
/// `INSTALLED_FACILITY_MIN_SEPARATION_M`, `MAX_INSTALLED_FACILITY_GRID_GROWTH`.
const MIN_SEPARATION: f64 = 0.6;
const GRID_GROWTH: usize = 16;
const MAX_BED_ROUTES: usize = 256;
const MAX_ADMISSION_POINTS: usize = 256;
/// `reserve_sleep_bed`: how near another person stands to be using a bed, in metres.
const BED_RADIUS: f64 = 0.75;
/// `room_admission_point`: the grid of candidates and how near a resident stands to take a point.
const ADMISSION_STEP: f64 = 0.75;
const ADMISSION_RADIUS: f64 = 0.5;
/// How near a moved facility may stand to a wall.
const WALL_CLEARANCE: f64 = 0.5;

/// What a rule says where it reads a floor plan the PyO3 crate could not read (`Geo::of` raised):
/// the PyO3 crate raises that error instead.
pub const PLAN_UNREAD: &str = "the floor plan could not be read";

/// The text of a room admission that no longer lets a person in.
const NOT_AVAILABLE: &str = "temporary room admission is no longer available";

type Doc = IndexMap<Arc<str>, V>;

// ---- Reading ---------------------------------------------------------------------------------------

#[track_caller]
fn refuse<T>(reason: &'static str) -> Result<T> {
    Err(Error::unsupported(reason))
}

/// `raise ValueError(text)` of the rules.
#[track_caller]
fn invalid<T>(text: &str) -> Result<T> {
    Err(Error::value_error(text))
}

/// The plan the PyO3 crate read, or `PLAN_UNREAD`.
#[track_caller]
fn plan(geo: Option<&Geo>) -> Result<&Geo> {
    geo.ok_or_else(|| Error::unsupported(PLAN_UNREAD))
}

/// The row of a namespace the world holds (`kit::held_row`).
fn state_of<'w>(world: &'w World, name: &str, held: &Held) -> Result<Option<ShardRef<'w, ValueRow>>> {
    kit::held_row::<ValueRow>(world, name, held)
}

/// A node of a dict, read as Python's `node[name]`, the node being a plain dict.
fn child<'a>(node: &'a V, name: &str) -> Result<Option<&'a V>> {
    match node {
        V::Map(map) => Ok(map.get(name)),
        _ => refuse("a value is not a plain dict"),
    }
}

fn required<'a>(node: &'a V, name: &str) -> Result<&'a V> {
    child(node, name)?.ok_or_else(|| Error::unsupported("the state has no such field"))
}

fn required_mut<'a>(node: &'a mut V, name: &str) -> Result<&'a mut V> {
    match node {
        V::Map(map) => map.get_mut(name).ok_or_else(|| Error::unsupported("the state has no such field")),
        _ => refuse("a value is not a plain dict"),
    }
}

/// `state.get(name, {})` as a value.
fn part(state: &V, name: &str) -> Result<V> {
    Ok(child(state, name)?.cloned().unwrap_or_else(|| V::Map(IndexMap::new())))
}

fn map_of(value: &V) -> Result<&Doc> {
    match value {
        V::Map(map) => Ok(map),
        _ => refuse("a value is not a dict"),
    }
}

/// A part of the state that the rule writes, as a plain dict.
fn dict_mut(value: &mut V) -> Result<&mut Doc> {
    match value {
        V::Map(map) => Ok(map),
        _ => refuse("a value is not a plain dict"),
    }
}

fn list_of(value: &V) -> Result<&[V]> {
    match value {
        V::List(items) => Ok(items),
        _ => refuse("a value is not a list"),
    }
}

/// A history the rule appends to, as a plain list.
fn list_mut(value: &mut V) -> Result<&mut Vec<V>> {
    match value {
        V::List(items) => Ok(items),
        _ => refuse("a value is not a plain list"),
    }
}

fn int_of(map: &Doc, key: &str) -> Result<i64> {
    match map.get(key) {
        Some(V::Int(number)) => Ok(*number),
        _ => refuse("a value is not a plain int"),
    }
}

fn str_of<'a>(map: &'a Doc, key: &str) -> Result<&'a Arc<str>> {
    match map.get(key) {
        Some(V::Str(text)) => Ok(text),
        _ => refuse("a value is not a str"),
    }
}

/// `row.get(key)` as a truth: a bool or nothing. Other values are refused.
fn flag_of(map: &Doc, key: &str) -> Result<bool> {
    match map.get(key) {
        Some(V::Bool(flag)) => Ok(*flag),
        Some(V::Null) | None => Ok(false),
        _ => refuse("a flag is not a bool"),
    }
}

fn has_exactly(map: &Doc, names: &[&str]) -> bool {
    map.len() == names.len() && names.iter().all(|name| map.contains_key(*name))
}

/// `tuple(Point2D.model_validate(point) for point in value)`, for a polygon a rule then tests.
fn polygon_of(value: Option<&V>) -> Result<Vec<Pt>> {
    let Some(value) = value else {
        return refuse("no polygon");
    };
    let polygon = points_in_value(value)?;
    if polygon.is_empty() {
        return refuse("a polygon has no corners");
    }
    Ok(polygon)
}

fn square(value: f64) -> Result<f64> {
    Ok(pow(value, 2.0)?)
}

/// `(x - a) ** 2 + (y - b) ** 2`.
fn squared_distance(first: Pt, second: Pt) -> Result<f64> {
    Ok(square(first.x - second.x)? + square(first.y - second.y)?)
}

fn text(value: &str) -> V {
    V::Str(Arc::from(value))
}

fn doc<const N: usize>(items: [(&str, V); N]) -> V {
    V::Map(items.into_iter().map(|(key, value)| (Arc::from(key), value)).collect())
}

fn point_doc(point: Pt) -> V {
    doc([("x", V::Float(point.x)), ("y", V::Float(point.y))])
}

fn id_list<'a>(ids: impl IntoIterator<Item = &'a str>) -> V {
    V::List(ids.into_iter().map(text).collect())
}

fn hex_of(value: &str) -> String {
    let digest = Sha256::digest(value.as_bytes());
    let mut out = String::with_capacity(24);
    for byte in digest.iter().take(12) {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// `_bed_target_id`.
pub fn bed_target_id(room_id: &str, bed_index: i64) -> String {
    format!("bed-{}", hex_of(&format!("{room_id}:{bed_index}")))
}

/// `sorted({...})`.
fn sorted_set<'a>(values: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let mut out: Vec<&str> = values.into_iter().collect();
    out.sort_unstable_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    out.dedup();
    out
}

/// `history.append(entry)` and `del history[:-limit]`.
fn append_bounded(history: &mut Vec<V>, entry: V, limit: usize) {
    history.push(entry);
    if history.len() > limit {
        let extra = history.len() - limit;
        history.drain(..extra);
    }
}

/// The zones and the zone edges, which a rule reads as `world.zones` and `world.zone_edges`.
fn zone_rows(world: &World) -> Result<ShardRef<'_, Entities>> {
    let rows = world.entities.try_borrow()?;
    if rows.zones_tainted().is_some() {
        return refuse("the zones are tainted");
    }
    Ok(rows)
}

/// The row of `movement_v1` where the world holds it.
fn movement_of<'w>(world: &'w World, held: &Held) -> Result<Option<ShardRef<'w, MovementRow>>> {
    kit::held_row::<MovementRow>(world, MOVEMENT_KEY, held)
}

fn position_of(movement: Option<&MovementRow>, agent_id: &str) -> Result<Option<Pt>> {
    let Some(row) = movement else {
        return Ok(None);
    };
    let Some(point) = row.positions.get(agent_id).and_then(|position| position.point) else {
        return Ok(None);
    };
    if !usable(point.x, point.y) {
        return refuse("a position is not a usable point");
    }
    Ok(Some(Pt { x: point.x, y: point.y }))
}

/// `config.get("schema_version")` of the overlay's config (`config::overlay::overlay_schema`).
#[track_caller]
fn schema_of(schema: &Kept<Option<i64>>) -> Result<Option<i64>> {
    Ok(*kit::kept(schema)?)
}

// ---- domain/temporary_spaces.py -----------------------------------------------------------------

/// `register_temporary_space`, of a record the caller made.
pub fn register_space(world: &World, held: &Held, record: V, event_id: &str) -> Result<()> {
    let Some(state) = state_of(world, SPACES_KEY, held)? else {
        return invalid("temporary_spaces_v1 is not installed");
    };
    let V::Map(fields) = &record else {
        return refuse("a value is not a plain dict");
    };
    let Some(space_id) = fields.get("space_id") else {
        return refuse("the record has no space_id");
    };
    let space_text = kit::exact_str(space_id)?.clone();
    let Some(zone_id) = fields.get("zone_id") else {
        return refuse("the record has no zone_id");
    };
    let (space_value, zone_value) = (space_id.clone(), zone_id.clone());
    {
        let spaces = required(&state.0, "spaces")?;
        let history = required(&state.0, "history")?;
        let V::Map(spaces) = spaces else {
            return refuse("a value is not a plain dict");
        };
        if spaces.contains_key(&*space_text) {
            return invalid("temporary space ID already exists");
        }
        if spaces.len() >= MAX_SPACES {
            return invalid("temporary space runtime limit reached");
        }
        if !matches!(history, V::List(_)) {
            return refuse("a value is not a plain list");
        }
    }
    drop(state);
    let minute = world.minute_index()?;
    let entry = doc([
        ("event_id", text(event_id)),
        ("space_id", space_value),
        ("zone_id", zone_value),
        ("minute", V::Int(minute)),
        ("transition", text("activated")),
        ("causation_id", V::Null),
    ]);
    let mut row = world.row_mut::<ValueRow>(SPACES_KEY)?;
    dict_mut(required_mut(&mut row.0, "spaces")?)?.insert(space_text, record);
    append_bounded(list_mut(required_mut(&mut row.0, "history")?)?, entry, MAX_SPACE_HISTORY);
    Ok(())
}

/// `expire_temporary_space`: whether the space was active.
pub fn expire_space(world: &World, held: &Held, space_id: &str, event_id: &str, causation_id: Option<&str>) -> Result<bool> {
    let Some(state) = state_of(world, SPACES_KEY, held)? else {
        return invalid("temporary_spaces_v1 is not installed");
    };
    let spaces = required(&state.0, "spaces")?;
    let Some(record) = child(spaces, space_id)? else {
        return Ok(false);
    };
    let V::Bool(active) = required(record, "active")? else {
        return refuse("not a bool");
    };
    if !active {
        return Ok(false);
    }
    let held_id = required(record, "space_id")?.clone();
    let zone_id = required(record, "zone_id")?.clone();
    if !matches!(required(&state.0, "history")?, V::List(_)) {
        return refuse("a value is not a plain list");
    }
    drop(state);
    let minute = world.minute_index()?;
    let entry = doc([
        ("event_id", text(event_id)),
        ("space_id", held_id),
        ("zone_id", zone_id),
        ("minute", V::Int(minute)),
        ("transition", text("expired")),
        ("causation_id", causation_id.map_or(V::Null, text)),
    ]);
    let mut row = world.row_mut::<ValueRow>(SPACES_KEY)?;
    let record = dict_mut(required_mut(&mut row.0, "spaces")?)?.get_mut(space_id).ok_or_else(|| Error::unsupported("the space is gone"))?;
    let record = dict_mut(record)?;
    record.insert(Arc::from("active"), V::Bool(false));
    record.insert(Arc::from("end_event_id"), text(event_id));
    append_bounded(list_mut(required_mut(&mut row.0, "history")?)?, entry, MAX_SPACE_HISTORY);
    Ok(true)
}

/// `validate_temporary_spaces_state`: true only for what the Python rule passes.
pub fn validate_spaces(world: &World, held: &Held) -> Result<bool> {
    let Some(state) = state_of(world, SPACES_KEY, held)? else {
        return Ok(true);
    };
    let rows = zone_rows(world)?;
    spaces_pass(&state.0, &rows)
}

/// The rules of `validate_temporary_spaces_state` on the namespace as a `V`.
fn spaces_pass(state: &V, rows: &Entities) -> Result<bool> {
    let state = map_of(state)?;
    let (Some(V::Map(spaces)), Some(V::List(history))) = (state.get("spaces"), state.get("history")) else {
        return refuse("the state has another shape");
    };
    if !has_exactly(state, &["schema_version", "policy_version", "spaces", "history"])
        || state.get("schema_version") != Some(&V::Int(1))
        || str_of(state, "policy_version")?.as_ref() != POLICY_SPACES
        || spaces.len() > MAX_SPACES
        || history.len() > MAX_SPACE_HISTORY
    {
        return refuse("the state has another shape");
    }
    for (space_id, row) in spaces {
        let row = map_of(row)?;
        let names = [
            "space_id", "zone_id", "people_capacity", "sleeping_capacity", "start_minute", "end_minute", "active",
            "start_event_id", "end_event_id", "program_id", "effect_id", "source_ids", "geometry_status",
        ];
        if !has_exactly(row, &names) || str_of(row, "space_id")? != space_id || !rows.zones.contains_key(str_of(row, "zone_id")?) {
            return refuse("a space has another shape");
        }
        let people = int_of(row, "people_capacity")?;
        let sleeping = int_of(row, "sleeping_capacity")?;
        let (start, end) = (int_of(row, "start_minute")?, int_of(row, "end_minute")?);
        let Some(V::Bool(active)) = row.get("active") else {
            return refuse("a space has another shape");
        };
        let ended = match row.get("end_event_id") {
            Some(V::Null) => false,
            Some(V::Str(id)) => !id.is_empty(),
            _ => return refuse("a space's end event is another value"),
        };
        if !(1..=1000).contains(&people)
            || !(0..=people).contains(&sleeping)
            || start < 0
            || end <= start
            || str_of(row, "start_event_id")?.is_empty()
            || (*active && ended)
            || (*active && row.get("end_event_id") != Some(&V::Null))
            || (!*active && !ended)
        {
            return refuse("a space's values are not what the Python rule passes");
        }
        str_of(row, "program_id")?;
        str_of(row, "effect_id")?;
        if !list_of(row.get("source_ids").unwrap_or(&V::Null))?.iter().all(|item| matches!(item, V::Str(id) if !id.is_empty())) {
            return refuse("a space's source is another value");
        }
        if str_of(row, "geometry_status")?.as_ref() != "zone_only_capacity" {
            return refuse("a space's status is another value");
        }
    }
    for row in history {
        let row = map_of(row)?;
        if !has_exactly(row, &["event_id", "space_id", "zone_id", "minute", "transition", "causation_id"]) {
            return refuse("a history record has another shape");
        }
        str_of(row, "event_id")?;
        str_of(row, "space_id")?;
        int_of(row, "minute")?;
        if !rows.zones.contains_key(str_of(row, "zone_id")?)
            || !matches!(&**str_of(row, "transition")?, "activated" | "expired")
            || !matches!(row.get("causation_id"), Some(V::Null | V::Str(_)))
        {
            return refuse("a history record has another value");
        }
    }
    Ok(true)
}

/// What `capacity_metrics` reads beside the world's rows: whether the world holds the two
/// namespaces, and the overlay's schema.
pub struct Places<'a> {
    pub spaces: &'a Held,
    pub overlay: &'a Held,
    pub schema: &'a Kept<Option<i64>>,
}

/// `capacity_metrics` of a zone with a capacity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Capacity {
    pub effective: i64,
    pub active_people: i64,
    pub expired_people: i64,
    pub retained: i64,
    pub sleeping: i64,
    pub room_capacity: i64,
    pub active_admissions: i64,
    pub retained_rooms: i64,
}

impl Capacity {
    /// `effective_total_capacity`: the effective capacity with the rooms.
    pub fn total(&self) -> Result<i64> {
        let total = self.effective.checked_add(self.room_capacity).and_then(|sum| sum.checked_add(self.retained_rooms));
        total.ok_or_else(|| Error::unsupported("a capacity out of range"))
    }
}

/// `capacity_metrics` of a zone with a capacity (`nominal`), for this many people in it, or as many
/// as stand in it now.
pub fn capacity(world: &World, places: &Places<'_>, zone_id: &str, nominal: i64, occupancy: Option<i64>) -> Result<Capacity> {
    let current = match occupancy {
        Some(occupancy) => occupancy,
        None => world.rows()?.agents.iter().filter(|(_, agent)| &*agent.zone_id == zone_id).count() as i64,
    };
    let spaces = state_of(world, SPACES_KEY, places.spaces)?;
    let spaces = spaces.as_deref().map(|row| &row.0);
    // The overlay is read after the spaces are counted and the schema is read, as the rule reads it.
    let overlay = || state_of(world, OVERLAY_KEY, places.overlay);
    capacity_in(spaces, overlay, places.schema, zone_id, nominal, current)
}

/// `capacity_metrics` of the values of the two namespaces (None where `model_state` has no such
/// key), for `current` people in the zone: for a world the core does not hold (a save's world
/// being validated, one whose `model_state` is another mapping), whose values the PyO3 crate reads.
pub fn capacity_of_values(spaces: Option<&V>, overlay: Option<&V>, schema: &Kept<Option<i64>>, zone_id: &str, nominal: i64, current: i64) -> Result<Capacity> {
    capacity_in(spaces, || Ok(overlay), schema, zone_id, nominal, current)
}

/// A namespace's value, borrowed from its row or given.
trait Value {
    fn value(&self) -> &V;
}

impl Value for &V {
    fn value(&self) -> &V {
        self
    }
}

impl Value for ShardRef<'_, ValueRow> {
    fn value(&self) -> &V {
        &self.0
    }
}

fn capacity_in<O: Value>(
    spaces: Option<&V>,
    overlay: impl FnOnce() -> Result<Option<O>>,
    schema: &Kept<Option<i64>>,
    zone_id: &str,
    nominal: i64,
    current: i64,
) -> Result<Capacity> {
    let add = |first: i64, second: i64| first.checked_add(second).ok_or_else(|| Error::unsupported("a capacity out of range"));
    let (mut active_people, mut expired_people, mut sleeping) = (0, 0, 0);
    let mut count = |row: &V| -> Result<()> {
        let row = map_of(row)?;
        if !matches!(row.get("zone_id"), Some(V::Str(zone)) if &**zone == zone_id) {
            if !matches!(row.get("zone_id"), Some(V::Str(_) | V::Null) | None) {
                return refuse("a space's zone is another value");
            }
            return Ok(());
        }
        let Some(V::Bool(active)) = row.get("active") else {
            return refuse("a space's active is not a bool");
        };
        let people = int_of(row, "people_capacity")?;
        if *active {
            active_people = add(active_people, people)?;
            sleeping = add(sleeping, int_of(row, "sleeping_capacity")?)?;
        } else {
            expired_people = add(expired_people, people)?;
        }
        Ok(())
    };
    if let Some(state) = spaces {
        let V::Map(fields) = state else {
            return refuse("the spaces' state is not a plain dict");
        };
        if let Some(spaces) = fields.get("spaces") {
            let V::Map(spaces) = spaces else {
                return refuse("a value is not a plain dict");
            };
            for row in spaces.values() {
                count(row)?;
            }
        }
    }
    let beyond = current.checked_sub(nominal).and_then(|over| over.checked_sub(active_people));
    let retained = expired_people.min(beyond.ok_or_else(|| Error::unsupported("a capacity out of range"))?.max(0));
    let effective = add(add(nominal, active_people)?, retained)?;

    let rooms_count = matches!(schema_of(schema)?, Some(3 | 4));
    let (mut room_capacity, mut active_admissions, mut expired_capacity, mut expired_admissions) = (0, 0, 0, 0);
    if let Some(overlay) = overlay()? {
        let V::Map(overlay) = overlay.value() else {
            return refuse("the overlay is not a plain dict");
        };
        let empty = V::Map(IndexMap::new());
        let rooms = map_of(overlay.get("temporary_rooms").unwrap_or(&empty))?;
        if !rooms.is_empty() {
            let counts = admission_counts(overlay.get("room_admissions").unwrap_or(&empty));
            let mut active_ids: HashSet<Arc<str>> = HashSet::new();
            for row in rooms.values() {
                let V::Map(row) = row else {
                    continue;
                };
                if !matches!(row.get("zone_id"), Some(V::Str(zone)) if &**zone == zone_id) {
                    continue;
                }
                let room_id = str_of(row, "room_id")?;
                let held = counts.get(&**room_id).copied().unwrap_or(0);
                let people = int_of(row, "people_capacity")?;
                if flag_of(row, "active")? {
                    if rooms_count {
                        room_capacity = add(room_capacity, people)?;
                    }
                    if active_ids.insert(room_id.clone()) {
                        active_admissions = add(active_admissions, held)?;
                    }
                } else {
                    expired_capacity = add(expired_capacity, people)?;
                    expired_admissions = add(expired_admissions, held)?;
                }
            }
        }
    }
    Ok(Capacity {
        effective,
        active_people,
        expired_people,
        retained,
        sleeping,
        room_capacity,
        active_admissions,
        retained_rooms: expired_capacity.min(expired_admissions),
    })
}

/// `world.zones[zone_id].capacity`: Unsupported for a zone the world does not have.
pub fn nominal_capacity(world: &World, zone_id: &str) -> Result<Option<i64>> {
    let rows = zone_rows(world)?;
    let Some(zone) = rows.zones.get(zone_id) else {
        return refuse("no such zone");
    };
    Ok(zone.capacity)
}

/// `capacity_metrics`' reads of a zone: its nominal capacity and what the spaces and rooms add, or
/// None for a zone without a capacity.
pub fn zone_capacity(world: &World, places: &Places<'_>, zone_id: &str, occupancy: Option<i64>) -> Result<Option<(i64, Capacity)>> {
    let Some(nominal) = nominal_capacity(world, zone_id)? else {
        return Ok(None);
    };
    Ok(Some((nominal, capacity(world, places, zone_id, nominal, occupancy)?)))
}

/// `incoming_travelers`: the people moving to a facility of the zone, or to the zone itself.
pub fn incoming_travelers(world: &World, zone_id: &str, excluding: Option<&str>) -> Result<i64> {
    let rows = world.rows()?;
    let mut count = 0;
    for (agent_id, agent) in rows.agents.iter() {
        if excluding == Some(&**agent_id) || agent.activity != Activity::Moving {
            continue;
        }
        let Some(target) = &agent.target_id else {
            continue;
        };
        let place = rows.facilities.get(target).map_or(target, |facility| &facility.zone_id);
        count += i64::from(&**place == zone_id);
    }
    Ok(count)
}

// ---- Beds and intake ----------------------------------------------------------------------------

/// `release_sleep_bed` (`part` "bed_reservations") and `release_temporary_room_admission`
/// ("room_admissions"): whether the person held one. `agent_id` is None for an ID that is not a str.
pub fn release(world: &World, overlay: &Held, part: &str, agent_id: Option<&str>) -> Result<bool> {
    let Some(state) = state_of(world, OVERLAY_KEY, overlay)? else {
        return Ok(false);
    };
    let Some(held) = child(&state.0, part)? else {
        return Ok(false);
    };
    let V::Map(held) = held else {
        return refuse("a value is not a plain dict");
    };
    let Some(agent_id) = agent_id else {
        return refuse("a value is not a str");
    };
    if !held.contains_key(agent_id) {
        return Ok(false);
    }
    drop(state);
    let mut row = world.row_mut::<ValueRow>(OVERLAY_KEY)?;
    let removed = dict_mut(required_mut(&mut row.0, part)?)?.shift_remove(agent_id);
    Ok(removed.is_some_and(|value| value != V::Null))
}

/// `{room_id: count}` of the room admissions, as `available_temporary_room` and `capacity_metrics`.
fn admission_counts(admissions: &V) -> HashMap<Arc<str>, i64> {
    let mut counts = HashMap::new();
    if let V::Map(admissions) = admissions {
        for row in admissions.values() {
            if let V::Map(row) = row {
                if let Some(V::Str(room_id)) = row.get("room_id") {
                    *counts.entry(room_id.clone()).or_insert(0) += 1;
                }
            }
        }
    }
    counts
}

fn sorted_rooms(rooms: &Doc) -> Vec<(&Arc<str>, &V)> {
    let mut sorted: Vec<(&Arc<str>, &V)> = rooms.iter().collect();
    sorted.sort_by(|first, second| first.0.as_bytes().cmp(second.0.as_bytes()));
    sorted
}

/// What the intake of a room reads beside the world's rows.
pub struct Intake<'a> {
    pub overlay: &'a Held,
    pub movement: &'a Held,
    pub schema: &'a Kept<Option<i64>>,
    pub geo: Option<&'a Geo>,
}

/// `state.get("temporary_rooms", {})` and `state.get("room_admissions", {})` of the overlay, copied.
struct RoomParts {
    rooms: V,
    admissions: V,
}

impl RoomParts {
    /// Of the overlay's row; None for no overlay.
    fn of_world(world: &World, overlay: &Held) -> Result<Option<Self>> {
        let Some(state) = state_of(world, OVERLAY_KEY, overlay)? else {
            return Ok(None);
        };
        let V::Map(state) = &state.0 else {
            return refuse("a value is not a plain dict");
        };
        let part = |name: &str| state.get(name).cloned().unwrap_or_else(|| V::Map(IndexMap::new()));
        Ok(Some(Self { rooms: part("temporary_rooms"), admissions: part("room_admissions") }))
    }
}

/// `room_admission_point`, after the admissions a plan has given this far: `(room, point)`, the
/// point being None in a world that keeps no positions.
fn admission_point_after(world: &World, intake: &Intake<'_>, state: &RoomParts, room_id: &str, planned: &[(Arc<str>, Option<Pt>)]) -> Result<Option<Pt>> {
    let rooms = &state.rooms;
    let Some(V::Map(room)) = map_of(rooms)?.get(room_id) else {
        return if map_of(rooms)?.contains_key(room_id) { refuse("a room is another value") } else { Ok(None) };
    };
    let polygon = match room.get("polygon_m") {
        Some(value) => points_in_value(value)?,
        None => Vec::new(),
    };
    if polygon.len() < 3 {
        return Ok(None);
    }
    let lowest = |value: fn(&Pt) -> f64| polygon.iter().map(value).fold(f64::INFINITY, py_min);
    let highest = |value: fn(&Pt) -> f64| polygon.iter().map(value).fold(f64::NEG_INFINITY, py_max);
    let (min_x, max_x, min_y, max_y) = (lowest(|point| point.x), highest(|point| point.x), lowest(|point| point.y), highest(|point| point.y));
    let movement = movement_of(world, intake.movement)?;
    let mut occupied = Vec::new();
    if let V::Map(admissions) = &state.admissions {
        for (agent_id, admission) in admissions {
            let V::Map(admission) = admission else {
                continue;
            };
            if !matches!(admission.get("room_id"), Some(V::Str(id)) if &**id == room_id) {
                continue;
            }
            if let Some(point) = position_of(movement.as_deref(), agent_id)? {
                occupied.push(point);
            }
        }
    }
    drop(movement);
    for (other, point) in planned {
        if &**other == room_id {
            occupied.extend(point.iter().copied());
        }
    }
    let mut candidates = match room.get("bed_points_m") {
        Some(value) => points_in_value(value)?,
        None => Vec::new(),
    };
    let mut inspected = 0;
    let mut x = min_x + ADMISSION_STEP / 2.0;
    while x < max_x && inspected < MAX_ADMISSION_POINTS {
        let mut y = min_y + ADMISSION_STEP / 2.0;
        while y < max_y && inspected < MAX_ADMISSION_POINTS {
            let candidate = Pt { x, y };
            if point_in_polygon(candidate, &polygon) {
                candidates.push(candidate);
            }
            inspected += 1;
            y += ADMISSION_STEP;
        }
        x += ADMISSION_STEP;
    }
    // {(round(x, 8), round(y, 8)): point}: the later point in the earlier place, -0.0 being 0.0.
    let mut places: HashMap<(u64, u64), usize> = HashMap::with_capacity(candidates.len());
    let mut unique: Vec<Pt> = Vec::with_capacity(candidates.len());
    let bits = |value: f64| (value + 0.0).to_bits();
    for point in candidates {
        let not_plain = || Error::unsupported("a round Python decides");
        let key = (bits(py_round(point.x, 8).ok_or_else(not_plain)?), bits(py_round(point.y, 8).ok_or_else(not_plain)?));
        match places.get(&key) {
            Some(&place) => unique[place] = point,
            None => {
                places.insert(key, unique.len());
                unique.push(point);
            }
        }
    }
    let count = polygon.len() as f64;
    let center = Pt {
        x: py_sum(&polygon.iter().map(|point| point.x).collect::<Vec<_>>()) / count,
        y: py_sum(&polygon.iter().map(|point| point.y).collect::<Vec<_>>()) / count,
    };
    let mut ordered = Vec::with_capacity(unique.len());
    for point in unique {
        ordered.push((squared_distance(point, center)?, point));
    }
    if ordered.iter().any(|(key, _)| !key.is_finite()) {
        return refuse("a distance is not finite");
    }
    ordered.sort_by(|first, second| {
        let (a, b) = (first.1, second.1);
        first.0.partial_cmp(&second.0).unwrap_or(std::cmp::Ordering::Equal).then(a.x.partial_cmp(&b.x).unwrap_or(std::cmp::Ordering::Equal)).then(a.y.partial_cmp(&b.y).unwrap_or(std::cmp::Ordering::Equal))
    });
    let radius = square(ADMISSION_RADIUS)?;
    let zone_id = match room.get("zone_id") {
        Some(V::Str(zone_id)) => zone_id.clone(),
        _ => return refuse("a room's zone is not a str"),
    };
    let geo = plan(intake.geo)?;
    for (_, candidate) in ordered.into_iter().take(MAX_ADMISSION_POINTS) {
        let mut taken = false;
        for other in &occupied {
            if squared_distance(candidate, *other)? < radius {
                taken = true;
                break;
            }
        }
        if taken {
            continue;
        }
        if geo.room_route(&zone_id, Some(candidate), candidate, "independent").map_err(Error::unsupported)?.is_some() {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

/// `room_admission_point`.
pub fn room_admission_point(world: &World, intake: &Intake<'_>, room_id: &str) -> Result<Option<Pt>> {
    let Some(state) = RoomParts::of_world(world, intake.overlay)? else {
        return Ok(None);
    };
    admission_point_after(world, intake, &state, room_id, &[])
}

/// `available_temporary_room`, after the admissions a plan has given this far, with the point
/// `admit_to_temporary_room` then gives.
pub fn available_room_after(world: &World, intake: &Intake<'_>, zone_id: &str, planned: &[(Arc<str>, Option<Pt>)]) -> Result<Option<(Arc<str>, Pt)>> {
    if !matches!(schema_of(intake.schema)?, Some(3 | 4)) {
        return Ok(None);
    }
    let Some(state) = RoomParts::of_world(world, intake.overlay)? else {
        return Ok(None);
    };
    let mut counts = admission_counts(&state.admissions);
    for (room_id, _) in planned {
        *counts.entry(room_id.clone()).or_insert(0) += 1;
    }
    for (room_id, room) in sorted_rooms(map_of(&state.rooms)?) {
        let room = map_of(room)?;
        if !matches!(room.get("active"), Some(V::Bool(true))) || !matches!(room.get("zone_id"), Some(V::Str(zone)) if &**zone == zone_id) {
            continue;
        }
        let capacity = match room.get("people_capacity") {
            Some(V::Int(capacity)) => *capacity,
            None => 0,
            _ => return refuse("a room's capacity is not a plain int"),
        };
        if counts.get(room_id).copied().unwrap_or(0) < capacity {
            if let Some(point) = admission_point_after(world, intake, &state, room_id, planned)? {
                return Ok(Some((room_id.clone(), point)));
            }
        }
    }
    Ok(None)
}

/// `admit_to_temporary_room`: the person's point in the room.
pub fn admit(world: &World, intake: &Intake<'_>, agent_id: &str, room_id: &str, minute: i64, event_id: &str) -> Result<Pt> {
    let Some(state) = state_of(world, OVERLAY_KEY, intake.overlay)? else {
        return invalid(NOT_AVAILABLE);
    };
    let Some(admissions) = child(&state.0, "room_admissions")? else {
        return invalid(NOT_AVAILABLE);
    };
    let V::Map(admissions) = admissions else {
        return refuse("a value is not a plain dict");
    };
    let admissions = V::Map(admissions.clone());
    let rooms = part(&state.0, "temporary_rooms")?;
    drop(state);
    let room = match map_of(&rooms)?.get(room_id) {
        Some(V::Map(room)) => room,
        None => return invalid(NOT_AVAILABLE),
        Some(_) => return refuse("a room is another value"),
    };
    let known = world.rows()?.agents.contains_key(agent_id);
    if !matches!(room.get("active"), Some(V::Bool(true))) || minute < 0 || !known {
        return invalid(NOT_AVAILABLE);
    }
    let parts = RoomParts { rooms: rooms.clone(), admissions };
    let Some(point) = admission_point_after(world, intake, &parts, room_id, &[])? else {
        return invalid(NOT_AVAILABLE);
    };
    let sources = list_of(room.get("source_ids").unwrap_or(&V::Null))?;
    let mut names = vec![INTAKE_SOURCE];
    for source in sources {
        let V::Str(source) = source else {
            return refuse("a source is not a str");
        };
        names.push(source);
    }
    let record = doc([
        ("agent_id", text(agent_id)),
        ("room_id", text(room_id)),
        ("admitted_minute", V::Int(minute)),
        ("event_id", text(event_id)),
        ("source_ids", id_list(sorted_set(names))),
    ]);
    let mut row = world.row_mut::<ValueRow>(OVERLAY_KEY)?;
    dict_mut(required_mut(&mut row.0, "room_admissions")?)?.insert(Arc::from(agent_id), record);
    Ok(point)
}

/// What `reserve_sleep_bed` finds, written or not.
#[derive(Clone, Debug, PartialEq)]
pub struct Bed {
    pub room_id: Arc<str>,
    pub bed_index: i64,
    pub target_id: String,
    pub reserved_minute: i64,
    pub point: Pt,
    pub path: Arc<[Pt]>,
    pub travel_minutes: i64,
    /// A reservation to make: the person has none.
    pub is_new: bool,
}

/// What `reserve_sleep_bed` reads beside the world's rows.
pub struct Beds<'a> {
    pub overlay: &'a Held,
    pub movement: &'a Held,
    pub schema: &'a Kept<Option<i64>>,
    /// The movement section of the configs, for the minutes in the room.
    pub walking: &'a Kept<Movement>,
    pub geo: Option<&'a Geo>,
}

/// `reserve_sleep_bed`, writing only the empty reservations of a state saved before beds were
/// reserved: None for no bed.
pub fn plan_bed(world: &World, beds: &Beds<'_>, agent_id: &str) -> Result<Option<Bed>> {
    let Some(state) = state_of(world, OVERLAY_KEY, beds.overlay)? else {
        return Ok(None);
    };
    let V::Map(fields) = &state.0 else {
        return refuse("a value is not a plain dict");
    };
    let has_reservations = fields.contains_key("bed_reservations");
    drop(state);
    if !matches!(schema_of(beds.schema)?, Some(2..=4)) {
        return Ok(None);
    }
    let Some(zone_id) = world.rows()?.agents.get(agent_id).map(|agent| agent.zone_id.clone()) else {
        return Ok(None);
    };
    if !has_reservations {
        // `state.setdefault("bed_reservations", {})`: a state saved before beds were reserved has
        // none, and the rule writes the empty map even when no bed follows.
        let mut row = world.row_mut::<ValueRow>(OVERLAY_KEY)?;
        dict_mut(&mut row.0)?.insert(Arc::from("bed_reservations"), V::Map(IndexMap::new()));
    }
    let (reserved, rooms_value) = {
        let state = state_of(world, OVERLAY_KEY, beds.overlay)?.ok_or_else(|| Error::unsupported("no overlay"))?;
        let fields = map_of(&state.0)?;
        (fields.get("bed_reservations").cloned().unwrap_or(V::Null), fields.get("temporary_rooms").cloned())
    };
    let geo = plan(beds.geo)?;
    if geo.layout.is_none() {
        return Ok(None);
    }
    let Some(movement) = movement_of(world, beds.movement)? else {
        return refuse("a bed's positions without the movement state");
    };
    let rooms_empty = match &rooms_value {
        Some(rooms) => map_of(rooms)?.is_empty(),
        None => true,
    };
    let start = position_of(Some(&movement), agent_id)?;
    let rows = world.rows()?;
    // Each person in the zone: a cell of the grid the size of a bed's radius.
    let mut by_cell: HashMap<(i64, i64), Vec<Pt>> = HashMap::new();
    for (other_id, other) in rows.agents.iter() {
        if &**other_id == agent_id || other.zone_id != zone_id {
            continue;
        }
        let Some(point) = movement.positions.get(&**other_id).and_then(|position| position.point) else {
            continue;
        };
        if !usable(point.x, point.y) {
            return refuse("a position is not a usable point");
        }
        if !rooms_empty {
            by_cell.entry(cell_of(Pt { x: point.x, y: point.y })?).or_default().push(Pt { x: point.x, y: point.y });
        }
    }
    drop(movement);
    if rooms_empty {
        return Ok(None);
    }
    let reserved = map_of(&reserved)?;
    let mut occupied: HashSet<(Arc<str>, i64)> = HashSet::new();
    for row in reserved.values() {
        if let V::Map(row) = row {
            occupied.insert((str_of(row, "room_id")?.clone(), int_of(row, "bed_index")?));
        }
    }
    let existing = match reserved.get(agent_id) {
        None | Some(V::Null) => None,
        Some(V::Map(row)) => Some(row),
        _ => return refuse("a reservation is another value"),
    };
    let held_room = existing.map(|row| str_of(row, "room_id")).transpose()?;
    let held_index = existing.map(|row| int_of(row, "bed_index")).transpose()?;
    let rooms = rooms_value.ok_or_else(|| Error::unsupported("no rooms"))?;
    let movement = kit::kept(beds.walking)?;
    let radius = square(BED_RADIUS)?;
    let mut attempts = 0;
    let mut best: Option<(i64, Arc<str>, i64, Pt, Arc<[Pt]>)> = None;
    let mut consider = |travel: i64, room_id: &Arc<str>, index: i64, bed: Pt, path: Arc<[Pt]>| {
        let better = best.as_ref().is_none_or(|(known, known_room, known_index, _, _)| {
            (travel, room_id.as_bytes(), index) < (*known, known_room.as_bytes(), *known_index)
        });
        if better {
            best = Some((travel, room_id.clone(), index, bed, path));
        }
    };
    let Some(agent) = rows.agents.get(agent_id) else {
        return refuse("no such agent");
    };
    let (mobility, agent_zone) = (kit::mobility_text(agent), agent.zone_id.clone());
    'rooms: for (room_id, room) in sorted_rooms(map_of(&rooms)?) {
        if attempts >= MAX_BED_ROUTES {
            break;
        }
        let room = map_of(room)?;
        if !matches!(room.get("zone_id"), Some(V::Str(zone)) if *zone == zone_id) {
            continue;
        }
        if existing.is_none() && !flag_of(room, "active")? {
            continue;
        }
        if held_room.is_some_and(|held| held != room_id) {
            continue;
        }
        let beds = match room.get("bed_points_m") {
            Some(beds) => list_of(beds)?,
            None => &[],
        };
        for (index, raw_bed) in beds.iter().enumerate() {
            let index = index as i64;
            if attempts >= MAX_BED_ROUTES {
                break 'rooms;
            }
            if existing.is_none() && occupied.contains(&(room_id.clone(), index)) {
                continue;
            }
            if held_index.is_some_and(|held| held != index) {
                continue;
            }
            let bed = point_in_value(raw_bed)?;
            let cell = cell_of(bed)?;
            let mut used = false;
            'cells: for dx in [-1, 0, 1] {
                for dy in [-1, 0, 1] {
                    for point in by_cell.get(&(cell.0 + dx, cell.1 + dy)).map_or(&[][..], Vec::as_slice) {
                        if squared_distance(*point, bed)? <= radius {
                            used = true;
                            break 'cells;
                        }
                    }
                }
            }
            if used {
                continue;
            }
            attempts += 1;
            let Some(path) = geo.room_route(&zone_id, start, bed, mobility).map_err(Error::unsupported)? else {
                continue;
            };
            let mut lengths = Vec::with_capacity(path.len());
            for pair in path.windows(2) {
                let length = pow(squared_distance(pair[1], pair[0])?, 0.5)?;
                lengths.push(length);
            }
            let travel = if py_sum(&lengths) <= 1e-8 || path.len() < 2 {
                0
            } else {
                let length = polyline_length(&path).ok_or_else(|| Error::unsupported("not a plain sum"))?;
                walking::room_minutes(&movement.model, geo, &rows, &agent_zone, mobility, length)?
            };
            consider(travel, room_id, index, bed, path);
            if attempts >= MAX_BED_ROUTES {
                break 'rooms;
            }
        }
    }
    let Some((travel_minutes, room_id, bed_index, point, path)) = best else {
        return Ok(None);
    };
    let minute = world.minute_index()?;
    let reserved_minute = match existing {
        Some(row) => int_of(row, "reserved_minute")?,
        None => minute,
    };
    let target_id = bed_target_id(&room_id, bed_index);
    Ok(Some(Bed { room_id, bed_index, target_id, reserved_minute, point, path, travel_minutes, is_new: existing.is_none() }))
}

/// `(math.floor(x / 0.75), math.floor(y / 0.75))`.
fn cell_of(point: Pt) -> Result<(i64, i64)> {
    let (x, y) = ((point.x / BED_RADIUS).floor(), (point.y / BED_RADIUS).floor());
    const LIMIT: f64 = 4_503_599_627_370_496.0;
    if x.abs() >= LIMIT || y.abs() >= LIMIT {
        return refuse("a cell of the grid is too far");
    }
    Ok((x as i64, y as i64))
}

/// The reservation of `plan_bed`, when it is new.
pub fn write_bed(world: &World, overlay: &Held, agent_id: &str, bed: &Bed) -> Result<()> {
    if !bed.is_new {
        return Ok(());
    }
    if state_of(world, OVERLAY_KEY, overlay)?.is_none() {
        return refuse("no overlay");
    }
    let record = doc([
        ("agent_id", text(agent_id)),
        ("room_id", V::Str(bed.room_id.clone())),
        ("bed_index", V::Int(bed.bed_index)),
        ("target_id", text(&bed.target_id)),
        ("reserved_minute", V::Int(bed.reserved_minute)),
    ]);
    let mut row = world.row_mut::<ValueRow>(OVERLAY_KEY)?;
    dict_mut(required_mut(&mut row.0, "bed_reservations")?)?.insert(Arc::from(agent_id), record);
    Ok(())
}

// ---- Facilities installed during a Run ----------------------------------------------------------

/// `installed_facility_placement_enabled`: schema 4, and an overlay state that is a dict (the PyO3
/// crate says whether `model_state` holds one).
pub fn placement_enabled(schema: &Kept<Option<i64>>, overlay_is_dict: bool) -> Result<bool> {
    if schema_of(schema)? != Some(4) {
        return Ok(false);
    }
    Ok(overlay_is_dict)
}

/// `facility_grid_point`.
fn grid_point(polygon: &[Pt], index: usize, count: usize) -> Pt {
    let min_x = polygon.iter().map(|point| point.x).fold(f64::INFINITY, py_min);
    let max_x = polygon.iter().map(|point| point.x).fold(f64::NEG_INFINITY, py_max);
    let min_y = polygon.iter().map(|point| point.y).fold(f64::INFINITY, py_min);
    let max_y = polygon.iter().map(|point| point.y).fold(f64::NEG_INFINITY, py_max);
    let columns = (count as f64).sqrt().ceil() as usize;
    let rows = (count as f64 / columns as f64).ceil() as usize;
    let (column, row) = (index % columns, index / columns);
    let x_fraction = if count == 1 { 0.35 } else { (column + 1) as f64 / (columns + 1) as f64 };
    let y_fraction = 0.68 + 0.28 * (row + 1) as f64 / (rows + 1) as f64;
    Pt { x: min_x + (max_x - min_x) * x_fraction, y: min_y + (max_y - min_y) * y_fraction }
}

/// `installed_facility_point`.
pub fn installed_point(world: &World, overlay: &Held, geo: Option<&Geo>, facility_id: &str) -> Result<Option<Pt>> {
    let rows = world.rows()?;
    let geo = plan(geo)?;
    let Some(layout) = &geo.layout else {
        return Ok(None);
    };
    let Some(facility) = rows.facilities.get(facility_id) else {
        return Ok(None);
    };
    let zone_id = facility.zone_id.clone();
    let mut others = Vec::new();
    let mut ids: Vec<&Arc<str>> = rows.facilities.iter().map(|(id, _)| id).collect();
    ids.sort_unstable_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    for id in ids {
        if &**id == facility_id || rows.facilities.get(id).is_none_or(|item| item.zone_id != zone_id) {
            continue;
        }
        if let Some(point) = layout.facility_points.get(&**id) {
            others.push(*point);
        }
    }
    drop(rows);
    let Some(zone) = layout.zones.get(&*zone_id) else {
        return Ok(None);
    };
    let mut blocked: Vec<Vec<Pt>> = geo.constraints.as_ref().map_or_else(Vec::new, |constraints| constraints.blocked.clone());
    if let Some(state) = state_of(world, OVERLAY_KEY, overlay)? {
        let rooms = part(&state.0, "temporary_rooms")?;
        for room in map_of(&rooms)?.values() {
            let room = map_of(room)?;
            if flag_of(room, "active")? {
                blocked.push(polygon_of(room.get("polygon_m"))?);
            }
        }
    }
    for count in others.len() + 1..others.len() + 1 + GRID_GROWTH {
        for index in 0..count {
            let point = grid_point(&zone.polygon, index, count);
            if !usable(point.x, point.y) {
                return refuse("a grid point is not a usable point");
            }
            if point_walkable(point, &zone.polygon, &blocked)
                && others.iter().all(|other| hypot(&[point.x - other.x, point.y - other.y]) >= MIN_SEPARATION)
                && geo.room_route(&zone_id, None, point, "independent").map_err(Error::unsupported)?.is_some()
            {
                return Ok(Some(point));
            }
        }
    }
    Ok(None)
}

/// `place_installed_facility`: the facility's point, recorded with a history row.
pub fn place_facility(world: &World, overlay: &Held, geo: Option<&Geo>, facility_id: &str, event_id: &str, source_ids: &[&str]) -> Result<Pt> {
    let Some(point) = installed_point(world, overlay, geo, facility_id)? else {
        return invalid(&format!("no free floor-plan point for installed facility {}", py_repr(facility_id)));
    };
    {
        let state = state_of(world, OVERLAY_KEY, overlay)?.ok_or_else(|| Error::unsupported("no overlay"))?;
        if !matches!(required(&state.0, "facility_points")?, V::Map(_)) {
            return refuse("a value is not a plain dict");
        }
        if !matches!(required(&state.0, "history")?, V::List(_)) {
            return refuse("a value is not a plain list");
        }
    }
    let minute = world.minute_index()?;
    let names = sorted_set(source_ids.iter().copied().chain([PLACEMENT_SOURCE]));
    let record = doc([("point_m", point_doc(point)), ("event_id", text(event_id)), ("source_ids", id_list(names.iter().copied())), ("minute", V::Int(minute))]);
    let transition = format!("install-{}", hex_of(&format!("{event_id}:{facility_id}")));
    let entry = doc([
        ("transition_id", text(&transition)),
        ("op", text("install_facility")),
        ("target_id", text(facility_id)),
        ("status", text("applied")),
        ("causation_id", text(event_id)),
        ("event_id", text(event_id)),
        ("source_ids", id_list(names.iter().copied())),
        ("minute", V::Int(minute)),
    ]);
    let mut row = world.row_mut::<ValueRow>(OVERLAY_KEY)?;
    dict_mut(required_mut(&mut row.0, "facility_points")?)?.insert(Arc::from(facility_id), record);
    append_bounded(list_mut(required_mut(&mut row.0, "history")?)?, entry, MAX_OVERLAY_HISTORY);
    Ok(point)
}

// ---- The geometry the checks of a change read: region/facility.py --------------------------------

fn sides(polygon: &[Pt]) -> impl Iterator<Item = (Pt, Pt)> + '_ {
    polygon.iter().copied().zip(polygon.iter().copied().skip(1).chain(polygon.first().copied()))
}

/// `_point_on_segment`.
fn point_on_segment(point: Pt, start: Pt, end: Pt) -> bool {
    let cross = (point.x - start.x) * (end.y - start.y) - (point.y - start.y) * (end.x - start.x);
    let within = py_min(start.x, end.x) - 1e-8 <= point.x
        && point.x <= py_max(start.x, end.x) + 1e-8
        && py_min(start.y, end.y) - 1e-8 <= point.y
        && point.y <= py_max(start.y, end.y) + 1e-8;
    cross.abs() <= 1e-8 && within
}

/// Whether `_segment_intersection_points` finds any.
fn segments_meet(first_start: Pt, first_end: Pt, second_start: Pt, second_end: Pt) -> bool {
    let cross = |ax: f64, ay: f64, bx: f64, by: f64| ax * by - ay * bx;
    let (rx, ry) = (first_end.x - first_start.x, first_end.y - first_start.y);
    let (sx, sy) = (second_end.x - second_start.x, second_end.y - second_start.y);
    let denominator = cross(rx, ry, sx, sy);
    let (qpx, qpy) = (second_start.x - first_start.x, second_start.y - first_start.y);
    if denominator.abs() <= 1e-9 {
        if cross(qpx, qpy, rx, ry).abs() > 1e-9 {
            return false;
        }
        return [first_start, first_end, second_start, second_end]
            .iter()
            .any(|point| point_on_segment(*point, first_start, first_end) && point_on_segment(*point, second_start, second_end));
    }
    let t = cross(qpx, qpy, sx, sy) / denominator;
    let u = cross(qpx, qpy, rx, ry) / denominator;
    (-1e-9..=1.0 + 1e-9).contains(&t) && (-1e-9..=1.0 + 1e-9).contains(&u)
}

/// `_point_segment_distance` of region/facility.py: its clamp is the other way round.
fn region_point_distance(point: Pt, start: Pt, end: Pt) -> f64 {
    let (dx, dy) = (end.x - start.x, end.y - start.y);
    let length_squared = dx * dx + dy * dy;
    if length_squared <= 1e-18 {
        return hypot(&[point.x - start.x, point.y - start.y]);
    }
    let fraction = py_max(0.0, py_min(1.0, ((point.x - start.x) * dx + (point.y - start.y) * dy) / length_squared));
    hypot(&[point.x - (start.x + fraction * dx), point.y - (start.y + fraction * dy)])
}

/// `_segment_distance`.
fn segment_distance(start: Pt, end: Pt, other_start: Pt, other_end: Pt) -> f64 {
    if segments_meet(start, end, other_start, other_end) {
        return 0.0;
    }
    let distances = [
        region_point_distance(start, other_start, other_end),
        region_point_distance(end, other_start, other_end),
        region_point_distance(other_start, start, end),
        region_point_distance(other_end, start, end),
    ];
    distances.into_iter().fold(f64::INFINITY, py_min)
}

/// `_segment_distance_to_polygon`.
fn segment_to_polygon(start: Pt, end: Pt, polygon: &[Pt]) -> f64 {
    if point_in_polygon(start, polygon) || point_in_polygon(end, polygon) {
        return 0.0;
    }
    sides(polygon).map(|(first, second)| segment_distance(start, end, first, second)).fold(f64::INFINITY, py_min)
}

/// `_valid_simple_polygon`.
fn valid_simple(polygon: &[Pt]) -> bool {
    if polygon.len() < 3 {
        return false;
    }
    let crosses: Vec<f64> = sides(polygon).map(|(a, b)| a.x * b.y - b.x * a.y).collect();
    if py_sum(&crosses).abs() <= 1e-9 {
        return false;
    }
    let edges: Vec<(Pt, Pt)> = sides(polygon).collect();
    let count = edges.len();
    for (index, &(start, end)) in edges.iter().enumerate() {
        for (other_index, &(other_start, other_end)) in edges.iter().enumerate().skip(index + 1) {
            if other_index == index + 1 || other_index == (index + count - 1) % count {
                continue;
            }
            if segments_meet(start, end, other_start, other_end) {
                return false;
            }
        }
    }
    true
}

/// `_polygons_overlap` of the overlay.
fn polygons_overlap(left: &[Pt], right: &[Pt]) -> bool {
    if left.iter().any(|point| point_in_polygon(*point, right)) || right.iter().any(|point| point_in_polygon(*point, left)) {
        return true;
    }
    sides(left).any(|(a, b)| sides(right).any(|(c, d)| segments_meet(a, b, c, d)))
}

/// `_validate_partition_polygon`, `_validate_room_polygon`: whether the Python rule passes.
fn inside_zone(zone: &[Pt], polygon: &[Pt]) -> bool {
    valid_simple(polygon) && polygon.iter().all(|point| point_in_polygon(*point, zone)) && sides(polygon).all(|(a, b)| segment_inside_polygon(a, b, zone))
}

// ---- validate_geometry_change -------------------------------------------------------------------
//
// Each check raises the Python rule's ValueError with its text, in its order
// (docs/rust-port/lanes/l8-texts-rest.md). What the rows cannot judge as Python does (a value that is
// not a plain point, a flag that is not a bool) says Unsupported. The parameters are read from the
// contract's `model_dump(mode="json")`.

/// What a geometry change reads beside the world's rows.
pub struct Change<'a> {
    pub overlay: &'a Held,
    pub movement: &'a Held,
    pub staff_operations: &'a Held,
    pub resource_service: &'a Held,
    /// The resource service's section of the configs, for the skills a moved station needs.
    pub service: &'a Kept<ResourceService>,
    pub geo: Option<&'a Geo>,
}

/// A field of the contract's dump, as an attribute of the model is read.
fn field<'a>(data: &'a V, name: &str) -> Result<&'a V> {
    match data {
        V::Map(map) => map.get(name).ok_or_else(|| Error::unsupported("an input has no such attribute")),
        _ => refuse("an input has no such attribute"),
    }
}

fn float_field(point: &V, name: &str) -> Result<f64> {
    match field(point, name)? {
        V::Float(number) => Ok(*number),
        _ => refuse("a value is not a float"),
    }
}

fn point_of(point: &V) -> Result<Pt> {
    let (x, y) = (float_field(point, "x")?, float_field(point, "y")?);
    if !usable(x, y) {
        return refuse("a point is not a usable point");
    }
    Ok(Pt { x, y })
}

/// A `Point2D` field of the dump.
fn point_field(data: &V, name: &str) -> Result<Pt> {
    point_of(field(data, name)?)
}

/// A `tuple[Point2D, ...]` field of the dump.
fn points_field(data: &V, name: &str) -> Result<Vec<Pt>> {
    let V::List(points) = field(data, name)? else {
        return refuse("not a tuple");
    };
    points.iter().map(point_of).collect()
}

fn text_field(data: &V, name: &str) -> Result<Arc<str>> {
    Ok(kit::exact_str(field(data, name)?)?.clone())
}

/// A route of `world.zone_edges`.
struct Edge {
    from: Arc<str>,
    to: Arc<str>,
    closed: bool,
}

fn edges_of(world: &World) -> Result<HashMap<Arc<str>, Edge>> {
    let rows = zone_rows(world)?;
    Ok(rows.zone_edges.iter().map(|(id, edge)| (id.clone(), Edge { from: edge.from_zone_id.clone(), to: edge.to_zone_id.clone(), closed: edge.closed })).collect())
}

fn constraints_of_plan(geo: &Geo) -> Result<&Constraints> {
    geo.constraints.as_ref().ok_or_else(|| Error::unsupported("no constraints"))
}

fn layout_of_plan(geo: &Geo) -> Result<&Layout> {
    geo.layout.as_ref().ok_or_else(|| Error::unsupported("no floor plan"))
}

fn walls_are_near(constraints: &Constraints, point: Pt) -> bool {
    constraints.walls.iter().any(|wall| point_segment_distance(point, wall.start, wall.end) < WALL_CLEARANCE)
}

fn walkable_at(geo: &Geo, zone_id: &str, point: Pt) -> Result<bool> {
    Ok(geo.room_route(zone_id, Some(point), point, "independent").map_err(Error::unsupported)?.is_some())
}

/// `_validate_facility_room_clearance`.
fn clear_of_rooms(geo: &Geo, rooms: &Doc, facility_id: &str, zone_id: &str, point: Pt) -> Result<()> {
    let layout = layout_of_plan(geo)?;
    let constraints = constraints_of_plan(geo)?;
    let Some(&old) = layout.facility_points.get(facility_id) else {
        return refuse("the facility has no point");
    };
    let (dx, dy) = (point.x - old.x, point.y - old.y);
    for room in rooms.values() {
        let room = map_of(room)?;
        if !flag_of(room, "active")? || !matches!(room.get("zone_id"), Some(V::Str(zone)) if &**zone == zone_id) {
            continue;
        }
        let polygon = polygon_of(room.get("polygon_m"))?;
        if point_in_polygon(point, &polygon) {
            return invalid("facility_in_use: facility move would enter an active temporary room");
        }
        for service in &layout.service_points {
            if &*service.facility_id == facility_id && point_in_polygon(Pt { x: service.point.x + dx, y: service.point.y + dy }, &polygon) {
                return invalid("facility_in_use: moved service point would enter an active temporary room");
            }
        }
        if let Some((_, footprint)) = constraints.footprints.iter().find(|(id, _)| &**id == facility_id) {
            let shifted: Vec<Pt> = footprint.iter().map(|item| Pt { x: item.x + dx, y: item.y + dy }).collect();
            if polygons_overlap(&shifted, &polygon) {
                return invalid("facility_in_use: moved equipment footprint would enter an active temporary room");
            }
        }
    }
    Ok(())
}

/// A facility with people or a staff task: `facility_in_use`, with the texts of the operation. The
/// facility's zone and kind.
fn facility_is_free(world: &World, change: &Change<'_>, target: &str, busy: &str, tasked: &str) -> Result<(Arc<str>, Arc<str>)> {
    let found = {
        let rows = world.rows()?;
        let Some(facility) = rows.facilities.get(target) else {
            return refuse("the facility is gone");
        };
        if !facility.queue_agent_ids.is_empty()
            || !facility.active_agent_ids.is_empty()
            || !facility.resource_queue_agent_ids.is_empty()
            || !facility.resource_active_agent_ids.is_empty()
        {
            return invalid(busy);
        }
        (facility.zone_id.clone(), facility.kind.clone())
    };
    if let Some(operations) = kit::held_row::<StaffOperationsRow>(world, STAFF_OPERATIONS, change.staff_operations)? {
        if operations.staff.values().any(|record| record.target_id.as_deref() == Some(target)) {
            return invalid(tasked);
        }
    }
    Ok(found)
}

/// `_zones_are_connected`.
fn zones_connected(edges: &HashMap<Arc<str>, Edge>, source: &str, destination: &str) -> bool {
    if source == destination {
        return false;
    }
    let mut next: HashMap<&str, Vec<&str>> = HashMap::new();
    #[allow(clippy::iter_over_hash_type, reason = "only builds adjacency lists; reachability does not depend on their order")]
    for edge in edges.values() {
        if !edge.closed {
            next.entry(&edge.from).or_default().push(&edge.to);
        }
    }
    let mut pending = vec![source];
    let mut visited = HashSet::from([source]);
    while let Some(zone) = pending.pop() {
        for other in next.get(zone).map_or(&[][..], Vec::as_slice) {
            if *other == destination {
                return true;
            }
            if visited.insert(other) {
                pending.push(other);
            }
        }
    }
    false
}

/// `_require_service_staff_in_zone`.
fn service_staff_in_zone(world: &World, change: &Change<'_>, geo: &Geo, facility_id: &str, kind: &str, zone_id: &str) -> Result<()> {
    let water = match kind {
        "water_station" => true,
        "meal_distribution" => false,
        _ => return Ok(()),
    };
    let service = kit::kept(change.service)?;
    if !service.present {
        return Ok(());
    }
    let skills = walking::config_read(service.skills.clone())?;
    // Whether it is there, as it is held: a detached one is not rebuilt for that.
    if matches!(change.resource_service, Held::Absent) {
        return Ok(());
    }
    let Some(skill) = (if water { skills.water } else { skills.meal }) else {
        return Ok(());
    };
    let mut wanted: Vec<Arc<str>> = vec![skill];
    for point in &layout_of_plan(geo)?.service_points {
        if &*point.facility_id == facility_id {
            wanted.extend(point.required_staff_skills.iter().cloned());
        }
    }
    let staffed = world.rows()?.staff.iter().any(|(_, staff)| &*staff.zone_id == zone_id && wanted.iter().all(|name| staff.skills.0.contains(name)));
    if staffed {
        return Ok(());
    }
    // `', '.join(sorted(required))` of a set; str sorts by code point, as UTF-8 bytes do.
    wanted.sort_unstable_by(|a, b| a.as_bytes().cmp(b.as_bytes()));
    wanted.dedup();
    let skills: Vec<&str> = wanted.iter().map(|name| &**name).collect();
    invalid(&format!("staff_unavailable: no staff with {} works in {}, so the moved station would not be served", skills.join(", "), py_repr(zone_id)))
}

/// The corners of a footprint moved by `to - from`, as the operation adds them.
fn shifted_footprint(footprint: &[Pt], from: Pt, to: Pt, transfer: bool) -> Result<Vec<Pt>> {
    footprint
        .iter()
        .map(|point| {
            let shifted = if transfer {
                Pt { x: point.x + (to.x - from.x), y: point.y + (to.y - from.y) }
            } else {
                Pt { x: point.x + to.x - from.x, y: point.y + to.y - from.y }
            };
            if usable(shifted.x, shifted.y) {
                Ok(shifted)
            } else {
                refuse("a moved corner is not a usable point")
            }
        })
        .collect()
}

/// The footprint a transfer gives: inside the zone, past no wall, overlapping nothing.
fn transferred_footprint_fits(constraints: &Constraints, facility_id: &str, zone: &[Pt], from: Pt, to: Pt) -> Result<()> {
    let Some((_, footprint)) = constraints.footprints.iter().find(|(id, _)| &**id == facility_id) else {
        return Ok(());
    };
    let shifted = shifted_footprint(footprint, from, to, true)?;
    if shifted.iter().any(|point| !point_in_polygon(*point, zone)) {
        return invalid("area_invalid: transferred equipment footprint leaves its destination zone");
    }
    if sides(&shifted).any(|(a, b)| constraints.walls.iter().any(|wall| segments_meet(a, b, wall.start, wall.end))) {
        return invalid("clear_width_insufficient: transferred equipment footprint crosses a wall");
    }
    // `item.facility_id` of an obstacle raises AttributeError in the transfer's Python rule.
    if constraints.obstacle_count > 0 {
        return refuse("the Python rule raises on an obstacle");
    }
    let others = constraints.footprints.iter().filter(|(id, _)| &**id != facility_id).map(|(_, polygon)| polygon);
    for shape in others {
        if shifted.iter().any(|point| point_in_polygon(*point, shape))
            || point_in_polygon(shape[0], &shifted)
            || sides(&shifted).any(|(a, b)| segment_to_polygon(a, b, shape) < 1e-8)
        {
            return invalid("area_invalid: transferred equipment overlaps another obstacle");
        }
    }
    Ok(())
}

/// The footprint a same-zone move gives, in the checks' order of the Python rule.
fn moved_footprint_fits(constraints: &Constraints, facility_id: &str, zone: &[Pt], from: Pt, to: Pt) -> Result<()> {
    let Some((_, footprint)) = constraints.footprints.iter().find(|(id, _)| &**id == facility_id) else {
        return Ok(());
    };
    let shifted = shifted_footprint(footprint, from, to, false)?;
    if shifted.iter().any(|point| !point_in_polygon(*point, zone)) {
        return invalid("area_invalid: moved footprint leaves its zone");
    }
    if sides(&shifted).any(|(a, b)| constraints.walls.iter().any(|wall| segments_meet(a, b, wall.start, wall.end))) {
        return invalid("clear_width_insufficient: moved equipment crosses a wall");
    }
    let others: Vec<&Vec<Pt>> = constraints.blocked[..constraints.obstacle_count]
        .iter()
        .chain(constraints.footprints.iter().filter(|(id, _)| &**id != facility_id).map(|(_, polygon)| polygon))
        .collect();
    if others.iter().any(|shape| shifted.iter().any(|point| point_in_polygon(*point, shape)) || point_in_polygon(shape[0], &shifted)) {
        return invalid("area_invalid: moved footprint overlaps existing obstacles or equipment");
    }
    if sides(&shifted).any(|(a, b)| others.iter().any(|shape| segment_to_polygon(a, b, shape) < 1e-8)) {
        return invalid("clear_width_insufficient: moved equipment intersects an obstacle");
    }
    Ok(())
}

/// The contract of an operator's parameters; None for an operator the overlay does not have.
fn contract_of(op: &str) -> Option<Contract> {
    Some(match op {
        "set_route_access" => Contract::RouteAccess,
        "transfer_equipment" => Contract::EquipmentTransfer,
        "move_facility" => Contract::MoveFacility,
        "set_partition" => Contract::Partition,
        "temporary_room_geometry" => Contract::TemporaryRoom,
        _ => return None,
    })
}

/// The overlay is there and a plain dict, and the floor plan has a layout and constraints: the
/// checks before the parameters' contract.
fn change_is_possible<'g>(world: &World, change: &Change<'g>) -> Result<&'g Geo> {
    let Some(state) = state_of(world, OVERLAY_KEY, change.overlay)? else {
        return invalid("geometry_overlay_required: declare the versioned geometry overlay before approving effects");
    };
    if !matches!(state.0, V::Map(_)) {
        return refuse("a value is not a plain dict");
    }
    drop(state);
    let geo = plan(change.geo)?;
    if geo.layout.is_none() {
        return invalid("geometry_unavailable");
    }
    constraints_of_plan(geo)?;
    Ok(geo)
}

/// `validate_geometry_change`: the contract's dump of the parameters when the change may be applied.
/// The contract's own error (policy E15) comes as the host gives it.
pub fn check_change<C: WorldCell, H: Host>(
    cell: &C,
    host: &mut H,
    change: &Change<'_>,
    op: &str,
    target: &str,
    parameters: &V,
    end_minute: Option<i64>,
) -> std::result::Result<V, H::Error> {
    let geo = cell.with(|world| change_is_possible(world, change))??;
    let Some(contract) = contract_of(op) else {
        return Err(Error::value_error("unsupported_geometry_operator").into());
    };
    let data = host.contract(&contract, parameters)?;
    cell.with(|world| check_op(world, change, geo, op, target, &data, end_minute))??;
    Ok(data)
}

/// The checks of an operator, on its parameters' dump.
fn check_op(world: &World, change: &Change<'_>, geo: &Geo, op: &str, target: &str, data: &V, end_minute: Option<i64>) -> Result<()> {
    let layout = layout_of_plan(geo)?;
    let constraints = constraints_of_plan(geo)?;
    let state = state_of(world, OVERLAY_KEY, change.overlay)?.ok_or_else(|| Error::unsupported("no overlay"))?;
    match op {
        "set_route_access" => {
            let V::Bool(accessible) = field(data, "accessible")? else {
                return refuse("accessible is not a bool");
            };
            let edges = edges_of(world)?;
            let Some(edge) = edges.get(target) else {
                return invalid("target_missing: route does not exist");
            };
            if end_minute.is_none() {
                return invalid("expiry_required: route-access changes require a bounded end");
            }
            if *accessible && edge.closed {
                return invalid("unsupported_reopening: this operation cannot override a prior graph closure");
            }
            if !*accessible {
                let rooms = part(&state.0, "temporary_rooms")?;
                for room in map_of(&rooms)?.values() {
                    let room = map_of(room)?;
                    if !flag_of(room, "active")? {
                        continue;
                    }
                    let ids = match room.get("route_edge_ids") {
                        Some(ids) => list_of(ids)?,
                        None => &[],
                    };
                    if !ids.iter().any(|id| matches!(id, V::Str(id) if &**id == target)) {
                        continue;
                    }
                    let room_zone = str_of(room, "zone_id")?;
                    let mut incoming = false;
                    let mut outgoing = false;
                    for id in ids {
                        let V::Str(id) = id else {
                            return refuse("a route's ID is not a str");
                        };
                        if &**id == target {
                            continue;
                        }
                        let (Some(edge), Some(route)) = (edges.get(id), layout.routes.get(&**id)) else {
                            continue;
                        };
                        if edge.closed || !route.accessible {
                            continue;
                        }
                        incoming |= edge.to == *room_zone;
                        outgoing |= edge.from == *room_zone;
                    }
                    if !incoming || !outgoing {
                        return invalid("route_in_use: closure would disconnect an active temporary room");
                    }
                }
            }
            Ok(())
        }
        "transfer_equipment" => {
            let (equipment, from_zone, to_zone) = (text_field(data, "equipment_id")?, text_field(data, "from_zone_id")?, text_field(data, "to_zone_id")?);
            let point = point_field(data, "point_m")?;
            let known = world.rows()?.facilities.get(target).map(|facility| facility.zone_id.clone());
            let (Some(zone_now), Some(&old)) = (known, layout.facility_points.get(target)) else {
                return invalid("target_missing: equipment must identify a positioned facility");
            };
            if &*equipment != target {
                return invalid("target_mismatch: equipment_id must match the selected facility");
            }
            if from_zone != zone_now || !zone_rows(world)?.zones.contains_key(&*to_zone) {
                return invalid("precondition_failed: equipment source/destination zone does not match");
            }
            if end_minute.is_some() {
                return invalid("unsupported_transfer_expiry: equipment transfers are permanent");
            }
            let (_, kind) = facility_is_free(
                world,
                change,
                target,
                "facility_in_use: equipment queues and services must be empty",
                "facility_in_use: an active staff task targets this equipment",
            )?;
            let (Some(_), Some(destination)) = (layout.zones.get(&*from_zone), layout.zones.get(&*to_zone)) else {
                return invalid("target_missing: equipment zone has no surveyed polygon");
            };
            if !point_in_polygon(point, &destination.polygon) {
                return invalid("area_invalid: transfer point must lie inside the destination zone");
            }
            let rooms = part(&state.0, "temporary_rooms")?;
            clear_of_rooms(geo, map_of(&rooms)?, target, &to_zone, point)?;
            if !zones_connected(&edges_of(world)?, &from_zone, &to_zone) {
                return invalid("route_unavailable: equipment zones have no open directed route");
            }
            service_staff_in_zone(world, change, geo, target, &kind, &to_zone)?;
            if walls_are_near(constraints, point) {
                return invalid("clear_width_insufficient: transferred equipment must remain half a metre from walls");
            }
            if !walkable_at(geo, &to_zone, point)? {
                return invalid("clear_width_insufficient: transfer point is not walkable");
            }
            let (dx, dy) = (point.x - old.x, point.y - old.y);
            for service in &layout.service_points {
                if &*service.facility_id != target {
                    continue;
                }
                let shifted = Pt { x: service.point.x + dx, y: service.point.y + dy };
                if !usable(shifted.x, shifted.y) {
                    return refuse("a moved service point is not a usable point");
                }
                if !point_in_polygon(shifted, &destination.polygon) {
                    return invalid("area_invalid: transferred service point leaves its destination zone");
                }
                if walls_are_near(constraints, shifted) || !walkable_at(geo, &to_zone, shifted)? {
                    return invalid("clear_width_insufficient: transferred service point is not walkable");
                }
            }
            transferred_footprint_fits(constraints, target, &destination.polygon, old, point)
        }
        "move_facility" => {
            let point = point_field(data, "point_m")?;
            let known = world.rows()?.facilities.contains_key(target);
            let (true, Some(&old)) = (known, layout.facility_points.get(target)) else {
                return invalid("target_missing: facility has no surveyed position");
            };
            if end_minute.is_some() {
                return invalid("unsupported_move_expiry: same-zone moves are permanent; approve an explicit later move");
            }
            let (zone_id, _) = facility_is_free(
                world,
                change,
                target,
                "facility_in_use: queues and services must be empty",
                "facility_in_use: an active staff task targets this facility",
            )?;
            let Some(zone) = layout.zones.get(&*zone_id) else {
                return refuse("a facility in a zone with no polygon");
            };
            if !point_in_polygon(point, &zone.polygon) {
                return invalid("area_invalid: cross-zone facility moves are unsupported");
            }
            let rooms = part(&state.0, "temporary_rooms")?;
            clear_of_rooms(geo, map_of(&rooms)?, target, &zone_id, point)?;
            if walls_are_near(constraints, point) {
                return invalid("clear_width_insufficient: moved service point requires half a metre from walls");
            }
            for service in &layout.service_points {
                if &*service.facility_id != target {
                    continue;
                }
                let shifted = Pt { x: service.point.x + point.x - old.x, y: service.point.y + point.y - old.y };
                if !usable(shifted.x, shifted.y) {
                    return refuse("a moved service point is not a usable point");
                }
                if !point_in_polygon(shifted, &zone.polygon) {
                    return invalid("area_invalid: moved facility would move a service point outside its zone");
                }
                if walls_are_near(constraints, shifted) {
                    return invalid("clear_width_insufficient: moved service point is too close to a wall");
                }
                if !walkable_at(geo, &zone_id, shifted)? {
                    return invalid("clear_width_insufficient: moved service point is not walkable");
                }
            }
            if !walkable_at(geo, &zone_id, point)? {
                return invalid("clear_width_insufficient: moved service point is not walkable");
            }
            moved_footprint_fits(constraints, target, &zone.polygon, old, point)
        }
        "set_partition" => {
            let polygon = points_field(data, "polygon_m")?;
            let Some(zone) = layout.zones.get(target) else {
                return invalid("target_missing: zone does not exist");
            };
            if end_minute.is_none() {
                return invalid("expiry_required: partitions require an explicit bounded end");
            }
            // `_validate_partition_polygon`.
            if !valid_simple(&polygon) || polygon.iter().any(|point| !point_in_polygon(*point, &zone.polygon)) {
                return invalid("area_invalid: partition requires a simple positive-area polygon inside its zone");
            }
            if sides(&polygon).any(|(a, b)| !segment_inside_polygon(a, b, &zone.polygon)) {
                return invalid("area_invalid: partition leaves a concave zone");
            }
            if layout.facility_points.values().any(|point| point_in_polygon(*point, &polygon)) {
                return invalid("area_invalid: partition covers a facility");
            }
            for shape in &constraints.blocked {
                if polygon.iter().any(|point| point_in_polygon(*point, shape))
                    || point_in_polygon(shape[0], &polygon)
                    || sides(&polygon).any(|(a, b)| segment_to_polygon(a, b, shape) < 1e-8)
                {
                    return invalid("area_invalid: partition overlaps equipment or another obstacle");
                }
            }
            let movement = movement_of(world, change.movement)?;
            if let Some(row) = &movement {
                for position in row.positions.values() {
                    if let Some(point) = position.point {
                        if !usable(point.x, point.y) {
                            return refuse("a position is not a usable point");
                        }
                        if point_in_polygon(Pt { x: point.x, y: point.y }, &polygon) {
                            return invalid("route_in_use: partition covers a current person");
                        }
                    }
                }
            }
            let staff_zones: Vec<Arc<str>> = world.rows()?.staff.iter().map(|(_, staff)| staff.zone_id.clone()).collect();
            for zone_id in staff_zones {
                if layout.zones.get(&*zone_id).is_some() {
                    if let Some(centre) = geo.center(&zone_id)? {
                        if point_in_polygon(centre, &polygon) {
                            return invalid("route_in_use: partition covers a staff standing point");
                        }
                    }
                }
            }
            for route in layout.routes.values() {
                if route.points.windows(2).any(|pair| !segment_clear_of_polygon(pair[0], pair[1], &polygon, route.width / 2.0)) {
                    return invalid("clear_width_insufficient: partition obstructs a declared passage");
                }
            }
            if let Some(row) = &movement {
                for journey in row.journeys.values() {
                    let points: Vec<Pt> = journey.route_points.iter().map(|point| Pt { x: point.x, y: point.y }).collect();
                    if points.iter().any(|point| !usable(point.x, point.y)) {
                        return refuse("a route point is not a usable point");
                    }
                    if points.windows(2).any(|pair| !segment_clear_of_polygon(pair[0], pair[1], &polygon, WALL_CLEARANCE))
                        && !walking::is_queue_trip(&journey.purpose)
                    {
                        return invalid("route_in_use: partition intersects current room travel");
                    }
                }
            }
            Ok(())
        }
        "temporary_room_geometry" => {
            let polygon = points_field(data, "polygon_m")?;
            let beds = points_field(data, "bed_points_m")?;
            let room_id = text_field(data, "room_id")?;
            let zone_id = text_field(data, "zone_id")?;
            let V::List(route_ids) = field(data, "route_edge_ids")? else {
                return refuse("not a tuple");
            };
            let route_ids: Vec<Arc<str>> = route_ids.iter().map(|id| Ok(kit::exact_str(id)?.clone())).collect::<Result<_>>()?;
            let duration = world.duration_minutes()?;
            if &*zone_id != target {
                return invalid("target_mismatch: temporary room zone must match its selected zone");
            }
            if end_minute.is_none_or(|end| end >= duration) {
                return invalid("expiry_required: temporary rooms need an in-run expiry");
            }
            let Some(zone) = layout.zones.get(target) else {
                return invalid("target_missing: temporary room zone has no surveyed polygon");
            };
            // `_validate_room_polygon`.
            if !valid_simple(&polygon) {
                return invalid("area_invalid: temporary room polygon must be simple and nondegenerate");
            }
            if polygon.iter().any(|point| !point_in_polygon(*point, &zone.polygon)) {
                return invalid("area_invalid: temporary room polygon must lie inside its zone");
            }
            if sides(&polygon).any(|(a, b)| !segment_inside_polygon(a, b, &zone.polygon)) {
                return invalid("area_invalid: temporary room polygon crosses the zone boundary");
            }
            let edges = edges_of(world)?;
            for id in &route_ids {
                let (Some(edge), Some(route)) = (edges.get(id), layout.routes.get(&**id)) else {
                    return invalid("route_unavailable: temporary room access route must connect to its zone");
                };
                if &*edge.from != target && &*edge.to != target {
                    return invalid("route_unavailable: temporary room access route must connect to its zone");
                }
                if edge.closed || !route.accessible {
                    return invalid("route_unavailable: temporary room access route is closed or inaccessible");
                }
            }
            let incoming = route_ids.iter().any(|id| &*edges[id].to == target);
            let outgoing = route_ids.iter().any(|id| &*edges[id].from == target);
            if !incoming || !outgoing {
                return invalid("route_unavailable: temporary room requires directed ingress and egress routes");
            }
            let rooms = part(&state.0, "temporary_rooms")?;
            let rooms = map_of(&rooms)?;
            if rooms.contains_key(&*room_id) {
                return invalid("room_exists: temporary room ID already exists");
            }
            for row in rooms.values() {
                let row = map_of(row)?;
                if flag_of(row, "active")? && matches!(row.get("zone_id"), Some(V::Str(zone)) if &**zone == target) && polygons_overlap(&polygon, &polygon_of(row.get("polygon_m"))?) {
                    return invalid("area_invalid: temporary room overlaps another active room");
                }
            }
            if layout.facility_points.values().any(|point| point_in_polygon(*point, &polygon)) {
                return invalid("area_invalid: temporary room covers a facility");
            }
            let reservations = part(&state.0, "bed_reservations")?;
            for reservation in map_of(&reservations)?.values() {
                let reservation = map_of(reservation)?;
                let Some(V::Str(room)) = reservation.get("room_id") else {
                    continue;
                };
                let Some(V::Map(room)) = rooms.get(room) else {
                    continue;
                };
                let held = match room.get("bed_points_m") {
                    Some(held) => list_of(held)?,
                    None => &[],
                };
                if let Some(V::Int(index)) = reservation.get("bed_index") {
                    if *index >= 0 && (*index as usize) < held.len() && point_in_polygon(point_in_value(&held[*index as usize])?, &polygon) {
                        return invalid("room_in_use: temporary room would cover a reserved sleep bed");
                    }
                }
            }
            if constraints.blocked.iter().any(|shape| polygons_overlap(&polygon, shape)) {
                return invalid("area_invalid: temporary room overlaps an obstacle or equipment footprint");
            }
            if sides(&polygon).any(|(a, b)| constraints.walls.iter().any(|wall| segments_meet(a, b, wall.start, wall.end))) {
                return invalid("area_invalid: temporary room boundary crosses a declared wall");
            }
            if let Some(row) = movement_of(world, change.movement)? {
                for (positions, text) in [
                    (&row.positions, "room_in_use: temporary room would cover a current person"),
                    (&row.staff_positions, "room_in_use: temporary room would cover a current staff member"),
                ] {
                    for position in positions.values() {
                        if let Some(point) = position.point {
                            if !usable(point.x, point.y) {
                                return refuse("a position is not a usable point");
                            }
                            if point_in_polygon(Pt { x: point.x, y: point.y }, &polygon) {
                                return invalid(text);
                            }
                        }
                    }
                }
            }
            let mut access = Vec::with_capacity(route_ids.len() + 1);
            for id in &route_ids {
                let (edge, route) = (&edges[id], &layout.routes[&**id]);
                let point = if &*edge.to == target { route.points[route.points.len() - 1] } else { route.points[0] };
                access.push(point);
            }
            if let Some(center) = geo.center(target)? {
                access.push(center);
            }
            for bed in &beds {
                let mut reachable = false;
                for entry in &access {
                    if geo.room_route(target, Some(*entry), *bed, "independent").map_err(Error::unsupported)?.is_some() {
                        reachable = true;
                        break;
                    }
                }
                if !reachable {
                    return invalid("route_unavailable: every bed must be reachable from a room access point");
                }
            }
            Ok(())
        }
        _ => invalid("unsupported_geometry_operator"),
    }
}

// ---- apply_geometry_change ----------------------------------------------------------------------

/// `{..first, **common, "end_minute": end}`.
fn stamped_record<const N: usize>(first: [(&str, V); N], event_id: &str, sources: &[&str], minute: i64, end: Option<V>) -> V {
    let mut map: Doc = first.into_iter().map(|(key, value)| (Arc::from(key), value)).collect();
    map.insert(Arc::from("event_id"), text(event_id));
    map.insert(Arc::from("source_ids"), id_list(sources.iter().copied()));
    map.insert(Arc::from("minute"), V::Int(minute));
    if let Some(end) = end {
        map.insert(Arc::from("end_minute"), end);
    }
    V::Map(map)
}

/// `apply_geometry_change`: the event's writes on this world, after its checks (none for an
/// expiry).
pub fn apply_change<C: WorldCell, H: Host>(cell: &C, host: &mut H, change: &Change<'_>, event: &EventRow, expire: bool) -> std::result::Result<(), H::Error> {
    let V::Map(payload) = &event.payload else {
        return Err(Error::unsupported("the payload is not a dict").into());
    };
    let (op, target, modifier) = (str_of(payload, "op")?, str_of(payload, "target_id")?, str_of(payload, "modifier_id")?);
    cell.with(|world| -> Result<()> {
        let Some(state) = state_of(world, OVERLAY_KEY, change.overlay)? else {
            return invalid("geometry_overlay_required");
        };
        if !matches!(state.0, V::Map(_)) {
            return refuse("a value is not a plain dict");
        }
        Ok(())
    })??;
    let Some(raw_parameters) = payload.get("parameters") else {
        return Err(Error::unsupported("no parameters").into());
    };
    let end_minute = match payload.get("end_minute") {
        Some(V::Int(minute)) => Some(*minute),
        Some(V::Null) => None,
        _ => return Err(Error::unsupported("the end minute is another value").into()),
    };
    let checked = if expire { None } else { Some(check_change(cell, host, change, op, target, raw_parameters, end_minute)?) };
    // {event.source_id, *parameters.source_ids, *marker.assumption_ids}, what is a str with text.
    let mut sources: Vec<&str> = Vec::new();
    if let Some(source) = event.source_id.as_deref().filter(|source| !source.is_empty()) {
        sources.push(source);
    }
    match raw_parameters {
        V::Map(parameters) => {
            if let Some(list) = parameters.get("source_ids") {
                for item in list_of(list)? {
                    if let V::Str(item) = item {
                        if !item.is_empty() {
                            sources.push(item);
                        }
                    }
                }
            }
        }
        _ => return Err(Error::unsupported("the parameters are another value").into()),
    }
    if let V::Map(provenance) = &event.provenance {
        match provenance.get("effect_program_v1") {
            Some(V::Map(marker)) => {
                if let Some(list) = marker.get("assumption_ids") {
                    for item in list_of(list)? {
                        if let V::Str(item) = item {
                            if !item.is_empty() {
                                sources.push(item);
                            }
                        }
                    }
                }
            }
            None => {}
            _ => return Err(Error::unsupported("the marker is another value").into()),
        }
    }
    let sources = sorted_set(sources);
    // The parameters as their contract gives them, for the operators that read the model: what the
    // check validated, or, for an expiry, the contract now, whose refusal is a refusal.
    let data = match &**op {
        "move_facility" | "transfer_equipment" | "temporary_room_geometry" => Some(match checked {
            Some(data) => data,
            None => {
                let contract = contract_of(op).ok_or_else(|| Error::unsupported("no contract"))?;
                match host.contract(&contract, raw_parameters) {
                    Ok(data) => data,
                    Err(_) => return Err(Error::unsupported("the contract refuses the parameters").into()),
                }
            }
        }),
        "set_partition" if !expire => checked,
        _ => None,
    };
    let write = Write { change, event, payload, op, target, modifier, expire, end_minute, sources: &sources, data: data.as_ref() };
    cell.with(|world| write.apply(world))??;
    Ok(())
}

/// What `apply_geometry_change` writes, planned.
struct Write<'a> {
    change: &'a Change<'a>,
    event: &'a EventRow,
    payload: &'a Doc,
    op: &'a Arc<str>,
    target: &'a Arc<str>,
    modifier: &'a Arc<str>,
    expire: bool,
    end_minute: Option<i64>,
    sources: &'a [&'a str],
    /// The parameters as their contract gives them.
    data: Option<&'a V>,
}

impl Write<'_> {
    fn data(&self) -> Result<&V> {
        self.data.ok_or_else(|| Error::unsupported("the contract refuses the parameters"))
    }

    /// The overlay's part `name`, which must be a plain dict.
    fn part_is_a_dict(&self, world: &World, name: &str) -> Result<()> {
        let state = state_of(world, OVERLAY_KEY, self.change.overlay)?.ok_or_else(|| Error::unsupported("no overlay"))?;
        if !matches!(required(&state.0, name)?, V::Map(_)) {
            return refuse("a value is not a plain dict");
        }
        Ok(())
    }

    fn apply(&self, world: &World) -> Result<()> {
        let (event_id, target, modifier) = (&*self.event.event_id, &**self.target, &**self.modifier);
        {
            let state = state_of(world, OVERLAY_KEY, self.change.overlay)?.ok_or_else(|| Error::unsupported("no overlay"))?;
            if !matches!(required(&state.0, "history")?, V::List(_)) {
                return refuse("a value is not a plain list");
            }
        }
        let minute = world.minute_index()?;
        let end = || self.end_minute.map_or(V::Null, V::Int);
        let mut history_target: Arc<str> = self.target.clone();
        match &**self.op {
            "move_facility" | "transfer_equipment" => {
                let data = self.data()?;
                let point = point_field(data, "point_m")?;
                let zone = if &**self.op == "transfer_equipment" { Some(text_field(data, "to_zone_id")?) } else { None };
                self.part_is_a_dict(world, "facility_points")?;
                let record = stamped_record([("point_m", point_doc(point))], event_id, self.sources, minute, None);
                if let Some(zone) = zone {
                    let mut rows = world.rows_mut()?;
                    let Some(facility) = rows.facilities.get_mut(target) else {
                        return refuse("no such facility");
                    };
                    facility.zone_id = zone;
                }
                let mut row = world.row_mut::<ValueRow>(OVERLAY_KEY)?;
                dict_mut(required_mut(&mut row.0, "facility_points")?)?.insert(self.target.clone(), record);
            }
            "set_partition" => {
                self.part_is_a_dict(world, "partitions")?;
                let record = if self.expire {
                    None
                } else {
                    let polygon = points_field(self.data()?, "polygon_m")?;
                    let corners = V::List(polygon.iter().map(|point| point_doc(*point)).collect());
                    Some(stamped_record([("zone_id", text(target)), ("polygon_m", corners)], event_id, self.sources, minute, Some(end())))
                };
                let mut row = world.row_mut::<ValueRow>(OVERLAY_KEY)?;
                let partitions = dict_mut(required_mut(&mut row.0, "partitions")?)?;
                match record {
                    None => {
                        partitions.shift_remove(modifier);
                    }
                    Some(record) => {
                        partitions.insert(self.modifier.clone(), record);
                    }
                }
            }
            "set_route_access" => self.route_access(world, minute, end())?,
            "temporary_room_geometry" => {
                let data = self.data()?;
                let room_id = text_field(data, "room_id")?;
                {
                    let state = state_of(world, OVERLAY_KEY, self.change.overlay)?.ok_or_else(|| Error::unsupported("no overlay"))?;
                    match child(&state.0, "temporary_rooms")? {
                        None => return refuse("the rooms are still to be made"),
                        Some(V::Map(rooms)) => {
                            if self.expire && !rooms.contains_key(&*room_id) {
                                return refuse("the room did not start");
                            }
                        }
                        Some(_) => return refuse("a value is not a plain dict"),
                    }
                }
                let record = if self.expire {
                    None
                } else {
                    let V::Map(mut fields) = data.clone() else {
                        return refuse("not a dict");
                    };
                    fields.insert(Arc::from("start_minute"), V::Int(minute));
                    fields.insert(Arc::from("end_minute"), end());
                    fields.insert(Arc::from("active"), V::Bool(true));
                    fields.insert(Arc::from("start_event_id"), text(event_id));
                    fields.insert(Arc::from("end_event_id"), V::Null);
                    fields.insert(Arc::from("program_id"), V::Str(str_of(self.payload, "program_id")?.clone()));
                    fields.insert(Arc::from("effect_id"), V::Str(str_of(self.payload, "effect_id")?.clone()));
                    fields.insert(Arc::from("source_ids"), id_list(self.sources.iter().copied()));
                    Some(V::Map(fields))
                };
                let mut row = world.row_mut::<ValueRow>(OVERLAY_KEY)?;
                let rooms = dict_mut(required_mut(&mut row.0, "temporary_rooms")?)?;
                match record {
                    None => {
                        let record = rooms.get_mut(&*room_id).ok_or_else(|| Error::unsupported("the room did not start"))?;
                        let record = dict_mut(record)?;
                        record.insert(Arc::from("active"), V::Bool(false));
                        record.insert(Arc::from("end_event_id"), text(event_id));
                    }
                    Some(record) => {
                        rooms.insert(room_id.clone(), record);
                    }
                }
                history_target = room_id;
            }
            _ => return invalid("unsupported_geometry_operator"),
        }
        let entry = stamped_record(
            [
                ("transition_id", text(event_id)),
                ("op", V::Str(self.op.clone())),
                ("target_id", V::Str(history_target)),
                ("status", text(if self.expire { "expired" } else { "applied" })),
                ("causation_id", self.event.causation_id.as_deref().map_or(V::Null, text)),
            ],
            event_id,
            self.sources,
            minute,
            None,
        );
        let mut row = world.row_mut::<ValueRow>(OVERLAY_KEY)?;
        append_bounded(list_mut(required_mut(&mut row.0, "history")?)?, entry, MAX_OVERLAY_HISTORY);
        Ok(())
    }

    /// `set_route_access`: the modifier written or popped, and the route's closure from its base and
    /// modifiers, written on the edge's row.
    fn route_access(&self, world: &World, minute: i64, end: V) -> Result<()> {
        let (target, modifier) = (&**self.target, &**self.modifier);
        let base_now = match zone_rows(world)?.zone_edges.get(target) {
            Some(edge) => edge.closed,
            None => return refuse("no such route"),
        };
        self.part_is_a_dict(world, "route_modifiers")?;
        let accessible = if self.expire {
            None
        } else {
            match self.payload.get("parameters") {
                Some(V::Map(parameters)) => match parameters.get("accessible") {
                    Some(V::Bool(flag)) => Some(*flag),
                    _ => return refuse("accessible is not a bool"),
                },
                _ => return refuse("no parameters"),
            }
        };
        let closed = {
            let mut row = world.row_mut::<ValueRow>(OVERLAY_KEY)?;
            let routes = dict_mut(required_mut(&mut row.0, "route_modifiers")?)?;
            if !routes.contains_key(target) {
                routes.insert(self.target.clone(), doc([("base_closed", V::Bool(base_now)), ("modifiers", V::Map(IndexMap::new()))]));
            }
            let record = routes.get_mut(target).ok_or_else(|| Error::unsupported("the state has no such field"))?;
            let modifiers = dict_mut(required_mut(record, "modifiers")?)?;
            match accessible {
                Some(accessible) => {
                    let entry = stamped_record([("accessible", V::Bool(accessible))], &self.event.event_id, self.sources, minute, Some(end));
                    modifiers.insert(self.modifier.clone(), entry);
                }
                None => {
                    modifiers.shift_remove(modifier);
                }
            }
            let V::Bool(mut closed) = *required(record, "base_closed")? else {
                return refuse("not a bool");
            };
            let modifiers = map_of(required(record, "modifiers")?)?;
            for item in modifiers.values() {
                let V::Bool(flag) = required(item, "accessible")? else {
                    return refuse("not a bool");
                };
                closed |= !*flag;
            }
            if modifiers.is_empty() {
                routes.shift_remove(target);
            }
            closed
        };
        let mut rows = world.entities.try_borrow_mut()?;
        if rows.zones_tainted().is_some() {
            return refuse("the zones are tainted");
        }
        let Some(edge) = rows.zone_edges.get_mut(target) else {
            return refuse("no such route");
        };
        edge.closed = closed;
        Ok(())
    }
}

// ---- validate_geometry_overlay_state ------------------------------------------------------------

fn id_list_is_good(value: Option<&V>) -> bool {
    matches!(value, Some(V::List(items)) if items.iter().all(|item| matches!(item, V::Str(id) if is_identifier(id))))
}

fn ids_are_unique(value: Option<&V>) -> bool {
    let Some(V::List(items)) = value else {
        return false;
    };
    let mut seen = HashSet::new();
    items.iter().all(|item| matches!(item, V::Str(id) if seen.insert(id.clone())))
}

/// What `_FacilityPointOverride`, `_PartitionOverride` and `_RouteModifier` have in common.
fn stamped_is_good(row: &Doc) -> Result<i64> {
    let minute = int_of(row, "minute")?;
    if minute < 0 || !matches!(row.get("event_id"), Some(V::Str(id)) if is_identifier(id)) || !id_list_is_good(row.get("source_ids")) {
        return refuse("a record has another shape");
    }
    Ok(minute)
}

/// `validate_geometry_overlay_state`: `_GeometryOverlayState` for what it takes as it is, and the
/// rule's checks after it. Ok only for what the Python rule passes.
pub fn check_overlay(world: &World, overlay: &Held, geo: Option<&Geo>) -> Result<()> {
    let Some(value) = state_of(world, OVERLAY_KEY, overlay)? else {
        return Ok(());
    };
    let state = map_of(&value.0)?;
    const NAMES: [&str; 9] = [
        "schema_version", "policy_version", "facility_points", "partitions", "route_modifiers", "history", "temporary_rooms",
        "bed_reservations", "room_admissions",
    ];
    if state.keys().any(|key| !NAMES.contains(&&**key)) || state.get("schema_version") != Some(&V::Int(1)) || str_of(state, "policy_version")?.as_ref() != POLICY_OVERLAY {
        return refuse("the overlay has another shape");
    }
    let empty = IndexMap::new();
    let optional = |name: &str| -> Result<&Doc> {
        match state.get(name) {
            Some(V::Map(map)) => Ok(map),
            None => Ok(&empty),
            _ => refuse("a part of the overlay is another value"),
        }
    };
    let points = map_of(state.get("facility_points").unwrap_or(&V::Null))?;
    let partitions = map_of(state.get("partitions").unwrap_or(&V::Null))?;
    let route_modifiers = map_of(state.get("route_modifiers").unwrap_or(&V::Null))?;
    let history = list_of(state.get("history").unwrap_or(&V::Null))?;
    let rooms = optional("temporary_rooms")?;
    let reservations = optional("bed_reservations")?;
    let admissions = optional("room_admissions")?;
    if history.len() > MAX_OVERLAY_HISTORY
        || points.len() > 256
        || partitions.len() > 256
        || route_modifiers.len() > 256
        || rooms.len() > 256
        || reservations.len() > 10_000
        || admissions.len() > 10_000
    {
        return refuse("the overlay exceeds a limit");
    }
    let minute = world.minute_index()?;
    let mut seen = HashSet::new();
    let mut previous = None;
    for row in history {
        let row = map_of(row)?;
        if !has_exactly(row, &["transition_id", "op", "target_id", "status", "causation_id", "event_id", "source_ids", "minute"]) {
            return refuse("a history record has another shape");
        }
        let at = stamped_is_good(row)?;
        let transition = str_of(row, "transition_id")?;
        if !is_identifier(transition)
            || !is_identifier(str_of(row, "target_id")?)
            || !matches!(&**str_of(row, "op")?, "move_facility" | "transfer_equipment" | "set_partition" | "set_route_access" | "temporary_room_geometry" | "install_facility")
            || !matches!(&**str_of(row, "status")?, "applied" | "expired")
            || !match row.get("causation_id") {
                Some(V::Null) => true,
                Some(V::Str(id)) => is_identifier(id),
                _ => false,
            }
            || !seen.insert(transition.clone())
            || at > minute
            || previous.is_some_and(|before| before > at)
        {
            return refuse("a history record is not what the Python rule passes");
        }
        previous = Some(at);
    }
    let geo = plan(geo)?;
    let layout = layout_of_plan(geo)?;
    let constraints = geo.constraints.as_ref();
    let edges = edges_of(world)?;
    // Facility and person facts, read once.
    let (facility_zones, agents): (HashMap<Arc<str>, Arc<str>>, HashSet<Arc<str>>) = {
        let rows = world.rows()?;
        (
            rows.facilities.iter().map(|(id, facility)| (id.clone(), facility.zone_id.clone())).collect(),
            rows.agents.iter().map(|(id, _)| id.clone()).collect(),
        )
    };
    let mut active_rooms: Vec<(&Arc<str>, Vec<Pt>, &Arc<str>)> = Vec::new();
    let mut room_capacity: HashMap<&str, i64> = HashMap::new();
    let mut sleeping: HashMap<&str, i64> = HashMap::new();
    for (room_id, room) in rooms {
        let room = map_of(room)?;
        let names = [
            "schema_version", "room_id", "zone_id", "polygon_m", "people_capacity", "sleeping_capacity", "bed_points_m", "route_edge_ids",
            "start_minute", "end_minute", "active", "start_event_id", "end_event_id", "program_id", "effect_id", "source_ids",
        ];
        if !has_exactly(room, &names) || room.get("schema_version") != Some(&V::Int(1)) || !is_identifier(room_id) {
            return refuse("a room has another shape");
        }
        let zone_id = str_of(room, "zone_id")?;
        let (people, beds_wanted) = (int_of(room, "people_capacity")?, int_of(room, "sleeping_capacity")?);
        let (start, end) = (int_of(room, "start_minute")?, int_of(room, "end_minute")?);
        let Some(V::Bool(active)) = room.get("active") else {
            return refuse("a room's active is not a bool");
        };
        let ended = match room.get("end_event_id") {
            Some(V::Null) => false,
            Some(V::Str(id)) if is_identifier(id) => true,
            _ => return refuse("a room's end event is another value"),
        };
        let polygon = points_in_value(room.get("polygon_m").unwrap_or(&V::Null))?;
        let beds = points_in_value(room.get("bed_points_m").unwrap_or(&V::Null))?;
        if str_of(room, "room_id")? != room_id
            || !is_identifier(zone_id)
            || !(1..=1_000_000).contains(&people)
            || !(0..=1_000_000).contains(&beds_wanted)
            || start < 0
            || end <= 0
            || end <= start
            || beds_wanted > people
            || beds_wanted != beds.len() as i64
            || *active == ended
            || !id_list_is_good(room.get("route_edge_ids"))
            || !ids_are_unique(room.get("route_edge_ids"))
            || !id_list_is_good(room.get("source_ids"))
            || !ids_are_unique(room.get("source_ids"))
            || !is_identifier(str_of(room, "start_event_id")?)
            || !is_identifier(str_of(room, "program_id")?)
            || !is_identifier(str_of(room, "effect_id")?)
        {
            return refuse("a room is not what its contract takes");
        }
        let Some(zone) = layout.zones.get(&**zone_id) else {
            return refuse("a room in an unknown zone");
        };
        if start > minute || (*active && minute > end) || !inside_zone(&zone.polygon, &polygon) {
            return refuse("a room is not what the Python rule passes");
        }
        if beds.iter().any(|point| !point_in_polygon(*point, &polygon)) {
            return refuse("a bed lies outside its room");
        }
        let route_ids = list_of(room.get("route_edge_ids").unwrap_or(&V::Null))?;
        let (mut incoming, mut outgoing) = (false, false);
        for id in route_ids {
            let V::Str(id) = id else {
                return refuse("a route's ID is not a str");
            };
            let Some(edge) = edges.get(id) else {
                return refuse("a room references an unknown route");
            };
            if !edge.closed && layout.routes.get(&**id).is_some_and(|route| route.accessible) {
                incoming |= edge.to == *zone_id;
                outgoing |= edge.from == *zone_id;
            }
        }
        if *active && (!incoming || !outgoing) {
            return refuse("an active room lacks a route");
        }
        room_capacity.insert(room_id, people);
        sleeping.insert(room_id, beds_wanted);
        if *active {
            active_rooms.push((room_id, polygon, zone_id));
        }
    }
    for (index, (_, polygon, zone_id)) in active_rooms.iter().enumerate() {
        for (_, other, other_zone) in &active_rooms[index + 1..] {
            if zone_id == other_zone && polygons_overlap(polygon, other) {
                return refuse("active rooms overlap");
            }
        }
        if layout.facility_points.values().any(|point| point_in_polygon(*point, polygon)) {
            return refuse("an active room covers a facility");
        }
    }
    for (key, row) in points {
        let row = map_of(row)?;
        if !has_exactly(row, &["point_m", "event_id", "source_ids", "minute"]) || !is_identifier(key) {
            return refuse("a move has another shape");
        }
        let at = stamped_is_good(row)?;
        let point = point_in_value(row.get("point_m").unwrap_or(&V::Null))?;
        let Some(zone) = facility_zones.get(&**key).and_then(|zone| layout.zones.get(&**zone)) else {
            return refuse("a move of an unknown facility");
        };
        if at > minute || !point_in_polygon(point, &zone.polygon) {
            return refuse("a move is not what the Python rule passes");
        }
    }
    for service in &layout.service_points {
        let Some(zone) = facility_zones.get(&*service.facility_id).and_then(|zone| layout.zones.get(&**zone)) else {
            return refuse("a service point of an unknown facility");
        };
        if !point_in_polygon(service.point, &zone.polygon) {
            return refuse("a service point leaves its zone");
        }
    }
    for (key, row) in partitions {
        let row = map_of(row)?;
        if !has_exactly(row, &["zone_id", "polygon_m", "event_id", "source_ids", "minute", "end_minute"]) || !is_identifier(key) {
            return refuse("a partition has another shape");
        }
        let at = stamped_is_good(row)?;
        let end = int_of(row, "end_minute")?;
        let polygon = points_in_value(row.get("polygon_m").unwrap_or(&V::Null))?;
        let zone_id = str_of(row, "zone_id")?;
        let Some(zone) = layout.zones.get(&**zone_id) else {
            return refuse("a partition in an unknown zone");
        };
        if !is_identifier(zone_id) || !(3..=64).contains(&polygon.len()) || end <= at || end <= 0 || at > minute || !inside_zone(&zone.polygon, &polygon) {
            return refuse("a partition is not what the Python rule passes");
        }
    }
    let mut nested = 0;
    for (key, row) in route_modifiers {
        let row = map_of(row)?;
        let (Some(V::Bool(base)), Some(V::Map(modifiers)), 2) = (row.get("base_closed"), row.get("modifiers"), row.len()) else {
            return refuse("a route's overlay has another shape");
        };
        let Some(edge) = edges.get(&**key) else {
            return refuse("an unknown route");
        };
        if modifiers.is_empty() || !is_identifier(key) {
            return refuse("a route's overlay has no modifier");
        }
        nested += modifiers.len();
        let mut closed = *base;
        for (id, record) in modifiers {
            let record = map_of(record)?;
            if !has_exactly(record, &["accessible", "event_id", "source_ids", "minute", "end_minute"]) || !is_identifier(id) {
                return refuse("a route modifier has another shape");
            }
            let at = stamped_is_good(record)?;
            let end = int_of(record, "end_minute")?;
            let Some(V::Bool(accessible)) = record.get("accessible") else {
                return refuse("accessible is not a bool");
            };
            if end <= at || end <= 0 || at > minute {
                return refuse("a route modifier is not what the Python rule passes");
            }
            closed |= !*accessible;
        }
        if edge.closed != closed {
            return refuse("the overlay and the graph disagree");
        }
    }
    if nested > 256 {
        return refuse("the overlay exceeds a limit");
    }
    let Some(constraints) = constraints else {
        return refuse("no constraints");
    };
    for (facility_id, footprint) in &constraints.footprints {
        let Some(zone_id) = facility_zones.get(facility_id) else {
            return refuse("a footprint of an unknown facility");
        };
        let Some(zone) = layout.zones.get(&**zone_id) else {
            return refuse("a footprint in an unknown zone");
        };
        if footprint.iter().any(|point| !point_in_polygon(*point, &zone.polygon)) {
            return refuse("a footprint leaves its zone");
        }
        if active_rooms.iter().any(|(_, polygon, room_zone)| *room_zone == zone_id && polygons_overlap(polygon, footprint)) {
            return refuse("an active room overlaps a footprint");
        }
    }
    let mut beds_held: HashSet<(Arc<str>, i64)> = HashSet::new();
    for (agent_id, reservation) in reservations {
        let reservation = map_of(reservation)?;
        if !has_exactly(reservation, &["agent_id", "room_id", "bed_index", "target_id", "reserved_minute"]) {
            return refuse("a reservation has another shape");
        }
        let room_id = str_of(reservation, "room_id")?;
        let index = int_of(reservation, "bed_index")?;
        let at = int_of(reservation, "reserved_minute")?;
        let target = str_of(reservation, "target_id")?;
        if str_of(reservation, "agent_id")? != agent_id
            || !is_identifier(agent_id)
            || !is_identifier(target)
            || index < 0
            || at < 0
            || !agents.contains(agent_id)
            || sleeping.get(&**room_id).is_none_or(|beds| index >= *beds)
            || **target != *bed_target_id(room_id, index)
            || at > minute
            || !beds_held.insert((room_id.clone(), index))
        {
            return refuse("a reservation is not what the Python rule passes");
        }
    }
    let mut counts: HashMap<&str, i64> = HashMap::new();
    for (agent_id, admission) in admissions {
        let admission = map_of(admission)?;
        if !has_exactly(admission, &["agent_id", "room_id", "admitted_minute", "event_id", "source_ids"]) {
            return refuse("an admission has another shape");
        }
        let room_id = str_of(admission, "room_id")?;
        let at = int_of(admission, "admitted_minute")?;
        if str_of(admission, "agent_id")? != agent_id
            || !is_identifier(agent_id)
            || !is_identifier(room_id)
            || !is_identifier(str_of(admission, "event_id")?)
            || !id_list_is_good(admission.get("source_ids"))
            || at < 0
            || at > minute
            || !agents.contains(agent_id)
            || !room_capacity.contains_key(&**room_id)
        {
            return refuse("an admission is not what the Python rule passes");
        }
        *counts.entry(room_id).or_insert(0) += 1;
    }
    if counts.iter().any(|(room_id, count)| room_capacity.get(room_id).is_none_or(|capacity| count > capacity)) {
        return refuse("admissions exceed a room");
    }
    Ok(())
}
