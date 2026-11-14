//! The check at the end of a tick (`WorldState.validate_after_tick`) in one call, with its memo.
//!
//! It holds no error text. It returns only if every rule of `validate(validate_event_history=False)`
//! passes, and anything else is a refusal: `validate_after_tick` then runs that Python check, with no
//! memo, which passes or raises its own text (policy E6 of docs/rust-port/step5-plan.md). So what a
//! yes means is the one thing that must hold (`tests/integration/test_after_tick_kernel.py`). Since
//! every rule passes, their order can't show.
//!
//! The rules read the rows (Issue #152, lane P8 d4): the entity rows (`rules/tick_validation.rs`),
//! the rows of the namespaces and fields, and the queue's event rows. The validators of the
//! namespaces, the effect program's check and the reading of the saved body config are asked
//! through `Checks`, with the world given back (rule R6 of design-p8.md): the check borrows the
//! world twice, before and after them. The native path answers them with what Python registers
//! (the PyO3 crate's `PyChecks`), a world without Python with the core's own
//! (`rules::tick_checks::CoreChecks`, #250); both run the same rules of the core.
//!
//! The memo (`Memo`) is the facts of the last check this kernel passed, and lets it skip what is
//! provably unchanged since: see "What the memo skips" below. A check that refuses leaves the memo
//! as it was, but for the stamp of the configs and the people the body config names, which hold
//! whatever the state is.
//!
//! The configs are read by the contracts in Python (`core/tick_validation.py`): the plan
//! (`TickPlan`) is the section `tick_plan` of the configs, kept by the stamp of `model_configs`
//! (`Configs.tick_plan`, lane 7-1), which the PyO3 crate fills at the first check after a write to
//! the configs. The people the saved body config names (`body_config_ids`) are asked of Python when
//! that config's JSON changed, and kept here with the JSON while the configs' stamp stands.
//!
//! What the memo skips (each rests on one argument; a full validation at each save reads them all):
//! - The configs are parsed only when the stamp of `model_configs` changes, which every write to it
//!   gives (`Slot::touch`).
//! - A settled support request (resolved or unresolved) depends only on its own fields, on the set
//!   of Agent IDs, and on the clock only through "not after the current minute". It is checked again
//!   when its row is written or filled (`Table::marked`, from Rust or from Python) or when the Agent
//!   IDs change. The scans of unsettled requests during a tick use the same marks
//!   (`unsettled_requests`).
//! - The population presence, lifecycle and dynamics states and the geometry overlay are skipped when
//!   everything their rules read is unchanged: their own state by its slot's stamp, the Agent and zone
//!   IDs, and the facilities' zones for the overlay. A write of the same value counts as a change.
//! - The saved body config is checked again when its JSON or the configs change.

use std::sync::Arc;

use rustc_hash::{FxHashMap, FxHashSet};

use crate::config::movement::RouteCapacities;
use crate::config::TickPlan;
use crate::entities::{RequestRow, RequestStatus, Table};
use crate::error::{Error, Result};
use crate::row::ValueRow;
use crate::rules::kit::{Held, Kept};
use crate::rules::overlay::Places;
use crate::rules::tick_validation::{self as entity, EntityInputs, Requests};
use crate::stamp::Stamp;
use crate::state::body_state::BodyRow;
use crate::value::V;
use crate::world::{World, WorldCell};

pub const PRESENCE: &str = "population_presence_v1";
pub const LIFECYCLE: &str = "population_lifecycle_v1";
pub const DYNAMICS: &str = "population_dynamics_v1";
pub const OVERLAY: &str = "geometry_overlay_v1";
pub const BODY: &str = "body_state_v1";
pub const SHARED: &str = "shared_operations_v1";
pub const QUEUES: &str = "facility_queues_v1";
pub const QUEUES_LEGACY: &str = "facility_queues_legacy_mode_v1";
pub const SPACES: &str = "temporary_spaces_v1";
pub const OPERATIONAL: &str = "operational_events_v1";
pub const INFRASTRUCTURE: &str = "infrastructure_v1";
pub const ENVIRONMENT: &str = "zone_environment_v1";
pub const SERVICE: &str = "resource_service_v1";
pub const LEDGER: &str = "resource_ledger_v1";
pub const INFORMATION: &str = "information_spread_v1";
pub const PSYCHOLOGY: &str = "psychological_distress_v1";
const ENVIRONMENT_EVENTS: &str = "effect_program_environment";

/// The namespaces the check reads or asks about besides the declared and required ones, for the
/// PyO3 crate to say whether the Python world holds each (`Inputs::held`).
pub const READ: [&str; 16] = [
    PRESENCE,
    LIFECYCLE,
    DYNAMICS,
    OVERLAY,
    BODY,
    SHARED,
    QUEUES,
    QUEUES_LEGACY,
    SPACES,
    OPERATIONAL,
    INFRASTRUCTURE,
    ENVIRONMENT,
    SERVICE,
    LEDGER,
    INFORMATION,
    PSYCHOLOGY,
];

/// The namespaces the memo compares, in the order of `Memo::seen`.
const COMPARED: [&str; 4] = [PRESENCE, LIFECYCLE, DYNAMICS, OVERLAY];

#[track_caller]
fn refuse<T>(reason: &'static str) -> Result<T> {
    Err(Error::unsupported(reason))
}

/// What the Python check would call besides the rules here: asked with the world given back. An
/// error refuses the check. Each yes must hold only where the Python validator passes.
pub trait Checks {
    /// The validator registered for a namespace (`state_mirror._RUST_VALIDATORS`): whether it takes
    /// the world, its rows being what the world holds; None where none is registered.
    fn namespace(&mut self, name: &str) -> Result<Option<bool>>;
    /// `body_state._ROWS_VALID`: the rows of the body state are the world's and valid.
    fn body_rows(&mut self) -> Result<bool>;
    /// `common_operations._TICK_VALID`: the shared operations of the tick.
    fn shared_operations(&mut self) -> Result<bool>;
    /// The effect program's check, `effects.validate_state(world, validate_event_history=False)`.
    fn effects(&mut self) -> Result<()>;
    /// `tick_validation.body_config_ids(world)`: the people the saved body config names.
    fn body_config_ids(&mut self) -> Result<Vec<Arc<str>>>;
}

/// Whether the Python world holds each namespace the check reads, as the PyO3 crate read it before
/// the check: the names of `READ` and the plan's declared and required ones.
#[derive(Default)]
pub struct HeldNamespaces(FxHashMap<Arc<str>, Held>);

impl HeldNamespaces {
    pub fn insert(&mut self, name: Arc<str>, held: Held) {
        self.0.insert(name, held);
    }

    #[track_caller]
    fn of(&self, name: &str) -> Result<&Held> {
        self.0.get(name).ok_or_else(|| Error::unsupported("a namespace the check reads was not read"))
    }

    /// `name in model_state`.
    fn contains(&self, name: &str) -> Result<bool> {
        Ok(!matches!(self.of(name)?, Held::Absent))
    }
}

/// What the check reads besides the world and the memo.
pub struct Inputs<'a> {
    pub plan: &'a TickPlan,
    /// The stamp of `model_configs`, which the plan was read for.
    pub configs: Stamp,
    pub held: &'a HeldNamespaces,
    pub route_capacities: &'a Kept<Option<RouteCapacities>>,
    /// The overlay's schema, for the capacity of zones with spaces or rooms.
    pub overlay_schema: &'a Kept<Option<i64>>,
}

/// How a namespace stood at a passed check.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Seen {
    Absent,
    Stamp(Stamp),
}

/// The facts of the last check this kernel passed for one world. It holds no simulation state:
/// dropping it costs one slower check and changes no result.
#[derive(Clone, Debug)]
pub struct Memo {
    /// The minute of that check; rules bound past times by the clock, so a memo only holds going
    /// forward, and `validate_after_tick` starts a new one for an earlier minute.
    pub minute: i64,
    /// The stamp of `model_configs` the last plan was read for.
    pub configs: Option<Stamp>,
    pub agent_ids: Option<FxHashSet<Arc<str>>>,
    pub zone_ids: Option<FxHashSet<Arc<str>>>,
    /// `{key: item.zone_id for key, item in facilities.items()}`.
    pub facility_zones: Option<FxHashMap<Arc<str>, Arc<str>>>,
    pub seen: [Option<Seen>; 4],
    pub presence_is_self_contained: bool,
    /// The keys of the support requests, in order.
    pub requests: Vec<Arc<str>>,
    /// The keys of the ones open, in the order of the requests.
    pub open: Vec<Arc<str>>,
    /// The saved body config's JSON read under these configs, and the people it names.
    pub body_config: Option<(Vec<u8>, Vec<Arc<str>>)>,
    /// What the last passed check found unchanged: configs, presence, lifecycle, dynamics, overlay.
    pub unchanged: [bool; 5],
}

impl Default for Memo {
    fn default() -> Self {
        Self {
            minute: -1,
            configs: None,
            agent_ids: None,
            zone_ids: None,
            facility_zones: None,
            seen: [None; 4],
            presence_is_self_contained: false,
            requests: Vec::new(),
            open: Vec::new(),
            body_config: None,
            unchanged: [false; 5],
        }
    }
}

fn is_settled(row: &RequestRow) -> bool {
    matches!(row.status, RequestStatus::Resolved | RequestStatus::Unresolved)
}

/// Whether the requests' keys start with these, in order.
fn starts_with(table: &Table<RequestRow>, kept: &[Arc<str>]) -> bool {
    table.len() >= kept.len() && table.slots().zip(kept).all(|((key, _), kept)| Arc::ptr_eq(key, kept) || key == kept)
}

impl Memo {
    /// The requests that may not be settled, in the order of the table, or None if that is not
    /// known and the caller has to read them all: for the scans during a tick
    /// (`unsettled_support_requests`).
    ///
    /// What was settled at the last check marks any write, so this needs only the requests open
    /// then, the ones added since, and that no other one was opened since. The result may hold
    /// requests settled since; the caller reads their status.
    pub fn unsettled_requests(&self, world: &World) -> Result<Option<Vec<Arc<str>>>> {
        let rows = world.rows()?;
        let table = &rows.support_requests;
        if !starts_with(table, &self.requests) {
            return Ok(None);
        }
        let added: Vec<(&Arc<str>, u32)> = table.slots().skip(self.requests.len()).collect();
        let known: FxHashSet<u32> = self.open.iter().filter_map(|key| table.slot_of(key)).chain(added.iter().map(|(_, slot)| *slot)).collect();
        for slot in table.marked() {
            if !known.contains(slot) && table.row_at(*slot).is_some_and(|row| !is_settled(row)) {
                return Ok(None);
            }
        }
        let mut result = self.open.clone();
        result.extend(added.into_iter().map(|(key, _)| key.clone()));
        Ok(Some(result))
    }
}

/// What the first part of a check found: what it asks of Python, and what the memo keeps if the
/// rest passes.
struct Found {
    validators: Vec<(Arc<str>, Asked)>,
    body_rows: bool,
    shared_operations: bool,
    /// The saved body config's JSON, if the world has a body state.
    body_config: Option<Vec<u8>>,
    agents_unchanged: bool,
    zones_unchanged: bool,
    facilities_unchanged: bool,
    seen: [Option<Seen>; 4],
    presence_is_self_contained: bool,
    unchanged: [bool; 5],
    requests: Checked,
}

/// The support requests a check reads.
struct Checked {
    /// Their slots, in the order they are checked: the ones open at the last check, the ones
    /// written since, the ones added since; or, after a restart, all.
    slots: Vec<u32>,
    /// The keys and slots of the ones open at the last check, and of the ones added since.
    open: Vec<(Arc<str>, u32)>,
    added: Vec<(Arc<str>, u32)>,
    /// The memo's requests are no longer the table's first, or the Agents changed.
    restart: bool,
    /// A request settled at the last check is open again.
    reopened: bool,
}

/// Who wants a validator's yes: what it says if the answer is no.
#[derive(Clone, Copy)]
enum Asked {
    /// A declared namespace (`validate_declared_states`).
    Declared,
    /// A rule of `validate_simulator_extensions` or `validate`.
    Rule,
}

/// The check, with the memo moved on; a refusal leaves the memo as it was, but for the configs'
/// stamp and the body config's people.
pub fn check<C: WorldCell, K: Checks>(cell: &C, memo: &mut Memo, inputs: &Inputs<'_>, checks: &mut K) -> Result<()> {
    let configs_unchanged = memo.configs == Some(inputs.configs);
    if !configs_unchanged {
        memo.configs = Some(inputs.configs);
        memo.body_config = None;
    }
    let found = cell.with(|world| first(world, memo, inputs, configs_unchanged))??;
    for (name, asked) in &found.validators {
        match (checks.namespace(name)?, asked) {
            (Some(true), _) => {}
            (None, Asked::Declared) => return refuse("a declared namespace has no kernel"),
            (Some(false), Asked::Declared) => return refuse("a declared namespace's kernel did not take it"),
            (None, Asked::Rule) => return Err(Error::unsupported(format!("{name}: no kernel is registered"))),
            (Some(false), Asked::Rule) => return Err(Error::unsupported(format!("{name}: its kernel did not take it"))),
        }
    }
    if found.body_rows && !checks.body_rows()? {
        return refuse("the rows of the body state are the Python rule's");
    }
    if found.shared_operations && !checks.shared_operations()? {
        return refuse("the shared operations are the Python rule's");
    }
    checks.effects()?;
    let named = match &found.body_config {
        None => None,
        Some(json) => match memo.body_config.as_ref().filter(|(kept, _)| kept == json) {
            Some((_, named)) => Some(named.clone()),
            None => {
                let named = checks.body_config_ids()?;
                memo.body_config = Some((json.clone(), named.clone()));
                Some(named)
            }
        },
    };
    cell.with(|world| last(world, memo, inputs, found, named.as_deref()))?
}

/// `model_state.get(name)` is not None: a value row holding None reads as absent, as the rules
/// read it.
fn present(world: &World, held: &HeldNamespaces, name: &str) -> Result<bool> {
    match held.of(name)? {
        Held::Absent => Ok(false),
        Held::Other(reason) => Err(Error::unsupported(*reason)),
        Held::Held => {
            let Some(slot) = world.slots.get(name) else {
                return refuse("the namespace is not owned");
            };
            let slot = slot.try_borrow()?;
            match slot.row.as_deref() {
                None => refuse("the namespace has no row"),
                Some(row) => Ok(row.as_any().downcast_ref::<ValueRow>().is_none_or(|row| !matches!(row.0, V::Null))),
            }
        }
    }
}

/// The value of a namespace held as a `V`: None for an absent key.
fn value_of<'w>(world: &'w World, held: &HeldNamespaces, name: &str) -> Result<Option<crate::cell::ShardRef<'w, V>>> {
    match held.of(name)? {
        Held::Absent => Ok(None),
        Held::Other(reason) => Err(Error::unsupported(*reason)),
        Held::Held => world.slots.try_value(name),
    }
}

/// How a namespace the memo compares stands now.
fn seen_now(world: &World, held: &HeldNamespaces, name: &str) -> Result<Seen> {
    match held.of(name)? {
        Held::Absent => Ok(Seen::Absent),
        Held::Other(_) => refuse("a namespace the memo compares holds another tree"),
        Held::Held => match world.stamp_of(name)? {
            Some(stamp) => Ok(Seen::Stamp(stamp)),
            None => refuse("a namespace the memo compares has no mirror"),
        },
    }
}

/// A map's part is absent or empty (not truthy), in a state that is a map.
fn empty_part(world: &World, held: &HeldNamespaces, name: &str, part: &str) -> Result<bool> {
    let Some(state) = value_of(world, held, name)? else {
        return Ok(false);
    };
    let V::Map(state) = &*state else {
        return Ok(false);
    };
    Ok(!state.get(part).is_some_and(V::is_truthy))
}

/// Whether no presence record points into other model state, which may change.
fn no_archive_paths(world: &World, held: &HeldNamespaces) -> Result<bool> {
    let Some(presence) = value_of(world, held, PRESENCE)? else {
        return Ok(false);
    };
    let V::Map(presence) = &*presence else {
        return Ok(false);
    };
    let Some(V::Map(people)) = presence.get("people") else {
        return Ok(false);
    };
    Ok(people.values().all(|person| !matches!(person, V::Map(person) if person.get("archive_path").is_some_and(V::is_truthy))))
}

/// `table.keys() == kept`, as sets.
fn same_keys<R>(table: &Table<R>, kept: Option<&FxHashSet<Arc<str>>>) -> bool {
    let Some(kept) = kept else {
        return false;
    };
    table.len() == kept.len() && table.slots().all(|(key, _)| kept.contains(key))
}

fn keys_of<R>(table: &Table<R>) -> FxHashSet<Arc<str>> {
    table.slots().map(|(key, _)| key.clone()).collect()
}

/// The IDs a body record or config may name besides the people here: the population state's
/// `initial_agent_ids` and `profile_traits`, and its departures for the history.
fn historical_ids(world: &World, held: &HeldNamespaces, with_departures: bool) -> Result<FxHashSet<Arc<str>>> {
    let mut ids = FxHashSet::default();
    let Some(population) = value_of(world, held, DYNAMICS)? else {
        return Ok(ids);
    };
    let V::Map(population) = &*population else {
        return Ok(ids);
    };
    if let Some(initial) = population.get("initial_agent_ids") {
        let V::List(initial) = initial else {
            return refuse("the initial agents are not a list");
        };
        for id in initial {
            let V::Str(id) = id else {
                return refuse("a value is not a str");
            };
            ids.insert(id.clone());
        }
    }
    if let Some(traits) = population.get("profile_traits") {
        let V::Map(traits) = traits else {
            return refuse("a value is not a plain dict");
        };
        ids.extend(traits.keys().cloned());
    }
    if with_departures {
        if let Some(departures) = population.get("departures") {
            let V::List(departures) = departures else {
                return refuse("the departures are not a list");
            };
            for item in departures {
                if let V::Map(item) = item {
                    if let Some(V::Str(id)) = item.get("agent_id") {
                        ids.insert(id.clone());
                    }
                }
            }
        }
    }
    Ok(ids)
}

/// The history rule of `validate_body_state`, for the records since the last tick
/// (`records_since_previous_tick`: from the last one before the tick), past the rows' own check and
/// the config's.
fn body_history(world: &World, held: &HeldNamespaces, body: &BodyRow, minute: i64) -> Result<()> {
    let items = &body.history.items;
    let parse = |index: usize| -> Result<serde_json::Value> { serde_json::from_slice(&items[index]).map_err(|_| Error::unsupported("a body history item is not JSON")) };
    let mut start = items.len();
    while start > 0 {
        let item = parse(start - 1)?;
        if let Some(recorded) = item.get("minute").filter(|value| value.is_number()) {
            match recorded.as_i64() {
                Some(recorded) if recorded < minute - 1 => break,
                Some(_) => {}
                None if recorded.is_f64() => {}
                None => return refuse("a body history minute is past i64"),
            }
        }
        start -= 1;
    }
    let rows = world.rows()?;
    let mut historical: Option<FxHashSet<Arc<str>>> = None;
    for index in start.saturating_sub(1)..items.len() {
        let item = parse(index)?;
        let Some(item) = item.as_object() else {
            return refuse("a body history item is the Python rule's");
        };
        let read = |name: &str| item.get(name).ok_or_else(|| Error::unsupported("a body history item lacks a field"));
        if !read("event_id")?.is_string() || !read("kind")?.is_string() {
            return refuse("a value is not a str");
        }
        read("topic_id")?;
        read("causation_id")?;
        let actors = read("actor_ids")?;
        let deltas = read("deltas")?;
        let minute_is_count = read("minute")?.as_i64().is_some_and(|recorded| recorded >= 0);
        if item.len() != 7 || !minute_is_count || !deltas.is_object() {
            return refuse("a body history item is the Python rule's");
        }
        let Some(actors) = actors.as_array() else {
            return refuse("a body history item's actors are the Python rule's");
        };
        for actor in actors {
            let Some(actor_id) = actor.as_str() else {
                return refuse("a value is not a str");
            };
            if rows.agents.contains_key(actor_id) {
                continue;
            }
            if historical.is_none() {
                historical = Some(historical_ids(world, held, true)?);
            }
            if !historical.as_ref().is_some_and(|ids| ids.contains(actor_id)) {
                return refuse("a body history actor is unknown");
            }
        }
    }
    Ok(())
}

/// `_has_exact_schema` of the zone environment.
fn environment_schema(state: Option<&V>) -> Result<()> {
    let Some(V::Map(state)) = state else {
        return refuse("the zone environment is the Python rule's");
    };
    if state.len() != 2 || !matches!(state.get("schema_version"), Some(V::Int(1))) || !matches!(state.get("modifiers"), Some(V::Map(_))) {
        return refuse("the zone environment is the Python rule's");
    }
    Ok(())
}

/// The first half of `validate_operational_events_state`, which is all of it at a tick. The events
/// are read by the entity rules' `watch`.
fn operational_schema(state: &V) -> Result<()> {
    let V::Map(state) = state else {
        return refuse("the operational events are the Python rule's");
    };
    let is_map = |name: &str| matches!(state.get(name), Some(V::Map(_)));
    if state.len() != 5
        || !matches!(state.get("schema_version"), Some(V::Int(1)))
        || !is_map("shipments")
        || !is_map("water_outages")
        || !is_map("staff_status")
        || !matches!(state.get("cancelled_staff_task_ids"), Some(V::List(_)))
    {
        return refuse("the operational events are the Python rule's");
    }
    Ok(())
}

/// `validate_infrastructure_runtime_state` of a tick with its config, past the config's own check
/// (`tick_plan`): the state's shape and the base capacity of each facility.
fn infrastructure(world: &World, state: Option<&V>) -> Result<()> {
    let Some(V::Map(state)) = state else {
        return refuse("the infrastructure state is the Python rule's");
    };
    let is_map = |name: &str| matches!(state.get(name), Some(V::Map(_)));
    let base = match state.get("base_working_units") {
        Some(V::Map(base)) => Some(base),
        _ => None,
    };
    if state.len() != 5 || !matches!(state.get("schema_version"), Some(V::Int(1))) || !is_map("outages") || !is_map("environmental_stressors") || !is_map("route_interruptions") {
        return refuse("the infrastructure state is the Python rule's");
    }
    let Some(base) = base else {
        return refuse("the infrastructure state is the Python rule's");
    };
    let rows = world.rows()?;
    for (facility_id, units) in base {
        let Some(facility) = rows.facilities.get(facility_id) else {
            return refuse("an infrastructure base capacity names an unknown facility");
        };
        let V::Int(units) = units else {
            return refuse("a value is not an int");
        };
        if !(0..=facility.capacity).contains(units) {
            return refuse("an infrastructure base capacity is out of range");
        }
    }
    Ok(())
}

/// The rules read with the world borrowed, before the validators are asked.
fn first(world: &World, memo: &Memo, inputs: &Inputs<'_>, configs_unchanged: bool) -> Result<Found> {
    let (plan, held) = (inputs.plan, inputs.held);
    // A WorldState v5 is the world of a Scenario without resource_service, written today
    // (docs/rust-port/lanes/l7b-world-v5.md); it differs only in the resource service's rules.
    let v6 = match world.schema_version()? {
        6 => true,
        5 => false,
        _ => return refuse("not a WorldState v5 or v6"),
    };
    let minute = world.minute_index()?;
    // "resource_service requires WorldState v6; v6 requires resource_service": the plan has a
    // resource config exactly when the configs hold resource_service, and the entity rules below
    // take a v6 world only with one and a v5 world only without.

    // ---- What the memo compares.
    let (agents_unchanged, zones_unchanged, facilities_unchanged) = {
        let rows = entity::tables(world)?;
        let facilities_unchanged = match &memo.facility_zones {
            Some(kept) => kept.len() == rows.facilities.len() && rows.facilities.iter().all(|(key, facility)| kept.get(key) == Some(&facility.zone_id)),
            None => false,
        };
        (same_keys(&rows.agents, memo.agent_ids.as_ref()), same_keys(&rows.zones, memo.zone_ids.as_ref()), facilities_unchanged)
    };
    let mut seen = [None; 4];
    for (index, name) in COMPARED.iter().enumerate() {
        seen[index] = Some(seen_now(world, held, name)?);
    }
    let same = |index: usize| seen[index] == memo.seen[index];
    let presence = present(world, held, PRESENCE)?;
    let presence_is_self_contained = if same(0) { memo.presence_is_self_contained } else { no_archive_paths(world, held)? };
    let presence_unchanged = agents_unchanged && zones_unchanged && same(0) && presence_is_self_contained;
    let lifecycle_unchanged = agents_unchanged
        && zones_unchanged
        && same(1)
        && (!presence || presence_unchanged)
        // A pending relocation is compared with the tasks and the event queue.
        && empty_part(world, held, LIFECYCLE, "pending_relocations")?;
    let dynamics_unchanged = agents_unchanged
        && zones_unchanged
        && same(2)
        // A pending arrival is compared with the event queue, the clock and the presence.
        && empty_part(world, held, DYNAMICS, "pending_arrivals")?;
    let overlay_unchanged = configs_unchanged
        && agents_unchanged
        && facilities_unchanged
        && same(3)
        // Rooms expire with the clock, and route access is compared with the graph.
        && empty_part(world, held, OVERLAY, "temporary_rooms")?
        && empty_part(world, held, OVERLAY, "route_modifiers")?;

    // ---- The support requests to check: open, written or new since the last check. Their order does
    // not matter to a valid world; with several broken ones it decides only which error comes first.
    let requests = {
        let rows = world.rows()?;
        let table = &rows.support_requests;
        if !agents_unchanged || !starts_with(table, &memo.requests) {
            // The table no longer starts with the requests held, or the Agents changed: read them all.
            Checked { slots: table.slots().map(|(_, slot)| slot).collect(), open: Vec::new(), added: Vec::new(), restart: true, reopened: false }
        } else {
            let open: Vec<(Arc<str>, u32)> = memo.open.iter().filter_map(|key| Some((key.clone(), table.slot_of(key)?))).collect();
            let added: Vec<(Arc<str>, u32)> = table.slots().skip(memo.requests.len()).map(|(key, slot)| (key.clone(), slot)).collect();
            let known: FxHashSet<u32> = open.iter().chain(&added).map(|(_, slot)| *slot).collect();
            // A request settled at the last check and written since may be open again.
            let reopened = table.marked().iter().any(|slot| !known.contains(slot) && table.row_at(*slot).is_some_and(|row| !is_settled(row)));
            let mut read: FxHashSet<u32> = FxHashSet::default();
            let mut slots = Vec::with_capacity(open.len() + added.len());
            let order = open.iter().map(|(_, slot)| *slot).chain(table.marked().iter().copied()).chain(added.iter().map(|(_, slot)| *slot));
            for slot in order {
                if table.row_at(slot).is_some() && read.insert(slot) {
                    slots.push(slot);
                }
            }
            Checked { slots, open, added, restart: false, reopened }
        }
    };

    let mut validators: Vec<(Arc<str>, Asked)> = Vec::new();
    let mut rule = |name: &str| validators.push((Arc::from(name), Asked::Rule));

    // ---- `validate_declared_states`.
    let mut declared: Vec<(Arc<str>, Asked)> = Vec::new();
    for (name, version) in &plan.declared {
        if !held.contains(name)? {
            return refuse("a declared namespace is missing");
        }
        let found = match value_of(world, held, name)? {
            Some(state) => match &*state {
                V::Map(state) => match state.get("schema_version") {
                    Some(V::Int(found)) => *found,
                    Some(_) => return refuse("a value is not an int"),
                    None => return refuse("no schema version"),
                },
                _ => return refuse("a declared namespace is the Python rule's"),
            },
            None => return refuse("a declared namespace is the Python rule's"),
        };
        if found != *version {
            return refuse("a declared namespace has another version");
        }
        if &**name == PRESENCE && presence_unchanged {
            continue;
        }
        declared.push((name.clone(), Asked::Declared));
    }

    // ---- `validate_simulator_extensions`.
    for name in &plan.required {
        if !held.contains(name)? {
            return refuse("a required namespace is missing");
        }
    }
    if held.contains(SPACES)? && !plan.required.iter().any(|name| &**name == SPACES) {
        return refuse("temporary spaces without their contract");
    }
    if held.contains(QUEUES_LEGACY)? {
        // A checkpoint of the queues before facility_queues_v1, an old model: L9 of step4-plan.md.
        return refuse("a legacy queue checkpoint");
    }
    rule("agent_dynamics_v1");
    let mut body_config = None;
    let body_rows = present(world, held, BODY)?;
    if body_rows {
        let body = world.row::<BodyRow>(BODY)?;
        let mut encoder = crate::json::Encoder { out: Vec::new() };
        body.config.write_json(&mut encoder)?;
        body_config = Some(encoder.out);
        body_history(world, held, &body, minute)?;
    }
    if !dynamics_unchanged {
        rule(DYNAMICS);
    }
    if !lifecycle_unchanged {
        rule(LIFECYCLE);
    }
    rule("social_network_v1");
    rule("staff_dynamics_v1");
    rule("staff_operations_v1");
    rule(SPACES);
    rule("movement_v1");
    if !overlay_unchanged {
        rule(OVERLAY);
    }
    if present(world, held, QUEUES)? || plan.independent_queues {
        rule(QUEUES);
    }
    if held.contains(SHARED)? && !plan.shared_configured {
        return refuse("shared operations without their contract");
    }
    let shared_operations = present(world, held, SHARED)?;

    // ---- The rest of `validate`.
    // A v6 world has a ledger and the service state; a v5 world has no service state, and a ledger
    // only with an initial stock.
    let ledger = held.contains(LEDGER)?;
    if v6 {
        if !ledger {
            return refuse("WorldState v6 without a ledger");
        }
        rule(SERVICE);
    } else if held.contains(SERVICE)? {
        return refuse("resource service state in a WorldState v5");
    }
    if plan.information_enabled {
        rule(INFORMATION);
    } else if held.contains(INFORMATION)? {
        return refuse("information state without its model");
    }
    if plan.mean_reverting {
        rule(PSYCHOLOGY);
    } else if held.contains(PSYCHOLOGY)? {
        return refuse("psychological state without its model");
    }
    let operational = present(world, held, OPERATIONAL)?;
    if operational {
        match value_of(world, held, OPERATIONAL)? {
            Some(state) => operational_schema(&state)?,
            None => return refuse("the operational events are the Python rule's"),
        }
    } else if plan.operational_enabled != Some(false) {
        return refuse("operational events without their state");
    }
    let infrastructure_present = present(world, held, INFRASTRUCTURE)?;
    if plan.infrastructure_configured {
        let state = if infrastructure_present { value_of(world, held, INFRASTRUCTURE)? } else { None };
        infrastructure(world, state.as_deref())?;
    } else if infrastructure_present {
        return refuse("infrastructure state without its model");
    }
    let environment_present = present(world, held, ENVIRONMENT)?;
    match &plan.environment_zones {
        Some(baselines) => {
            {
                let rows = entity::tables(world)?;
                if baselines.iter().any(|zone| !rows.zones.contains_key(zone)) {
                    return refuse("a zone environment baseline names an unknown zone");
                }
            }
            let state = if environment_present { value_of(world, held, ENVIRONMENT)? } else { None };
            environment_schema(state.as_deref())?;
        }
        None if environment_present => return refuse("a zone environment without its config"),
        None => {}
    }
    let mut event_rule_broken: Option<&'static str> = None;
    let entity_inputs = EntityInputs {
        requests: Requests::Slots(&requests.slots),
        resource_config: plan.resource_config.as_ref(),
        route_capacities: inputs.route_capacities,
        ledger: held.of(LEDGER)?,
        service: held.of(SERVICE)?,
        spaces: held.of(SPACES)?,
        overlay: held.of(OVERLAY)?,
    };
    let left = entity::entity_rules(world, &entity_inputs, &mut |kind: &str| {
        if !plan.infrastructure_configured && plan.infrastructure_events.contains(kind) {
            event_rule_broken = Some("an infrastructure event without the model");
        } else if plan.environment_zones.is_none() && kind.starts_with(ENVIRONMENT_EVENTS) {
            event_rule_broken = Some("a zone environment event without the policy");
        } else if !operational && plan.operational_events.contains(kind) {
            event_rule_broken = Some("an operational event without its state");
        }
    })?;
    if let Some(reason) = event_rule_broken {
        return refuse(reason);
    }
    if ledger {
        rule(LEDGER);
    }
    // `_validate_zone_capacity` of the zones the entity rules leave: their temporary spaces and
    // rooms count too.
    let places = Places { spaces: held.of(SPACES)?, overlay: held.of(OVERLAY)?, schema: inputs.overlay_schema };
    entity::zone_capacity_passes(world, &places, &left)?;

    declared.append(&mut validators);
    Ok(Found {
        validators: declared,
        body_rows,
        shared_operations,
        body_config,
        agents_unchanged,
        zones_unchanged,
        facilities_unchanged,
        seen,
        presence_is_self_contained,
        unchanged: [configs_unchanged, presence_unchanged, lifecycle_unchanged, dynamics_unchanged, overlay_unchanged],
        requests,
    })
}

/// The rules read after the validators, and the memo moved on.
fn last(world: &mut World, memo: &mut Memo, inputs: &Inputs<'_>, found: Found, named: Option<&[Arc<str>]>) -> Result<()> {
    // Each person a per-person parameter of the body config names is here or in the population
    // state's history.
    if let Some(named) = named {
        let rows = world.rows()?;
        let mut historical: Option<FxHashSet<Arc<str>>> = None;
        for id in named {
            if rows.agents.contains_key(id) {
                continue;
            }
            if historical.is_none() {
                historical = Some(historical_ids(world, inputs.held, false)?);
            }
            if !historical.as_ref().is_some_and(|ids| ids.contains(id)) {
                return refuse("the body config names an unknown resident");
            }
        }
    }

    // ---- Everything passed: what the memo keeps.
    let minute = world.minute_index()?;
    let mut rows = world.rows_mut()?;
    let requests = found.requests;
    {
        let table = &mut rows.support_requests;
        let still_open: Vec<Arc<str>> = if requests.restart || requests.reopened {
            // A request open again keeps its place in the table.
            table.iter().filter(|(_, row)| !is_settled(row)).map(|(key, _)| key.clone()).collect()
        } else {
            let open = |(key, slot): &(Arc<str>, u32)| table.row_at(*slot).is_some_and(|row| !is_settled(row)).then(|| key.clone());
            requests.open.iter().filter_map(open).chain(requests.added.iter().filter_map(open)).collect()
        };
        if requests.restart {
            memo.requests = table.slots().map(|(key, _)| key.clone()).collect();
        } else {
            memo.requests.extend(requests.added.into_iter().map(|(key, _)| key));
        }
        memo.open = still_open;
        table.take_marked();
    }
    if !found.agents_unchanged {
        memo.agent_ids = Some(keys_of(&rows.agents));
    }
    if !found.zones_unchanged {
        memo.zone_ids = Some(keys_of(&rows.zones));
    }
    if !found.facilities_unchanged {
        memo.facility_zones = Some(rows.facilities.iter().map(|(key, facility)| (key.clone(), facility.zone_id.clone())).collect());
    }
    memo.seen = found.seen;
    memo.presence_is_self_contained = found.presence_is_self_contained;
    memo.unchanged = found.unchanged;
    memo.minute = minute;
    Ok(())
}
