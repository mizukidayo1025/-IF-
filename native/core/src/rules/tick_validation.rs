//! The entity and event rules of `WorldState.validate` at the end of a tick, in one call: the keys
//! and IDs, zone edges, people, facilities and their queues, active tasks, staff, the support
//! requests the memo has not settled, the inventory, the ID counters and the event queue. And the
//! validators of two namespaces the population rules declare: `population_dynamics_v1` with no one
//! pending, and `population_presence_v1`.
//!
//! This holds no error text. It returns only if every rule passes, and anything else, including
//! what the rules would accept but is not plain, is a refusal (`Error::Unsupported`):
//! `_validate_entity_rules` and `_validate_event_rules` in `core/world.py` then run as if this
//! weren't here, and pass or raise their own text (policy E6 of docs/rust-port/step5-plan.md). So a
//! refusal never changes a result, and the one thing that must hold is that a return means the
//! Python rules pass (`tests/integration/test_entity_rules_kernel.py`).
//!
//! **It reads the rows** (Issue #152, lane P8 d4): the entity rows, the rows of the namespaces and
//! the fields, and the rows of the queue's events. The rows are the truth and the Python objects
//! follow them (policy E12); a write to an object's `__dict__` past `__setattr__` reaches no row,
//! and so no rule here, and a value that does not fit a row taints the store, which these refuse.
//! The PyO3 crate reads the rows of the pending events held only as Python objects before it calls
//! (`read_queue_rows`); an object that is no row is refused with `NO_ROW`.

use std::sync::Arc;

use indexmap::IndexMap;
use rustc_hash::{FxHashMap, FxHashSet};

use crate::cell::ShardRef;
use crate::config::movement::RouteCapacities;
use crate::config::ResourceNumbers;
use crate::entities::{Activity, Entities, RequestStatus};
use crate::error::{Error, Result};
use crate::events::{EventKind, EventRow};
use crate::row::ValueRow;
use crate::rules::kit::{self, Held, Kept};
use crate::rules::overlay::{self, Places};
use crate::state::resource_ledger::LedgerRow;
use crate::state::resource_service::ServiceRow;
use crate::value::V;
use crate::world::{World, ID_COUNTERS};

const WATER_KIND: &str = "water_station";
const MEAL_KIND: &str = "meal_distribution";
pub const PRESENCE: &str = "population_presence_v1";
pub const DYNAMICS: &str = "population_dynamics_v1";
pub const LIFECYCLE: &str = "population_lifecycle_v1";
pub const SPACES: &str = "temporary_spaces_v1";
pub const OVERLAY: &str = "geometry_overlay_v1";
pub const LEDGER: &str = "resource_ledger_v1";
pub const SERVICE: &str = "resource_service_v1";

/// The refusal for a pending event that is only a Python object, which no row can hold.
pub const NO_ROW: &str = "a pending event has no row";

#[track_caller]
fn refuse<T>(reason: &'static str) -> Result<T> {
    Err(Error::unsupported(reason))
}

/// What the entity rules read besides the world.
pub struct EntityInputs<'a> {
    /// The requests to check.
    pub requests: Requests<'a>,
    /// What the rules read of the parsed `resource_service` config: None for a WorldState v5.
    pub resource_config: Option<&'a ResourceNumbers>,
    /// `layout_routing`, as the end-of-tick check reads it.
    pub route_capacities: &'a Kept<Option<RouteCapacities>>,
    /// Whether the Python world holds these namespaces.
    pub ledger: &'a Held,
    pub service: &'a Held,
    pub spaces: &'a Held,
    pub overlay: &'a Held,
}

/// The requests the rules check.
#[derive(Clone, Copy)]
pub enum Requests<'a> {
    /// Every request of the table, in its order.
    All,
    /// These slots of the requests' table, each once. A slot with no row is skipped.
    Slots(&'a [u32]),
}

/// The people's tables and the zones', borrowed shared: Unsupported while either part is tainted.
#[track_caller]
pub fn tables(world: &World) -> Result<ShardRef<'_, Entities>> {
    let rows = world.rows()?;
    if rows.zones_tainted().is_some() {
        return refuse("the zones are tainted");
    }
    Ok(rows)
}

fn no_repeats(ids: &[Arc<str>]) -> bool {
    match ids.len() {
        0 | 1 => true,
        // These lists hold a few IDs; a queue may hold a hundred.
        count if count <= 16 => ids.iter().enumerate().all(|(place, id)| !ids[..place].contains(id)),
        _ => {
            let distinct: FxHashSet<&str> = ids.iter().map(|id| &**id).collect();
            distinct.len() == ids.len()
        }
    }
}

fn contains(ids: &[Arc<str>], id: &str) -> bool {
    ids.iter().any(|item| &**item == id)
}

/// `[id] == ids`.
fn only(ids: &[Arc<str>], id: &str) -> bool {
    ids.len() == 1 && &*ids[0] == id
}

fn is_unit(value: f64) -> bool {
    // A NaN fails the range, as it fails `0 <= value <= 1`.
    (0.0..=1.0).contains(&value)
}

struct Person<'a> {
    id: &'a str,
    activity: Activity,
    zone: &'a str,
    target: Option<&'a str>,
    request: Option<&'a str>,
}

struct Place<'a> {
    id: &'a str,
    kind: &'a str,
    zone: &'a str,
    is_toilet: bool,
    is_resource: bool,
    queue: &'a [Arc<str>],
    active: &'a [Arc<str>],
    resource_queue: &'a [Arc<str>],
    resource_active: &'a [Arc<str>],
}

struct Work<'a> {
    id: &'a str,
    kind: &'a str,
    agent: Option<&'a str>,
    resource: Option<&'a str>,
    start: i64,
    due: i64,
    staff: Option<&'a str>,
}

struct Worker<'a> {
    active_task: Option<&'a str>,
    work_queue: &'a [Arc<str>],
    busy_until: i64,
    zone: &'a str,
    skills: &'a [Arc<str>],
}

/// How many events of a kind name a task, and the first.
#[derive(Clone, Copy)]
struct Named {
    count: u32,
    first: usize,
}

fn add_named<'a>(map: &mut FxHashMap<&'a str, Named>, key: &'a str, index: usize) {
    map.entry(key).and_modify(|named| named.count += 1).or_insert(Named { count: 1, first: index });
}

fn one_named(map: &FxHashMap<&str, Named>, key: &str) -> Option<usize> {
    map.get(key).filter(|named| named.count == 1).map(|named| named.first)
}

/// `payload.get(key)`, where a key that is absent reads as None.
#[derive(Clone, Copy)]
enum Field<'a> {
    Null,
    Text(&'a str),
    Int(i64),
    Other,
}

impl<'f> Field<'f> {
    /// `payload.get(key) == text`: a value of another type differs, as it does in Python.
    fn is(self, text: &str) -> bool {
        matches!(self, Field::Text(value) if value == text)
    }

    /// `payload.get(key) == expected`, where expected may be None.
    fn same(self, expected: Option<&str>) -> bool {
        match (self, expected) {
            (Field::Null, None) => true,
            (Field::Text(value), Some(text)) => value == text,
            _ => false,
        }
    }

    fn text(self) -> Option<&'f str> {
        match self {
            Field::Text(value) => Some(value),
            _ => None,
        }
    }
}

/// `event.payload.get(key)`.
fn field<'a>(event: &'a EventRow, key: &str) -> Result<Field<'a>> {
    let V::Map(fields) = &event.payload else {
        return refuse("a payload is not a mapping");
    };
    Ok(match fields.get(key) {
        None | Some(V::Null) => Field::Null,
        Some(V::Str(text)) => Field::Text(text),
        Some(V::Int(number)) => Field::Int(*number),
        Some(_) => Field::Other,
    })
}

/// `key in event.payload`.
fn has_field(event: &EventRow, key: &str) -> bool {
    matches!(&event.payload, V::Map(fields) if fields.contains_key(key))
}

/// A value a section parsed, or its error as the read site says it (`configs::read`).
#[track_caller]
fn read(value: &std::result::Result<i64, crate::config::Fault>) -> Result<i64> {
    match value {
        Ok(value) => Ok(*value),
        Err(fault) if fault.is_refusal() => Err(Error::unsupported(fault.text.to_string())),
        Err(_) => Err(Error::corrupt(crate::world::MODEL_CONFIGS, "a value of a section holds an error")),
    }
}

/// The `layout_routing` config of `validate`. The config's own checks were made where the section
/// was parsed, and said where it was read.
fn route_capacities_pass(routing: &RouteCapacities, rows: &Entities) -> Result<()> {
    for (edge, capacity) in &routing.capacities {
        if !rows.zone_edges.contains_key(edge) || !(1..=100).contains(&read(capacity)?) {
            return refuse("a route capacity is the Python rule's");
        }
    }
    Ok(())
}

/// Whether `model_state.get(name, {}).get(part, {})` is an empty dict.
fn part_is_empty(world: &World, name: &str, held: &Held, part: &str) -> Result<bool> {
    let Some(state) = held_value(world, name, held)? else {
        return Ok(true);
    };
    let V::Map(state) = &*state else {
        return Ok(false);
    };
    Ok(match state.get(part) {
        None => true,
        Some(V::Map(items)) => items.is_empty(),
        Some(_) => false,
    })
}

/// The value of a namespace the world holds: None for an absent key, the refusal of `Other`.
fn held_value<'w>(world: &'w World, name: &str, held: &Held) -> Result<Option<ShardRef<'w, V>>> {
    match held {
        Held::Absent => Ok(None),
        Held::Other(reason) => Err(Error::unsupported(*reason)),
        Held::Held => world.slots.try_value(name),
    }
}

/// The entity and event rules of `validate`, for a WorldState v5 or v6 at the end of a tick. The
/// result lists `(zone_id, occupancy)` of the zones whose capacity is the overlay's rule
/// (`zone_capacity_passes`): their temporary spaces and rooms count too. `watch` is told the type
/// of each pending event, for a caller with rules of its own about them (`rules/after_tick.rs`),
/// which then needs no second walk of the queue.
///
/// A v5 world is one whose Scenario has no resource service, and is written today. Its rules are the
/// v6 rules but for the resource service: no resource slots, tasks, completions or service state, no
/// ledger reservations to match, both hydration levels 0, and a water or meal facility may use the
/// ordinary queues (`_validate_entity_rules`, `if self.schema_version == 5`).
pub fn entity_rules(world: &World, inputs: &EntityInputs<'_>, watch: &mut dyn FnMut(&str)) -> Result<Vec<(Arc<str>, i64)>> {
    let v6 = match world.schema_version()? {
        6 => true,
        5 => false,
        _ => return refuse("not a WorldState v5 or v6"),
    };
    if v6 && inputs.resource_config.is_none() {
        return refuse("no resource service config");
    }
    if !v6 && inputs.resource_config.is_some() {
        return refuse("a resource service config for a WorldState v5");
    }
    let (minute, duration) = (world.minute_index()?, world.duration_minutes()?);
    // A seed of any size, past 64 bits too: `self.seed < 0`.
    let negative_seed = world.seed_text()?.starts_with('-');
    if duration <= 0 || !(0..=duration).contains(&minute) || negative_seed {
        return refuse("the clock or seed is the Python rule's");
    }
    let rows = tables(world)?;

    // A ledger the Python rules would stumble over is theirs, even with no reservation to read. The
    // rules of a v5 world read no reservation, and its ledger, if any, is checked by its own kernel.
    let ledger = if v6 {
        match kit::held_row::<LedgerRow>(world, LEDGER, inputs.ledger)? {
            Some(ledger) => Some(ledger),
            None => return refuse("the resource ledger is not owned"),
        }
    } else {
        None
    };

    // ---- Zones and edges. Each map's key is the item's stable ID.
    let mut occupancy: FxHashMap<&str, i64> = FxHashMap::default();
    for (key, zone) in rows.zones.iter() {
        if key.is_empty() || zone.id != *key {
            return refuse("a zone key is not its ID");
        }
        occupancy.insert(key, 0);
    }
    for (key, edge) in rows.zone_edges.iter() {
        if key.is_empty() || edge.id != *key {
            return refuse("an edge key is not its ID");
        }
        let (from, to) = (&*edge.from_zone_id, &*edge.to_zone_id);
        if !occupancy.contains_key(from) || !occupancy.contains_key(to) || from == to || edge.travel_minutes <= 0 {
            return refuse("an edge is the Python rule's");
        }
    }
    if let Some(routing) = &*kit::kept(inputs.route_capacities)? {
        route_capacities_pass(routing, &rows)?;
    }

    // ---- People: world_agents_valid, the keys, and what the rules below read.
    let mut people: Vec<Person<'_>> = Vec::with_capacity(rows.agents.len());
    let mut person_at: FxHashMap<&str, usize> = FxHashMap::default();
    person_at.reserve(rows.agents.len());
    for (key, agent) in rows.agents.iter() {
        if key.is_empty() || agent.profile.id != *key {
            return refuse("an agent key is not its ID");
        }
        let needs = &agent.needs;
        if ![needs.toilet_urgency, needs.hunger, needs.fatigue, needs.hydration_need].into_iter().all(is_unit) {
            return refuse("a need is the Python rule's");
        }
        // "WorldState v5 requires hydration_need to be zero".
        if !v6 && (needs.hydration_need != 0.0 || agent.profile.initial_needs.hydration_need != 0.0) {
            return refuse("a WorldState v5 person has a hydration need");
        }
        if !is_unit(agent.stress) || !is_unit(agent.trust) || agent.next_decision_at < 0 || agent.support_cooldown_until < 0 {
            return refuse("an agent's value is the Python rule's");
        }
        let zone = &*agent.zone_id;
        let Some(count) = occupancy.get_mut(zone) else {
            return refuse("an agent's zone is unknown");
        };
        *count += 1;
        // The rules tell apart only these four; the rest are alike to them.
        let activity = match agent.activity {
            Activity::Sleeping | Activity::Resting => Activity::Idle,
            other => other,
        };
        person_at.insert(key, people.len());
        people.push(Person {
            id: key,
            activity,
            zone,
            target: agent.target_id.as_deref(),
            request: agent.support_request_id.as_deref(),
        });
    }

    // ---- Facilities and who is in them.
    let mut places: Vec<Place<'_>> = Vec::with_capacity(rows.facilities.len());
    let mut place_at: FxHashMap<&str, usize> = FxHashMap::default();
    for (key, facility) in rows.facilities.iter() {
        if key.is_empty() || facility.id != *key {
            return refuse("a facility key is not its ID");
        }
        let kind = &*facility.kind;
        if !kind.is_ascii() {
            return refuse("a facility kind is not ASCII");
        }
        let zone = &*facility.zone_id;
        let capacity = facility.capacity;
        if !occupancy.contains_key(zone) || capacity < 0 || !(0..=capacity).contains(&facility.working_units) {
            return refuse("a facility's zone or units are the Python rule's");
        }
        let place = Place {
            id: key,
            kind,
            zone,
            is_toilet: kind.as_bytes().windows(6).any(|part| part.eq_ignore_ascii_case(b"toilet")),
            is_resource: kind == WATER_KIND || kind == MEAL_KIND,
            queue: &facility.queue_agent_ids,
            active: &facility.active_agent_ids,
            resource_queue: &facility.resource_queue_agent_ids,
            resource_active: &facility.resource_active_agent_ids,
        };
        if place.active.len() as i64 > capacity || place.resource_active.len() as i64 > capacity {
            return refuse("a facility is over capacity");
        }
        if !no_repeats(place.queue) || !no_repeats(place.active) || !no_repeats(place.resource_queue) || !no_repeats(place.resource_active) {
            return refuse("a facility has duplicate entries");
        }
        // A v5 world has no resource service: its water and meal facilities use the ordinary queues.
        // "WorldState v5 cannot contain resource service occupancy" needs no rule of its own here:
        // a resource slot needs its resource task below, which a v5 world may not have.
        if v6 && place.is_resource && (!place.queue.is_empty() || !place.active.is_empty()) {
            return refuse("a resource facility uses the legacy queues");
        }
        if !place.is_resource && (!place.resource_queue.is_empty() || !place.resource_active.is_empty()) {
            return refuse("another facility holds resource slots");
        }
        place_at.insert(key, places.len());
        places.push(place);
    }
    let mut waiting: FxHashSet<&str> = FxHashSet::default();
    let mut served: FxHashSet<&str> = FxHashSet::default();
    let person = |id: &str| person_at.get(id).map(|index| &people[*index]);
    for place in &places {
        for id in place.queue {
            if person(id).is_none_or(|who| who.activity != Activity::Queued) || served.contains(&**id) || !waiting.insert(id) {
                return refuse("a facility queue is the Python rule's");
            }
        }
        for id in place.active {
            if person(id).is_none_or(|who| who.activity != Activity::InService) || waiting.contains(&**id) || !served.insert(id) {
                return refuse("a facility service is the Python rule's");
            }
        }
    }
    for place in &places {
        let holds_here = |who: &Person<'_>| who.target == Some(place.id) && who.zone == place.zone;
        for id in place.resource_queue {
            if person(id).is_none_or(|who| who.activity != Activity::Queued || !holds_here(who)) || served.contains(&**id) || !waiting.insert(id) {
                return refuse("a resource queue is the Python rule's");
            }
        }
        for id in place.resource_active {
            if person(id).is_none_or(|who| who.activity != Activity::InService || !holds_here(who)) || waiting.contains(&**id) || !served.insert(id) {
                return refuse("a resource service is the Python rule's");
            }
        }
    }
    for who in &people {
        if (who.activity == Activity::Queued && !waiting.contains(who.id)) || (who.activity == Activity::InService && !served.contains(who.id)) {
            return refuse("someone is missing from the facilities");
        }
    }

    // ---- The pending events, from the rows of the queue.
    let queue = world.queue()?;
    if queue.validate() != 0 {
        return refuse("the event queue repeats an identity");
    }
    let mut events: Vec<&EventRow> = Vec::with_capacity(queue.ordered().len());
    for (_, serial) in queue.ordered() {
        match &queue.slot(*serial).row {
            Some(row) => events.push(row),
            None => return refuse(NO_ROW),
        }
    }
    let mut toilet_events: FxHashMap<&str, Named> = FxHashMap::default();
    let mut resource_events: FxHashMap<&str, Named> = FxHashMap::default();
    let mut travel_events: FxHashMap<&str, Named> = FxHashMap::default();
    let mut staff_events: FxHashMap<&str, Named> = FxHashMap::default();
    let mut timeouts: FxHashMap<&str, Named> = FxHashMap::default();
    for (index, event) in events.iter().enumerate() {
        let kind = &*event.event_type;
        watch(kind);
        let target = match kind {
            "toilet_service_complete" => &mut toilet_events,
            "resource_service_complete" => &mut resource_events,
            "agent_travel_complete" => &mut travel_events,
            "staff_support_complete" => &mut staff_events,
            "support_request_timeout" => {
                // Another type of request ID is no ID of a request.
                if let Some(request_id) = field(event, "request_id")?.text() {
                    add_named(&mut timeouts, request_id, index);
                }
                continue;
            }
            _ => continue,
        };
        // A task ID that is no str names no task here, as it fills no list in Python.
        if kind == "resource_service_complete" && !v6 {
            // Python reads the v6 service state for it, which a v5 world has not.
            return refuse("a resource completion in a WorldState v5");
        }
        if let Some(task_id) = field(event, "task_id")?.text() {
            add_named(target, task_id, index);
        } else if kind == "resource_service_complete" {
            return refuse("a resource completion lacks a task ID");
        }
    }

    // ---- Tasks.
    let mut works: Vec<Work<'_>> = Vec::with_capacity(rows.active_tasks.len());
    let mut work_at: FxHashMap<&str, usize> = FxHashMap::default();
    let mut travel_by_agent: FxHashMap<&str, usize> = FxHashMap::default();
    let mut toilet_queue: FxHashSet<(&str, &str)> = FxHashSet::default();
    let mut toilet_service: FxHashMap<(&str, &str), usize> = FxHashMap::default();
    let mut resource_queue: FxHashSet<(&str, &str)> = FxHashSet::default();
    let mut resource_service: FxHashMap<(&str, &str), usize> = FxHashMap::default();
    for (key, task) in rows.active_tasks.iter() {
        if key.is_empty() || task.id != *key {
            return refuse("a task key is not its ID");
        }
        let work = Work {
            id: key,
            kind: &task.kind,
            agent: task.agent_id.as_deref(),
            resource: task.resource_id.as_deref(),
            start: task.start_minute,
            due: task.due_minute,
            staff: task.staff_id.as_deref(),
        };
        let index = works.len();
        match work.kind {
            "travel_to_toilet" | "travel_to_resource" | "travel_return" | "staff_support_travel" => {
                let (Some(agent), None) = (work.agent, work.resource) else {
                    return refuse("a travel task's references are the Python rule's");
                };
                if work.start > minute || work.due <= work.start || travel_by_agent.insert(agent, index).is_some() {
                    return refuse("a travel task is the Python rule's");
                }
            }
            "resource_service_queue" | "resource_service_water" | "resource_service_food" => {
                if !v6 {
                    return refuse("a resource task in a WorldState v5");
                }
                let (Some(agent), Some(resource)) = (work.agent, work.resource) else {
                    return refuse("a resource task's references are the Python rule's");
                };
                if !place_at.get(resource).is_some_and(|at| places[*at].is_resource) || work.start > minute {
                    return refuse("a resource task is the Python rule's");
                }
                if work.kind == "resource_service_queue" {
                    if work.due != work.start || work.staff.is_some() || !resource_queue.insert((resource, agent)) {
                        return refuse("a resource queue task is the Python rule's");
                    }
                } else if work.due <= work.start || work.staff.is_none() || resource_service.insert((resource, agent), index).is_some() {
                    return refuse("a resource service task is the Python rule's");
                }
            }
            "toilet_queue" | "toilet_service" => {
                let (Some(agent), Some(resource)) = (work.agent, work.resource) else {
                    return refuse("a toilet task's references are the Python rule's");
                };
                if !place_at.get(resource).is_some_and(|at| places[*at].is_toilet) || work.start > minute {
                    return refuse("a toilet task is the Python rule's");
                }
                if work.kind == "toilet_queue" {
                    if work.due != work.start || !toilet_queue.insert((resource, agent)) {
                        return refuse("a toilet queue task is the Python rule's");
                    }
                } else if work.due <= work.start || toilet_service.insert((resource, agent), index).is_some() {
                    return refuse("a toilet service task is the Python rule's");
                }
            }
            _ => {}
        }
        work_at.insert(key, index);
        works.push(work);
    }

    // ---- Staff, read before the tasks are matched with them.
    let mut workers: FxHashMap<&str, Worker<'_>> = FxHashMap::default();
    let mut assigned: FxHashSet<&str> = FxHashSet::default();
    for (key, member) in rows.staff.iter() {
        if key.is_empty() || member.id != *key {
            return refuse("a staff key is not its ID");
        }
        let zone = &*member.zone_id;
        let work_queue = &member.work_queue[..];
        let busy_until = member.busy_until;
        let active_task = member.active_task_id.as_deref();
        if !occupancy.contains_key(zone) || !no_repeats(work_queue) || busy_until < 0 {
            return refuse("a staff member is the Python rule's");
        }
        for task_id in work_queue {
            if !work_at.contains_key(&**task_id) || !assigned.insert(task_id) {
                return refuse("a staff queue is the Python rule's");
            }
        }
        match active_task {
            None if !work_queue.is_empty() || busy_until != 0 => {
                return refuse("an idle staff member keeps work");
            }
            Some(task) if !only(work_queue, task) => {
                return refuse("a staff member's work is the Python rule's");
            }
            _ => {}
        }
        workers.insert(key, Worker { active_task, work_queue, busy_until, zone, skills: &member.skills.0 });
    }

    // ---- Facilities and tasks each name the other.
    let queue_person_targets = |id: &str, place: &Place<'_>| person(id).is_some_and(|who| who.target == Some(place.id));
    for place in &places {
        if place.is_toilet {
            for id in place.queue {
                if !toilet_queue.contains(&(place.id, &**id)) || !queue_person_targets(id, place) {
                    return refuse("someone in a toilet queue has no task");
                }
            }
            for id in place.active {
                if !toilet_service.contains_key(&(place.id, &**id)) || !queue_person_targets(id, place) {
                    return refuse("someone in a toilet has no task");
                }
            }
        }
        if place.is_resource {
            for id in place.resource_queue {
                if !resource_queue.contains(&(place.id, &**id)) || !queue_person_targets(id, place) {
                    return refuse("someone in a resource queue has no task");
                }
            }
            for id in place.resource_active {
                if !resource_service.contains_key(&(place.id, &**id)) || !queue_person_targets(id, place) {
                    return refuse("someone at a resource has no task");
                }
            }
        }
    }
    let place_of = |id: &str| place_at.get(id).map(|index| &places[*index]);
    #[allow(clippy::iter_over_hash_type, reason = "read-only checks; a refusal here hands the world to the Python check, which says its own text")]
    for (facility, agent) in &toilet_queue {
        if !place_of(facility).is_some_and(|place| contains(place.queue, agent)) || person(agent).is_none_or(|who| who.activity != Activity::Queued) {
            return refuse("a toilet queue task has no entry");
        }
    }
    #[allow(clippy::iter_over_hash_type, reason = "read-only checks; a refusal here hands the world to the Python check, which says its own text")]
    for ((facility, agent), index) in &toilet_service {
        let work = &works[*index];
        if !place_of(facility).is_some_and(|place| contains(place.active, agent)) || person(agent).is_none_or(|who| who.activity != Activity::InService) {
            return refuse("a toilet service task has no slot");
        }
        let Some(event) = one_named(&toilet_events, work.id).map(|at| events[at]) else {
            return refuse("a toilet service task has no completion");
        };
        let staff_matches = !has_field(event, "staff_id") || field(event, "staff_id")?.same(work.staff);
        if event.kind != EventKind::Completion
            || event.due_minute != work.due
            || !field(event, "facility_id")?.is(facility)
            || !field(event, "agent_id")?.is(agent)
            || !staff_matches
        {
            return refuse("a toilet service task's completion is the Python rule's");
        }
    }
    #[allow(clippy::iter_over_hash_type, reason = "read-only checks; a refusal here hands the world to the Python check, which says its own text")]
    for (facility, agent) in &resource_queue {
        if !place_of(facility).is_some_and(|place| contains(place.resource_queue, agent)) || person(agent).is_none_or(|who| who.activity != Activity::Queued) {
            return refuse("a resource queue task has no entry");
        }
    }
    let mut active_reservations: FxHashSet<&str> = FxHashSet::default();
    if !resource_service.is_empty() {
        let Some(config) = inputs.resource_config else {
            return refuse("no resource service config");
        };
        #[allow(clippy::iter_over_hash_type, reason = "read-only checks and a set of ids; a refusal hands the world to the Python check")]
        for ((facility, agent), index) in &resource_service {
            let work = &works[*index];
            let Some(place) = place_of(facility) else {
                return refuse("a resource service task's facility is unknown");
            };
            if !contains(place.resource_active, agent) || person(agent).is_none_or(|who| who.activity != Activity::InService) {
                return refuse("a resource service task has no slot");
            }
            let is_water = place.kind == WATER_KIND;
            let resource_id = if is_water { "water_ml" } else { "food_meal_units" };
            let quantity = if is_water { config.water_ml_per_service } else { config.food_meal_units_per_service };
            let skill = if is_water { &*config.water_staff_skill } else { &*config.meal_staff_skill };
            let Some(event) = one_named(&resource_events, work.id).map(|at| events[at]) else {
                return refuse("a resource service task has no completion");
            };
            if event.due_minute != work.due
                || !field(event, "facility_id")?.is(facility)
                || !field(event, "agent_id")?.is(agent)
                || !field(event, "resource_id")?.is(resource_id)
                || !field(event, "reservation_id")?.is(work.id)
                || !field(event, "staff_id")?.same(work.staff)
                || event.kind != EventKind::Completion
                || !matches!(field(event, "quantity")?, Field::Int(value) if value == quantity)
            {
                return refuse("a resource service task's completion is the Python rule's");
            }
            let Some(ledger) = &ledger else {
                return refuse("a resource service task without a ledger");
            };
            let account = ledger.accounts.of(usize::from(!is_water));
            let reserved = account.reservations.get(work.id).is_some_and(|reservation| {
                &*reservation.agent_id == *agent && &*reservation.resource_id == resource_id && reservation.quantity == quantity
            });
            if !reserved {
                return refuse("a resource service task has no reservation");
            }
            active_reservations.insert(work.id);
            let Some(worker) = work.staff.and_then(|id| workers.get(id)) else {
                return refuse("a resource service task's staff is unknown");
            };
            let has_skill = worker.skills.iter().any(|item| &**item == skill);
            if worker.active_task != Some(work.id) || !only(worker.work_queue, work.id) || worker.busy_until != work.due || worker.zone != place.zone || !has_skill {
                return refuse("a resource service task's staff is the Python rule's");
            }
        }
    }
    // A v5 world has no service state: no resource task or completion came this far.
    if v6 {
        let Some(service) = kit::held_row::<ServiceRow>(world, SERVICE, inputs.service)? else {
            return refuse("no resource service state");
        };
        #[allow(clippy::iter_over_hash_type, reason = "read-only checks; a refusal here hands the world to the Python check, which says its own text")]
        for (task_id, _) in resource_events.iter() {
            if !work_at.contains_key(task_id) && !contains(&service.cancelled_task_ids, task_id) {
                return refuse("a resource completion names an inactive task");
            }
        }
        let listed: FxHashSet<&str> = service.active_reservation_ids.iter().map(|id| &**id).collect();
        if listed != active_reservations {
            return refuse("the active reservations differ from the active tasks");
        }
        // Each of these was found in the ledger above, in its task's account.
    }

    // ---- Travel.
    #[allow(clippy::iter_over_hash_type, reason = "read-only checks; a refusal here hands the world to the Python check, which says its own text")]
    for (agent_id, index) in &travel_by_agent {
        let work = &works[*index];
        let Some(who) = person(agent_id).filter(|who| who.activity == Activity::Moving) else {
            return refuse("a travel task needs a moving agent");
        };
        let Some(event) = one_named(&travel_events, work.id).map(|at| events[at]) else {
            return refuse("a travel task has no completion");
        };
        if event.due_minute != work.due {
            return refuse("a travel task's completion is late");
        }
        let purpose = field(event, "purpose")?.text();
        let Some(destination) = field(event, "destination_zone_id")?.text() else {
            return refuse("a travel event's destination is not a str");
        };
        if !occupancy.contains_key(destination) {
            return refuse("a travel event's destination is unknown");
        }
        let points_right = if matches!(purpose, Some("toilet_request" | "resource_request")) {
            field(event, "facility_id")?.same(who.target)
        } else {
            who.target == Some(destination)
        };
        if !points_right {
            return refuse("a travel event's target differs");
        }
        if work.kind == "staff_support_travel" {
            let request_known = field(event, "request_id")?.text().map(|id| rows.support_requests.contains_key(id));
            let staff_known = field(event, "staff_id")?.text().is_some_and(|id| workers.contains_key(id));
            if purpose != Some("staff_support_travel") || request_known != Some(true) || !staff_known {
                return refuse("a staff support trip is the Python rule's");
            }
        }
    }
    for who in &people {
        if who.activity == Activity::Moving && !travel_by_agent.contains_key(who.id) {
            return refuse("someone moving has no travel task");
        }
    }

    // ---- The support requests the memo says may have changed.
    // Open requests by agent: how many, and the ID of the first.
    let mut open_by_agent: FxHashMap<&str, (u32, &str)> = FxHashMap::default();
    let slots: Vec<u32> = match inputs.requests {
        Requests::All => rows.support_requests.slots().map(|(_, slot)| slot).collect(),
        Requests::Slots(slots) => slots.to_vec(),
    };
    for slot in slots {
        let Some(request) = rows.support_requests.row_at(slot) else {
            continue;
        };
        let id = &*request.id;
        if id.is_empty() || !rows.support_requests.contains_key(id) {
            return refuse("a request's ID is the Python rule's");
        }
        let agent = &*request.agent_id;
        let (requested, timeout) = (request.requested_minute, request.timeout_minute);
        if !person_at.contains_key(agent) || !(0..=minute).contains(&requested) || timeout < requested || request.cooldown_until < 0 {
            return refuse("a request's agent or timing is the Python rule's");
        }
        let attempt = request.last_assignment_attempt_minute;
        if attempt.is_some_and(|attempt| !(requested..=minute).contains(&attempt)) {
            return refuse("a request's attempt is the Python rule's");
        }
        if let Some(reason) = &request.last_assignment_wait_reason_code {
            let known = matches!(
                &**reason,
                "no_available_qualified_staff" | "route_unavailable_at_assignment" | "insufficient_run_time" | "resident_travel_schedule_rejected"
            );
            if attempt.is_none() || !known {
                return refuse("a request's wait reason is the Python rule's");
            }
        }
        let staff_id = request.assigned_staff_id.as_deref();
        let task_id = request.task_id.as_deref();
        match request.status {
            RequestStatus::Assigned => {
                let (Some(worker_id), Some(task_id)) = (staff_id, task_id) else {
                    return refuse("an assigned request names no staff or task");
                };
                let (Some(worker), Some(work)) = (workers.get(worker_id), work_at.get(task_id).map(|at| &works[*at])) else {
                    return refuse("an assigned request's staff or task is unknown");
                };
                if work.kind != "staff_support" || work.staff != Some(worker_id) || work.agent != Some(agent) || worker.active_task != Some(work.id) || work.due != worker.busy_until {
                    return refuse("an assigned request lacks one matching staff task");
                }
                let Some(who) = person(agent) else {
                    return refuse("an assigned request's agent is unknown");
                };
                let travel = travel_by_agent.get(agent).map(|at| &works[*at]);
                if who.activity == Activity::Moving {
                    let event = travel.and_then(|trip| one_named(&travel_events, trip.id)).map(|at| events[at]);
                    let right = match (travel, event) {
                        (Some(trip), Some(event)) => {
                            trip.kind == "staff_support_travel"
                                && field(event, "request_id")?.is(id)
                                && field(event, "staff_id")?.is(worker_id)
                                && field(event, "destination_zone_id")?.is(worker.zone)
                        }
                        _ => false,
                    };
                    if !right {
                        return refuse("an assigned request's travel is the Python rule's");
                    }
                } else if who.activity != Activity::SeekingHelp || travel.is_some_and(|trip| trip.kind == "staff_support_travel") {
                    return refuse("an assigned request must seek help or travel to staff");
                }
                let Some(event) = one_named(&staff_events, task_id).map(|at| events[at]) else {
                    return refuse("an assigned request needs one completion");
                };
                if event.due_minute != work.due || !field(event, "request_id")?.is(id) || !field(event, "agent_id")?.is(agent) || !field(event, "staff_id")?.is(worker_id) {
                    return refuse("an assigned request's completion is the Python rule's");
                }
                open_by_agent.entry(agent).or_insert((0, id)).0 += 1;
            }
            RequestStatus::Pending => {
                if staff_id.is_some() || task_id.is_some() {
                    return refuse("a pending request keeps an assignment");
                }
                if timeout < duration && !one_named(&timeouts, id).is_some_and(|at| events[at].due_minute == timeout) {
                    return refuse("a pending request needs one timeout");
                }
                open_by_agent.entry(agent).or_insert((0, id)).0 += 1;
            }
            RequestStatus::Resolved | RequestStatus::Unresolved => {}
        }
    }
    for who in &people {
        match open_by_agent.get(who.id) {
            Some((count, _)) if *count > 1 => return refuse("someone has duplicate open requests"),
            Some((_, request)) => {
                if who.request != Some(*request) || !matches!(who.activity, Activity::SeekingHelp | Activity::Moving) {
                    return refuse("someone's open request is the Python rule's");
                }
            }
            None => {
                if who.request.is_some() {
                    return refuse("someone names a request that is not open");
                }
            }
        }
    }

    // ---- Every task once more.
    for work in &works {
        if work.agent.is_some_and(|agent| !person_at.contains_key(agent)) {
            return refuse("a task's agent is unknown");
        }
        if work.kind != "staff_support" && work.resource.is_some_and(|resource| !place_at.contains_key(resource)) {
            return refuse("a task's facility is unknown");
        }
        if work.kind == "staff_support" {
            let right = work.agent.is_some()
                && work.resource.is_none()
                && work.due > work.start
                && work.staff.and_then(|id| workers.get(id)).is_some_and(|worker| worker.active_task == Some(work.id) && worker.busy_until == work.due)
                && one_named(&staff_events, work.id).is_some_and(|at| events[at].due_minute == work.due);
            if !right {
                return refuse("a staff support task is inconsistent");
            }
        }
        if work.start < 0 || work.due < work.start {
            return refuse("a task's timing is the Python rule's");
        }
    }

    // ---- The inventory, the counters and the event identities.
    if world.resources()?.values().any(|value| *value < 0) {
        return refuse("the inventory is the Python rule's");
    }
    let counters = world.slots.try_row::<ValueRow>(ID_COUNTERS)?;
    let V::Map(counters) = &counters.0 else {
        return refuse("id_counters is not a map");
    };
    for value in counters.values() {
        if kit::exact_int(value)? < 0 {
            return refuse("an ID counter is negative");
        }
    }
    let log = world.log()?;
    let mut newest = log.max_sequence();
    for event in &events {
        if log.has_event_id(&event.event_id) || log.has_sequence(event.sequence) || event.due_minute >= duration {
            return refuse("a pending event is the Python rule's");
        }
        newest = newest.max(event.sequence);
    }
    let counter = counters.get("event").map_or(Ok(0), kit::exact_int)?;
    if counter <= newest {
        return refuse("the event counter is not past the sequences");
    }

    // ---- Zone capacity, last so a Python rule reads it after everything else passed.
    let spaces_are_empty = part_is_empty(world, SPACES, inputs.spaces, "spaces")? && part_is_empty(world, OVERLAY, inputs.overlay, "temporary_rooms")?;
    let mut left = Vec::new();
    for (id, zone) in rows.zones.iter() {
        let Some(nominal) = zone.capacity else {
            continue;
        };
        let inside = occupancy[&**id];
        // With no space or room, effective_total_zone_capacity is the zone's own.
        if !spaces_are_empty || inside > nominal {
            left.push((id.clone(), inside));
        }
    }
    Ok(left)
}

/// `_validate_zone_capacity` of the zones the entity rules leave: their temporary spaces and rooms
/// count too (`rules/overlay.rs`).
pub fn zone_capacity_passes(world: &World, places: &Places<'_>, left: &[(Arc<str>, i64)]) -> Result<()> {
    for (zone, occupancy) in left {
        if let Some((_, found)) = overlay::zone_capacity(world, places, zone, Some(*occupancy))? {
            if *occupancy > found.total()? {
                return refuse("a zone holds more than its capacity");
            }
        }
    }
    Ok(())
}

// ---- The validators of the population's namespaces ---------------------------------------------

const POPULATION_POLICY_VERSION: &str = "capacity-aware-arrivals-departures-v1";
const POPULATION_SOURCE_ID: &str = "ASM-POPULATION-DYNAMICS-V1";

fn required_list<'a>(state: &'a IndexMap<Arc<str>, V>, name: &str) -> Result<&'a [V]> {
    match state.get(name) {
        Some(V::List(items)) => Ok(items),
        _ => refuse("a list of the population state is the Python rule's"),
    }
}

/// What `population_dynamics` found.
#[derive(Debug, PartialEq, Eq)]
pub enum Dynamics {
    /// The rules pass.
    Taken,
    /// Someone waits, or has a context: the population's own validation reads those
    /// (`validate_population_dynamics_state`, `rules/population.rs`), and its answer is this one.
    Validate,
}

/// `validate_population_dynamics_state` of a state with no dynamic context and no one pending, the
/// parts of it that read `PersonContext`, `PersonTraits` and the approach routes.
pub fn population_dynamics(world: &World, dynamics: &Held) -> Result<Dynamics> {
    let Some(state) = held_value(world, DYNAMICS, dynamics)? else {
        return Ok(Dynamics::Taken);
    };
    let state = match &*state {
        V::Null => return Ok(Dynamics::Taken),
        V::Map(state) => state,
        _ => return refuse("a value is not a plain dict"),
    };
    let text_is = |name: &str, expected: &str| matches!(state.get(name), Some(V::Str(text)) if &**text == expected);
    let version_is_one = matches!(state.get("schema_version"), Some(V::Int(1)));
    let context = state.get("profile_context");
    let no_context = match context {
        None => true,
        Some(V::Map(context)) => context.is_empty(),
        Some(_) => return refuse("a value is not a plain dict"),
    };
    let initial = required_list(state, "initial_agent_ids")?;
    let pending = required_list(state, "pending_arrivals")?;
    required_list(state, "arrivals")?;
    required_list(state, "departures")?;
    let Some(traits) = state.get("profile_traits") else {
        return refuse("the population state lacks its traits");
    };
    let V::Map(traits) = traits else {
        return refuse("a value is not a plain dict");
    };
    let fields = if context.is_some() { 8 + 1 } else { 8 };
    if state.len() != fields || !version_is_one || !text_is("policy_version", POPULATION_POLICY_VERSION) || !text_is("source_id", POPULATION_SOURCE_ID) {
        return refuse("the population state is the Python rule's");
    }
    if !no_context || !pending.is_empty() {
        return Ok(Dynamics::Validate);
    }
    let mut seen: FxHashSet<&str> = FxHashSet::default();
    seen.reserve(initial.len());
    for id in initial {
        let V::Str(id) = id else {
            return refuse("a value is not a str");
        };
        if !seen.insert(id) {
            return refuse("the initial agents repeat an ID");
        }
    }
    for (id, _) in world.rows()?.agents.iter() {
        if !seen.contains(&**id) && !traits.contains_key(&**id) {
            return refuse("an active agent has no profile");
        }
    }
    Ok(Dynamics::Taken)
}

/// Whether `value[part]...` reaches something through these parts, all str keys of dicts.
fn value_path_exists(mut value: &V, path: &[V]) -> bool {
    for part in path {
        let (V::Map(map), V::Str(part)) = (value, part) else {
            return false;
        };
        match map.get(&**part) {
            Some(next) => value = next,
            None => return false,
        }
    }
    true
}

/// `validate_presence_state` (`contracts/presence.py`), the validator of the declared namespace
/// `population_presence_v1`: Ok, or a refusal for the contract and the rule to say. The parse of the
/// contract is `rules/population.rs`' `presence_is_plain`, which refuses what it isn't sure of. An
/// archive path is read through the namespace its first part names: the lifecycle state, the only
/// one the population's rules archive to (`lifecycle` says whether the world holds it), or the
/// presence state itself; any other is refused.
pub fn validate_presence(world: &World, presence: &Held, lifecycle: &Held) -> Result<()> {
    let Some(raw) = held_value(world, PRESENCE, presence)? else {
        // The declared check said what a missing namespace is; here there is nothing to read.
        return refuse("no presence state");
    };
    crate::rules::population::presence_is_plain(&*raw)?;
    let V::Map(state) = &*raw else {
        return refuse("the presence state is not a mapping");
    };
    let minute = world.minute_index()?;
    let rows = tables(world)?;
    let known_zone = |zone: Option<&V>| -> bool {
        match zone {
            None | Some(V::Null) => true,
            Some(V::Str(zone)) => rows.zones.contains_key(zone),
            Some(_) => false,
        }
    };
    let empty = IndexMap::new();
    let people = match state.get("people") {
        Some(V::Map(people)) => people,
        _ => &empty,
    };
    for (key, person) in people {
        let V::Map(person) = person else {
            return refuse("a presence record is not a mapping");
        };
        let Some(V::Str(agent_id)) = person.get("agent_id") else {
            return refuse("a presence record lacks its ID");
        };
        let Some(V::Int(since)) = person.get("since_minute") else {
            return refuse("a presence record lacks its minute");
        };
        let admitted = matches!(person.get("status"), Some(V::Str(status)) if &**status == "admitted");
        if key != agent_id
            || *since > minute
            || admitted != rows.agents.contains_key(key)
            || !known_zone(person.get("transition_zone_id"))
            || !known_zone(person.get("return_zone_id"))
        {
            return refuse("a presence record disagrees with the world");
        }
        if let Some(V::List(path)) = person.get("archive_path") {
            // `value = world.model_state; value = value[part]`: the first part names a namespace.
            let Some((V::Str(first), rest)) = path.split_first() else {
                return refuse("an archive path holds another value");
            };
            let found = if &**first == LIFECYCLE {
                held_value(world, LIFECYCLE, lifecycle)?.is_some_and(|value| value_path_exists(&value, rest))
            } else if &**first == PRESENCE {
                value_path_exists(&raw, rest)
            } else {
                return refuse("a presence archive path names another namespace");
            };
            if !found {
                return refuse("a presence archive path is missing");
            }
        }
    }
    for (key, _) in rows.agents.iter() {
        if !people.contains_key(&**key) {
            return refuse("the presence omits an admitted person");
        }
    }
    if let Some(V::List(history)) = state.get("history") {
        let mut ids: FxHashSet<&str> = FxHashSet::default();
        let mut previous = -1;
        for receipt in history {
            let V::Map(receipt) = receipt else {
                return refuse("a presence receipt is not a mapping");
            };
            let (Some(V::Str(id)), Some(V::Str(agent_id)), Some(V::Int(at))) = (receipt.get("transition_id"), receipt.get("agent_id"), receipt.get("minute")) else {
                return refuse("a presence receipt lacks a field");
            };
            if !ids.insert(id) || !people.contains_key(&**agent_id) || *at < previous || *at > minute {
                return refuse("a presence receipt's identity, order or time disagrees");
            }
            if !known_zone(receipt.get("from_zone_id")) || !known_zone(receipt.get("to_zone_id")) {
                return refuse("a presence receipt names an unknown zone");
            }
            previous = *at;
        }
    }
    Ok(())
}
