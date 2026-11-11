//! The rules of `events/effects.py` that run in a tick, on the rows of a `World` (moved to the core
//! in P8 d3): a guard, the five handlers of an effect's events, what other modules ask of the
//! modifier stacks, and the check of `effect_program_v1`. The PyO3 crate's file of the same name
//! reads what Python holds (whether `model_state` holds each namespace, the sections of the configs
//! kept by stamp, the floor plan), makes the event's row, and calls these.
//!
//! A handler's Python rule made a copy of the world, changed the copy, checked it and swapped it in.
//! Here a handler is two steps: its checks on the world (`plan_*`, `check_*`), and its writes on the
//! world of the transaction that stands for the copy (`write_*`; lane 6-3's `RuleTxn`, which stays in
//! the PyO3 crate with its copy for a world the transaction refuses). A write step ends with the
//! whole world's validation through the host (`Host::validate_world`, policy E6), and an error rolls
//! the transaction back.
//!
//! A suppressed or failed effect is the Python rule's `Error::suppressed` or `Error::failed`, with
//! its text, in its order of checks. Where the Python rule wraps what a call raised (`except
//! ValueError as exc: raise EventApplicationError(f"...{exc}")`), the rule here wraps the host's
//! error the same way, with `str(exc)` of it (`HostFailure::text`): the pydantic contracts of the
//! parameters and of a guard (`Host::contract`, policy E15), the check of the changed world, and the
//! errors of the rules of other files a handler calls (`overlay`'s temporary spaces, geometry changes
//! and the place of a new facility). Those rules were called by name, through Python's
//! `kernel_only`; they are direct calls now, and a refusal of one is the `KernelRefusal` that
//! `kernel_only` made of it (`Error::refused` with its label), wrapped or let through as before.
//!
//! Nothing here reads LLM output: what a program may do was validated before it was scheduled, by
//! `validate_effect_program`, which is not touched (docs/rust-port/lanes/m6-effects.md).

use std::collections::HashSet;
use std::sync::Arc;

use indexmap::IndexMap;
use sha2::{Digest, Sha256};

use crate::config::environment::ZoneEnvironmentSpec;
use crate::entities::{Entities, FacilityRow, Skills, StaffRow};
use crate::error::{refused_text, Error, Result};
use crate::events::writer::WriteError;
use crate::events::{is_stable_id, EventRow};
use crate::geometry::Geo;
use crate::host::{Contract, ErrorClass, Host, HostFailure};
use crate::row::ValueRow;
use crate::rules::care_receipts::{self, Made};
use crate::rules::common_operations::py_eq;
use crate::rules::kit::{self, Counts, Held, Kept};
use crate::rules::movement::{self, StaffOps};
use crate::rules::{agent_dynamics, overlay, staff_dynamics, staff_operation_events, staff_order};
use crate::state::effect_program::{EffectProgramRow, FacilityModifiers, Modifier, Program, NAME};
use crate::state::resource_ledger::{resource_index, LedgerRow, NAME as LEDGER};
use crate::state::staff_operations::{OperationRecord, Phase, StaffOperationsRow, NAME as STAFF_OPERATIONS};
use crate::value::V;
use crate::world::{World, WorldCell};

pub const MARKER: &str = NAME;
pub const OPERATIONAL: &str = "operational_events_v1";
pub const INFRASTRUCTURE: &str = "infrastructure_v1";
pub const ENVIRONMENT: &str = "zone_environment_v1";
const ENVIRONMENT_POLICY: &str = "zone_environment_v1";
const TEMPORARY_SPACES: &str = overlay::SPACES_KEY;

/// The refusal of the environment rule where it reads `ScenarioZoneEnvironmentConfig`, whose read
/// raised: the PyO3 crate raises what that raised.
pub const CONFIG_UNREAD: &str = "the zone environment config could not be read";
/// The refusal of a geometry change that reads the floor plan the PyO3 crate could not read: it
/// raises what `Geo::of` raised, wrapped as the rule wraps a ValueError, KeyError or TypeError.
pub const GEOMETRY_PLAN_UNREAD: &str = "the floor plan of a geometry change could not be read";
/// The refusal of the staff positions of added staff, whose floor plan or staff operations the
/// PyO3 crate could not read: it raises what that raised.
pub const SNAPSHOT_UNREAD: &str = "the floor plan or the staff operations of the staff positions could not be read";
/// A pending event has no row: the PyO3 crate reads the rows of the events Python scheduled and asks
/// again.
pub const PENDING_UNREAD: &str = "a pending event has no row";

/// The labels of the rules of other files a handler calls, as `kernel_only` named them.
const REGISTER: &str = "overlay.register_temporary_space";
const APPLY_GEOMETRY: &str = "overlay.apply_geometry_change";
const PLACE: &str = "overlay.place_installed_facility";
const ANNOUNCE: &str = "agent_dynamics.announce_installed_facility";
const SNAPSHOTS: &str = "movement.staff_position_snapshots";
const AVAILABLE: &str = "staff_dynamics.is_available";

// ---- What ran and what was refused --------------------------------------------------------------

/// What the effects' rules ran and refused, by label: the PyO3 crate's `stats` and its `counted` of
/// each entry. The core counts the branch of an addition it ran (`structure.add_staff`).
pub static COUNTS: Counts = Counts::new();

// ---- Reading -------------------------------------------------------------------------------------

type Doc = IndexMap<Arc<str>, V>;

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn str_v(value: &str) -> V {
    V::Str(text(value))
}

#[track_caller]
fn map_of(value: &V) -> Result<&Doc> {
    match value {
        V::Map(map) => Ok(map),
        _ => Err(Error::unsupported("not an object")),
    }
}

fn str_of<'a>(map: &'a Doc, key: &str) -> Option<&'a Arc<str>> {
    match map.get(key) {
        Some(V::Str(text)) => Some(text),
        _ => None,
    }
}

#[track_caller]
fn required_str<'a>(map: &'a Doc, key: &str) -> Result<&'a Arc<str>> {
    str_of(map, key).ok_or_else(|| Error::unsupported("a value is not a str"))
}

/// `set(map) == set(names)`.
fn has_exactly(map: &Doc, names: &[&str]) -> bool {
    map.len() == names.len() && names.iter().all(|name| map.contains_key(*name))
}

/// A list of str of the marker, for `name in list` and the sources of a record.
fn str_list<'a>(map: &'a Doc, key: &str) -> Result<Vec<&'a Arc<str>>> {
    match map.get(key) {
        None => Ok(Vec::new()),
        Some(V::List(items)) => items
            .iter()
            .map(|item| match item {
                V::Str(text) => Ok(text),
                _ => Err(Error::unsupported("a list of the marker holds another value")),
            })
            .collect(),
        Some(_) => Err(Error::unsupported("a list of the marker is another value")),
    }
}

/// The event's marker, if it is a mapping.
fn marker_of(row: &EventRow) -> Result<Option<&Doc>> {
    Ok(match map_of(&row.provenance)?.get(MARKER) {
        Some(V::Map(marker)) => Some(marker),
        _ => None,
    })
}

/// The row of `effect_program_v1`, or None when the world doesn't hold the namespace.
fn program_state<R>(world: &World, held: &Held, read: impl FnOnce(Option<&EffectProgramRow>) -> Result<R>) -> Result<R> {
    match kit::held_row::<EffectProgramRow>(world, NAME, held)? {
        None => read(None),
        Some(row) => read(Some(&*row)),
    }
}

/// `model_state.get(name)` of a namespace held as a value: None for an absent key.
fn state_value(world: &World, name: &str, held: &Held) -> Result<Option<V>> {
    Ok(kit::held_row::<ValueRow>(world, name, held)?.map(|row| row.0.clone()))
}

fn modifier_id(program_id: &str, effect_id: &str) -> String {
    let digest = Sha256::digest(format!("{program_id}:{effect_id}").as_bytes());
    let mut text = String::with_capacity(41);
    text.push_str("modifier-");
    for byte in &digest[..16] {
        text.push_str(&format!("{byte:02x}"));
    }
    text
}

/// `zone_id in world.zones`, of the zone rows (a table a value did not fit is refused).
#[track_caller]
fn zone_known(rows: &Entities, zone_id: &str) -> Result<bool> {
    if rows.zones_tainted().is_some() {
        return Err(Error::unsupported("the zone table is tainted"));
    }
    Ok(rows.zones.contains_key(zone_id))
}

/// The pending events in queue order, as their rows.
fn pending_rows(world: &World) -> Result<Vec<Arc<EventRow>>> {
    let queue = world.queue()?;
    queue
        .ordered()
        .iter()
        .map(|(_, serial)| queue.slot(*serial).row.clone().ok_or_else(|| Error::unsupported(PENDING_UNREAD)))
        .collect()
}

/// Write the dict at this path of a namespace held as a value (its tree follows the row, lane
/// P8-0c): each step a key of a plain dict.
fn write_value(world: &World, name: &str, path: &[&str], f: impl FnOnce(&mut Doc) -> Result<()>) -> Result<()> {
    let mut row = world.row_mut::<ValueRow>(name)?;
    let mut value = &mut row.0;
    for key in path {
        let V::Map(entries) = value else {
            return Err(Error::unsupported("a value is not a plain dict"));
        };
        value = entries.get_mut(*key).ok_or_else(|| Error::Key((*key).to_owned()))?;
    }
    match value {
        V::Map(entries) => f(entries),
        _ => Err(Error::unsupported("a value is not a plain dict")),
    }
}

// ---- Python's answers on the values of a frozen payload ------------------------------------------
//
// An event's payload and provenance are frozen (`_freeze_json`): a dict is a mappingproxy, a list a
// tuple, and every other value an exact JSON type. The checks that raise a rule's text answer
// as Python's operators do on them; where Python would raise instead, they say Unsupported.

/// Whether Python can hash the value (`x in a_set`, `a_dict.get(x)`): a mappingproxy can't, and a
/// tuple can when its items can.
fn hashable(value: &V) -> bool {
    match value {
        V::Map(_) => false,
        V::List(items) => items.iter().all(hashable),
        _ => true,
    }
}

/// `value == text`, for a value that may be absent (None): only a str equals a str.
fn is_text(value: Option<&V>, text: &str) -> bool {
    matches!(value, Some(V::Str(item)) if **item == *text)
}

/// `a != b` for two values that may be absent (None), where Rust can tell what Python says.
fn differ(a: Option<&V>, b: Option<&V>) -> Result<bool> {
    let null = |value: Option<&V>| matches!(value, None | Some(V::Null));
    match (a, b) {
        (Some(V::Str(a)), Some(V::Str(b))) => Ok(a != b),
        (Some(V::Str(_)), _) | (_, Some(V::Str(_))) => Ok(true),
        _ if null(a) && null(b) => Ok(false),
        _ if null(a) || null(b) => Ok(true),
        _ => Err(Error::unsupported("two values Python compares by its own rules")),
    }
}

/// `value != text` of a dict's value and a str.
fn ne_text(value: &V, text: &str) -> bool {
    !matches!(value, V::Str(item) if **item == *text)
}

/// `value != other` of a dict's value and an event's optional ID.
fn ne_optional(value: &V, other: Option<&str>) -> bool {
    match other {
        Some(other) => ne_text(value, other),
        None => !matches!(value, V::Null),
    }
}

/// `needle in marker.get(key, [])`: absent is the empty list, a str is searched for a substring and
/// a mapping for a key. In anything else Python can't search, and raises.
fn holds(value: Option<&V>, needle: &str) -> Result<bool> {
    match value {
        None => Ok(false),
        Some(V::List(items)) => Ok(items.iter().any(|item| matches!(item, V::Str(item) if **item == *needle))),
        Some(V::Str(text)) => Ok(text.contains(needle)),
        Some(V::Map(map)) => Ok(map.contains_key(needle)),
        Some(_) => Err(Error::unsupported("`in` of a value Python can't search")),
    }
}

/// `key in mapping` for a dict of the world whose keys are str (`world.zones`, `world.staff`): any
/// other value Python can hash is not a key; one it can't hash raises.
fn key_in(contains: impl FnOnce(&str) -> Result<bool>, key: Option<&V>) -> Result<bool> {
    match key {
        Some(V::Str(text)) => contains(text),
        Some(value) if !hashable(value) => Err(Error::unsupported("a key Python can't hash")),
        _ => Ok(false),
    }
}

/// `map.get(key)` of a dict whose keys are str, for any key: None for one that is not a str, a
/// refusal for one Python can't hash.
fn lookup<'a>(map: &'a Doc, key: &V) -> Result<Option<&'a V>> {
    match key {
        V::Str(key) => Ok(map.get(&**key)),
        value if !hashable(value) => Err(Error::unsupported("a key Python can't hash")),
        _ => Ok(None),
    }
}

/// `value.get(key)` of a value the rule reads as a mapping.
fn get_of<'a>(map: &'a V, key: &V) -> Result<Option<&'a V>> {
    match map {
        V::Map(map) => lookup(map, key),
        _ => Err(Error::unsupported("a value is not a mapping")),
    }
}

/// `sorted(set(values))` of str.
fn sorted_unique<'a>(values: impl IntoIterator<Item = &'a str>) -> Vec<&'a str> {
    let mut items: Vec<&str> = values.into_iter().collect();
    items.sort_unstable();
    items.dedup();
    items
}

fn str_list_v<'a>(items: impl IntoIterator<Item = &'a str>) -> V {
    V::List(items.into_iter().map(str_v).collect())
}

// ---- What the host's errors become -----------------------------------------------------------------

/// The classes of a Python rule's `except ... as exc`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Catch {
    Value,
    ValueOrType,
    ValueKeyOrType,
}

impl Catch {
    /// Whether the clause catches an error of this class. A refusal (`Unsupported`) is not a
    /// ValueError, so a kernel's refusal is never caught; a `KernelRefusal` is.
    fn takes(self, class: ErrorClass) -> bool {
        match class {
            ErrorClass::Value => true,
            ErrorClass::Type => self != Catch::Value,
            ErrorClass::Key => self == Catch::ValueKeyOrType,
            ErrorClass::Refused | ErrorClass::Other => false,
        }
    }
}

/// `except <catch> as exc: raise EventApplicationError(f"{prefix}{exc}")`; any other error goes on.
fn wrapped<E: From<Error> + HostFailure>(error: E, catch: Catch, prefix: &str) -> E {
    if catch.takes(error.class()) {
        E::from(Error::failed(format!("{prefix}{}", error.text())))
    } else {
        error
    }
}

/// What a Python rule did with an error a rule of another file raised, called through
/// `kernel_only`: a refusal is that rule's `KernelRefusal` (a ValueError), and `except <catch> as exc:
/// raise EventApplicationError(str(exc))`. Without a catch, the error goes on as that rule raised it.
fn by_name<E: From<Error> + HostFailure>(label: &str, error: E, catch: Option<Catch>) -> E {
    let error = if error.class() == ErrorClass::Refused { E::from(Error::Refused { label: label.to_owned(), reason: error.text().into() }) } else { error };
    match catch {
        Some(catch) => wrapped(error, catch, ""),
        None => error,
    }
}

/// `Model.model_validate(raw)` of an effect's parameters (policy E15), whose error the rule wraps.
fn parameters<H: Host>(host: &mut H, op: &str, raw: &V, catch: Catch, prefix: &str) -> std::result::Result<V, H::Error> {
    host.contract(&Contract::EffectParameters(text(op)), raw).map_err(|error| wrapped(error, catch, prefix))
}

/// A field of a contract's dump, as an attribute of the model reads it.
#[track_caller]
fn field<'a>(data: &'a V, name: &str) -> Result<&'a V> {
    match data {
        V::Map(map) => map.get(name).ok_or_else(|| Error::unsupported("the contract's dump lacks a field")),
        _ => Err(Error::unsupported("the contract's dump is not an object")),
    }
}

#[track_caller]
fn field_str<'a>(data: &'a V, name: &str) -> Result<&'a Arc<str>> {
    kit::exact_str(field(data, name)?)
}

/// `world.validate()`, whose error the rule raises as it is; the transaction then rolls back.
fn validate<H: Host>(host: &mut H) -> std::result::Result<(), H::Error> {
    host.validate_world()
}

/// `event.payload[key]`.
#[track_caller]
fn payload_item<'a>(row: &'a EventRow, key: &str) -> Result<&'a V> {
    map_of(&row.payload)?.get(key).ok_or_else(|| Error::Key(key.to_owned()))
}

// ---- The guard -----------------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Compare {
    Eq,
    Ne,
    Gt,
    Gte,
    Lt,
    Lte,
}

#[derive(Clone, Copy)]
enum GuardField {
    Minute,
    Capacity,
    WorkingUnits,
    QueueLength,
    ZoneOccupancy,
    ResourceAvailable,
    StaffAvailable,
}

impl GuardField {
    /// `field_name`, as `{field_name!r}` writes it: a fixed ASCII text in single quotes.
    fn repr(self) -> &'static str {
        match self {
            GuardField::Minute => "'simulation.minute'",
            GuardField::Capacity => "'target.capacity'",
            GuardField::WorkingUnits => "'target.working_units'",
            GuardField::QueueLength => "'target.queue_length'",
            GuardField::ZoneOccupancy => "'zone.occupancy'",
            GuardField::ResourceAvailable => "'resource.available_quantity'",
            GuardField::StaffAvailable => "'staff.available'",
        }
    }
}

/// A guard's answer, or the text of the `EventApplicationError` the Python rule raises instead
/// (`should_apply_effect_guard`, `_evaluate_guard_node`).
type Judged<T> = std::result::Result<T, String>;

enum Guard<'a> {
    All(Vec<Guard<'a>>),
    Any(Vec<Guard<'a>>),
    Leaf { compare: Compare, field: GuardField, value: &'a V },
}

/// The fields of `GuardExpression` of one node, or None when the contract refuses them.
/// `depth` starts at 1 and `nodes` counts the tree, as `measure` does.
fn parse_guard<'a>(value: &'a V, depth: usize, nodes: &mut usize) -> Option<Guard<'a>> {
    if depth > 4 {
        return None;
    }
    *nodes += 1;
    if *nodes > 20 {
        return None;
    }
    let V::Map(map) = value else {
        return None;
    };
    if map.keys().any(|key| !matches!(&**key, "operator" | "field" | "value" | "operands")) {
        return None;
    }
    let V::Str(operator) = map.get("operator")? else {
        return None;
    };
    let field = match map.get("field") {
        None | Some(V::Null) => None,
        Some(V::Str(field)) => Some(field),
        Some(_) => return None,
    };
    let leaf_value = match map.get("value") {
        None | Some(V::Null) => None,
        Some(item) => Some(item),
    };
    let operands: &[V] = match map.get("operands") {
        None => &[],
        Some(V::List(items)) if items.len() <= 4 => items,
        Some(_) => return None,
    };
    let compare = match &**operator {
        "eq" => Compare::Eq,
        "ne" => Compare::Ne,
        "gt" => Compare::Gt,
        "gte" => Compare::Gte,
        "lt" => Compare::Lt,
        "lte" => Compare::Lte,
        "and" | "or" => {
            if field.is_some() || leaf_value.is_some() || operands.len() < 2 {
                return None;
            }
            let children = operands.iter().map(|item| parse_guard(item, depth + 1, nodes)).collect::<Option<Vec<_>>>()?;
            return Some(if &**operator == "and" { Guard::All(children) } else { Guard::Any(children) });
        }
        _ => return None,
    };
    if !operands.is_empty() {
        return None;
    }
    let value = leaf_value?;
    let numeric = match value {
        // An int past 64 bits is a StrictInt too.
        V::Int(_) | V::Big(_) => true,
        V::Float(number) if number.is_finite() => true,
        V::Bool(_) => false,
        V::Str(text) if text.chars().count() <= 256 => false,
        // What the contract refuses.
        _ => return None,
    };
    if !numeric && !matches!(compare, Compare::Eq | Compare::Ne) {
        return None;
    }
    let field = match &***field.as_ref()? {
        "simulation.minute" => GuardField::Minute,
        "target.capacity" => GuardField::Capacity,
        "target.working_units" => GuardField::WorkingUnits,
        "target.queue_length" => GuardField::QueueLength,
        "zone.occupancy" => GuardField::ZoneOccupancy,
        "resource.available_quantity" => GuardField::ResourceAvailable,
        "staff.available" => GuardField::StaffAvailable,
        _ => return None,
    };
    Some(Guard::Leaf { compare, field, value })
}

enum Actual {
    Int(i64),
    Bool(bool),
}

/// `actual` against `expected` by value, exactly, as Python compares an int with an int or a float.
fn numeric_order(actual: i64, expected: &V) -> Result<std::cmp::Ordering> {
    use std::cmp::Ordering::{Equal, Greater, Less};
    match expected {
        V::Int(expected) => Ok(actual.cmp(expected)),
        // `int.__repr__` of an int past 64 bits: below i64 when negative, above it otherwise.
        V::Big(text) => Ok(if text.starts_with('-') { Greater } else { Less }),
        V::Float(expected) if expected.is_finite() => {
            // The float's integral part is exact; past i64 it is past `actual`. Within, the ints
            // compare, and on a tie the fraction decides.
            let whole = expected.trunc();
            if whole >= 9_223_372_036_854_775_808.0 {
                return Ok(Less);
            }
            if whole < -9_223_372_036_854_775_808.0 {
                return Ok(Greater);
            }
            Ok(match actual.cmp(&(whole as i64)) {
                Equal => (0.0).partial_cmp(&(expected - whole)).unwrap_or(Equal),
                order => order,
            })
        }
        V::Float(_) => Err(Error::unsupported("a guard's value is not finite")),
        _ => Err(Error::unsupported("a guard's value is not a number")),
    }
}

fn compare_guard(compare: Compare, actual: &Actual, expected: &V) -> Result<bool> {
    use std::cmp::Ordering::{Equal, Greater, Less};
    let number = match actual {
        Actual::Int(number) if matches!(expected, V::Int(_) | V::Big(_) | V::Float(_)) => Some(*number),
        _ => None,
    };
    let equal = match (number, actual, expected) {
        (Some(number), _, _) => numeric_order(number, expected)? == Equal,
        (None, Actual::Bool(left), V::Bool(right)) => left == right,
        // Different types: `type(actual) is type(expected)` is false.
        _ => false,
    };
    Ok(match compare {
        Compare::Eq => equal,
        Compare::Ne => !equal,
        _ => {
            let Actual::Int(actual) = actual else {
                return Ok(false);
            };
            let order = numeric_order(*actual, expected)?;
            match compare {
                Compare::Gt => order == Greater,
                Compare::Gte => order != Less,
                Compare::Lt => order == Less,
                _ => order != Greater,
            }
        }
    })
}

/// What a guard reads besides the world's rows: whether `model_state` holds `effect_program_v1` and
/// `resource_ledger_v1`.
pub struct GuardIn<'a> {
    pub program: &'a Held,
    pub ledger: &'a Held,
}

/// `_evaluate_guard_node`: `all` and `any` stop at the first answer, so a later operand's failure is
/// reached only where Python reaches it.
fn evaluate_guard(world: &World, ins: &GuardIn<'_>, row: &EventRow, guard: &Guard<'_>) -> Result<Judged<bool>> {
    match guard {
        Guard::All(children) => {
            for child in children {
                match evaluate_guard(world, ins, row, child)? {
                    Ok(true) => {}
                    other => return Ok(other),
                }
            }
            Ok(Ok(true))
        }
        Guard::Any(children) => {
            for child in children {
                match evaluate_guard(world, ins, row, child)? {
                    Ok(false) => {}
                    other => return Ok(other),
                }
            }
            Ok(Ok(false))
        }
        Guard::Leaf { compare, field, value } => match guard_field(world, ins, row, *field)? {
            Ok(actual) => compare_guard(*compare, &actual, value).map(Ok),
            Err(text) => Ok(Err(text)),
        },
    }
}

/// The first of the payload's IDs that is a str; a payload without one is a shipment's.
fn guard_target(world: &World, ins: &GuardIn<'_>, row: &EventRow) -> Result<Option<Arc<str>>> {
    let payload = map_of(&row.payload)?;
    for name in ["facility_id", "staff_id", "zone_id", "resource_id", "target_id"] {
        if let Some(target) = str_of(payload, name) {
            return Ok(Some(target.clone()));
        }
    }
    let marker = match map_of(&row.provenance)?.get(MARKER) {
        Some(V::Map(marker)) => marker,
        _ => return Err(Error::unsupported("the guard's marker is another value")),
    };
    let single = match marker.get("program_id") {
        Some(V::Str(program_id)) => program_state(world, ins.program, |state| {
            Ok(state.and_then(|state| state.programs.get(&**program_id)).is_some_and(|program| program.effect_ids.len() == 1))
        })?,
        Some(V::Null) | None => false,
        Some(_) => return Err(Error::unsupported("the guard's marker has another program ID")),
    };
    if !single {
        return Ok(None);
    }
    match payload.get("shipment_id") {
        None | Some(V::Null) => Ok(None),
        Some(V::Str(shipment)) => Ok(Some(shipment.clone())),
        Some(_) => Err(Error::unsupported("a shipment ID is another value")),
    }
}

/// The value a leaf reads, or the text `_evaluate_guard_node` raises for a target it can't find.
fn guard_field(world: &World, ins: &GuardIn<'_>, row: &EventRow, field: GuardField) -> Result<Judged<Actual>> {
    let payload = map_of(&row.payload)?;
    let target = guard_target(world, ins, row)?;
    let target_text = target.as_deref().unwrap_or("");
    match field {
        GuardField::Minute => Ok(Ok(Actual::Int(world.minute_index()?))),
        GuardField::Capacity | GuardField::WorkingUnits | GuardField::QueueLength => {
            // `world.facilities.get(target_id or "")`.
            let rows = world.rows()?;
            let actual = rows.facilities.get(target_text).map(|facility| {
                Actual::Int(match field {
                    GuardField::Capacity => facility.capacity,
                    GuardField::WorkingUnits => facility.working_units,
                    _ => (facility.queue_agent_ids.len() + facility.resource_queue_agent_ids.len()) as i64,
                })
            });
            Ok(actual.ok_or_else(|| format!("guard field {} has no facility target", field.repr())))
        }
        GuardField::ZoneOccupancy => {
            // `event.payload.get("zone_id") or (...)`: a falsy value of another type is refused.
            let own_zone = match payload.get("zone_id") {
                None | Some(V::Null) => None,
                Some(V::Str(zone)) if zone.is_empty() => None,
                Some(V::Str(zone)) => Some(zone.clone()),
                Some(_) => return Err(Error::unsupported("a zone ID is another value")),
            };
            let rows = world.rows()?;
            let zone = match own_zone {
                Some(zone) => Some(zone),
                // A target that is no facility is the zone itself; None is no zone.
                None => rows.facilities.get(target_text).map(|facility| facility.zone_id.clone()).or_else(|| target.clone()),
            };
            let Some(zone) = zone else {
                return Ok(Err("guard zone.occupancy has no known zone target".to_owned()));
            };
            if !zone_known(&rows, &zone)? {
                return Ok(Err("guard zone.occupancy has no known zone target".to_owned()));
            }
            let count = rows.agents.iter().filter(|(_, agent)| agent.zone_id == zone).count();
            Ok(Ok(Actual::Int(count as i64)))
        }
        GuardField::ResourceAvailable => {
            let no_target = || Ok(Err("guard resource.available_quantity has no ledger target".to_owned()));
            // `not isinstance(ledger, dict)` comes first.
            let Some(ledger) = kit::held_row::<LedgerRow>(world, LEDGER, ins.ledger)? else {
                return no_target();
            };
            // `resource_id not in ledger["accounts"]`: None is in no ledger; the row's accounts are
            // exactly its two resources. A value of another type is Python's to judge.
            let resource = match payload.get("resource_id") {
                None | Some(V::Null) => None,
                Some(V::Str(resource)) => Some(resource.clone()),
                Some(_) => return Err(Error::unsupported("a resource ID is another value")),
            };
            let Some(index) = resource.as_deref().and_then(resource_index) else {
                return no_target();
            };
            let account = ledger.accounts.of(index);
            match account.remaining.checked_sub(account.reserved) {
                Some(left) => Ok(Ok(Actual::Int(left))),
                None => Err(Error::unsupported("a quantity past 64 bits")),
            }
        }
        GuardField::StaffAvailable => {
            // `event.payload.get("staff_id") or target_id`.
            let staff_id = match payload.get("staff_id") {
                Some(V::Str(staff)) if !staff.is_empty() => Some(staff.clone()),
                None | Some(V::Null) | Some(V::Str(_)) => target.clone(),
                Some(_) => return Err(Error::unsupported("a staff ID is another value")),
            };
            let known = match &staff_id {
                Some(staff_id) => world.rows()?.staff.contains_key(staff_id),
                None => false,
            };
            let (Some(staff_id), true) = (staff_id, known) else {
                return Ok(Err("guard staff.available has no known staff target".to_owned()));
            };
            let available = staff_dynamics::with_staff_status(world, |status| staff_dynamics::is_available(status, &staff_id))?;
            Ok(Ok(Actual::Bool(available)))
        }
    }
}

/// What a guard decides of the world: its answer, the text of a guard it can't read, or a guard the
/// contract must word (one this kernel does not read).
enum Decided {
    Judged(Judged<bool>),
    Contract(V),
}

/// `should_apply_effect_guard` of an event's row, read on the world.
fn guard_decides(world: &World, ins: &GuardIn<'_>, row: &EventRow) -> Result<Decided> {
    let Some(marker) = marker_of(row)? else {
        return Ok(Decided::Judged(Ok(true)));
    };
    let guard = match marker.get("guard") {
        None | Some(V::Null) => return Ok(Decided::Judged(Ok(true))),
        Some(guard) => guard,
    };
    let mut nodes = 0;
    match parse_guard(guard, 1, &mut nodes) {
        Some(parsed) => evaluate_guard(world, ins, row, &parsed).map(Decided::Judged),
        None => Ok(Decided::Contract(guard.clone())),
    }
}

/// `should_apply_effect_guard`: the guard's answer, or the `EventApplicationError` of a guard it can't
/// read. A guard the contract turns down is worded by the contract (`GuardExpression`, policy E15):
/// "EffectProgram guard is invalid: ..." with its ValueError's text. One the contract takes is a
/// refusal (an int past 64 bits).
pub fn guard<C: WorldCell, H: Host>(cell: &C, host: &mut H, ins: &GuardIn<'_>, row: &EventRow) -> std::result::Result<bool, H::Error> {
    let decided = cell.with(|world| guard_decides(world, ins, row))??;
    match decided {
        Decided::Judged(Ok(answer)) => Ok(answer),
        Decided::Judged(Err(text)) => Err(Error::failed(text).into()),
        Decided::Contract(guard) => match host.contract(&Contract::Guard, &guard) {
            Ok(_) => Err(Error::unsupported("a guard the contract takes and this kernel does not read").into()),
            Err(error) if error.class() == ErrorClass::Value => Err(Error::failed(format!("EffectProgram guard is invalid: {}", error.text())).into()),
            Err(error) => Err(error),
        },
    }
}

/// `effect_event_guard_or_suppress`: false suppresses, a guard that fails raises its own text.
pub fn guard_or_suppress<C: WorldCell, H: Host>(cell: &C, host: &mut H, ins: &GuardIn<'_>, row: &EventRow) -> std::result::Result<(), H::Error> {
    if guard(cell, host, ins, row)? {
        Ok(())
    } else {
        Err(Error::suppressed("EffectProgram guard evaluated false").into())
    }
}

// ---- What other modules ask ----------------------------------------------------------------------

/// `has_active_facility_modifier`.
pub fn has_active_modifier(world: &World, program: &Held, facility_id: &str) -> Result<bool> {
    program_state(world, program, |state| Ok(kit::has_active_modifier(state, facility_id)))
}

/// `has_pending_facility_modifier`: whether a pending `effect_program_apply` or `_expire` event
/// targets the facility. `PENDING_UNREAD` for a pending event without its row.
pub fn has_pending_modifier(world: &World, facility_id: &str) -> Result<bool> {
    for event in pending_rows(world)? {
        if !matches!(&*event.event_type, "effect_program_apply" | "effect_program_expire") {
            continue;
        }
        if is_text(map_of(&event.payload)?.get("facility_id"), facility_id) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `active_facility_outage`: `bool(infrastructure_event_context_ids(world, facility_id))`.
pub fn active_outage(world: &World, infrastructure: &Held, facility_id: &str) -> Result<bool> {
    let state = state_value(world, INFRASTRUCTURE, infrastructure)?;
    kit::active_outage(state.as_ref(), facility_id)
}

fn occupancy(world: &World, facility: &FacilityRow) -> Result<i64> {
    let mut count = facility.active_agent_ids.len();
    if world.schema_version()? == 6 {
        count += facility.resource_active_agent_ids.len();
    }
    Ok(count as i64)
}

fn effect_is_scheduled(program: &Program, effect_id: &str) -> bool {
    program.effect_ids.iter().any(|item| &**item == effect_id)
}

// ---- EffectProgramFacilityHandler ----------------------------------------------------------------

const FACILITY_APPLY_FIELDS: [&str; 5] = ["program_id", "effect_id", "modifier_id", "facility_id", "working_units"];
const FACILITY_EXPIRE_FIELDS: [&str; 4] = ["program_id", "effect_id", "modifier_id", "facility_id"];

/// What a facility modifier's event writes, once its checks passed.
#[derive(Clone)]
pub struct FacilityPlan {
    facility_id: Arc<str>,
    change: FacilityChange,
    /// The facility's working units from now.
    target: i64,
}

#[derive(Clone)]
enum FacilityChange {
    /// A start: the stack is made first with these baseline units, if there is none.
    Start { new_stack: Option<i64>, modifier: Modifier },
    /// An expiry: the other modifiers stay, or the stack goes with its last.
    Expire { modifier_id: Arc<str>, others: bool },
}

/// The checks of `EffectProgramFacilityHandler.apply`, on the world.
pub fn plan_facility(world: &World, program: &Held, infrastructure: &Held, row: &EventRow, expire: bool) -> Result<FacilityPlan> {
    let payload = map_of(&row.payload)?;
    let fields: &[&str] = if expire { &FACILITY_EXPIRE_FIELDS } else { &FACILITY_APPLY_FIELDS };
    if !has_exactly(payload, fields) {
        let action = if expire { "expire" } else { "apply" };
        return Err(Error::failed(format!("effect program {action} payload fields do not match")));
    }
    let program_id = required_str(payload, "program_id")?;
    let effect_id = required_str(payload, "effect_id")?;
    let modifier = required_str(payload, "modifier_id")?;
    let facility_id = required_str(payload, "facility_id")?;
    let scheduled = program_state(world, program, |state| {
        Ok(state.is_some_and(|state| state.programs.get(&**program_id).is_some_and(|program| effect_is_scheduled(program, effect_id))))
    })?;
    if !scheduled {
        return Err(Error::failed("effect program event does not reference a scheduled effect"));
    }
    let (working, capacity, occupied) = {
        let rows = world.rows()?;
        let Some(facility) = rows.facilities.get(facility_id) else {
            return Err(Error::failed("effect program references an unknown facility"));
        };
        (facility.working_units, facility.capacity, occupancy(world, facility)?)
    };
    if active_outage(world, infrastructure, facility_id)? {
        return Err(Error::failed("facility availability modifier conflicts with an active infrastructure outage"));
    }
    program_state(world, program, |state| {
        let Some(state) = state else {
            return Err(Error::unsupported("no effect_program_v1"));
        };
        let current = state.active_facility_modifiers.get(&**facility_id);
        if !expire {
            let units = match payload.get("working_units") {
                Some(V::Int(units)) => *units,
                Some(V::Big(_)) => return Err(Error::unsupported("working_units is past 64 bits")),
                _ => return Err(Error::failed("working_units must be an integer")),
            };
            if units < occupied || units < 0 || units > working || units > capacity {
                return Err(Error::failed("facility availability modifier violates capacity or occupancy"));
            }
            let baseline = current.map_or(working, |record| record.baseline_working_units);
            if current.is_some_and(|record| record.modifiers.iter().any(|item| item.modifier_id == *modifier)) {
                return Err(Error::suppressed("facility availability modifier is already active"));
            }
            let mut target = baseline.min(units);
            for item in current.iter().flat_map(|record| record.modifiers.iter()) {
                target = target.min(item.working_units);
            }
            let modifier = Modifier {
                modifier_id: modifier.clone(),
                program_id: program_id.clone(),
                effect_id: effect_id.clone(),
                working_units: units,
                start_minute: row.due_minute,
                sequence: row.sequence,
            };
            let new_stack = if current.is_some() { None } else { Some(working) };
            return Ok(FacilityPlan { facility_id: facility_id.clone(), change: FacilityChange::Start { new_stack, modifier }, target });
        }
        let Some(current) = current else {
            return Err(Error::suppressed("facility availability modifier is not active"));
        };
        if !current.modifiers.iter().any(|item| item.modifier_id == *modifier) {
            return Err(Error::suppressed("facility availability modifier already expired or never started"));
        }
        let mut target = current.baseline_working_units;
        let mut others = false;
        for item in current.modifiers.iter().filter(|item| item.modifier_id != *modifier) {
            target = target.min(item.working_units);
            others = true;
        }
        if target < occupied {
            return Err(Error::failed("modifier expiry would reduce capacity below active occupancy"));
        }
        Ok(FacilityPlan { facility_id: facility_id.clone(), change: FacilityChange::Expire { modifier_id: modifier.clone(), others }, target })
    })
}

/// The writes of a facility modifier's event, on the transaction's world, and the world's check.
pub fn write_facility<C: WorldCell, H: Host>(cell: &C, host: &mut H, plan: &FacilityPlan) -> std::result::Result<(), H::Error> {
    cell.with(|world| -> Result<()> {
        {
            let mut row = world.row_mut::<EffectProgramRow>(NAME)?;
            let records = &mut row.active_facility_modifiers;
            match &plan.change {
                FacilityChange::Start { new_stack, modifier } => {
                    if let Some(baseline) = new_stack {
                        records.insert(plan.facility_id.clone(), FacilityModifiers { baseline_working_units: *baseline, modifiers: Vec::new() });
                    }
                    let stack = records.get_mut(&*plan.facility_id).ok_or_else(|| Error::Key(plan.facility_id.to_string()))?;
                    stack.modifiers.push(modifier.clone());
                }
                FacilityChange::Expire { modifier_id, others: true } => {
                    let stack = records.get_mut(&*plan.facility_id).ok_or_else(|| Error::Key(plan.facility_id.to_string()))?;
                    stack.modifiers.retain(|item| item.modifier_id != *modifier_id);
                }
                FacilityChange::Expire { others: false, .. } => {
                    records.shift_remove(&*plan.facility_id);
                }
            }
        }
        let mut rows = world.rows_mut()?;
        let facility = rows.facilities.get_mut(&plan.facility_id).ok_or_else(|| Error::Key(plan.facility_id.to_string()))?;
        facility.working_units = plan.target;
        Ok(())
    })??;
    validate(host)
}

// ---- EffectProgramEnvironmentHandler -------------------------------------------------------------

const ZONE_EXPIRE_FIELDS: [&str; 4] = ["program_id", "effect_id", "modifier_id", "zone_id"];
const ENVIRONMENT_APPLY_FIELDS: [&str; 7] = ["program_id", "effect_id", "modifier_id", "zone_id", "op", "parameters", "end_minute"];
const ENVIRONMENT_OPERATORS: [&str; 6] =
    ["set_ambient_noise", "modify_demand_rate", "set_ventilation", "set_lighting", "set_privacy", "set_temperature"];

/// What the environment's rule reads besides the world's rows.
pub struct EnvironmentIn<'a> {
    pub program: &'a Held,
    /// `zone_environment_v1`.
    pub environment: &'a Held,
    /// `"resource_service_v1" in model_state`.
    pub service_present: bool,
    /// `ScenarioZoneEnvironmentConfig` of the configs, or None where its read raised (`CONFIG_UNREAD`).
    pub config: Option<&'a ZoneEnvironmentSpec>,
}

/// What an environment modifier's event writes, once its checks passed.
#[derive(Debug, Clone, PartialEq)]
pub enum EnvironmentPlan {
    Expire { modifier: Arc<str> },
    Apply { modifier: Arc<str>, record: V },
}

/// The checks of `EffectProgramEnvironmentHandler.apply`, on the world. The parameters' contract
/// goes through the host.
pub fn plan_environment<C: WorldCell, H: Host>(
    cell: &C,
    host: &mut H,
    ins: &EnvironmentIn<'_>,
    row: &EventRow,
    expire: bool,
) -> std::result::Result<EnvironmentPlan, H::Error> {
    let checked = cell.with(|world| environment_checks(world, ins, row, expire))??;
    let (modifier, op) = match checked {
        Checked::Expire(modifier) => return Ok(EnvironmentPlan::Expire { modifier }),
        Checked::Apply { modifier, op } => (modifier, op),
    };
    let raw = payload_item(row, "parameters")?.clone();
    let parsed = parameters(host, &op, &raw, Catch::Value, "zone environment parameters are invalid: ")?;
    let record = cell.with(|world| environment_record(world, ins, row, &modifier, &op, parsed))??;
    Ok(EnvironmentPlan::Apply { modifier, record })
}

enum Checked {
    Expire(Arc<str>),
    Apply { modifier: Arc<str>, op: Arc<str> },
}

fn environment_checks(world: &World, ins: &EnvironmentIn<'_>, row: &EventRow, expire: bool) -> Result<Checked> {
    let payload = map_of(&row.payload)?;
    let fields: &[&str] = if expire { &ZONE_EXPIRE_FIELDS } else { &ENVIRONMENT_APPLY_FIELDS };
    if !has_exactly(payload, fields) {
        return Err(Error::failed("zone environment event payload fields do not match"));
    }
    let (Some(program_id), Some(effect_id), Some(modifier), Some(zone_id)) =
        (str_of(payload, "program_id"), str_of(payload, "effect_id"), str_of(payload, "modifier_id"), str_of(payload, "zone_id"))
    else {
        return Err(Error::failed("zone environment event IDs must be strings"));
    };
    // The approved operator of a program that lists the effect; None when it doesn't.
    let approved_op = program_state(world, ins.program, |state| {
        Ok(state
            .and_then(|state| state.programs.get(&**program_id))
            .filter(|program| effect_is_scheduled(program, effect_id))
            .map(|program| program.operators.get(&**effect_id).cloned()))
    })?;
    // `model_state.get("zone_environment_v1")`, when it is a dict.
    let environment = state_value(world, ENVIRONMENT, ins.environment)?.filter(|state| matches!(state, V::Map(_)));
    let marker = marker_of(row)?;
    let installed = match (&approved_op, marker) {
        (Some(_), Some(marker)) => holds(marker.get("policy_dependencies"), ENVIRONMENT_POLICY)? && environment.is_some(),
        _ => false,
    };
    let (Some(approved_op), Some(_), Some(environment), true) = (approved_op, marker, environment, installed) else {
        return Err(Error::failed("zone environment event has no installed program policy"));
    };
    if *modifier.as_ref() != *modifier_id(program_id, effect_id) {
        return Err(Error::failed("zone environment modifier ID does not match its effect"));
    }
    let modifiers = match map_of(&environment)?.get("modifiers") {
        Some(V::Map(modifiers)) => modifiers,
        Some(_) => return Err(Error::unsupported("a value is not a plain dict")),
        None => return Err(Error::Key("modifiers".to_owned())),
    };
    if expire {
        let Some(record) = modifiers.get(&**modifier) else {
            return Err(Error::suppressed("zone environment modifier is not active"));
        };
        let record = map_of(record)?;
        let item = |key: &str| record.get(key).ok_or_else(|| Error::Key(key.to_owned()));
        if ne_text(item("program_id")?, program_id)
            || ne_text(item("effect_id")?, effect_id)
            || ne_text(item("zone_id")?, zone_id)
            || ne_text(item("end_event_id")?, &row.event_id)
            || ne_optional(item("start_event_id")?, row.causation_id.as_deref())
        {
            return Err(Error::failed("zone environment expiry does not match its start"));
        }
        return Ok(Checked::Expire(modifier.clone()));
    }
    let op = match payload.get("op") {
        Some(V::Str(op)) if ENVIRONMENT_OPERATORS.contains(&&**op) => op,
        Some(value) if !hashable(value) => return Err(Error::unsupported("an operator Python can't hash")),
        _ => return Err(Error::failed("zone environment operator is unsupported")),
    };
    if approved_op.as_deref() != Some(&**op) {
        return Err(Error::failed("zone environment operator does not match its program"));
    }
    if !zone_known(&*world.rows()?, zone_id)? {
        return Err(Error::failed("zone environment target does not exist"));
    }
    if modifiers.contains_key(&**modifier) {
        return Err(Error::suppressed("zone environment modifier is already active"));
    }
    Ok(Checked::Apply { modifier: modifier.clone(), op: op.clone() })
}

/// The checks after the parameters' contract, and the record of a new modifier.
fn environment_record(world: &World, ins: &EnvironmentIn<'_>, row: &EventRow, modifier: &Arc<str>, op: &str, parsed: V) -> Result<V> {
    let payload = map_of(&row.payload)?;
    if op == "modify_demand_rate" {
        let action = field_str(&parsed, "action")?;
        if matches!(&**action, "fetch_water" | "collect_meal") && !ins.service_present {
            return Err(Error::failed("zone demand action requires resource-service policy"));
        }
    }
    let duration = world.duration_minutes()?;
    // An int past 64 bits is outside the Run either way; any other type is not an int.
    let end_minute = match payload.get("end_minute") {
        Some(V::Int(minute)) if *minute > row.due_minute && *minute < duration => *minute,
        _ => return Err(Error::failed("zone environment modifier expiry minute is invalid")),
    };
    let mut expiries = Vec::new();
    for pending in pending_rows(world)? {
        if &*pending.event_type != "effect_program_environment_expire" {
            continue;
        }
        if is_text(map_of(&pending.payload)?.get("modifier_id"), modifier) {
            expiries.push(pending);
        }
    }
    let end_event_id = match expiries.as_slice() {
        [expiry] if expiry.due_minute == end_minute => expiry.event_id.clone(),
        _ => return Err(Error::failed("zone environment modifier has no unique expiry event")),
    };
    // The Python rule wraps a ValueError of this too, but a world whose config fails it fails the
    // copy the Python rule makes first (`from_dict` checks the configs): its error is raised as it is.
    let config = ins.config.ok_or_else(|| Error::unsupported(CONFIG_UNREAD))?;
    if matches!(op, "set_ventilation" | "set_lighting" | "set_privacy" | "set_temperature") && &*config.policy_version != "zone-environment-v2" {
        return Err(Error::failed("condition modifier requires zone-environment-v2"));
    }
    let marker = marker_of(row)?.ok_or_else(|| Error::unsupported("no marker"))?;
    let assumptions = match marker.get("assumption_ids") {
        Some(V::List(items)) if items.iter().all(|item| matches!(item, V::Str(_))) => str_list(marker, "assumption_ids")?,
        _ => return Err(Error::failed("zone environment assumption provenance is invalid")),
    };
    let assumptions = sorted_unique(assumptions.iter().map(|item| &***item));
    let mut record = Doc::with_capacity(12);
    record.insert(text("modifier_id"), V::Str(modifier.clone()));
    record.insert(text("program_id"), V::Str(required_str(payload, "program_id")?.clone()));
    record.insert(text("effect_id"), V::Str(required_str(payload, "effect_id")?.clone()));
    record.insert(text("op"), str_v(op));
    record.insert(text("zone_id"), V::Str(required_str(payload, "zone_id")?.clone()));
    record.insert(text("parameters"), parsed);
    record.insert(text("start_event_id"), V::Str(row.event_id.clone()));
    record.insert(text("end_event_id"), V::Str(end_event_id));
    record.insert(text("start_minute"), V::Int(row.due_minute));
    record.insert(text("end_minute"), V::Int(end_minute));
    record.insert(text("source_id"), V::Str(config.source_id.clone()));
    record.insert(text("assumption_ids"), str_list_v(assumptions));
    Ok(V::Map(record))
}

/// The writes of an environment modifier's event, on the transaction's world, and the world's check:
/// a ValueError of it is "zone environment modifier is invalid: ..." (the handler's `except
/// ValueError` after `except (EventApplicationError, EventSuppressed): raise`).
pub fn write_environment<C: WorldCell, H: Host>(cell: &C, host: &mut H, plan: &EnvironmentPlan) -> std::result::Result<(), H::Error> {
    cell.with(|world| {
        write_value(world, ENVIRONMENT, &["modifiers"], |modifiers| {
            match plan {
                EnvironmentPlan::Expire { modifier } => {
                    modifiers.shift_remove(&**modifier).ok_or_else(|| Error::Key(modifier.to_string()))?;
                }
                EnvironmentPlan::Apply { modifier, record } => {
                    modifiers.insert(modifier.clone(), record.clone());
                }
            }
            Ok(())
        })
    })??;
    match validate(host) {
        Err(error) if error.class() == ErrorClass::Value && !error.is_outcome() => {
            Err(Error::failed(format!("zone environment modifier is invalid: {}", error.text())).into())
        }
        other => other,
    }
}

// ---- EffectProgramTemporarySpaceHandler ----------------------------------------------------------

/// What the temporary space's rule reads besides the world's rows.
pub struct SpaceIn<'a> {
    pub guard: &'a GuardIn<'a>,
    /// `temporary_spaces_v1`.
    pub spaces: &'a Held,
}

/// What a temporary space's event writes, once its checks passed.
#[derive(Debug, Clone, PartialEq)]
pub enum SpacePlan {
    Expire { modifier: Arc<str> },
    Register { record: V },
}

/// The checks of `EffectProgramTemporarySpaceHandler.apply`, on the world: the guard and the
/// parameters' contract go through the host.
pub fn plan_temporary_space<C: WorldCell, H: Host>(
    cell: &C,
    host: &mut H,
    ins: &SpaceIn<'_>,
    row: &EventRow,
    expire: bool,
) -> std::result::Result<SpacePlan, H::Error> {
    let modifier = cell.with(|world| space_checks(world, ins, row, expire))??;
    if expire {
        return Ok(SpacePlan::Expire { modifier });
    }
    // "temporary-space operator differs from its approved program" can't follow: the program's
    // operator for the effect and the payload's op were both just seen to be add_temporary_space.
    guard_or_suppress(cell, host, ins.guard, row)?;
    let raw = payload_item(row, "parameters")?.clone();
    let parsed = parameters(host, "add_temporary_space", &raw, Catch::Value, "temporary-space parameters are invalid: ")?;
    let record = cell.with(|world| space_record(world, row, &modifier, &parsed))??;
    Ok(SpacePlan::Register { record })
}

fn space_checks(world: &World, ins: &SpaceIn<'_>, row: &EventRow, expire: bool) -> Result<Arc<str>> {
    let payload = map_of(&row.payload)?;
    let state = state_value(world, TEMPORARY_SPACES, ins.spaces)?.filter(|state| matches!(state, V::Map(_)));
    let marker = marker_of(row)?;
    let (program_id, effect_id) = (payload.get("program_id"), payload.get("effect_id"));
    // `programs.get(program_id) if isinstance(program_id, str) else None`, then whether it lists
    // the effect with this operator.
    let approved = match (program_id, effect_id) {
        (Some(V::Str(program_id)), effect_id) => program_state(world, ins.guard.program, |state| {
            Ok(state.and_then(|state| state.programs.get(&**program_id)).is_some_and(|program| match effect_id {
                Some(V::Str(effect_id)) => {
                    effect_is_scheduled(program, effect_id) && program.operators.get(&**effect_id).is_some_and(|op| &**op == "add_temporary_space")
                }
                _ => false,
            }))
        })?,
        _ => false,
    };
    let references = (|| -> Result<bool> {
        let (Some(_), true, Some(V::Str(program_id)), Some(V::Str(effect_id))) = (&state, approved, program_id, effect_id) else {
            return Ok(false);
        };
        if !is_text(payload.get("modifier_id"), &modifier_id(program_id, effect_id)) {
            return Ok(false);
        }
        let Some(marker) = marker else {
            return Ok(false);
        };
        if !holds(marker.get("policy_dependencies"), TEMPORARY_SPACES)? {
            return Ok(false);
        }
        let rows = world.rows()?;
        key_in(|zone| zone_known(&rows, zone), payload.get("zone_id"))
    })()?;
    let (Some(state), Some(_), true) = (state, marker, references) else {
        return Err(Error::failed("temporary-space event does not reference an approved installed effect"));
    };
    let program_id = required_str(payload, "program_id")?;
    let effect_id = required_str(payload, "effect_id")?;
    let modifier = required_str(payload, "modifier_id")?;
    let zone_id = required_str(payload, "zone_id")?;
    if expire {
        if !has_exactly(payload, &ZONE_EXPIRE_FIELDS) {
            return Err(Error::failed("temporary-space expiry payload fields do not match"));
        }
        let spaces = map_of(&state)?.get("spaces").ok_or_else(|| Error::Key("spaces".to_owned()))?;
        let matches = match map_of(spaces)?.get(&**modifier) {
            Some(V::Map(space)) => {
                let item = |key: &str| space.get(key).ok_or_else(|| Error::Key(key.to_owned()));
                item("active")?.is_truthy()
                    && !ne_text(item("zone_id")?, zone_id)
                    && !ne_text(item("program_id")?, program_id)
                    && !ne_text(item("effect_id")?, effect_id)
                    && !ne_optional(item("start_event_id")?, row.causation_id.as_deref())
                    && py_eq(&V::Int(row.due_minute), item("end_minute")?)
            }
            _ => false,
        };
        if !matches {
            return Err(Error::suppressed("temporary-space effect did not start or does not match its expiry"));
        }
        return Ok(modifier.clone());
    }
    if !has_exactly(payload, &ENVIRONMENT_APPLY_FIELDS) || !is_text(payload.get("op"), "add_temporary_space") {
        return Err(Error::failed("temporary-space event payload fields do not match"));
    }
    Ok(modifier.clone())
}

/// The checks after the parameters' contract, and the record `register_temporary_space` takes.
fn space_record(world: &World, row: &EventRow, modifier: &Arc<str>, parsed: &V) -> Result<V> {
    let payload = map_of(&row.payload)?;
    let duration = world.duration_minutes()?;
    // An int past 64 bits is outside the interval either way; any other type is not an int.
    let end_minute = match payload.get("end_minute") {
        Some(V::Int(minute)) if row.due_minute < *minute && *minute < duration => *minute,
        _ => return Err(Error::failed("temporary-space end minute is outside its approved interval")),
    };
    // sorted(set([event.source_id, *assumption_ids])); None doesn't sort with a str.
    let Some(source) = row.source_id.as_deref() else {
        return Err(Error::unsupported("no source"));
    };
    let marker = marker_of(row)?.ok_or_else(|| Error::unsupported("no marker"))?;
    let assumptions = str_list(marker, "assumption_ids")?;
    let sources = sorted_unique(std::iter::once(source).chain(assumptions.iter().map(|item| &***item)));
    let mut record = Doc::with_capacity(13);
    record.insert(text("space_id"), V::Str(modifier.clone()));
    record.insert(text("zone_id"), V::Str(required_str(payload, "zone_id")?.clone()));
    record.insert(text("people_capacity"), field(parsed, "people_capacity")?.clone());
    record.insert(text("sleeping_capacity"), field(parsed, "sleeping_capacity")?.clone());
    record.insert(text("start_minute"), V::Int(row.due_minute));
    record.insert(text("end_minute"), V::Int(end_minute));
    record.insert(text("active"), V::Bool(true));
    record.insert(text("start_event_id"), V::Str(row.event_id.clone()));
    record.insert(text("end_event_id"), V::Null);
    record.insert(text("program_id"), V::Str(required_str(payload, "program_id")?.clone()));
    record.insert(text("effect_id"), V::Str(required_str(payload, "effect_id")?.clone()));
    record.insert(text("source_ids"), str_list_v(sources));
    record.insert(text("geometry_status"), str_v("zone_only_capacity"));
    Ok(V::Map(record))
}

/// The writes of a temporary space's event, on the transaction's world (`spaces`: whether it holds
/// `temporary_spaces_v1`), and the world's check. `register_temporary_space`'s ValueError is the
/// event's failure with its text.
pub fn write_temporary_space<C: WorldCell, H: Host>(
    cell: &C,
    host: &mut H,
    spaces: &Held,
    row: &EventRow,
    plan: &SpacePlan,
) -> std::result::Result<(), H::Error> {
    match plan {
        SpacePlan::Expire { modifier } => {
            // False only for a space that is gone or not active, which the checks saw it is not.
            let expired = cell.with(|world| overlay::expire_space(world, spaces, modifier, &row.event_id, row.causation_id.as_deref()))??;
            if !expired {
                return Err(Error::suppressed("temporary-space effect is already expired").into());
            }
        }
        SpacePlan::Register { record } => {
            let registered = cell.with(|world| overlay::register_space(world, spaces, record.clone(), &row.event_id))?;
            if let Err(error) = registered {
                return Err(wrapped(H::Error::from(Error::refused(REGISTER, error)), Catch::Value, ""));
            }
        }
    }
    validate(host)
}

// ---- EffectProgramGeometryHandler ----------------------------------------------------------------

const GEOMETRY_FIELDS: [&str; 7] = ["program_id", "effect_id", "modifier_id", "target_id", "op", "parameters", "end_minute"];
const GEOMETRY_OPERATORS: [&str; 5] = ["move_facility", "transfer_equipment", "set_partition", "set_route_access", "temporary_room_geometry"];

/// What the geometry rule's checks read besides the world's rows.
pub struct GeometryIn<'a> {
    pub guard: &'a GuardIn<'a>,
    /// `geometry_overlay_v1`.
    pub overlay: &'a Held,
}

/// The checks of `EffectProgramGeometryHandler.apply` before its change, on the world: the
/// approved program, the guard, and for an expiry that the change is active.
pub fn check_geometry<C: WorldCell, H: Host>(cell: &C, host: &mut H, ins: &GeometryIn<'_>, row: &EventRow, expire: bool) -> std::result::Result<(), H::Error> {
    let op = cell.with(|world| geometry_checks(world, ins, row))??;
    guard_or_suppress(cell, host, ins.guard, row)?;
    if expire && !cell.with(|world| geometry_active(world, ins, row, &op))?? {
        return Err(Error::suppressed("geometry modifier was not applied or already expired").into());
    }
    Ok(())
}

fn geometry_checks(world: &World, ins: &GeometryIn<'_>, row: &EventRow) -> Result<Arc<str>> {
    let payload = map_of(&row.payload)?;
    if !has_exactly(payload, &GEOMETRY_FIELDS) {
        return Err(Error::failed("geometry effect has invalid fields or operator"));
    }
    let op = match payload.get("op") {
        Some(V::Str(op)) if GEOMETRY_OPERATORS.contains(&&**op) => op,
        Some(value) if !hashable(value) => return Err(Error::unsupported("an operator Python can't hash")),
        _ => return Err(Error::failed("geometry effect has invalid fields or operator")),
    };
    let (program_id, effect_id) = (payload.get("program_id"), payload.get("effect_id"));
    // `.get(payload["program_id"], {})` and `.get(payload["effect_id"])` raise on a key Python
    // can't hash, whatever the dicts hold.
    if program_id.is_some_and(|id| !hashable(id)) || effect_id.is_some_and(|id| !hashable(id)) {
        return Err(Error::unsupported("an ID Python can't hash"));
    }
    let approved = match (program_id, effect_id) {
        (Some(V::Str(program_id)), Some(V::Str(effect_id))) => {
            program_state(world, ins.guard.program, |state| {
                Ok(state
                    .and_then(|state| state.programs.get(&**program_id))
                    .and_then(|program| program.operators.get(&**effect_id))
                    .is_some_and(|approved| approved == op))
            })? && is_text(payload.get("modifier_id"), &modifier_id(program_id, effect_id))
        }
        _ => false,
    };
    if !approved {
        return Err(Error::failed("geometry effect does not reference its approved program"));
    }
    Ok(op.clone())
}

/// Whether the change an expiry ends is active: the Python rule's reads, `.get(key, {})` all the way
/// down, of the overlay (`{}` without it).
fn geometry_active(world: &World, ins: &GeometryIn<'_>, row: &EventRow, op: &str) -> Result<bool> {
    let empty = V::Map(Doc::new());
    let overlay = state_value(world, overlay::OVERLAY_KEY, ins.overlay)?.unwrap_or_else(|| empty.clone());
    let payload = map_of(&row.payload)?;
    let modifier = payload.get("modifier_id").ok_or_else(|| Error::Key("modifier_id".to_owned()))?;
    let get_or_empty = |map: &'_ V, key: &str| -> Result<V> { Ok(get_of(map, &str_v(key))?.cloned().unwrap_or_else(|| empty.clone())) };
    Ok(match op {
        "set_partition" => get_of(&get_or_empty(&overlay, "partitions")?, modifier)?.is_some(),
        "temporary_room_geometry" => {
            let parameters = payload.get("parameters").cloned().unwrap_or_else(|| empty.clone());
            let room_id = get_of(&parameters, &str_v("room_id"))?.cloned().unwrap_or(V::Null);
            match get_of(&get_or_empty(&overlay, "temporary_rooms")?, &room_id)? {
                Some(V::Map(room)) => {
                    matches!(room.get("active"), Some(V::Bool(true)))
                        && !ne_optional(room.get("start_event_id").unwrap_or(&V::Null), row.causation_id.as_deref())
                        && py_eq(room.get("end_minute").unwrap_or(&V::Null), &V::Int(row.due_minute))
                }
                _ => false,
            }
        }
        _ => {
            let routes = get_or_empty(&overlay, "route_modifiers")?;
            let target = payload.get("target_id").ok_or_else(|| Error::Key("target_id".to_owned()))?;
            let route = get_of(&routes, target)?.cloned().unwrap_or_else(|| empty.clone());
            get_of(&get_or_empty(&route, "modifiers")?, modifier)?.is_some()
        }
    })
}

/// `apply_geometry_change(world, event, expire=expire)` on the transaction's world (a direct call;
/// its refusal is its `KernelRefusal`), and the world's check: their ValueError, KeyError or
/// TypeError is the event's failure with its text. `GEOMETRY_PLAN_UNREAD` for a floor plan the
/// PyO3 crate could not read.
pub fn write_geometry<C: WorldCell, H: Host>(cell: &C, host: &mut H, change: &overlay::Change<'_>, row: &EventRow, expire: bool) -> std::result::Result<(), H::Error> {
    if let Err(error) = overlay::apply_change(cell, host, change, row, expire) {
        if error.class() == ErrorClass::Refused {
            let reason = error.text();
            if reason == overlay::PLAN_UNREAD {
                return Err(Error::unsupported(GEOMETRY_PLAN_UNREAD).into());
            }
            return Err(Error::failed(refused_text(APPLY_GEOMETRY, &reason)).into());
        }
        return Err(wrapped(error, Catch::ValueKeyOrType, ""));
    }
    validate(host).map_err(|error| wrapped(error, Catch::ValueKeyOrType, ""))
}

// ---- EffectProgramStructureHandler ---------------------------------------------------------------

/// The branches of `EffectProgramStructureHandler.apply`, told apart by the payload's fields in the
/// Python rule's order.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Structure {
    TaskOrder,
    Handoff,
    Repair,
    Role,
    Add,
}

pub fn structure_of(row: &EventRow) -> Result<Structure> {
    let payload = map_of(&row.payload)?;
    let base = ["program_id", "effect_id", "modifier_id", "op"];
    let shape = |extra: &[&str]| payload.len() == 4 + extra.len() && base.iter().chain(extra).all(|name| payload.contains_key(*name));
    Ok(if shape(&["staff_id", "parameters"]) {
        if str_of(payload, "op").is_some_and(|op| &**op == "staff_task_order") {
            Structure::TaskOrder
        } else {
            Structure::Handoff
        }
    } else if shape(&["facility_id", "parameters"]) {
        Structure::Repair
    } else if shape(&["staff_id", "skills"]) {
        Structure::Role
    } else {
        Structure::Add
    })
}

/// What the structure's rules read besides the world's rows.
pub struct StructureIn<'a> {
    pub guard: &'a GuardIn<'a>,
    /// `"shared_operations_v1" in model_state`.
    pub shared: bool,
    /// `infrastructure_v1`.
    pub infrastructure: &'a Held,
}

/// The first check of `staff_handoff` and `facility_repair`, in the Python rule's order: the marker
/// and the payload reference an effect of `op` that a current approved program scheduled as this
/// event, and `target` is a str.
fn references_program(world: &World, program: &Held, row: &EventRow, op: &str, target: &str) -> Result<bool> {
    let payload = map_of(&row.payload)?;
    let (program_id, effect_id) = (payload.get("program_id"), payload.get("effect_id"));
    // `state.get("programs", {}).get(program_id) if isinstance(state, dict) else None`, before the
    // checks: whether it lists the effect, with this operator, and this event.
    let listed = program_state(world, program, |state| {
        let Some(state) = state else {
            return Ok(None);
        };
        match program_id {
            Some(V::Str(program_id)) => Ok(state.programs.get(&**program_id).map(|program| match effect_id {
                Some(V::Str(effect_id)) => (
                    effect_is_scheduled(program, effect_id),
                    program.operators.get(&**effect_id).is_some_and(|item| &**item == op),
                    program.scheduled_event_ids.iter().any(|item| *item == row.event_id),
                ),
                _ => (false, false, false),
            })),
            Some(value) if !hashable(value) => Err(Error::unsupported("a program ID Python can't hash")),
            _ => Ok(None),
        }
    })?;
    let Some(marker) = marker_of(row)? else {
        return Ok(false);
    };
    if differ(marker.get("program_id"), program_id)?
        || differ(marker.get("effect_id"), effect_id)?
        || !is_text(marker.get("op"), op)
        || !is_text(marker.get("phase"), "start")
    {
        return Ok(false);
    }
    let (Some((true, true, true)), Some(V::Str(program_id)), Some(V::Str(effect_id))) = (listed, program_id, effect_id) else {
        return Ok(false);
    };
    Ok(is_text(payload.get("modifier_id"), &modifier_id(program_id, effect_id))
        && is_text(payload.get("op"), op)
        && matches!(payload.get(target), Some(V::Str(_))))
}

/// `state.get("programs", {}).get(payload.get("program_id")) if isinstance(state, dict) else None`
/// of `set_staff_role` and the additions, and whether it lists the payload's effect: None when there
/// is no program. A program ID Python can't hash raises, before the checks.
fn program_lists_effect(world: &World, program: &Held, payload: &Doc) -> Result<Option<bool>> {
    let (program_id, effect_id) = (payload.get("program_id"), payload.get("effect_id"));
    program_state(world, program, |state| {
        let Some(state) = state else {
            return Ok(None);
        };
        match program_id {
            Some(V::Str(program_id)) => Ok(state.programs.get(&**program_id).map(|program| match effect_id {
                Some(V::Str(effect_id)) => effect_is_scheduled(program, effect_id),
                _ => false,
            })),
            Some(value) if !hashable(value) => Err(Error::unsupported("a program ID Python can't hash")),
            _ => Ok(None),
        }
    })
}

// ---- staff_task_order ----

/// A staff task order that passed its checks: `order_staff_operation`'s arguments.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskOrder {
    staff_id: Arc<str>,
    task_kind: Arc<str>,
    target_zone_id: Arc<str>,
    target_id: Option<Arc<str>>,
    reason: Arc<str>,
    causation_event_id: Arc<str>,
    source_id: Option<Arc<str>>,
}

/// The checks of `staff_task_order`, on the world: the approval chain of `staff_handoff` and an event
/// due now, the parameters' contract, and the guard.
pub fn plan_task_order<C: WorldCell, H: Host>(cell: &C, host: &mut H, ins: &StructureIn<'_>, row: &EventRow) -> std::result::Result<TaskOrder, H::Error> {
    let approved = cell.with(|world| -> Result<bool> {
        Ok(references_program(world, ins.guard.program, row, "staff_task_order", "staff_id")? && world.minute_index()? == row.due_minute)
    })??;
    if !approved {
        return Err(Error::failed("staff task order does not reference a current approved program").into());
    }
    let raw = payload_item(row, "parameters")?.clone();
    let order = parameters(host, "staff_task_order", &raw, Catch::ValueOrType, "staff task order parameters are invalid: ")?;
    guard_or_suppress(cell, host, ins.guard, row)?;
    let target_id = match field(&order, "target_id")? {
        V::Null => None,
        value => Some(kit::exact_str(value)?.clone()),
    };
    Ok(TaskOrder {
        staff_id: required_str(map_of(&row.payload)?, "staff_id")?.clone(),
        task_kind: field_str(&order, "task_kind")?.clone(),
        target_zone_id: field_str(&order, "target_zone_id")?.clone(),
        target_id,
        reason: field_str(&order, "reason")?.clone(),
        causation_event_id: row.event_id.clone(),
        source_id: row.source_id.clone(),
    })
}

/// `order_staff_operation` on the transaction's world (its checks raise before any change), and the
/// world's check.
pub fn write_task_order<C: WorldCell, H: Host>(cell: &C, host: &mut H, inputs: &staff_operation_events::Inputs<'_>, order: &TaskOrder) -> std::result::Result<(), H::Error>
where
    H::Error: From<WriteError>,
{
    let order = staff_order::Order {
        staff_id: &order.staff_id,
        task_kind: &order.task_kind,
        target_zone_id: &order.target_zone_id,
        target_id: order.target_id.as_deref(),
        reason: &order.reason,
        causation_event_id: &order.causation_event_id,
        source_id: order.source_id.as_deref(),
    };
    cell.with(|world| staff_order::order_operation(world, inputs, &order))??;
    validate(host)
}

// ---- staff_handoff ----

/// A handoff that passed its first checks.
#[derive(Debug, Clone, PartialEq)]
pub struct Handoff {
    pub staff_id: Arc<str>,
    pub replacement_id: Arc<str>,
    pub task_id: Arc<str>,
    pub reason_code: Arc<str>,
    pub event_id: Arc<str>,
}

/// `_staff_has_task_skill`.
fn has_task_skill(skills: &Skills, task_kind: Option<&str>) -> bool {
    let wanted: [&str; 2] = match task_kind {
        Some("cleaning") => ["cleaning", "sanitation"],
        Some("maintenance") => ["maintenance", "repair"],
        Some("information_outreach") => ["support", "information"],
        Some("intake") => ["support", "intake"],
        Some("patrol") => ["support", "patrol"],
        // Another str, or not a str at all: no skill.
        _ => return false,
    };
    skills.0.iter().any(|skill| wanted.contains(&&**skill))
}

/// The checks of `staff_handoff` before its transaction, on the world: the approved program, the
/// parameters' contract, the participants and the task, and the guard.
pub fn plan_handoff<C: WorldCell, H: Host>(cell: &C, host: &mut H, ins: &StructureIn<'_>, row: &EventRow) -> std::result::Result<Handoff, H::Error> {
    if !cell.with(|world| references_program(world, ins.guard.program, row, "staff_handoff", "staff_id"))?? {
        return Err(Error::failed("staff handoff does not reference an approved program").into());
    }
    let staff_id = required_str(map_of(&row.payload)?, "staff_id")?.clone();
    let raw = payload_item(row, "parameters")?.clone();
    let handoff = parameters(host, "staff_handoff", &raw, Catch::ValueOrType, "staff handoff parameters are invalid: ")?;
    let replacement = field(&handoff, "replacement_staff_id")?;
    let task = field(&handoff, "task_id")?;
    let valid = cell.with(|world| -> Result<bool> {
        let rows = world.rows()?;
        Ok(!ne_text(field(&handoff, "staff_id")?, &staff_id)
            && !matches!(replacement, V::Null)
            && rows.staff.contains_key(&staff_id)
            && key_in(|id| Ok(rows.staff.contains_key(id)), Some(replacement))?
            && key_in(|id| Ok(rows.active_tasks.contains_key(id)), Some(task))?
            && ins.shared)
    })??;
    if !valid {
        return Err(Error::failed("staff handoff participants or task are invalid").into());
    }
    let plan = Handoff {
        staff_id,
        replacement_id: kit::exact_str(replacement)?.clone(),
        task_id: kit::exact_str(task)?.clone(),
        reason_code: field_str(&handoff, "reason_code")?.clone(),
        event_id: row.event_id.clone(),
    };
    guard_or_suppress(cell, host, ins.guard, row)?;
    Ok(plan)
}

fn idle_record() -> OperationRecord {
    OperationRecord {
        phase: Phase::Idle,
        task_id: None,
        task_kind: None,
        target_id: None,
        target_zone_id: None,
        reason: None,
        started_minute: None,
        travel_due_minute: None,
        arrival_minute: None,
        due_minute: None,
        route_zone_ids: Vec::new(),
        route_edge_ids: Vec::new(),
        travel_minutes: 0,
        causation_event_ids: Vec::new(),
    }
}

/// What `staff_handoff`'s transaction reads besides the world's rows.
pub struct HandoffIn<'a> {
    /// `staff_operations_v1`.
    pub operations: &'a Held,
}

/// The checks and the writes of `staff_handoff` on the transaction's world, and the world's check:
/// the working operation and its one pending completion go to the replacement, and the previous
/// member is idle. The slot of the completion, which gets a new row, is added to `taken`. Gives the
/// marker's assumptions, the sources of the receipt.
pub fn write_handoff<C: WorldCell, H: Host>(
    cell: &C,
    host: &mut H,
    ins: &HandoffIn<'_>,
    plan: &Handoff,
    row: &EventRow,
    taken: &mut Vec<u64>,
) -> std::result::Result<Vec<Arc<str>>, H::Error> {
    cell.with(|world| handoff_writes(world, ins, plan, taken))??;
    host.slots_changed(taken)?;
    validate(host)?;
    // The checks saw the marker is a mapping.
    let marker = marker_of(row)?.ok_or_else(|| Error::unsupported("no marker"))?;
    Ok(str_list(marker, "assumption_ids")?.into_iter().cloned().collect())
}

fn handoff_writes(world: &World, ins: &HandoffIn<'_>, plan: &Handoff, taken: &mut Vec<u64>) -> Result<()> {
    let minute = world.minute_index()?;
    let (staff_id, replacement_id) = (&plan.staff_id, &plan.replacement_id);
    // `operations.get("staff", {}).get(id) if isinstance(operations, dict) else None`.
    let (old_record, replacement_record) = {
        let records = kit::held_row::<StaffOperationsRow>(world, STAFF_OPERATIONS, ins.operations)?;
        let record_of = |id: &str| records.as_ref().and_then(|row| row.staff.get(id)).cloned();
        (record_of(staff_id), record_of(replacement_id))
    };
    let (task_due, task_key) = {
        let rows = world.rows()?;
        let task = rows.active_tasks.get(&plan.task_id).ok_or_else(|| Error::Key(plan.task_id.to_string()))?;
        let old_staff = rows.staff.get(staff_id).ok_or_else(|| Error::Key(staff_id.to_string()))?;
        let replacement = rows.staff.get(replacement_id).ok_or_else(|| Error::Key(replacement_id.to_string()))?;
        let qualified = (|| {
            if &*task.kind != "staff_operation" || task.staff_id.as_deref() != Some(&**staff_id) || task.due_minute <= minute {
                return false;
            }
            let Some(old_record) = &old_record else {
                return false;
            };
            if old_record.phase != Phase::Working || old_record.task_id.as_deref() != Some(&*task.id) {
                return false;
            }
            let Some(replacement_record) = &replacement_record else {
                return false;
            };
            let target_zone = old_record.target_zone_id.as_deref();
            if replacement_record.phase != Phase::Idle
                || replacement.active_task_id.is_some()
                || Some(&*old_staff.zone_id) != target_zone
                || Some(&*replacement.zone_id) != target_zone
            {
                return false;
            }
            has_task_skill(&replacement.skills, old_record.task_kind.as_deref())
        })();
        if !qualified {
            return Err(Error::failed("staff handoff requires a qualified idle replacement already at active work"));
        }
        (task.due_minute, task.id.clone())
    };
    let Some(old_record) = old_record else {
        return Err(Error::unsupported("no record"));
    };
    let available = staff_dynamics::with_staff_status(world, |status| staff_dynamics::is_available(status, replacement_id))?
        && staff_operation_events::dynamics_available(world, replacement_id, minute).map_err(|error| Error::refused(AVAILABLE, error))?;
    if !available {
        return Err(Error::failed("staff handoff replacement is unavailable"));
    }
    let completions: Vec<(u64, Arc<EventRow>)> = {
        let queue = world.queue()?;
        let mut found = Vec::new();
        for (_, serial) in queue.ordered() {
            let pending = queue.slot(*serial).row.clone().ok_or_else(|| Error::unsupported(PENDING_UNREAD))?;
            if &*pending.event_type != "staff_operation_complete" {
                continue;
            }
            let payload = map_of(&pending.payload)?;
            if is_text(payload.get("task_id"), &task_key) && is_text(payload.get("staff_id"), staff_id) && pending.due_minute == task_due {
                found.push((*serial, pending));
            }
        }
        found
    };
    let [(serial, completion)] = completions.as_slice() else {
        return Err(Error::failed("staff handoff requires one matching pending completion"));
    };
    // The records were found in `operations["staff"]`.
    if matches!(ins.operations, Held::Absent) {
        return Err(Error::unsupported("no staff_operations_v1"));
    }
    let V::Map(mut payload) = completion.payload.clone() else {
        return Err(Error::unsupported("a payload is not a mapping"));
    };
    payload.insert(text("staff_id"), V::Str(replacement_id.clone()));
    let V::Map(provenance) = completion.provenance.clone() else {
        return Err(Error::unsupported("a provenance is not a mapping"));
    };
    let replaced = kit::new_event(kit::NewEvent {
        event_id: completion.event_id.clone(),
        kind: completion.kind,
        due_minute: completion.due_minute,
        priority: completion.priority,
        sequence: completion.sequence,
        event_type: completion.event_type.clone(),
        payload,
        source_id: completion.source_id.clone(),
        causation_id: completion.causation_id.clone(),
        provenance,
    })?;
    // `event_queue.replace`: the same identity, as a new row of the slot.
    {
        let mut queue = world.queue_mut()?;
        queue.rekey(*serial, (replaced.due_minute, replaced.priority, replaced.sequence));
        queue.set_row(*serial, Arc::new(replaced));
    }
    taken.push(*serial);
    {
        let mut rows = world.rows_mut()?;
        rows.active_tasks.get_mut(&plan.task_id).ok_or_else(|| Error::Key(plan.task_id.to_string()))?.staff_id = Some(replacement_id.clone());
        let old_staff = rows.staff.get_mut(staff_id).ok_or_else(|| Error::Key(staff_id.to_string()))?;
        old_staff.work_queue.clear();
        old_staff.active_task_id = None;
        old_staff.busy_until = 0;
    }
    world.row_mut::<StaffOperationsRow>(STAFF_OPERATIONS)?.staff.insert(staff_id.clone(), idle_record());
    {
        let mut rows = world.rows_mut()?;
        let replacement = rows.staff.get_mut(replacement_id).ok_or_else(|| Error::Key(replacement_id.to_string()))?;
        replacement.active_task_id = Some(plan.task_id.clone());
        replacement.work_queue = vec![plan.task_id.clone()];
        replacement.busy_until = task_due;
    }
    let mut handed_off = old_record;
    handed_off.reason = Some(plan.reason_code.clone());
    let mut causes: Vec<Arc<str>> = handed_off.causation_event_ids.clone();
    causes.push(plan.event_id.clone());
    causes.sort_unstable();
    causes.dedup();
    handed_off.causation_event_ids = causes;
    world.row_mut::<StaffOperationsRow>(STAFF_OPERATIONS)?.staff.insert(replacement_id.clone(), handed_off);
    Ok(())
}

/// `make_staff_handoff_record(...)[0]` once the handoff is committed, or None without the shared
/// contract.
pub fn handoff_receipt(world: &World, shared: bool, plan: &Handoff, sources: Vec<Arc<str>>) -> Result<Option<Made>> {
    if !shared {
        return Ok(None);
    }
    care_receipts::handoff_record(world, &plan.task_id, &plan.staff_id, &plan.replacement_id, &plan.reason_code, &plan.event_id, sources).map(Some)
}

// ---- facility_repair ----

/// `facility_repair`, in place like the Python rule: the checks, the guard, the facility's (and the
/// infrastructure's baseline) working units, the world's check (whose error goes on as it is, with
/// the world changed) and the receipt.
pub fn facility_repair<C: WorldCell, H: Host>(cell: &C, host: &mut H, ins: &StructureIn<'_>, row: &EventRow) -> std::result::Result<Option<Made>, H::Error> {
    if !cell.with(|world| references_program(world, ins.guard.program, row, "facility_repair", "facility_id"))?? {
        return Err(Error::failed("facility repair does not reference an approved program").into());
    }
    let facility_id = required_str(map_of(&row.payload)?, "facility_id")?.clone();
    let raw = payload_item(row, "parameters")?.clone();
    let repair = parameters(host, "facility_repair", &raw, Catch::ValueOrType, "facility repair parameters are invalid: ")?;
    let known = cell.with(|world| -> Result<bool> { Ok(world.rows()?.facilities.contains_key(&facility_id)) })??;
    if ne_text(field(&repair, "facility_id")?, &facility_id) || !known {
        return Err(Error::failed("facility repair target does not match a known facility").into());
    }
    if !ins.shared {
        return Err(Error::failed("facility repair requires shared receipt state").into());
    }
    guard_or_suppress(cell, host, ins.guard, row)?;
    let expected = kit::exact_int(field(&repair, "expected_working_units")?)?;
    let restored = kit::exact_int(field(&repair, "restored_working_units")?)?;
    let (previous, sources) = cell.with(|world| -> Result<(i64, Vec<Arc<str>>)> {
        let (working, capacity) = {
            let rows = world.rows()?;
            let facility = rows.facilities.get(&facility_id).ok_or_else(|| Error::Key(facility_id.to_string()))?;
            (facility.working_units, facility.capacity)
        };
        let modified = || {
            program_state(world, ins.guard.program, |state| {
                Ok(state.and_then(|state| state.active_facility_modifiers.get(&*facility_id)).is_some_and(|record| !record.modifiers.is_empty()))
            })
        };
        if working != expected || working >= restored || restored > capacity || active_outage(world, ins.infrastructure, &facility_id)? || modified()? {
            return Err(Error::failed("facility repair preconditions no longer match the live facility state"));
        }
        let mut sources: Vec<Arc<str>> = match field(&repair, "source_ids")? {
            V::List(items) => items.iter().map(|item| kit::exact_str(item).cloned()).collect::<Result<_>>()?,
            _ => return Err(Error::unsupported("the sources are not a list")),
        };
        sources.extend(row.source_id.clone());
        sources.sort_unstable();
        sources.dedup();
        sources.retain(|source| !source.is_empty());
        // The infrastructure's baseline is written where the world holds the namespace as a dict.
        if let Held::Other(reason) = ins.infrastructure {
            return Err(Error::unsupported(*reason));
        }
        Ok((working, sources))
    })??;
    let component = field_str(&repair, "component_id")?.clone();
    // In place, like the Python rule: a failed check of the world now goes on as its own error.
    cell.with(|world| -> Result<()> {
        world.rows_mut()?.facilities.get_mut(&facility_id).ok_or_else(|| Error::Key(facility_id.to_string()))?.working_units = restored;
        if matches!(state_value(world, INFRASTRUCTURE, ins.infrastructure)?, Some(V::Map(_))) {
            write_value(world, INFRASTRUCTURE, &["base_working_units"], |units| {
                units.insert(facility_id.clone(), V::Int(restored));
                Ok(())
            })?;
        }
        Ok(())
    })??;
    validate(host)?;
    let made = cell.with(|world| -> Result<Option<Made>> {
        if !ins.shared {
            return Ok(None);
        }
        let units = world.rows()?.facilities.get(&facility_id).ok_or_else(|| Error::Key(facility_id.to_string()))?.working_units;
        care_receipts::facility_recovery_record(world, &facility_id, &component, "repaired", previous, units, &row.event_id, sources).map(Some)
    })??;
    Ok(made)
}

// ---- set_staff_role ----

/// A role change that passed its checks: the staff member and the skills, sorted.
#[derive(Debug, Clone, PartialEq)]
pub struct Role {
    staff_id: Arc<str>,
    skills: Vec<Arc<str>>,
}

/// The checks of `set_staff_role`, on the world, and the skills' contract.
pub fn plan_role<C: WorldCell, H: Host>(cell: &C, host: &mut H, ins: &StructureIn<'_>, row: &EventRow) -> std::result::Result<Role, H::Error> {
    let payload = map_of(&row.payload)?;
    let approved = cell.with(|world| -> Result<bool> {
        Ok(program_lists_effect(world, ins.guard.program, payload)? == Some(true)
            && is_text(payload.get("op"), "set_staff_role")
            && match payload.get("staff_id") {
                Some(V::Str(staff_id)) => world.rows()?.staff.contains_key(staff_id),
                _ => false,
            })
    })??;
    if !approved {
        return Err(Error::failed("staff-role event does not reference an approved staff target").into());
    }
    let staff_id = required_str(payload, "staff_id")?.clone();
    // `{"skills": event.payload["skills"]}`: a dict the rule builds around the payload's value.
    let mut raw = Doc::with_capacity(1);
    raw.insert(text("skills"), payload_item(row, "skills")?.clone());
    let role = parameters(host, "set_staff_role", &V::Map(raw), Catch::Value, "staff-role event skills are invalid: ")?;
    let mut skills: Vec<Arc<str>> = match field(&role, "skills")? {
        V::List(items) => items.iter().map(|item| kit::exact_str(item).cloned()).collect::<Result<_>>()?,
        _ => return Err(Error::unsupported("the skills are not a list").into()),
    };
    skills.sort_unstable();
    Ok(Role { staff_id, skills })
}

/// The skills of `set_staff_role` on the transaction's world (`operations`: whether it holds
/// `staff_operations_v1`), and the world's check.
pub fn write_role<C: WorldCell, H: Host>(cell: &C, host: &mut H, operations: &Held, plan: &Role) -> std::result::Result<(), H::Error> {
    cell.with(|world| -> Result<()> {
        // `candidate.model_state.get("staff_operations_v1", {}).get("staff", {}).get(staff_id)`.
        let busy = match kit::held_row::<StaffOperationsRow>(world, STAFF_OPERATIONS, operations)? {
            Some(row) => row.staff.get(&plan.staff_id).is_some_and(|record| record.phase != Phase::Idle),
            None => false,
        };
        let mut rows = world.rows_mut()?;
        let staff = rows.staff.get_mut(&plan.staff_id).ok_or_else(|| Error::Key(plan.staff_id.to_string()))?;
        if staff.active_task_id.is_some() || busy {
            return Err(Error::failed("staff role cannot change while work is active"));
        }
        staff.skills = Skills(plan.skills.clone());
        Ok(())
    })??;
    validate(host)
}

// ---- add_staff and add_toilet ----

/// An addition that passed its checks.
#[derive(Debug, Clone, PartialEq)]
pub struct Add {
    staff: bool,
    zone_id: Arc<str>,
    /// The count, or None for an int past 64 bits that is positive: past any bound.
    count: Option<i64>,
    /// The modifier ID without its prefix.
    suffix: Arc<str>,
    event_id: Arc<str>,
    source_id: Option<Arc<str>>,
}

/// The checks of `add_staff` and `add_toilet`, on the world.
pub fn plan_add(world: &World, program: &Held, row: &EventRow) -> Result<Add> {
    let payload = map_of(&row.payload)?;
    if !has_exactly(payload, &["program_id", "effect_id", "modifier_id", "op", "zone_id", "count"]) {
        return Err(Error::failed("structural primitive payload fields do not match"));
    }
    let listed = program_lists_effect(world, program, payload)? == Some(true);
    let valid = (|| -> Result<Option<Option<i64>>> {
        if !listed {
            return Ok(None);
        }
        match payload.get("op") {
            Some(V::Str(op)) if matches!(&**op, "add_staff" | "add_toilet") => {}
            Some(value) if !hashable(value) => return Err(Error::unsupported("an operator Python can't hash")),
            _ => return Ok(None),
        }
        let count = match payload.get("count") {
            Some(V::Int(count)) if *count > 0 => Some(*count),
            Some(V::Big(count)) if !count.starts_with('-') => None,
            _ => return Ok(None),
        };
        let rows = world.rows()?;
        Ok(key_in(|zone| zone_known(&rows, zone), payload.get("zone_id"))?.then_some(count))
    })()?;
    let Some(count) = valid else {
        return Err(Error::failed("structural primitive target or operation is invalid"));
    };
    let op = required_str(payload, "op")?;
    let zone_id = required_str(payload, "zone_id")?.clone();
    // `.removeprefix` of a value that is not a str raises.
    let modifier = required_str(payload, "modifier_id")?;
    let suffix = text(modifier.strip_prefix("modifier-").unwrap_or(modifier));
    Ok(Add { staff: &**op == "add_staff", zone_id, count, suffix, event_id: row.event_id.clone(), source_id: row.source_id.clone() })
}

/// What an addition reads of the transaction's world besides its rows.
pub struct AddIn<'a> {
    pub operational: &'a Held,
    pub dynamics: &'a Held,
    pub operations: &'a Held,
    pub movement: &'a Held,
    pub overlay: &'a Held,
    /// The overlay's schema version, for `installed_facility_placement_enabled`.
    pub overlay_schema: &'a Kept<Option<i64>>,
    /// The agents' dynamics policy, for `announce_installed_facility`.
    pub policy: &'a Kept<&'static str>,
    /// The floor plan the staff positions read, or None where `Geo::of` raised. A toilet's place
    /// reads the plan through the host, again for each toilet (`Host::floor_plan`).
    pub geo: Option<&'a Geo>,
    /// The staff's operations as the staff positions read them, or None where their read raised.
    pub staff_ops: Option<&'a StaffOps>,
}

/// The records of a new staff member: `add_staff` of `domain/staff_dynamics.py` and
/// `add_staff_operations_agent`, where the world holds each namespace.
fn add_one_staff(world: &mut World, ins: &AddIn<'_>, staff_id: &str, zone_id: &Arc<str>) -> Result<()> {
    let key: Arc<str> = text(staff_id);
    world.rows_mut()?.staff.insert(
        key.clone(),
        StaffRow { id: key.clone(), zone_id: zone_id.clone(), skills: Skills(vec![text("support")]), work_queue: Vec::new(), busy_until: 0, active_task_id: None },
    );
    if matches!(state_value(world, OPERATIONAL, ins.operational)?, Some(state) if !matches!(state, V::Null)) {
        let mut status = Doc::with_capacity(4);
        status.insert(text("available"), V::Bool(true));
        status.insert(text("unavailability_event_id"), V::Null);
        status.insert(text("until_minute"), V::Null);
        status.insert(text("restore_event_id"), V::Null);
        write_value(world, OPERATIONAL, &["staff_status"], |records| {
            records.insert(key.clone(), V::Map(status));
            Ok(())
        })?;
    }
    match ins.dynamics {
        Held::Absent => {}
        Held::Other(reason) => return Err(Error::unsupported(*reason)),
        Held::Held => staff_dynamics::add_staff(world, staff_id)?,
    }
    if kit::held_row::<StaffOperationsRow>(world, STAFF_OPERATIONS, ins.operations)?.is_some() {
        world.row_mut::<StaffOperationsRow>(STAFF_OPERATIONS)?.staff.insert(key, idle_record());
    }
    Ok(())
}

/// A new one-place toilet: its row, and whether it is to be placed on the floor plan
/// (`installed_facility_placement_enabled`).
fn add_one_toilet(world: &World, ins: &AddIn<'_>, plan: &Add, facility_id: &str) -> Result<bool> {
    if world.rows()?.facilities.contains_key(facility_id) {
        return Err(Error::failed("generated toilet ID already exists"));
    }
    let key: Arc<str> = text(facility_id);
    world.rows_mut()?.facilities.insert(
        key.clone(),
        FacilityRow {
            id: key.clone(),
            kind: text("toilet"),
            zone_id: plan.zone_id.clone(),
            capacity: 1,
            working_units: 1,
            active_agent_ids: Vec::new(),
            queue_agent_ids: Vec::new(),
            resource_active_agent_ids: Vec::new(),
            resource_queue_agent_ids: Vec::new(),
        },
    );
    overlay::placement_enabled(ins.overlay_schema, ins.overlay)
}

/// `place_installed_facility` of a new toilet on the plan read now, called by name: its ValueError
/// (its refusal is its `KernelRefusal`) is the event's failure with its text.
fn place_toilet<C: WorldCell, H: Host>(cell: &C, host: &mut H, ins: &AddIn<'_>, plan: &Add, facility_id: &str) -> std::result::Result<(), H::Error> {
    let geo = host.floor_plan().map_err(|error| by_name(PLACE, error, Some(Catch::Value)))?;
    let sources: Vec<&str> = plan.source_id.as_deref().filter(|source| !source.is_empty()).into_iter().collect();
    match cell.with(|world| overlay::place_facility(world, ins.overlay, Some(&*geo), facility_id, &plan.event_id, &sources))? {
        Ok(_) => Ok(()),
        Err(error) => Err(by_name(PLACE, H::Error::from(error), Some(Catch::Value))),
    }
}

/// The additions on the transaction's world, and the world's check: the label of the branch.
pub fn write_add<C: WorldCell, H: Host>(cell: &C, host: &mut H, ins: &AddIn<'_>, plan: &Add) -> std::result::Result<&'static str, H::Error> {
    cell.with(|world| -> Result<()> {
        if plan.staff {
            let room = 100 - world.rows()?.staff.len() as i64;
            let Some(count) = plan.count.filter(|count| *count <= room) else {
                return Err(Error::failed("adding staff would exceed the 100-staff runtime bound"));
            };
            for index in 1..=count {
                let staff_id = format!("effect-staff-{}-{index}", plan.suffix);
                if world.rows()?.staff.contains_key(&staff_id) {
                    return Err(Error::failed("generated staff ID already exists"));
                }
                add_one_staff(world, ins, &staff_id, &plan.zone_id)?;
            }
            match ins.movement {
                Held::Absent => {}
                Held::Other(reason) => return Err(Error::refused(SNAPSHOTS, Error::unsupported(*reason))),
                Held::Held => {
                    let (Some(geo), Some(staff)) = (ins.geo, ins.staff_ops) else {
                        return Err(Error::unsupported(SNAPSHOT_UNREAD));
                    };
                    let boundary = world.minute_index()?;
                    movement::staff_position_snapshots(world, geo, staff, boundary).map_err(|error| Error::refused(SNAPSHOTS, error))?;
                }
            }
        }
        Ok(())
    })??;
    if !plan.staff {
        let count = cell.with(|world| -> Result<i64> {
            let room = 200 - world.rows()?.facilities.len() as i64;
            plan.count.filter(|count| *count <= room).ok_or_else(|| Error::failed("adding toilets would exceed the 200-facility bound"))
        })??;
        for index in 1..=count {
            let facility_id = format!("effect-toilet-{}-{index}", plan.suffix);
            if cell.with(|world| add_one_toilet(world, ins, plan, &facility_id))?? {
                place_toilet(cell, host, ins, plan, &facility_id)?;
            }
            cell.with(|world| agent_dynamics::announce_installed_facility(world, Some(&facility_id), ins.policy).map_err(|error| Error::refused(ANNOUNCE, error)))??;
        }
    }
    validate(host)?;
    let label = if plan.staff { "structure.add_staff" } else { "structure.add_toilet" };
    COUNTS.ran(label);
    Ok(label)
}

// ---- validate_effect_program_state -------------------------------------------------------------

/// `DEFAULT_PRIMITIVE_REGISTRY`, which a test compares with this list.
pub const OPERATORS: [&str; 27] = [
    "move_facility",
    "transfer_equipment",
    "set_partition",
    "set_route_access",
    "temporary_room_geometry",
    "set_facility_availability",
    "facility_outage",
    "facility_repair",
    "staff_handoff",
    "staff_task_order",
    "deliver_resource",
    "modify_supply_schedule",
    "set_staff_availability",
    "set_staff_role",
    "add_staff",
    "add_toilet",
    "add_evacuees",
    "depart_evacuees",
    "population_presence",
    "add_temporary_space",
    "water_outage",
    "set_ambient_noise",
    "modify_demand_rate",
    "set_ventilation",
    "set_lighting",
    "set_privacy",
    "set_temperature",
];

/// `EFFECT_EVENT_TYPES[phase].get(op)`.
pub fn expected_event_type(phase: &str, op: &str) -> Option<&'static str> {
    Some(match (phase, op) {
        ("start", "set_facility_availability" | "facility_outage") => "effect_program_apply",
        ("start", "facility_repair" | "staff_handoff" | "staff_task_order" | "set_staff_role" | "add_staff" | "add_toilet") => "effect_program_structure",
        ("start", "deliver_resource") => "resource_delivery",
        ("start", "modify_supply_schedule") => "delivery_delayed",
        ("start", "set_staff_availability") => "staff_unavailable",
        ("start", "water_outage") => "water_outage",
        ("start", "set_ambient_noise" | "modify_demand_rate" | "set_ventilation" | "set_lighting" | "set_privacy" | "set_temperature") => {
            "effect_program_environment"
        }
        ("start", "add_evacuees" | "depart_evacuees" | "population_presence") => "effect_program_population",
        ("start", "add_temporary_space") => "effect_program_temporary_space",
        ("start", "move_facility" | "transfer_equipment" | "set_partition" | "set_route_access" | "temporary_room_geometry") => "effect_program_geometry",
        ("expiry", "set_facility_availability" | "facility_outage") => "effect_program_expire",
        ("expiry", "water_outage") => "water_restored",
        ("expiry", "set_ambient_noise" | "modify_demand_rate" | "set_ventilation" | "set_lighting" | "set_privacy" | "set_temperature") => {
            "effect_program_environment_expire"
        }
        ("expiry", "add_temporary_space") => "effect_program_temporary_space_expire",
        ("expiry", "set_partition" | "set_route_access" | "temporary_room_geometry") => "effect_program_geometry_expire",
        ("derived", "modify_supply_schedule") => "resource_delivery",
        ("derived", "set_staff_availability") => "staff_restored",
        ("derived", "add_evacuees") => "population_approach_complete",
        _ => return None,
    })
}

const MARKER_FIELDS: [&str; 9] = ["program_id", "effect_id", "op", "source_text_hash", "compiler_provenance", "assumption_ids", "policy_dependencies", "units", "phase"];

/// `CompilerProvenance.model_validate`, for a value the contract surely takes.
fn compiler_provenance_passes(value: &V) -> bool {
    let V::Map(map) = value else {
        return false;
    };
    if map.keys().any(|key| !matches!(&**key, "compiler_kind" | "compiler_version" | "model_id" | "prompt_hash")) {
        return false;
    }
    let optional_id = match map.get("model_id") {
        None | Some(V::Null) => true,
        Some(V::Str(id)) => is_stable_id(id),
        Some(_) => false,
    };
    let optional_hash = match map.get("prompt_hash") {
        None | Some(V::Null) => true,
        Some(V::Str(hash)) => hash.chars().count() <= 128,
        Some(_) => false,
    };
    optional_id
        && optional_hash
        && str_of(map, "compiler_kind").is_some_and(|kind| matches!(&**kind, "offline_rule" | "llm"))
        && str_of(map, "compiler_version").is_some_and(|version| is_stable_id(version))
}

/// `PlanAssumption.model_validate`, and its ID for the check that they differ.
fn assumption_id(value: &V) -> Option<&Arc<str>> {
    let V::Map(map) = value else {
        return None;
    };
    if map.keys().any(|key| !matches!(&**key, "assumption_id" | "description" | "source_id" | "units" | "value")) {
        return None;
    }
    let units = match map.get("units") {
        None | Some(V::Null) => true,
        Some(V::Str(units)) => is_stable_id(units),
        Some(_) => false,
    };
    let scalar = match map.get("value") {
        None | Some(V::Null | V::Str(_) | V::Int(_) | V::Bool(_)) => true,
        Some(V::Float(number)) => number.is_finite(),
        Some(_) => false,
    };
    let id = str_of(map, "assumption_id")?;
    (units
        && scalar
        && is_stable_id(id)
        && str_of(map, "source_id").is_some_and(|source| is_stable_id(source))
        && str_of(map, "description").is_some_and(|text| (1..=500).contains(&text.chars().count())))
    .then_some(id)
}

fn is_lower_hex_64(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

/// Whether a pending event's provenance has the marker's key (a marker that is no mapping passes,
/// and the Python rule knows which it is). `PENDING_UNREAD` for a pending event without its row.
fn pending_has_marker(world: &World) -> Result<bool> {
    Ok(pending_rows(world)?.iter().any(|event| matches!(&event.provenance, V::Map(provenance) if provenance.contains_key(MARKER))))
}

/// `validate_effect_program_state`: passes, or a refusal and the Python rule says what is wrong.
/// The history's records are looked up through the host (`Host::find_record`).
pub fn check_state<C: WorldCell, H: Host>(cell: &C, host: &mut H, program: &Held, history: bool) -> std::result::Result<(), H::Error> {
    let absent = matches!(program, Held::Absent);
    if absent || !history {
        cell.with(|world| -> Result<()> {
            if absent {
                if history && !world.log()?.with_provenance_object(MARKER).is_empty() {
                    return Err(Error::unsupported("marked events without the state"));
                }
                if pending_has_marker(world)? {
                    return Err(Error::unsupported("marked events without the state"));
                }
                return Ok(());
            }
            match kit::held_row::<EffectProgramRow>(world, NAME, program)? {
                Some(state) if state.schema_version == 1 => Ok(()),
                _ => Err(Error::unsupported("another schema version")),
            }
        })??;
        return Ok(());
    }
    // The marked events, pending and then logged, and the IDs the checks look up.
    let (pending, logged_ids, scheduled_ids) = cell.with(|world| -> Result<_> {
        let pending: Vec<Arc<EventRow>> = pending_rows(world)?
            .into_iter()
            .filter(|event| matches!(&event.provenance, V::Map(provenance) if !matches!(provenance.get(MARKER), None | Some(V::Null))))
            .collect();
        let logged_ids: Vec<Arc<str>> = {
            let log = world.log()?;
            let ids: Vec<&str> = log.event_ids().collect();
            log.with_provenance_object(MARKER).iter().map(|index| text(ids[*index])).collect()
        };
        let state = kit::held_row::<EffectProgramRow>(world, NAME, program)?.ok_or_else(|| Error::unsupported("no state"))?;
        let scheduled_ids: Vec<Arc<str>> = state.programs.values().flat_map(|program| program.scheduled_event_ids.iter().cloned()).collect();
        Ok((pending, logged_ids, scheduled_ids))
    })??;
    let mut found: IndexMap<Arc<str>, EventRow> = IndexMap::new();
    let look_up = |host: &mut H, id: &Arc<str>, found: &mut IndexMap<Arc<str>, EventRow>| -> std::result::Result<(), H::Error> {
        if !found.contains_key(id) {
            if let Some(logged) = host.find_record(id)? {
                found.insert(id.clone(), logged.event);
            }
        }
        Ok(())
    };
    for id in scheduled_ids.iter().chain(&logged_ids) {
        look_up(host, id, &mut found)?;
    }
    let mut events: Vec<EventRow> = pending.iter().map(|event| (**event).clone()).collect();
    for id in &logged_ids {
        events.push(found.get(id).cloned().ok_or_else(|| Error::unsupported("a logged event is not found"))?);
    }
    // A derived event's parent is looked up too.
    let causes: Vec<Arc<str>> = events.iter().filter_map(|event| event.causation_id.clone()).collect();
    for id in &causes {
        look_up(host, id, &mut found)?;
    }
    cell.with(|world| check_history(world, program, &events, &found))??;
    Ok(())
}

/// The checks of the history, with `found` the logged events by ID (a realized event wins).
fn check_history(world: &World, program: &Held, events: &[EventRow], found: &IndexMap<Arc<str>, EventRow>) -> Result<()> {
    let queue = world.queue()?;
    // `EventsById(event_log, pending)`: a realized event wins.
    let by_id = |id: &str| -> Result<Option<Arc<EventRow>>> {
        if let Some(event) = found.get(id) {
            return Ok(Some(Arc::new(event.clone())));
        }
        match queue.serial_of(id) {
            Some(serial) => queue.slot(serial).row.clone().map(Some).ok_or_else(|| Error::unsupported(PENDING_UNREAD)),
            None => Ok(None),
        }
    };
    let state = kit::held_row::<EffectProgramRow>(world, NAME, program)?.ok_or_else(|| Error::unsupported("no state"))?;
    if state.schema_version != 1 {
        return Err(Error::unsupported("another schema version"));
    }
    let mut scheduled: IndexMap<&str, &str> = IndexMap::new();
    let mut known: HashSet<(&str, &str)> = HashSet::new();
    for (program_id, program) in &state.programs {
        if !is_stable_id(program_id)
            || !is_lower_hex_64(&program.program_hash)
            || !is_lower_hex_64(&program.source_text_hash)
            || !(1..=1000).contains(&program.narrative.chars().count())
            || !matches!(&*program.support_mode, "native" | "composed" | "approximate" | "clarification_required" | "not_simulatable")
            || !compiler_provenance_passes(&program.compiler_provenance)
        {
            return Err(Error::unsupported("a program record the Python rule decides"));
        }
        let unique: HashSet<&str> = program.effect_ids.iter().map(|id| &**id).collect();
        if unique.len() != program.effect_ids.len()
            || program.operators.len() != unique.len()
            || program.operators.keys().any(|effect| !unique.contains(&**effect))
            || program.operators.values().any(|op| !OPERATORS.contains(&&**op))
        {
            return Err(Error::unsupported("a program's operators the Python rule decides"));
        }
        let V::List(assumptions) = &program.assumptions else {
            return Err(Error::unsupported("a program's assumptions the Python rule decides"));
        };
        let mut assumption_ids: HashSet<&str> = HashSet::new();
        for assumption in assumptions {
            match assumption_id(assumption) {
                Some(id) if assumption_ids.insert(id) => {}
                _ => return Err(Error::unsupported("a program's assumptions the Python rule decides")),
            }
        }
        for effect_id in &program.effect_ids {
            if !is_stable_id(effect_id) {
                return Err(Error::unsupported("an effect ID the Python rule decides"));
            }
            known.insert((program_id, effect_id));
        }
        for event_id in &program.scheduled_event_ids {
            if !is_stable_id(event_id) || scheduled.insert(event_id, program_id).is_some() {
                return Err(Error::unsupported("a scheduled event ID the Python rule decides"));
            }
        }
    }
    for (event_id, program_id) in &scheduled {
        let Some(event) = by_id(event_id)? else {
            return Err(Error::unsupported("an unknown scheduled event"));
        };
        let marker = match map_of(&event.provenance)?.get(MARKER) {
            None | Some(V::Null) => return Err(Error::unsupported("a scheduled event has no marker")),
            Some(V::Map(marker)) => marker,
            Some(_) => return Err(Error::unsupported("a marker the Python rule decides")),
        };
        let program = marker.get("program_id").ok_or_else(|| Error::unsupported("a marker the Python rule decides"))?;
        if !is_text(Some(program), program_id) {
            return Err(Error::unsupported("a marker of another program"));
        }
    }
    for event in events {
        let Some(V::Map(marker)) = map_of(&event.provenance)?.get(MARKER) else {
            return Err(Error::unsupported("a marker that is no mapping"));
        };
        let has_guard = marker.contains_key("guard");
        if marker.len() != MARKER_FIELDS.len() + usize::from(has_guard) || !MARKER_FIELDS.iter().all(|name| marker.contains_key(*name)) {
            return Err(Error::unsupported("a marker's fields the Python rule decides"));
        }
        let program_id = required_str(marker, "program_id")?;
        let effect_id = required_str(marker, "effect_id")?;
        if !known.contains(&(&**program_id, &**effect_id)) {
            return Err(Error::unsupported("an unknown effect"));
        }
        let program = &state.programs[&**program_id];
        let op = required_str(marker, "op")?;
        if program.operators.get(&**effect_id) != Some(op) || str_of(marker, "source_text_hash") != Some(&program.source_text_hash) {
            return Err(Error::unsupported("a marker of another program"));
        }
        let phase = required_str(marker, "phase")?;
        if !matches!(&**phase, "start" | "expiry" | "derived") {
            return Err(Error::unsupported("a marker's phase the Python rule decides"));
        }
        let lists_of_str = [marker.get("assumption_ids"), marker.get("policy_dependencies")]
            .iter()
            .all(|list| matches!(list, Some(V::List(items)) if items.iter().all(|item| matches!(item, V::Str(_)))));
        if !compiler_provenance_passes(marker.get("compiler_provenance").unwrap_or(&V::Null))
            || !lists_of_str
            || !str_of(marker, "units").is_some_and(|units| is_stable_id(units))
        {
            return Err(Error::unsupported("a marker's values the Python rule decides"));
        }
        if has_guard {
            if &**phase != "start" {
                return Err(Error::MarkedValue("only a start event may store a guard".to_owned()));
            }
            let mut nodes = 0;
            if parse_guard(&marker["guard"], 1, &mut nodes).is_none() {
                return Err(Error::unsupported("a marker's guard the Python rule decides"));
            }
        }
        if !scheduled.contains_key(&*event.event_id) && &**phase != "derived" {
            return Err(Error::unsupported("an unscheduled event without the derived mark"));
        }
        if &**phase == "derived" {
            match &event.causation_id {
                Some(cause) if by_id(cause)?.is_some() => {}
                _ => return Err(Error::unsupported("a derived event lacks a known parent")),
            }
        }
        if expected_event_type(phase, op) != Some(&*event.event_type) {
            return Err(Error::unsupported("an event type that isn't its operator's"));
        }
    }
    let rows = world.rows()?;
    for (facility_id, record) in &state.active_facility_modifiers {
        let Some(facility) = rows.facilities.get(facility_id) else {
            return Err(Error::unsupported("a modifier of an unknown facility"));
        };
        let baseline = record.baseline_working_units;
        if baseline < 0 || baseline > facility.capacity || record.modifiers.is_empty() {
            return Err(Error::unsupported("a stack's baseline the Python rule decides"));
        }
        let mut ids: HashSet<&str> = HashSet::new();
        let mut lowest = baseline;
        for item in &record.modifiers {
            if *item.modifier_id != *modifier_id(&item.program_id, &item.effect_id)
                || !ids.insert(&item.modifier_id)
                || !known.contains(&(&*item.program_id, &*item.effect_id))
                || item.working_units < 0
                || item.start_minute < 0
                || item.sequence < 0
                || item.working_units > baseline
            {
                return Err(Error::unsupported("a stack's entry the Python rule decides"));
            }
            lowest = lowest.min(item.working_units);
        }
        if facility.working_units != lowest {
            return Err(Error::unsupported("working units that aren't the stack's"));
        }
    }
    Ok(())
}
