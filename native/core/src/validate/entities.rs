//! `WorldState._validate_entity_rules` and `_validate_zone_capacity` (`core/world.py`), with
//! Python's errors and texts: the clock, the seed, the entity maps, the zone edges, the route
//! capacities, the Agents, the zones' capacity, the facilities and their queues, the tasks and the
//! events that serve them, the staff, the support requests and the stocks.
//!
//! Lane C of #250 (#348). It raises the `StateInvariantError`s of `WorldState.validate` itself, and
//! the `KeyError` or `TypeError` that Python's reads of a broken namespace raise (`_state_part`)
//! as they are. `rules/tick_validation.rs` holds the same rules without texts (pass or refuse), for
//! the end of a tick.
//!
//! The rows are typed, so what Python checks of a value's type (an Activity, a NeedState, an int
//! for a time, a request's status) holds here by construction; the rules that read values check
//! them as Python does, in Python's order, so the first error is the same.
//!
//! The helpers at the end (`index`, `contains`, `get`, ...) read a `V` as Python reads the value it
//! stands for, with Python's `KeyError` and `TypeError` texts; the other validators of lane C
//! (`operational_events.rs`, `infrastructure.rs`) use them too.

use std::sync::Arc;

use indexmap::IndexMap;
use rustc_hash::{FxHashMap, FxHashSet};

use super::resource_service::ServiceConfig;
use super::View;
use crate::config::overlay::overlay_schema;
use crate::entities::{Activity, AgentRow, Entities, NeedRow, RequestStatus, TaskRow};
use crate::error::{Error, Result};
use crate::events::{EventKind, EventRow};
use crate::rules::dispatch::py_eq;
use crate::rules::kit::Held;
use crate::rules::overlay::{self, Places};
use crate::state::resource_ledger::LedgerRow;
use crate::state::resource_service::ServiceRow;
use crate::text::{casefold, py_repr};
use crate::value::V;

const WATER_KIND: &str = "water_station";
const MEAL_KIND: &str = "meal_distribution";
const LEDGER: &str = "resource_ledger_v1";
const SERVICE: &str = "resource_service_v1";
const SPACES: &str = "temporary_spaces_v1";
const OVERLAY: &str = "geometry_overlay_v1";
/// The label of `effective_total_zone_capacity`, a kernel in Python too (`kernel_only`).
const CAPACITY_KERNEL: &str = "overlay.effective_total_zone_capacity";

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order, with the resource
/// service's config where the configs have one (`resource_config`). Python returns the pending
/// events, which the event rules read; here they read the queue again.
pub fn validate(view: &View<'_>, service: Option<&ServiceConfig>) -> Result<()> {
    let minute = view.minute_index()?;
    let duration = view.duration_minutes()?;
    if duration <= 0 || !(0..=duration).contains(&minute) {
        return invariant("world clock is outside the scenario duration");
    }
    // A seed of any size, past 64 bits too: `self.seed < 0`.
    if view.world.seed_text()?.starts_with('-') {
        return invariant("scenario seed cannot be negative");
    }
    let version = view.schema_version()?;
    let rows = view.rows()?;
    mapping_keys(&rows)?;
    edges(&rows)?;
    route_capacities(view, &rows)?;
    let occupancy = agents(&rows, version)?;
    zone_capacities(view, &occupancy)?;
    let (queued, active) = facilities(&rows, version)?;
    for (id, agent) in rows.agents.iter() {
        if agent.activity == Activity::Queued && !queued.contains(&**id) {
            return invariant(format!("queued Agent {} is missing from every facility queue", py_repr(id)));
        }
        if agent.activity == Activity::InService && !active.contains(&**id) {
            return invariant(format!("Agent {} in service is missing from every facility", py_repr(id)));
        }
    }
    let pending = pending_events(view)?;
    let events = Events::of(&pending);
    let tasks = Tasks::of(&rows, version, minute)?;
    tasks.toilets(&rows, &events)?;
    let reservations = tasks.resources(view, &rows, &events, service)?;
    resource_completions(view, &rows, &pending)?;
    if version == 6 {
        service_reservations(view, &reservations)?;
    }
    tasks.travel(&rows, &events)?;
    staff(&rows)?;
    requests(&rows, &tasks, &events, &pending, minute, duration)?;
    every_task(&rows, &events)?;
    if view.world.resources()?.values().any(|value| *value < 0) {
        return invariant("resource inventory must contain non-negative integers");
    }
    Ok(())
}

fn invariant<T>(text: impl Into<String>) -> Result<T> {
    Err(Error::Invariant(text.into()))
}

/// The rows of the pending events, in the queue's order (`event_queue.pending()`).
pub(super) fn pending_events(view: &View<'_>) -> Result<Vec<Arc<EventRow>>> {
    let queue = view.queue()?;
    queue
        .ordered()
        .iter()
        .map(|(_, serial)| queue.slot(*serial).row.clone().ok_or_else(|| Error::unsupported(crate::rules::tick_validation::NO_ROW)))
        .collect()
}

// ---- Keys, edges and routes -----------------------------------------------------------------------

fn mapping_keys(rows: &Entities) -> Result<()> {
    fn check<'a>(name: &str, items: impl Iterator<Item = (&'a Arc<str>, &'a str)>) -> Result<()> {
        for (key, id) in items {
            if key.is_empty() || **key != *id {
                return invariant(format!("{name} mapping key must match its stable ID"));
            }
        }
        Ok(())
    }
    check("agent", rows.agents.iter().map(|(key, row)| (key, &*row.profile.id)))?;
    check("zone", rows.zones.iter().map(|(key, row)| (key, &*row.id)))?;
    check("zone edge", rows.zone_edges.iter().map(|(key, row)| (key, &*row.id)))?;
    check("facility", rows.facilities.iter().map(|(key, row)| (key, &*row.id)))?;
    check("staff", rows.staff.iter().map(|(key, row)| (key, &*row.id)))?;
    check("active task", rows.active_tasks.iter().map(|(key, row)| (key, &*row.id)))
}

fn edges(rows: &Entities) -> Result<()> {
    for (_, edge) in rows.zone_edges.iter() {
        if !rows.zones.contains_key(&edge.from_zone_id) || !rows.zones.contains_key(&edge.to_zone_id) {
            return invariant(format!("zone edge {} references an unknown zone", py_repr(&edge.id)));
        }
        if edge.travel_minutes <= 0 || edge.from_zone_id == edge.to_zone_id {
            return invariant(format!("zone edge {} has invalid travel data", py_repr(&edge.id)));
        }
    }
    Ok(())
}

/// `model_configs["layout_routing"]`, which must be the exact assumption record.
fn route_capacities(view: &View<'_>, rows: &Entities) -> Result<()> {
    const INVALID: &str = "layout route capacity configuration is invalid";
    let Some(config) = view.config("layout_routing")? else {
        return Ok(());
    };
    let config = match &config {
        // `model_configs.get(...)` of a None value is None too.
        V::Null => return Ok(()),
        V::Map(config) => config,
        _ => return invariant(INVALID),
    };
    let keys = ["schema_version", "source_type", "source_id", "max_agents_in_transit_by_edge"];
    let fine = config.len() == keys.len()
        && keys.iter().all(|key| config.contains_key(*key))
        && config.get("schema_version").is_some_and(|value| py_eq(value, &V::Int(1)))
        && is_text(config.get("source_type"), "assumption")
        && is_text(config.get("source_id"), "ASM-A05-ROUTE-EDIT-V1")
        && match config.get("max_agents_in_transit_by_edge") {
            Some(V::Map(capacities)) => {
                capacities.keys().all(|edge| rows.zone_edges.contains_key(edge))
                    && capacities.values().all(|capacity| matches!(capacity, V::Int(number) if (1..=100).contains(number)))
            }
            _ => false,
        };
    if !fine {
        return invariant(INVALID);
    }
    Ok(())
}

// ---- Agents and zones -----------------------------------------------------------------------------

/// `validate_unit_interval` (`domain/models.py`): a NaN fails the range too.
fn unit(value: f64) -> bool {
    value.is_finite() && (0.0..=1.0).contains(&value)
}

/// `NeedState.validate()`: the first level outside 0..1.
fn need_levels(needs: &NeedRow) -> Option<&'static str> {
    [("toilet_urgency", needs.toilet_urgency), ("hunger", needs.hunger), ("fatigue", needs.fatigue), ("hydration_need", needs.hydration_need)]
        .into_iter()
        .find(|(_, value)| !unit(*value))
        .map(|(name, _)| name)
}

fn agent_rules(agent: &AgentRow, version: i64) -> Result<()> {
    let id = py_repr(&agent.profile.id);
    if let Some(name) = need_levels(&agent.needs) {
        return invariant(format!("Agent {id} has invalid need levels: {name} must be finite and between 0 and 1"));
    }
    if version == 5 && (agent.needs.hydration_need != 0.0 || agent.profile.initial_needs.hydration_need != 0.0) {
        return invariant("WorldState v5 requires hydration_need to be zero");
    }
    for (name, value) in [("stress", agent.stress), ("trust", agent.trust)] {
        if !unit(value) {
            return invariant(format!("Agent {id} has invalid {name}: {name} must be finite and between 0 and 1"));
        }
    }
    if agent.next_decision_at < 0 {
        return invariant(format!("Agent {id} has invalid next decision time"));
    }
    if agent.support_cooldown_until < 0 {
        return invariant(format!("Agent {id} has invalid support cooldown"));
    }
    Ok(())
}

/// The Agents' own rules, and how many stand in each zone, in the zones' order.
fn agents(rows: &Entities, version: i64) -> Result<IndexMap<Arc<str>, i64>> {
    let mut occupancy: IndexMap<Arc<str>, i64> = rows.zones.iter().map(|(id, _)| (id.clone(), 0)).collect();
    for (_, agent) in rows.agents.iter() {
        let Some(count) = occupancy.get_mut(&agent.zone_id) else {
            return invariant(format!("Agent {} references unknown zone {}", py_repr(&agent.profile.id), py_repr(&agent.zone_id)));
        };
        *count += 1;
        agent_rules(agent, version)?;
    }
    Ok(occupancy)
}

/// `_validate_zone_capacity` of each zone: its nominal capacity with the approved temporary spaces
/// and the physical rooms (`effective_total_zone_capacity`, a kernel of `rules/overlay.rs` in
/// Python too, whose refusal Python raises as a `KernelRefusal`).
fn zone_capacities(view: &View<'_>, occupancy: &IndexMap<Arc<str>, i64>) -> Result<()> {
    let held = |name: &str| -> Result<Held> { Ok(if view.contains(name)? { Held::Held } else { Held::Absent }) };
    let (spaces, rooms) = (held(SPACES)?, held(OVERLAY)?);
    let schema = overlay_schema(&*view.configs()?).map(Arc::new);
    let places = Places { spaces: &spaces, overlay: &rooms, schema: &schema };
    for (zone, count) in occupancy {
        let capacity = overlay::zone_capacity(view.world, &places, zone, Some(*count))
            .and_then(|found| found.map(|(_, found)| found.total()).transpose())
            .map_err(|error| Error::refused(CAPACITY_KERNEL, error))?;
        if capacity.is_some_and(|capacity| *count > capacity) {
            return invariant(format!("zone {} exceeds nominal plus approved temporary or physical-room capacity", py_repr(zone)));
        }
    }
    Ok(())
}

// ---- Facilities and who is in them ----------------------------------------------------------------

fn has_repeats(ids: &[Arc<str>]) -> bool {
    let distinct: FxHashSet<&str> = ids.iter().map(|id| &**id).collect();
    distinct.len() != ids.len()
}

fn is_resource(kind: &str) -> bool {
    kind == WATER_KIND || kind == MEAL_KIND
}

/// `"toilet" in kind.casefold()`.
fn is_toilet(kind: &str) -> bool {
    casefold(kind).contains("toilet")
}

fn holds(ids: &[Arc<str>], id: &str) -> bool {
    ids.iter().any(|item| &**item == id)
}

/// The facilities' rules: who waits and who is served, as Python's sets.
fn facilities(rows: &Entities, version: i64) -> Result<(FxHashSet<Arc<str>>, FxHashSet<Arc<str>>)> {
    for (_, facility) in rows.facilities.iter() {
        let id = py_repr(&facility.id);
        if !rows.zones.contains_key(&facility.zone_id) {
            return invariant(format!("facility {id} references an unknown zone"));
        }
        if facility.capacity < 0 || !(0..=facility.capacity).contains(&facility.working_units) {
            return invariant(format!("facility {id} has invalid capacity"));
        }
        // An outage can take units out of service while their users finish (#188).
        if facility.active_agent_ids.len() as i64 > facility.capacity {
            return invariant(format!("facility {id} is over capacity"));
        }
        if version == 5 && (!facility.resource_active_agent_ids.is_empty() || !facility.resource_queue_agent_ids.is_empty()) {
            return invariant("WorldState v5 cannot contain resource service occupancy");
        }
    }
    let mut queued: FxHashSet<Arc<str>> = FxHashSet::default();
    let mut active: FxHashSet<Arc<str>> = FxHashSet::default();
    let activity = |id: &str| rows.agents.get(id).map(|agent| agent.activity);
    let multiple = |id: &str| invariant(format!("Agent {} appears in multiple facility queues or services", py_repr(id)));
    for (_, facility) in rows.facilities.iter() {
        if has_repeats(&facility.queue_agent_ids) {
            return invariant(format!("facility {} has duplicate queue entries", py_repr(&facility.id)));
        }
        if has_repeats(&facility.active_agent_ids) {
            return invariant(format!("facility {} has duplicate active entries", py_repr(&facility.id)));
        }
        for id in &facility.queue_agent_ids {
            let Some(found) = activity(id) else {
                return invariant(format!("facility queue references unknown Agent {}", py_repr(id)));
            };
            if queued.contains(id) || active.contains(id) {
                return multiple(id);
            }
            if found != Activity::Queued {
                return invariant(format!("queued Agent {} must have queued activity", py_repr(id)));
            }
            queued.insert(id.clone());
        }
        for id in &facility.active_agent_ids {
            let Some(found) = activity(id) else {
                return invariant(format!("facility service references unknown Agent {}", py_repr(id)));
            };
            if active.contains(id) || queued.contains(id) {
                return multiple(id);
            }
            if found != Activity::InService {
                return invariant(format!("serviced Agent {} must have in_service activity", py_repr(id)));
            }
            active.insert(id.clone());
        }
    }
    if version == 5 {
        return Ok((queued, active));
    }
    for (_, facility) in rows.facilities.iter() {
        let id = py_repr(&facility.id);
        let resource = is_resource(&facility.kind);
        if resource && (!facility.queue_agent_ids.is_empty() || !facility.active_agent_ids.is_empty()) {
            return invariant(format!("resource facility {id} cannot use legacy facility queues"));
        }
        if !resource && (!facility.resource_active_agent_ids.is_empty() || !facility.resource_queue_agent_ids.is_empty()) {
            return invariant(format!("non-resource facility {id} cannot own resource service slots"));
        }
        if has_repeats(&facility.resource_queue_agent_ids) {
            return invariant(format!("facility {id} has duplicate resource queue entries"));
        }
        if has_repeats(&facility.resource_active_agent_ids) {
            return invariant(format!("facility {id} has duplicate resource active entries"));
        }
        if facility.resource_active_agent_ids.len() as i64 > facility.capacity {
            return invariant(format!("resource facility {id} is over its installed capacity"));
        }
        let at_facility = |agent: &AgentRow, wanted: Activity| {
            agent.activity == wanted && agent.target_id.as_deref() == Some(&*facility.id) && agent.zone_id == facility.zone_id
        };
        for agent_id in &facility.resource_queue_agent_ids {
            let Some(agent) = rows.agents.get(agent_id) else {
                return invariant(format!("resource queue references unknown Agent {}", py_repr(agent_id)));
            };
            if queued.contains(agent_id) || active.contains(agent_id) {
                return multiple(agent_id);
            }
            if !at_facility(agent, Activity::Queued) {
                return invariant(format!("resource queued Agent {} has inconsistent activity, target, or zone", py_repr(agent_id)));
            }
            queued.insert(agent_id.clone());
        }
        for agent_id in &facility.resource_active_agent_ids {
            let Some(agent) = rows.agents.get(agent_id) else {
                return invariant(format!("resource service references unknown Agent {}", py_repr(agent_id)));
            };
            if active.contains(agent_id) || queued.contains(agent_id) {
                return multiple(agent_id);
            }
            if !at_facility(agent, Activity::InService) {
                return invariant(format!("resource serviced Agent {} has inconsistent activity, target, or zone", py_repr(agent_id)));
            }
            active.insert(agent_id.clone());
        }
    }
    Ok((queued, active))
}

// ---- The events that serve tasks ------------------------------------------------------------------

/// The pending completion events by the task they name (a `task_id` that is a str), in the queue's
/// order: `completion_events_by_task`, `resource_completion_events_by_task`,
/// `travel_events_by_task` and `staff_events_by_task`.
struct Events<'a> {
    toilet: FxHashMap<&'a str, Vec<&'a EventRow>>,
    resource: FxHashMap<&'a str, Vec<&'a EventRow>>,
    travel: FxHashMap<&'a str, Vec<&'a EventRow>>,
    staff: FxHashMap<&'a str, Vec<&'a EventRow>>,
}

impl<'a> Events<'a> {
    fn of(pending: &'a [Arc<EventRow>]) -> Self {
        let mut events = Events { toilet: FxHashMap::default(), resource: FxHashMap::default(), travel: FxHashMap::default(), staff: FxHashMap::default() };
        for event in pending {
            let target = match &*event.event_type {
                "toilet_service_complete" => &mut events.toilet,
                "resource_service_complete" => &mut events.resource,
                "agent_travel_complete" => &mut events.travel,
                "staff_support_complete" => &mut events.staff,
                _ => continue,
            };
            if let Some(V::Str(task_id)) = get(&event.payload, "task_id") {
                target.entry(&**task_id).or_default().push(event);
            }
        }
        events
    }
}

fn named<'m, 'a>(map: &'m FxHashMap<&'a str, Vec<&'a EventRow>>, task_id: &str) -> &'m [&'a EventRow] {
    map.get(task_id).map_or(&[], Vec::as_slice)
}

// ---- Tasks ----------------------------------------------------------------------------------------

type Pair<'a> = (&'a str, &'a str);

/// The tasks of each kind as Python's dicts hold them, in the tasks' order: by (facility, Agent),
/// and the travel tasks by Agent (`travel_tasks`).
struct Tasks<'a> {
    toilet_queue: IndexMap<Pair<'a>, &'a TaskRow>,
    toilet_service: IndexMap<Pair<'a>, &'a TaskRow>,
    resource_queue: IndexMap<Pair<'a>, &'a TaskRow>,
    resource_service: IndexMap<Pair<'a>, &'a TaskRow>,
    travel: IndexMap<&'a str, &'a TaskRow>,
}

impl<'a> Tasks<'a> {
    fn of(rows: &'a Entities, version: i64, minute: i64) -> Result<Self> {
        let mut tasks = Tasks {
            toilet_queue: IndexMap::new(),
            toilet_service: IndexMap::new(),
            resource_queue: IndexMap::new(),
            resource_service: IndexMap::new(),
            travel: IndexMap::new(),
        };
        for (_, task) in rows.active_tasks.iter() {
            let id = py_repr(&task.id);
            match &*task.kind {
                "travel_to_toilet" | "travel_to_resource" | "travel_return" | "staff_support_travel" => {
                    let (Some(agent), None) = (task.agent_id.as_deref(), &task.resource_id) else {
                        return invariant(format!("travel task {id} has invalid references"));
                    };
                    if task.start_minute > minute || task.due_minute <= task.start_minute {
                        return invariant(format!("travel task {id} has invalid timing"));
                    }
                    if tasks.travel.contains_key(agent) {
                        return invariant(format!("Agent {} has duplicate travel tasks", py_repr(agent)));
                    }
                    tasks.travel.insert(agent, task);
                }
                kind @ ("resource_service_queue" | "resource_service_water" | "resource_service_food") => {
                    if version != 6 {
                        return invariant("resource service tasks require WorldState v6");
                    }
                    let (Some(agent), Some(resource)) = (task.agent_id.as_deref(), task.resource_id.as_deref()) else {
                        return invariant(format!("resource service task {id} needs Agent and facility IDs"));
                    };
                    if !rows.facilities.get(resource).is_some_and(|facility| is_resource(&facility.kind)) {
                        return invariant(format!("resource service task {id} references a non-resource facility"));
                    }
                    if task.start_minute > minute {
                        return invariant(format!("resource service task {id} starts in future"));
                    }
                    let queue = kind == "resource_service_queue";
                    if queue && (task.due_minute != task.start_minute || task.staff_id.is_some()) {
                        return invariant(format!("resource queue task {id} has invalid timing or staff"));
                    }
                    if !queue && (task.due_minute <= task.start_minute || task.staff_id.is_none()) {
                        return invariant(format!("resource service task {id} has invalid timing or staff"));
                    }
                    let target = if queue { &mut tasks.resource_queue } else { &mut tasks.resource_service };
                    if target.contains_key(&(resource, agent)) {
                        return invariant(format!("Agent {} has duplicate resource service tasks", py_repr(agent)));
                    }
                    target.insert((resource, agent), task);
                }
                kind @ ("toilet_queue" | "toilet_service") => {
                    let (Some(agent), Some(resource)) = (task.agent_id.as_deref(), task.resource_id.as_deref()) else {
                        return invariant(format!("toilet task {id} needs Agent and facility IDs"));
                    };
                    if !rows.facilities.get(resource).is_some_and(|facility| is_toilet(&facility.kind)) {
                        return invariant(format!("toilet task {id} references a non-toilet facility"));
                    }
                    if task.start_minute > minute {
                        return invariant(format!("toilet task {id} starts in the future"));
                    }
                    let queue = kind == "toilet_queue";
                    if queue && task.due_minute != task.start_minute {
                        return invariant(format!("toilet queue task {id} has invalid timing"));
                    }
                    if !queue && task.due_minute <= task.start_minute {
                        return invariant(format!("toilet service task {id} has invalid timing"));
                    }
                    let target = if queue { &mut tasks.toilet_queue } else { &mut tasks.toilet_service };
                    if target.contains_key(&(resource, agent)) {
                        return invariant(format!("Agent {} has duplicate toilet tasks", py_repr(agent)));
                    }
                    target.insert((resource, agent), task);
                }
                _ => {}
            }
        }
        Ok(tasks)
    }

    /// The toilets' queues and services, and the tasks and completion events that hold them.
    fn toilets(&self, rows: &Entities, events: &Events<'_>) -> Result<()> {
        let targets = |agent_id: &str, facility_id: &str| rows.agents.get(agent_id).is_some_and(|agent| agent.target_id.as_deref() == Some(facility_id));
        for (_, facility) in rows.facilities.iter() {
            if !is_toilet(&facility.kind) {
                continue;
            }
            for agent_id in &facility.queue_agent_ids {
                if !self.toilet_queue.contains_key(&(&*facility.id, &**agent_id)) || !targets(agent_id, &facility.id) {
                    return invariant(format!("queued Agent {} needs one matching toilet queue task", py_repr(agent_id)));
                }
            }
            for agent_id in &facility.active_agent_ids {
                if !self.toilet_service.contains_key(&(&*facility.id, &**agent_id)) || !targets(agent_id, &facility.id) {
                    return invariant(format!("serviced Agent {} needs one matching toilet service task", py_repr(agent_id)));
                }
            }
        }
        let activity = |agent_id: &str| rows.agents.get(agent_id).map(|agent| agent.activity);
        for ((facility_id, agent_id), task) in &self.toilet_queue {
            if !rows.facilities.get(facility_id).is_some_and(|facility| holds(&facility.queue_agent_ids, agent_id)) {
                return invariant(format!("toilet queue task {} has no queue entry", py_repr(&task.id)));
            }
            if activity(agent_id) != Some(Activity::Queued) {
                return invariant(format!("toilet queue task {} has a non-queued Agent", py_repr(&task.id)));
            }
        }
        for ((facility_id, agent_id), task) in &self.toilet_service {
            let id = py_repr(&task.id);
            if !rows.facilities.get(facility_id).is_some_and(|facility| holds(&facility.active_agent_ids, agent_id)) {
                return invariant(format!("toilet service task {id} has no occupied slot"));
            }
            if activity(agent_id) != Some(Activity::InService) {
                return invariant(format!("toilet service task {id} has a non-serviced Agent"));
            }
            let matches = match named(&events.toilet, &task.id) {
                [event] => {
                    event.kind == EventKind::Completion
                        && event.due_minute == task.due_minute
                        && is_text(get(&event.payload, "facility_id"), facility_id)
                        && is_text(get(&event.payload, "agent_id"), agent_id)
                        // `payload.get("staff_id", task.staff_id) != task.staff_id`.
                        && get(&event.payload, "staff_id").is_none_or(|staff| same(Some(staff), task.staff_id.as_deref()))
                }
                _ => false,
            };
            if !matches {
                return invariant(format!("toilet service task {id} needs one matching completion event"));
            }
        }
        Ok(())
    }

    /// The resource facilities' queues and services, and the tasks, completion events, ledger
    /// reservations and staff that hold them: the reservations of the services
    /// (`active_resource_reservations`).
    fn resources(&self, view: &View<'_>, rows: &Entities, events: &Events<'_>, service: Option<&ServiceConfig>) -> Result<FxHashSet<Arc<str>>> {
        let targets = |agent_id: &str, facility_id: &str| rows.agents.get(agent_id).is_some_and(|agent| agent.target_id.as_deref() == Some(facility_id));
        for (_, facility) in rows.facilities.iter() {
            if !is_resource(&facility.kind) {
                continue;
            }
            for agent_id in &facility.resource_queue_agent_ids {
                if !self.resource_queue.contains_key(&(&*facility.id, &**agent_id)) || !targets(agent_id, &facility.id) {
                    return invariant(format!("resource queued Agent {} needs one matching queue task", py_repr(agent_id)));
                }
            }
            for agent_id in &facility.resource_active_agent_ids {
                if !self.resource_service.contains_key(&(&*facility.id, &**agent_id)) || !targets(agent_id, &facility.id) {
                    return invariant(format!("resource serviced Agent {} needs one matching service task", py_repr(agent_id)));
                }
            }
        }
        let activity = |agent_id: &str| rows.agents.get(agent_id).map(|agent| agent.activity);
        for ((facility_id, agent_id), task) in &self.resource_queue {
            if !rows.facilities.get(facility_id).is_some_and(|facility| holds(&facility.resource_queue_agent_ids, agent_id)) {
                return invariant(format!("resource queue task {} has no matching queue entry", py_repr(&task.id)));
            }
            if activity(agent_id) != Some(Activity::Queued) {
                return invariant(format!("resource queue task {} has a non-queued Agent", py_repr(&task.id)));
            }
        }
        let mut reservations: FxHashSet<Arc<str>> = FxHashSet::default();
        for ((facility_id, agent_id), task) in &self.resource_service {
            let id = py_repr(&task.id);
            let Some(facility) = rows.facilities.get(facility_id).filter(|facility| holds(&facility.resource_active_agent_ids, agent_id)) else {
                return invariant(format!("resource service task {id} has no occupied facility slot"));
            };
            if activity(agent_id) != Some(Activity::InService) {
                return invariant(format!("resource service task {id} has a non-serviced Agent"));
            }
            let water = &*facility.kind == WATER_KIND;
            let resource_id = if water { "water_ml" } else { "food_meal_units" };
            let completion = match named(&events.resource, &task.id) {
                [event]
                    if event.due_minute == task.due_minute
                        && is_text(get(&event.payload, "facility_id"), facility_id)
                        && is_text(get(&event.payload, "agent_id"), agent_id)
                        && is_text(get(&event.payload, "resource_id"), resource_id)
                        && is_text(get(&event.payload, "reservation_id"), &task.id)
                        && same(get(&event.payload, "staff_id"), task.staff_id.as_deref()) =>
                {
                    *event
                }
                _ => return invariant(format!("resource service task {id} needs one matching completion event")),
            };
            let reservation = ledger_reservation(view, water, &task.id)?;
            // A v6 world has the config: `WorldState.validate` refused one without it before.
            let Some(config) = service else {
                return Err(Error::unsupported("a resource service task without the resource service config"));
            };
            let quantity = V::Int(if water { config.water_ml_per_service } else { config.food_meal_units_per_service });
            let reserved = reservation.is_some_and(|(agent, resource, amount)| {
                agent.as_deref() == Some(*agent_id) && resource.as_deref() == Some(resource_id) && amount.is_some_and(|amount| py_eq(&amount, &quantity))
            });
            if !reserved || completion.kind != EventKind::Completion || !get(&completion.payload, "quantity").is_some_and(|value| py_eq(value, &quantity)) {
                return invariant(format!("resource service task {id} needs one matching ledger reservation"));
            }
            reservations.insert(task.id.clone());
            let skill = if water { &*config.water_staff_skill } else { &*config.meal_staff_skill };
            let owner = rows.staff.get(task.staff_id.as_deref().unwrap_or("")).is_some_and(|member| {
                member.active_task_id.as_deref() == Some(&*task.id)
                    && member.work_queue.len() == 1
                    && member.work_queue[0] == task.id
                    && member.busy_until == task.due_minute
                    && member.zone_id == facility.zone_id
                    && member.skills.0.iter().any(|item| &**item == skill)
            });
            if !owner {
                return invariant(format!("resource service task {id} needs one matching idle-skilled staff owner"));
            }
        }
        Ok(reservations)
    }

    /// The travel tasks, their Agents and completion events, and the moving Agents.
    fn travel(&self, rows: &Entities, events: &Events<'_>) -> Result<()> {
        for (agent_id, task) in &self.travel {
            let id = py_repr(&task.id);
            let Some(agent) = rows.agents.get(agent_id).filter(|agent| agent.activity == Activity::Moving) else {
                return invariant(format!("travel task {id} needs a moving Agent"));
            };
            let event = match named(&events.travel, &task.id) {
                [event] if event.due_minute == task.due_minute => *event,
                _ => return invariant(format!("travel task {id} needs one matching completion event")),
            };
            let destination = get(&event.payload, "destination_zone_id");
            let purpose = get(&event.payload, "purpose");
            let target = if is_text(purpose, "toilet_request") || is_text(purpose, "resource_request") {
                get(&event.payload, "facility_id")
            } else {
                destination
            };
            if !in_table(destination, |key| rows.zones.contains_key(key))? || !same(target, agent.target_id.as_deref()) {
                return invariant(format!("travel task {id} has an invalid destination"));
            }
            if &*task.kind == "staff_support_travel"
                && (!is_text(purpose, "staff_support_travel")
                    || !in_table(get(&event.payload, "request_id"), |key| rows.support_requests.contains_key(key))?
                    || !in_table(get(&event.payload, "staff_id"), |key| rows.staff.contains_key(key))?)
            {
                return invariant(format!("staff support travel task {id} has invalid request ownership"));
            }
        }
        for (id, agent) in rows.agents.iter() {
            if agent.activity == Activity::Moving && !self.travel.contains_key(&**id) {
                return invariant(format!("moving Agent {} is missing a scheduled travel task", py_repr(id)));
            }
        }
        Ok(())
    }
}

/// The pending resource completions: each names its task, active or cancelled.
fn resource_completions(view: &View<'_>, rows: &Entities, pending: &[Arc<EventRow>]) -> Result<()> {
    for event in pending {
        if &*event.event_type != "resource_service_complete" {
            continue;
        }
        let Some(V::Str(task_id)) = get(&event.payload, "task_id") else {
            return invariant("resource completion event requires a task ID");
        };
        if rows.active_tasks.contains_key(task_id) {
            continue;
        }
        let cancelled = match view.row::<ServiceRow>(SERVICE)? {
            Some(row) => holds(&row.cancelled_task_ids, task_id),
            None => contains(&state_part(view, SERVICE, "cancelled_task_ids")?, task_id)?,
        };
        if !cancelled {
            return invariant(format!("resource completion event references inactive task {}", py_repr(task_id)));
        }
    }
    Ok(())
}

/// The service state's active reservations: those of the active resource services, each in the
/// ledger.
fn service_reservations(view: &View<'_>, active: &FxHashSet<Arc<str>>) -> Result<()> {
    let mut ledger: FxHashSet<Arc<str>> = FxHashSet::default();
    if let Some(row) = view.row::<LedgerRow>(LEDGER)? {
        for index in 0..2 {
            ledger.extend(row.accounts.of(index).reservations.keys().cloned());
        }
    } else {
        let accounts = state_part(view, LEDGER, "accounts")?;
        for resource_id in ["water_ml", "food_meal_units"] {
            for item in iterate(index(index(&accounts, resource_id)?, "reservations")?)? {
                state_hashable(&item)?;
                if let V::Str(id) = item {
                    ledger.insert(id);
                }
            }
        }
    }
    let mut service: FxHashSet<Arc<str>> = FxHashSet::default();
    let mut others = false;
    if let Some(row) = view.row::<ServiceRow>(SERVICE)? {
        service.extend(row.active_reservation_ids.iter().cloned());
    } else {
        for item in iterate(&state_part(view, SERVICE, "active_reservation_ids")?)? {
            state_hashable(&item)?;
            match item {
                V::Str(id) => {
                    service.insert(id);
                }
                _ => others = true,
            }
        }
    }
    if others || service != *active {
        return invariant("active service reservations must match active resource tasks");
    }
    if !service.is_subset(&ledger) {
        return invariant("active service reservation IDs must exist in the resource ledger");
    }
    Ok(())
}

// ---- Staff, support requests and every task --------------------------------------------------------

fn staff(rows: &Entities) -> Result<()> {
    let mut assigned: FxHashSet<&str> = FxHashSet::default();
    for (_, member) in rows.staff.iter() {
        let id = py_repr(&member.id);
        if !rows.zones.contains_key(&member.zone_id) {
            return invariant(format!("staff {id} references an unknown zone"));
        }
        if has_repeats(&member.work_queue) {
            return invariant(format!("staff {id} has duplicate work entries"));
        }
        if let Some(unknown) = member.work_queue.iter().filter(|task| !rows.active_tasks.contains_key(task)).min() {
            return invariant(format!("staff queue references unknown task {}", py_repr(unknown)));
        }
        if let Some(repeated) = member.work_queue.iter().filter(|task| assigned.contains(&***task)).min() {
            return invariant(format!("task {} is assigned to multiple staff queues", py_repr(repeated)));
        }
        assigned.extend(member.work_queue.iter().map(|task| &**task));
        if member.busy_until < 0 {
            return invariant(format!("staff {id} has invalid busy_until"));
        }
        match member.active_task_id.as_deref() {
            None if !member.work_queue.is_empty() || member.busy_until != 0 => {
                return invariant(format!("idle staff {id} cannot retain active work"));
            }
            Some(task) if !(member.work_queue.len() == 1 && &*member.work_queue[0] == task) => {
                return invariant(format!("staff {id} must have exactly one active work item"));
            }
            _ => {}
        }
    }
    Ok(())
}

const WAIT_REASONS: [&str; 4] = ["no_available_qualified_staff", "route_unavailable_at_assignment", "insufficient_run_time", "resident_travel_schedule_rejected"];

fn requests(rows: &Entities, tasks: &Tasks<'_>, events: &Events<'_>, pending: &[Arc<EventRow>], minute: i64, duration: i64) -> Result<()> {
    // The open (pending or assigned) requests of each Agent, in the requests' order.
    let mut open: FxHashMap<&str, Vec<&str>> = FxHashMap::default();
    // `timeouts_by_request`, made at the first pending request.
    let mut timeouts: Option<FxHashMap<&str, Vec<&EventRow>>> = None;
    for (_, request) in rows.support_requests.iter() {
        let id = py_repr(&request.id);
        if request.id.is_empty() || !rows.support_requests.contains_key(&request.id) {
            return invariant("support request mapping key must match its stable ID");
        }
        if !rows.agents.contains_key(&request.agent_id) {
            return invariant(format!("support request {id} references an unknown Agent"));
        }
        if !(0..=minute).contains(&request.requested_minute) || request.timeout_minute < request.requested_minute || request.cooldown_until < 0 {
            return invariant(format!("support request {id} has invalid timing"));
        }
        let attempt = request.last_assignment_attempt_minute;
        if attempt.is_some_and(|attempt| !(request.requested_minute..=minute).contains(&attempt)) {
            return invariant(format!("support request {id} has invalid assignment-attempt timing"));
        }
        if let Some(reason) = request.last_assignment_wait_reason_code.as_deref() {
            if !WAIT_REASONS.contains(&reason) || attempt.is_none() {
                return invariant(format!("support request {id} has invalid assignment-wait reason"));
            }
        }
        let agent_id = &*request.agent_id;
        let status = request.status;
        if matches!(status, RequestStatus::Pending | RequestStatus::Assigned) {
            open.entry(agent_id).or_default().push(&request.id);
        }
        if status == RequestStatus::Assigned {
            let member = rows.staff.get(request.assigned_staff_id.as_deref().unwrap_or(""));
            let task = rows.active_tasks.get(request.task_id.as_deref().unwrap_or(""));
            let (Some(member), Some(task), Some(agent)) = (member, task, rows.agents.get(agent_id)) else {
                return invariant(format!("assigned support request {id} lacks one matching staff task"));
            };
            if &*task.kind != "staff_support"
                || task.staff_id.as_deref() != Some(&*member.id)
                || task.agent_id.as_deref() != Some(agent_id)
                || member.active_task_id.as_deref() != Some(&*task.id)
                || task.due_minute != member.busy_until
            {
                return invariant(format!("assigned support request {id} lacks one matching staff task"));
            }
            let travel = tasks.travel.get(agent_id);
            if agent.activity == Activity::Moving {
                let right = match travel.map(|trip| (trip, named(&events.travel, &trip.id))) {
                    Some((trip, [event])) => {
                        &*trip.kind == "staff_support_travel"
                            && is_text(get(&event.payload, "request_id"), &request.id)
                            && is_text(get(&event.payload, "staff_id"), &member.id)
                            && is_text(get(&event.payload, "destination_zone_id"), &member.zone_id)
                    }
                    _ => false,
                };
                if !right {
                    return invariant(format!("assigned support request {id} has invalid resident travel"));
                }
            } else if agent.activity != Activity::SeekingHelp || travel.is_some_and(|trip| &*trip.kind == "staff_support_travel") {
                return invariant(format!("assigned support request {id} must seek help or travel to staff"));
            }
            let right = match named(&events.staff, request.task_id.as_deref().unwrap_or("")) {
                [event] => {
                    event.due_minute == task.due_minute
                        && is_text(get(&event.payload, "request_id"), &request.id)
                        && is_text(get(&event.payload, "agent_id"), agent_id)
                        && is_text(get(&event.payload, "staff_id"), &member.id)
                }
                _ => false,
            };
            if !right {
                return invariant(format!("assigned support request {id} needs one completion event"));
            }
        } else if (request.assigned_staff_id.is_some() || request.task_id.is_some()) && !matches!(status, RequestStatus::Resolved | RequestStatus::Unresolved) {
            return invariant(format!("support request {id} has assignment without assigned status"));
        }
        if status == RequestStatus::Pending {
            if timeouts.is_none() {
                let mut found: FxHashMap<&str, Vec<&EventRow>> = FxHashMap::default();
                for event in pending.iter().filter(|event| &*event.event_type == "support_request_timeout") {
                    let key = get(&event.payload, "request_id");
                    if let Some(key) = key {
                        payload_hashable(key)?;
                    }
                    if let Some(V::Str(key)) = key {
                        found.entry(&**key).or_default().push(&**event);
                    }
                }
                timeouts = Some(found);
            }
            let expirations = timeouts.as_ref().and_then(|found| found.get(&*request.id)).map_or(&[][..], Vec::as_slice);
            let one = matches!(expirations, [event] if event.due_minute == request.timeout_minute);
            if request.timeout_minute < duration && !one {
                return invariant(format!("pending support request {id} needs one timeout event"));
            }
        }
    }
    for (id, agent) in rows.agents.iter() {
        let active = open.get(&**id).map_or(&[][..], Vec::as_slice);
        if active.len() > 1 {
            return invariant(format!("Agent {} has duplicate active support requests", py_repr(id)));
        }
        if let [request] = active {
            if agent.support_request_id.as_deref() != Some(*request) {
                return invariant(format!("Agent {} does not reference its active support request", py_repr(id)));
            }
            if !matches!(agent.activity, Activity::SeekingHelp | Activity::Moving) {
                return invariant(format!("Agent {} with active support must seek help or travel to staff", py_repr(id)));
            }
        } else if agent.support_request_id.is_some() {
            return invariant(format!("Agent {} references a non-active support request", py_repr(id)));
        }
    }
    Ok(())
}

fn every_task(rows: &Entities, events: &Events<'_>) -> Result<()> {
    for (_, task) in rows.active_tasks.iter() {
        let id = py_repr(&task.id);
        if task.agent_id.as_deref().is_some_and(|agent| !rows.agents.contains_key(agent)) {
            return invariant(format!("task {id} references an unknown Agent"));
        }
        if &*task.kind != "staff_support" && task.resource_id.as_deref().is_some_and(|resource| !rows.facilities.contains_key(resource)) {
            return invariant(format!("task {id} references an unknown facility"));
        }
        if &*task.kind == "staff_support" {
            let right = task.agent_id.is_some()
                && rows
                    .staff
                    .get(task.staff_id.as_deref().unwrap_or(""))
                    .is_some_and(|member| member.active_task_id.as_deref() == Some(&*task.id) && member.busy_until == task.due_minute)
                && task.resource_id.is_none()
                && task.due_minute > task.start_minute
                && matches!(named(&events.staff, &task.id), [event] if event.due_minute == task.due_minute);
            if !right {
                return invariant(format!("staff support task {id} is inconsistent"));
            }
        }
        if task.start_minute < 0 || task.due_minute < task.start_minute {
            return invariant(format!("task {id} has invalid timing"));
        }
    }
    Ok(())
}

// ---- Values read as Python reads them --------------------------------------------------------------

/// `type(value).__name__` of the value a `V` stands for.
pub(super) fn type_name(value: &V) -> &'static str {
    match value {
        V::Null => "NoneType",
        V::Bool(_) => "bool",
        V::Int(_) | V::Big(_) => "int",
        V::Float(_) => "float",
        V::Str(_) => "str",
        V::List(_) => "list",
        V::Map(_) => "dict",
    }
}

/// What the resource rules read of a ledger reservation:
/// `model_state["resource_ledger_v1"]["accounts"][resource_id]["reservations"].get(task_id)`, as
/// its `agent_id`, `resource_id` and `quantity` (each None where it is missing or another value
/// than the rules compare with), or None where it is not a dict. The ledger's typed row is read
/// directly, without writing its histories as `state` would.
fn ledger_reservation(view: &View<'_>, water: bool, task_id: &str) -> Result<Option<(Option<Arc<str>>, Option<Arc<str>>, Option<V>)>> {
    if let Some(row) = view.row::<LedgerRow>(LEDGER)? {
        let found = row.accounts.of(usize::from(!water)).reservations.get(task_id);
        return Ok(found.map(|item| (Some(item.agent_id.clone()), Some(item.resource_id.clone()), Some(V::Int(item.quantity)))));
    }
    let accounts = state_part(view, LEDGER, "accounts")?;
    let resource_id = if water { "water_ml" } else { "food_meal_units" };
    let text = |value: Option<&V>| match value {
        Some(V::Str(text)) => Some(text.clone()),
        _ => None,
    };
    Ok(match get_of(index(index(&accounts, resource_id)?, "reservations")?, task_id)? {
        Some(V::Map(item)) => Some((text(item.get("agent_id")), text(item.get("resource_id")), item.get("quantity").cloned())),
        _ => None,
    })
}

/// `world.model_state[name][key]` (`_state_part` of `core/world.py`), copied: a `KeyError` for a
/// missing namespace or key, a `TypeError` where the namespace is no dict.
fn state_part(view: &View<'_>, name: &str, key: &str) -> Result<V> {
    let Some(state) = view.state(name)? else {
        return Err(Error::Key(name.to_owned()));
    };
    index(&state, key).cloned()
}

/// `value[key]` for a str key.
pub(super) fn index<'a>(value: &'a V, key: &str) -> Result<&'a V> {
    match value {
        V::Map(map) => map.get(key).ok_or_else(|| Error::Key(key.to_owned())),
        V::List(_) => Err(Error::Type("list indices must be integers or slices, not str".to_owned())),
        V::Str(_) => Err(Error::Type("string indices must be integers, not 'str'".to_owned())),
        other => Err(Error::Type(format!("'{}' object is not subscriptable", type_name(other)))),
    }
}

/// `value.get(key)` of a dict of `model_state`. Python raises an `AttributeError` for another
/// value, which the core has no class for: a `RuntimeError` with Python's text stands for it, which
/// no `except` of the validation catches either.
pub(super) fn get_of<'a>(value: &'a V, key: &str) -> Result<Option<&'a V>> {
    let name = match value {
        V::Map(map) => return Ok(map.get(key)),
        V::List(_) => "TrackedList",
        other => type_name(other),
    };
    Err(Error::Runtime(format!("'{name}' object has no attribute 'get'")))
}

/// `payload.get(key)` of a mapping: None for a missing key (or a payload that is no mapping).
pub(super) fn get<'a>(payload: &'a V, key: &str) -> Option<&'a V> {
    match payload {
        V::Map(map) => map.get(key),
        _ => None,
    }
}

/// `value == text`, where a missing value is None.
pub(super) fn is_text(value: Option<&V>, text: &str) -> bool {
    matches!(value, Some(V::Str(found)) if &**found == text)
}

/// `value == expected`, where a missing value and None are None.
pub(super) fn same(value: Option<&V>, expected: Option<&str>) -> bool {
    match (value, expected) {
        (None | Some(V::Null), None) => true,
        (Some(V::Str(found)), Some(text)) => &**found == text,
        _ => false,
    }
}

fn unhashable<T>(name: &str) -> Result<T> {
    Err(Error::Type(format!("unhashable type: '{name}'")))
}

/// `hash(value)` of a value of an event's payload, which the event holds frozen: a list is a tuple,
/// hashable when its items are, and a dict a mappingproxy, whose hash is its dict's.
pub(super) fn payload_hashable(value: &V) -> Result<()> {
    match value {
        V::List(items) => items.iter().try_for_each(payload_hashable),
        V::Map(_) => unhashable("dict"),
        _ => Ok(()),
    }
}

/// `hash(value)` of a value of `model_state`, whose lists and dicts are the state's tracked ones.
pub(super) fn state_hashable(value: &V) -> Result<()> {
    match value {
        V::List(_) => unhashable("TrackedList"),
        V::Map(_) => unhashable("TrackedDict"),
        _ => Ok(()),
    }
}

/// `value in table` of a payload's value, where the table is a dict of str keys: a str is looked
/// up, an unhashable value raises, and another value (or a missing one, None) is in no such table.
pub(super) fn in_table(value: Option<&V>, has: impl Fn(&str) -> bool) -> Result<bool> {
    match value {
        Some(V::Str(key)) => Ok(has(key)),
        Some(other) => payload_hashable(other).map(|()| false),
        None => Ok(false),
    }
}

/// `needle in value` for a str needle.
pub(super) fn contains(value: &V, needle: &str) -> Result<bool> {
    match value {
        V::Map(map) => Ok(map.contains_key(needle)),
        V::List(items) => Ok(items.iter().any(|item| is_text(Some(item), needle))),
        V::Str(text) => Ok(text.contains(needle)),
        other => Err(Error::Type(format!("argument of type '{}' is not iterable", type_name(other)))),
    }
}

/// `iter(value)`: a dict's keys, a list's items, a str's characters.
pub(super) fn iterate(value: &V) -> Result<Vec<V>> {
    match value {
        V::Map(map) => Ok(map.keys().map(|key| V::Str(key.clone())).collect()),
        V::List(items) => Ok(items.clone()),
        V::Str(text) => Ok(text.chars().map(|char| V::Str(Arc::from(char.to_string()))).collect()),
        other => Err(Error::Type(format!("'{}' object is not iterable", type_name(other)))),
    }
}
