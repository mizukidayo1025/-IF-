//! The physical queues of a facility: `prepare_facility_queue` (`domain/queueing.py`) and what
//! `domain/movement.py` does for them (lane c1 of Issue #152). The Python rules are gone
//! (`kernel_only`): the kernels raise the rules' ValueError texts themselves
//! (docs/rust-port/lanes/m4-queues.md).
//!
//! Each kernel is one call on the world borrowed whole: it plans, with nothing changed, and then
//! writes the rows (the entities, `movement_v1`, `facility_queues_v1`, `resource_service_v1`,
//! `metric_state`, the event queue and log). The trees of attached namespaces follow the rows
//! when the PyO3 crate gives the world back (lane P8-0c). The caller's loop is Python, and nobody
//! else moves between its calls, so the order of the people served is the order of those calls, as
//! in the Python rules.
//!
//! What the PyO3 crate hands in: the queue policy and the movement model of the configs, the floor
//! plan (`Geo`), whether the Python world holds the namespaces and `metric_state` (`Held`), and the
//! `repr` of a facility kind the rule turns down.
//!
//! What goes to Python, before anything is written: anything not plainly what the Python rule accepts.

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use indexmap::IndexMap;

use crate::config::movement::MovementModel;
use crate::config::queues::{QueuePolicy as Policy, RETRY_DELAY};
use crate::config::read::Read;
use crate::entities::{Activity, Entities, FacilityRow, NeedRow, TaskRow};
use crate::error::{Error, Result};
use crate::events::writer::{WriteError, WriteResult};
use crate::events::{EventKind, EventRow, EventStatus};
use crate::geometry::{distance, polyline_length, usable, Geo, Pt};
use crate::mathx::{py_max, py_min};
use crate::rules::facility_queues::{self as fifo, point_of, request_of_task};
use crate::rules::kit::movement::{
    approach_point, is_queue_trip, plan_start, position_point, queue_slot_in, refused, room_minutes, Changes, EXACT_INT, KEY,
    MOVEMENT_SOURCE_ID, POLICY_VERSION,
};
use crate::rules::kit::{add_metric, held_row, is_toilet, mobility_text, set_agent, Events, Held, WRITING_REFUSED};
use crate::state::facility_queues::{QueueRequest, QueuesRow};
use crate::state::movement::{Journey, MovementRow, Point};
use crate::state::resource_service::{ServiceRow, NAME as SERVICE_KEY};
use crate::value::V;
use crate::world::{World, METRIC_STATE};

pub const BLOCKED_METRIC: &str = "physical_queue_route_blocked_count";
/// `priority`: the most a wait adds, over the minutes of a day.
const AGING_WEIGHT: f64 = 0.1;
const AGING_MINUTES: f64 = 1440.0;
/// How near a person counts as standing in their slot, in metres.
const SLOT_TOLERANCE: f64 = 0.05;
/// `NeedWeightedPhysicalQueuePolicy.model_id`, `source_id`.
const NEED_WEIGHTED_ID: &str = "need-weighted-physical-queue-v1";
const NEED_WEIGHTED_SOURCE: &str = "ASM-NEED-WEIGHTED-PHYSICAL-QUEUE-V1";
const ABANDON_DETAIL: &str = "Agent left a prolonged low-urgency queue and will reconsider after a bounded delay";

fn text(value: &str) -> V {
    V::Str(Arc::from(value))
}

/// The independent FIFO a call is about, read when the call starts.
pub struct Fifo {
    pub queue_id: Arc<str>,
    pub service_point_id: Arc<str>,
    pub members: Vec<Arc<str>>,
    pub signed_point: Option<Pt>,
    pub requests: Vec<QueueRequest>,
    pub queued_request_ids: Vec<Arc<str>>,
}

impl Fifo {
    /// The queue of the row. A queue it doesn't have is the Python rule's error.
    pub fn of(row: &QueuesRow, geo: &Geo, queue_id: &str) -> Result<Self> {
        let Some(queue) = row.queues.get(queue_id) else {
            return Err(Error::unsupported("no such queue"));
        };
        Ok(Self {
            queue_id: Arc::from(queue_id),
            service_point_id: queue.service_point_id.clone(),
            members: fifo::members_of(row, queue_id)?,
            signed_point: point_of(geo, queue_id).map(|point| point.point),
            requests: row.requests.values().filter(|request| &*request.queue_id == queue_id).cloned().collect(),
            queued_request_ids: queue.queued_request_ids.clone(),
        })
    }

    /// The queue of a kernel's `queue_id`: a queue ID without the namespace is refused.
    pub fn read(world: &World, held: &Held, geo: &Geo, queue_id: &str) -> Result<Self> {
        match fifo::queues(world, held)? {
            Some(row) => Self::of(&row, geo, queue_id),
            None => Err(Error::unsupported("a queue ID without the queues")),
        }
    }

    /// `request_for_agent` of this queue.
    pub fn request_of(&self, agent_id: &str) -> Option<&QueueRequest> {
        self.requests.iter().find(|request| &*request.agent_id == agent_id)
    }

    /// The people and the signed point `queue_slot_in` reads.
    pub fn slot_view(&self) -> (&[Arc<str>], Option<Pt>) {
        (&self.members, self.signed_point)
    }
}

fn members(facility: &FacilityRow, resource: bool) -> &Vec<Arc<str>> {
    if resource {
        &facility.resource_queue_agent_ids
    } else {
        &facility.queue_agent_ids
    }
}

fn members_mut(facility: &mut FacilityRow, resource: bool) -> &mut Vec<Arc<str>> {
    if resource {
        &mut facility.resource_queue_agent_ids
    } else {
        &mut facility.queue_agent_ids
    }
}

/// A write stage's result: a refusal after a write is a bug to see (`kit::writing`).
fn writing<T>(result: WriteResult<T>) -> WriteResult<T> {
    result.map_err(|error| match error {
        WriteError::Error(Error::Unsupported(reason)) => {
            WriteError::Error(Error::Runtime(format!("{WRITING_REFUSED}: Unsupported: {reason}")))
        }
        other => other,
    })
}

// ---- The policy --------------------------------------------------------------------------------

/// The service of a facility's queue.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Service {
    Toilet,
    Water,
    Meal,
}

impl Service {
    fn name(self) -> &'static str {
        match self {
            Service::Toilet => "toilet",
            Service::Water => "water",
            Service::Meal => "meal",
        }
    }
}

/// `_service_name`: `"toilet" in kind.casefold()`, then the two resource kinds. None for a kind the
/// rule turns down.
pub fn service_of(kind: &str) -> Option<Service> {
    if is_toilet(kind) {
        Some(Service::Toilet)
    } else if kind == "water_station" {
        Some(Service::Water)
    } else if kind == "meal_distribution" {
        Some(Service::Meal)
    } else {
        None
    }
}

/// `_service_name` with its ValueError, whose text has the kind's Python `repr`.
fn service_named(kind: &str, kind_repr: Option<&str>) -> Result<Service> {
    match (service_of(kind), kind_repr) {
        (Some(service), _) => Ok(service),
        (None, Some(repr)) => Err(Error::value_error(format!("unsupported queue service facility kind {repr}"))),
        (None, None) => Err(Error::unsupported("a kind's repr was not given")),
    }
}

/// `_service_need`.
fn need_of(needs: &NeedRow, service: Service) -> Result<f64> {
    let value = match service {
        Service::Toilet => needs.toilet_urgency,
        Service::Water => needs.hydration_need,
        Service::Meal => needs.hunger,
    };
    if !value.is_finite() {
        return Err(Error::unsupported("a need is not finite"));
    }
    Ok(py_min(1.0, py_max(0.0, value)))
}

/// `priority`.
fn priority(policy: &Policy, need: f64, waited: i64) -> f64 {
    match policy {
        Policy::Strict => 0.0,
        Policy::NeedWeighted { .. } => need + AGING_WEIGHT * py_min(1.0, waited as f64 / AGING_MINUTES),
    }
}

/// `should_abandon`.
fn abandons(policy: &Policy, service: Service, need: f64, waited: i64) -> bool {
    match policy {
        Policy::Strict => false,
        Policy::NeedWeighted { limits, threshold, .. } => {
            let limit = match service {
                Service::Toilet => limits[0],
                Service::Water => limits[1],
                Service::Meal => limits[2],
            };
            waited >= limit && need < *threshold
        }
    }
}

fn policy_retry(policy: &Policy) -> i64 {
    match policy {
        Policy::Strict => RETRY_DELAY,
        Policy::NeedWeighted { retry, .. } => *retry,
    }
}

/// `_queue_trips`: the trips of `movement_v1` to a place of this facility's queue, or of one of its
/// independent queues.
fn fifo_trips<'a>(row: &'a MovementRow, facility_id: &'a str, queue_id: Option<&'a str>) -> impl Iterator<Item = (&'a Arc<str>, &'a Journey)> {
    row.journeys.iter().filter(move |(_, journey)| {
        journey.facility_id.as_deref() == Some(facility_id)
            && queue_id.is_none_or(|queue| journey.queue_id.as_deref() == Some(queue))
            && is_queue_trip(&journey.purpose)
    })
}

/// The movement row of a world that holds it, or None without the namespace.
#[track_caller]
fn movement_row<'w>(world: &'w World, held: &Held) -> Result<Option<crate::cell::ShardRef<'w, MovementRow>>> {
    held_row::<MovementRow>(world, KEY, held)
}

/// `world.metric_state` as `field_mirror_of` and its row's check read it.
fn check_metrics(world: &World, metric_state_held: bool) -> Result<()> {
    if world.slots.get(METRIC_STATE).is_some() && !metric_state_held {
        return Err(Error::unsupported("the field holds another dict"));
    }
    world.metric_state()?;
    Ok(())
}

// ---- prepare_facility_queue --------------------------------------------------------------------

/// What `prepare_facility_queue` and `prepare_independent_facility_queue` take besides the world.
pub struct Prepare<'a> {
    pub resource: bool,
    pub minute: i64,
    /// `create_queue_policy` of the configs.
    pub policy: Policy,
    /// Whether the world holds `movement_v1`.
    pub movement: Held,
    /// Whether the world holds `facility_queues_v1`.
    pub queues: Held,
    /// Whether the world holds `resource_service_v1`, read for a resource queue's stall.
    pub service: Held,
    /// `repr(kind)` of the facility's kind where `service_of` turns it down.
    pub kind_repr: Option<&'a str>,
}

/// What those who give up need, read only when someone does.
pub struct LeaveInputs<'a> {
    /// `"facility_geometry" in model_configs`.
    pub has_geometry: bool,
    pub model: &'a Read<MovementModel>,
    /// The floor plan, where the world is placed or the queue is independent.
    pub geo: Option<&'a Geo>,
    /// Whether `world.metric_state` is its slot's tree.
    pub metric_state_held: bool,
}

/// What a call of a preparing kernel did: done, or it needs the inputs of those who give up.
#[derive(Debug, PartialEq, Eq)]
pub enum Prepared {
    Done,
    NeedsLeaveInputs,
}

/// The kind of the facility, while its queue (`resource_queue_agent_ids` when resource, else
/// `queue_agent_ids`) has people: None for an unknown facility or an empty queue.
pub fn queued_kind(world: &World, facility_id: &str, resource: bool) -> Result<Option<Arc<str>>> {
    let rows = world.rows()?;
    Ok(rows.facilities.get(facility_id).filter(|facility| !members(facility, resource).is_empty()).map(|facility| facility.kind.clone()))
}

/// `prepare_facility_queue`, of a facility whose queue has people (`queued_kind`). Without
/// `leave`, it asks for them where someone gives up, having written nothing.
pub fn prepare(world: &World, facility_id: &str, prepare: &Prepare<'_>, leave: Option<&LeaveInputs<'_>>) -> WriteResult<Prepared> {
    let (resource, minute, policy) = (prepare.resource, prepare.minute, prepare.policy);
    if let Some(row) = movement_row(world, &prepare.movement)? {
        if fifo_trips(&row, facility_id, None).next().is_some() {
            return Ok(Prepared::Done);
        }
    }
    let kind = {
        let rows = world.rows()?;
        match rows.facilities.get(facility_id) {
            Some(facility) => facility.kind.clone(),
            None => return Err(Error::unsupported("no such facility").into()),
        }
    };
    let service = service_named(&kind, prepare.kind_repr)?;
    let (current, new_order, leaving) = {
        let rows = world.rows()?;
        let Some(facility) = rows.facilities.get(facility_id) else {
            return Err(Error::unsupported("no such facility").into());
        };
        let kind = if resource { "resource_service_queue" } else { "toilet_queue" };
        let mut tasks: BTreeMap<&str, (i64, &Arc<str>)> = BTreeMap::new();
        for (_, task) in rows.active_tasks.iter() {
            if task.resource_id.as_deref() == Some(facility_id) && &*task.kind == kind {
                let Some(agent_id) = &task.agent_id else {
                    return Err(Error::unsupported("a queue task has no agent").into());
                };
                // The last in the map's order, as a dict comprehension keeps.
                tasks.insert(agent_id, (task.start_minute, &task.id));
            }
        }
        let queue = members(facility, resource);
        let mut seen = HashSet::with_capacity(queue.len());
        let mut ranked = Vec::with_capacity(queue.len());
        for (order, agent_id) in queue.iter().enumerate() {
            if !seen.insert(&**agent_id) {
                return Err(Error::unsupported("a person stands in the queue twice").into());
            }
            let Some(&(start, task_id)) = tasks.get(&**agent_id) else {
                continue;
            };
            let waited = minute.checked_sub(start).ok_or_else(|| Error::unsupported("a start out of range"))?.max(0);
            if waited >= EXACT_INT {
                return Err(Error::unsupported("a wait too long for a float").into());
            }
            let Some(agent) = rows.agents.get(agent_id) else {
                return Err(Error::unsupported("no such agent").into());
            };
            let need = match policy {
                Policy::Strict => 0.0,
                Policy::NeedWeighted { .. } => need_of(&agent.needs, service)?,
            };
            let leaves = abandons(&policy, service, need, waited).then(|| Leaving { agent_id: agent_id.clone(), task_id: task_id.clone(), start });
            ranked.push((-priority(&policy, need, waited), start, order, agent_id.clone(), leaves));
        }
        // As `sorted` on the tuples: -0.0 equals 0.0, and every value is finite.
        ranked.sort_by(|first, second| {
            let by_urgency = first.0.partial_cmp(&second.0).unwrap_or(std::cmp::Ordering::Equal);
            by_urgency.then(first.1.cmp(&second.1)).then(first.2.cmp(&second.2))
        });
        let mut new_order = Vec::with_capacity(ranked.len());
        let mut leaving = Vec::new();
        for (_, _, _, agent_id, leaves) in ranked {
            new_order.push(agent_id);
            leaving.extend(leaves);
        }
        (queue.clone(), new_order, leaving)
    };
    let leave = match (leaving.is_empty(), leave) {
        (true, _) => None,
        (false, Some(leave)) => Some(leave),
        (false, None) => return Ok(Prepared::NeedsLeaveInputs),
    };
    let call = LeaveCall { facility_id, resource, minute, service, retry: policy_retry(&policy), queue_id: None, prepare, leave };
    plan_all_leaving(world, &call, &leaving)?;
    let stale = resource && stale_stall(world, prepare, facility_id, &new_order)?;
    if new_order != current {
        // `queue[:] = ...`: the object's list follows the row.
        let mut rows = world.rows_mut()?;
        let Some(facility) = rows.facilities.get_mut(facility_id) else {
            return Err(Error::unsupported("the rows' facility has no object").into());
        };
        *members_mut(facility, resource) = new_order;
    }
    if stale {
        remove_stall(world, facility_id)?;
    }
    writing(write_all_leaving(world, &call, &leaving))?;
    Ok(Prepared::Done)
}

/// What `prepare_independent_facility_queue` reads before the policy: the facility and its kind,
/// or None where it does nothing (no namespace or no such queue, someone walking to a place of the
/// queue, nobody queued). The kind is None for a facility the rows don't have.
pub fn independent_queue(world: &World, queue_id: &str, queues: &Held, movement: &Held) -> Result<Option<(Arc<str>, Option<Arc<str>>)>> {
    let (facility_id, anyone) = {
        let Some(row) = fifo::queues(world, queues)? else {
            return Ok(None);
        };
        let Some(queue) = row.queues.get(queue_id) else {
            return Ok(None);
        };
        (queue.facility_id.clone(), queue.queued_request_ids.iter().any(|id| row.requests.contains_key(&**id)))
    };
    if let Some(row) = movement_row(world, movement)? {
        if row.journeys.values().any(|journey| journey.queue_id.as_deref() == Some(queue_id) && is_queue_trip(&journey.purpose)) {
            return Ok(None);
        }
    }
    if !anyone {
        return Ok(None);
    }
    let kind = world.rows()?.facilities.get(&*facility_id).map(|facility| facility.kind.clone());
    Ok(Some((facility_id, kind)))
}

/// `prepare_independent_facility_queue`, of a queue `independent_queue` found: an independent FIFO
/// keeps its order, and only those who give up change anything. Without `leave`, it asks for them
/// where someone gives up, having written nothing.
pub fn prepare_independent(
    world: &World,
    queue_id: &str,
    facility_id: &str,
    prepare: &Prepare<'_>,
    leave: Option<&LeaveInputs<'_>>,
) -> WriteResult<Prepared> {
    let (resource, minute, policy) = (prepare.resource, prepare.minute, prepare.policy);
    let requests = {
        let Some(row) = fifo::queues(world, &prepare.queues)? else {
            return Err(Error::unsupported("no queue state").into());
        };
        let Some(queue) = row.queues.get(queue_id) else {
            return Err(Error::unsupported("no such queue").into());
        };
        queue
            .queued_request_ids
            .iter()
            .filter_map(|id| row.requests.get(&**id))
            .map(|request| (request.agent_id.clone(), request.queue_task_id.clone()))
            .collect::<Vec<_>>()
    };
    let kind = match world.rows()?.facilities.get(facility_id) {
        Some(facility) => facility.kind.clone(),
        None => return Err(Error::unsupported("no such facility").into()),
    };
    let service = service_named(&kind, prepare.kind_repr)?;
    let leaving = {
        let rows = world.rows()?;
        let Some(facility) = rows.facilities.get(facility_id) else {
            return Err(Error::unsupported("no such facility").into());
        };
        let mut leaving = Vec::new();
        for (agent_id, task_id) in &requests {
            let Some(task) = rows.active_tasks.get(task_id) else {
                continue;
            };
            let waited = minute.checked_sub(task.start_minute).ok_or_else(|| Error::unsupported("a start out of range"))?.max(0);
            let need = match policy {
                Policy::Strict => 0.0,
                Policy::NeedWeighted { .. } => match rows.agents.get(agent_id) {
                    Some(agent) => need_of(&agent.needs, service)?,
                    None => return Err(Error::unsupported("no such agent").into()),
                },
            };
            // `_abandon_and_retry`'s first check: someone not in the facility's list stays as they are.
            if abandons(&policy, service, need, waited) && members(facility, resource).contains(agent_id) {
                leaving.push(Leaving { agent_id: agent_id.clone(), task_id: task.id.clone(), start: task.start_minute });
            }
        }
        leaving
    };
    if leaving.is_empty() {
        return Ok(Prepared::Done);
    }
    let Some(leave) = leave else {
        return Ok(Prepared::NeedsLeaveInputs);
    };
    let call = LeaveCall { facility_id, resource, minute, service, retry: policy_retry(&policy), queue_id: Some(queue_id), prepare, leave: Some(leave) };
    plan_all_leaving(world, &call, &leaving)?;
    writing(write_all_leaving(world, &call, &leaving))?;
    Ok(Prepared::Done)
}

/// `_drop_stale_resource_stall` once the queue is in its new order: whether to delete the record.
fn stale_stall(world: &World, prepare: &Prepare<'_>, facility_id: &str, new_order: &[Arc<str>]) -> Result<bool> {
    let agent = {
        let Some(row) = held_row::<ServiceRow>(world, SERVICE_KEY, &prepare.service)? else {
            return Ok(false);
        };
        match row.stall_state.get(facility_id) {
            Some(stall) => stall.agent_id.clone(),
            None => return Ok(false),
        }
    };
    let is_head = match fifo::heads_in(world, &prepare.queues, facility_id)? {
        Some(heads) => heads.iter().any(|head| *head == agent),
        None => new_order.first().is_some_and(|head| *head == agent),
    };
    Ok(!is_head)
}

/// `del stall_state[facility_id]`, on the row.
fn remove_stall(world: &World, facility_id: &str) -> Result<()> {
    world.row_mut::<ServiceRow>(SERVICE_KEY)?.stall_state.shift_remove(facility_id);
    Ok(())
}

// ---- _abandon_and_retry --------------------------------------------------------------------------

/// Someone `should_abandon` picked, in the order the rule goes through them.
struct Leaving {
    agent_id: Arc<str>,
    task_id: Arc<str>,
    start: i64,
}

/// What a call of `_abandon_and_retry` has in common for those who leave.
struct LeaveCall<'a> {
    facility_id: &'a str,
    resource: bool,
    minute: i64,
    service: Service,
    retry: i64,
    queue_id: Option<&'a str>,
    prepare: &'a Prepare<'a>,
    leave: Option<&'a LeaveInputs<'a>>,
}

/// What `_abandon_and_retry` writes for a person, with the sequences it takes from `sequence`.
struct LeavePlan {
    abandon: EventRow,
    /// `finish_service`'s request and `record_queue_transition`'s record.
    transition: Option<(QueueRequest, EventRow)>,
    walk: Option<LeaveWalk>,
    sequences: i64,
}

struct LeaveWalk {
    task_id: Arc<str>,
    destination: Arc<str>,
    due: i64,
    event: EventRow,
    changes: Changes,
}

/// The walk back to the middle of the zone: None to leave at once, `Some(None)` when no path lets
/// the person out, so the rule leaves them in line.
fn return_path(geo: &Geo, movement: &MovementRow, rows: &Entities, model: &Read<MovementModel>, agent_id: &str) -> Result<Option<Option<(i64, Arc<[Pt]>)>>> {
    let Some(agent) = rows.agents.get(agent_id) else {
        return Err(Error::unsupported("no such agent"));
    };
    let current = position_point(movement.positions.get(agent_id));
    let destination = geo.center(&agent.zone_id)?;
    let (Some(current), Some(destination)) = (current, destination) else {
        return Ok(None);
    };
    if !usable(current.x, current.y) {
        return Err(Error::unsupported("a position is not a usable point"));
    }
    let mobility = mobility_text(agent);
    let Some(path) = geo.room_route(&agent.zone_id, Some(current), destination, mobility).map_err(refused)? else {
        return Ok(Some(None));
    };
    let mut minutes = 0;
    if path.len() >= 2 {
        let length = polyline_length(&path).ok_or_else(|| Error::unsupported("not a plain sum"))?;
        minutes = room_minutes(model, geo, rows, &agent.zone_id, mobility, length)?;
    }
    Ok(Some(Some((minutes, path))))
}

/// `_abandon_and_retry` of one person, changing nothing. None when the rule returns at once and the
/// person stays in line. Whether someone leaves doesn't turn on who left before: the path and the
/// crowd of the zone stay what they were.
fn plan_leave(world: &World, call: &LeaveCall<'_>, person: &Leaving, sequence: i64) -> Result<Option<LeavePlan>> {
    let minute = call.minute;
    let Some(leave) = call.leave else {
        return Err(Error::unsupported("no inputs for those who give up"));
    };
    let events = Events::of(world)?;
    let movement = &call.prepare.movement;
    let placed = !matches!(movement, Held::Absent) && leave.has_geometry;
    let path = if placed {
        let Some(geo) = leave.geo else {
            return Err(Error::unsupported("no floor plan"));
        };
        let Some(row) = movement_row(world, movement)? else {
            return Err(Error::unsupported("the world holds another movement state"));
        };
        let rows = world.rows()?;
        return_path(geo, &row, &rows, leave.model, &person.agent_id)?
    } else {
        None
    };
    let (minutes, points) = match path {
        Some(None) => return Ok(None),
        Some(Some((minutes, points))) => (minutes, Some(points)),
        None => (0, None),
    };
    let due = minute.checked_add(minutes).ok_or_else(|| Error::unsupported("a due minute out of range"))?;
    if due >= events.duration() {
        return Ok(None);
    }
    let retry_at = minute.checked_add(call.retry).ok_or_else(|| Error::unsupported("a minute out of range"))?;
    let abandon_id: Arc<str> = Arc::from(format!("facility-queue-abandon-{sequence}"));
    if events.is_taken(&abandon_id, Some(sequence))? {
        return Err(Error::unsupported("the event's identity is taken"));
    }
    let mut payload = IndexMap::with_capacity(8);
    payload.insert(Arc::from("facility_id"), text(call.facility_id));
    payload.insert(Arc::from("agent_id"), V::Str(person.agent_id.clone()));
    payload.insert(Arc::from("task_id"), V::Str(person.task_id.clone()));
    payload.insert(Arc::from("service"), text(call.service.name()));
    payload.insert(Arc::from("start_minute"), V::Int(person.start));
    payload.insert(Arc::from("wait_minutes"), V::Int(minute - person.start));
    payload.insert(Arc::from("retry_at_minute"), V::Int(retry_at));
    payload.insert(Arc::from("reason"), text("need_weighted_wait_threshold"));
    let mut provenance = IndexMap::with_capacity(4);
    provenance.insert(Arc::from("source_type"), text("assumption"));
    provenance.insert(Arc::from("source_id"), text(NEED_WEIGHTED_SOURCE));
    provenance.insert(Arc::from("policy_version"), text(NEED_WEIGHTED_ID));
    provenance.insert(Arc::from("transition"), text("abandon_queue_then_retry"));
    let Some(abandon) = EventRow::new(
        abandon_id.clone(),
        EventKind::Endogenous,
        minute,
        0,
        sequence,
        Arc::from("facility_queue_abandon"),
        payload,
        Some(Arc::from(NEED_WEIGHTED_SOURCE)),
        Some(person.task_id.clone()),
        provenance,
    ) else {
        return Err(Error::unsupported("the event's values are not what an event takes"));
    };
    let mut next = sequence + 1;
    let transition = match call.queue_id {
        None => None,
        Some(queue_id) => {
            let found = match fifo::queues(world, &call.prepare.queues)? {
                Some(row) => request_of_task(&row, &person.task_id).cloned(),
                None => None,
            };
            match found {
                None => None,
                Some(request) => {
                    let Some(geo) = leave.geo else {
                        return Err(Error::unsupported("no floor plan"));
                    };
                    let capacity = point_of(geo, queue_id).map(|point| point.service_capacity);
                    // After `finish_service` the request has no place in the queue.
                    let taken = events.is_realized(&fifo::transition_id(next), Some(next))?;
                    let wait = Some(minute - request.requested_minute);
                    let payload = fifo::transition_payload(taken, &request, "abandoned", &person.task_id, wait, Some(&person.task_id), None, capacity, next, minute)?;
                    let row = fifo::transition_event(payload, Some(&person.task_id), next, minute)?;
                    next += 1;
                    Some((request, row))
                }
            }
        }
    };
    let walk = match points.filter(|_| minutes > 0) {
        None => None,
        Some(points) => {
            let task_id: Arc<str> = Arc::from(format!("queue-abandon-travel-{next}"));
            let event_id: Arc<str> = Arc::from(format!("queue-abandon-arrival-{next}"));
            if events.is_taken(&event_id, Some(next))? {
                return Err(Error::unsupported("the event's identity is taken"));
            }
            if points.iter().any(|point| !usable(point.x, point.y)) {
                return Err(Error::unsupported("a route point is not a usable point"));
            }
            let Some(row) = movement_row(world, movement)? else {
                return Err(Error::unsupported("a walk without the floor plan"));
            };
            let rows = world.rows()?;
            let Some(agent) = rows.agents.get(&person.agent_id) else {
                return Err(Error::unsupported("no such agent"));
            };
            let zone = agent.zone_id.clone();
            let mut payload = IndexMap::with_capacity(9);
            payload.insert(Arc::from("agent_id"), V::Str(person.agent_id.clone()));
            payload.insert(Arc::from("task_id"), V::Str(task_id.clone()));
            payload.insert(Arc::from("purpose"), text("queue_abandon"));
            payload.insert(Arc::from("destination_zone_id"), V::Str(zone.clone()));
            payload.insert(Arc::from("origin_zone_id"), V::Str(zone.clone()));
            payload.insert(Arc::from("route_zone_ids"), V::List(vec![V::Str(zone.clone()), V::Str(zone.clone())]));
            payload.insert(Arc::from("route_edge_ids"), V::List(Vec::new()));
            payload.insert(Arc::from("travel_minutes"), V::Int(minutes));
            payload.insert(Arc::from("start_minute"), V::Int(minute));
            let mut provenance = IndexMap::with_capacity(4);
            provenance.insert(Arc::from("source_type"), text("assumption"));
            provenance.insert(Arc::from("source_id"), text(NEED_WEIGHTED_SOURCE));
            provenance.insert(Arc::from("policy_version"), text(NEED_WEIGHTED_ID));
            provenance.insert(Arc::from("movement"), text("in_room_return_to_zone_anchor"));
            let Some(event) = EventRow::new(
                event_id.clone(),
                EventKind::Completion,
                due,
                0,
                next,
                Arc::from("agent_travel_complete"),
                payload,
                Some(task_id.clone()),
                Some(abandon_id.clone()),
                provenance,
            ) else {
                return Err(Error::unsupported("the event's values are not what an event takes"));
            };
            let journey = Journey {
                event_id: event_id.clone(),
                task_id: task_id.clone(),
                purpose: Arc::from("queue_abandon"),
                facility_id: None,
                destination_zone_id: zone.clone(),
                route_zone_ids: vec![zone.clone(), zone.clone()],
                route_edge_ids: Vec::new(),
                route_points: points.iter().map(|point| Point { x: point.x, y: point.y }).collect(),
                started_minute: minute,
                due_minute: due,
                queue_id: None,
                support_request_id: None,
                support_staff_id: None,
            };
            let changes = start_on_path(&row, &rows, &person.agent_id, journey, &zone, &points, minute, minutes)?;
            next += 1;
            Some(LeaveWalk { task_id, destination: zone, due, event, changes })
        }
    };
    Ok(Some(LeavePlan { abandon, transition, walk, sequences: next - sequence }))
}

/// `start_movement` of a walk along a path of points.
#[allow(clippy::too_many_arguments)]
fn start_on_path(row: &MovementRow, rows: &Entities, agent_id: &Arc<str>, journey: Journey, origin: &Arc<str>, points: &[Pt], started: i64, duration: i64) -> Result<Changes> {
    if points.is_empty() {
        return Err(Error::unsupported("a walk without the points of its path"));
    }
    plan_start(row, rows, agent_id, journey, origin, points, &[], started, duration, None)
}

/// Everyone's plan, for what it refuses: the writes then plan again, on the state they find.
fn plan_all_leaving(world: &World, call: &LeaveCall<'_>, leaving: &[Leaving]) -> Result<()> {
    if leaving.is_empty() {
        return Ok(());
    }
    let mut sequence = Events::of(world)?.next_sequence()?;
    let metric_state_held = call.leave.is_some_and(|leave| leave.metric_state_held);
    check_metrics(world, metric_state_held)?;
    for person in leaving {
        if let Some(plan) = plan_leave(world, call, person, sequence)? {
            sequence += plan.sequences;
        }
    }
    Ok(())
}

/// `_abandon_and_retry`, in the order of its writes.
fn write_all_leaving(world: &World, call: &LeaveCall<'_>, leaving: &[Leaving]) -> WriteResult<()> {
    let minute = call.minute;
    for person in leaving {
        let events = Events::of(world)?;
        let Some(plan) = plan_leave(world, call, person, events.next_sequence()?)? else {
            continue;
        };
        events.allocate_sequence()?;
        let remaining = {
            let mut rows = world.rows_mut()?;
            let Some(facility) = rows.facilities.get_mut(call.facility_id) else {
                return Err(Error::unsupported("the rows' facility has no object").into());
            };
            let list = members_mut(facility, call.resource);
            // `list.remove`.
            let Some(place) = list.iter().position(|id| *id == person.agent_id) else {
                return Err(Error::Value("list.remove(x): x not in list".to_owned()).into());
            };
            list.remove(place);
            list.clone()
        };
        if call.resource && stale_stall(world, call.prepare, call.facility_id, &remaining)? {
            remove_stall(world, call.facility_id)?;
        }
        if let Some((request, row)) = &plan.transition {
            fifo::write_finish(world, request)?;
            fifo::write_transition(&events, row, minute)?;
        }
        if !world.rows_mut()?.active_tasks.remove_row(&person.task_id) {
            return Err(Error::Key(person.task_id.to_string()).into());
        }
        let next = minute + call.retry;
        {
            let mut rows = world.rows_mut()?;
            let Some(row) = rows.agents.get_mut(&person.agent_id) else {
                return Err(Error::unsupported("no such agent").into());
            };
            row.next_decision_at = next;
        }
        events.record(&plan.abandon, minute, EventStatus::Applied, Some(ABANDON_DETAIL))?;
        match plan.walk {
            Some(LeaveWalk { task_id, destination, due, event, changes }) => {
                events.allocate_sequence()?;
                world.rows_mut()?.active_tasks.insert(
                    task_id.clone(),
                    TaskRow {
                        id: task_id.clone(),
                        kind: Arc::from("travel_return"),
                        agent_id: Some(person.agent_id.clone()),
                        resource_id: None,
                        start_minute: minute,
                        due_minute: due,
                        staff_id: None,
                    },
                );
                set_agent(&mut *world.rows_mut()?, &person.agent_id, Activity::Moving, Some(&destination))?;
                events.schedule(event)?;
                changes.apply(&mut *world.row_mut::<MovementRow>(KEY)?);
            }
            None => set_agent(&mut *world.rows_mut()?, &person.agent_id, Activity::Idle, None)?,
        }
        add_metric(world, "facility_queue_abandon_count")?;
    }
    Ok(())
}

// ---- progress_physical_queue_positions and _head_to_service --------------------------------------

/// A route to walk: `_schedule_queue_trip`'s arguments.
pub struct Route {
    pub agent_id: Arc<str>,
    pub index: usize,
    pub minutes: i64,
    pub path: Arc<[Pt]>,
}

/// What a kernel found to write.
pub enum Plan {
    Nothing,
    Blocked,
    /// `_schedule_queue_trip` of a route that really walks.
    Trip(Box<Trip>),
}

impl Plan {
    /// What the plan changes in the movement row, for a caller that writes several together.
    pub fn take_changes(&mut self) -> Changes {
        match self {
            Plan::Trip(trip) => std::mem::take(&mut trip.changes),
            _ => Changes::default(),
        }
    }
}

pub struct Trip {
    agent_id: Arc<str>,
    event: EventRow,
    changes: Changes,
}

/// The ID of the trip `plan_trip` makes with this sequence, whose being taken its caller reads.
pub fn trip_id(sequence: i64) -> Arc<str> {
    Arc::from(format!("physical-queue-trip-{sequence}"))
}

/// `_schedule_queue_trip`, changing nothing, taking this event number. `duration` is the Run's,
/// and `taken` whether the queue or the log holds the event `trip_id(sequence)` names already.
#[allow(clippy::too_many_arguments)]
pub fn plan_trip(
    duration: i64,
    taken: bool,
    row: &MovementRow,
    rows: &Entities,
    facility_id: &str,
    resource: bool,
    route: Route,
    minute: i64,
    phase: &'static str,
    fifo: Option<&Fifo>,
    sequence: i64,
) -> Result<Plan> {
    let due = minute.checked_add(route.minutes).ok_or_else(|| Error::unsupported("a due minute out of range"))?;
    if route.minutes <= 0 || due >= duration {
        return Ok(Plan::Blocked);
    }
    if route.minutes >= EXACT_INT {
        return Err(Error::unsupported("a journey has no plain duration"));
    }
    let Some(facility) = rows.facilities.get(facility_id) else {
        return Err(Error::unsupported("no such facility"));
    };
    if facility_id.is_empty() {
        return Err(Error::unsupported("a facility with an empty ID"));
    }
    let task_kind = if resource { "resource_service_queue" } else { "toilet_queue" };
    // `request_for_agent`, whose task and place in the queue a trip names.
    let queue_request = match fifo {
        Some(fifo) => match fifo.requests.iter().find(|request| request.agent_id == route.agent_id) {
            Some(request) => Some((fifo, request)),
            None => return Err(Error::unsupported("a person of the queue has no request")),
        },
        None => None,
    };
    let task = match queue_request {
        Some((_, request)) => rows.active_tasks.get(&request.queue_task_id),
        None => rows
            .active_tasks
            .iter()
            .find(|(_, task)| {
                task.agent_id.as_deref() == Some(&*route.agent_id) && task.resource_id.as_deref() == Some(facility_id) && &*task.kind == task_kind
            })
            .map(|(_, task)| task),
    };
    let Some(task) = task else {
        return Err(Error::value_error("physical queue reservation has no matching active queue task"));
    };
    let fifo_position = match queue_request {
        Some((fifo, request)) => match fifo.queued_request_ids.iter().position(|id| *id == request.request_id) {
            Some(place) => place,
            None => return Err(Error::unsupported("a request of the walk is not queued")),
        },
        None => route.index,
    };
    if route.path.iter().any(|point| !usable(point.x, point.y)) {
        return Err(Error::unsupported("a route point is not a usable point"));
    }
    let event_id = trip_id(sequence);
    let purpose: Arc<str> = Arc::from(format!("{}_queue_{phase}", if resource { "resource" } else { "toilet" }));
    let zone = facility.zone_id.clone();
    let zone_ids = vec![zone.clone(), zone.clone()];
    let mut payload = IndexMap::with_capacity(12);
    payload.insert(Arc::from("agent_id"), V::Str(route.agent_id.clone()));
    payload.insert(Arc::from("task_id"), V::Str(task.id.clone()));
    payload.insert(Arc::from("purpose"), V::Str(purpose.clone()));
    payload.insert(Arc::from("destination_zone_id"), V::Str(zone.clone()));
    payload.insert(Arc::from("facility_id"), text(facility_id));
    if let Some((fifo, request)) = queue_request {
        payload.insert(Arc::from("queue_id"), V::Str(fifo.queue_id.clone()));
        payload.insert(Arc::from("request_id"), V::Str(request.request_id.clone()));
    }
    payload.insert(Arc::from("origin_zone_id"), V::Str(zone.clone()));
    payload.insert(Arc::from("route_zone_ids"), V::List(vec![V::Str(zone.clone()), V::Str(zone.clone())]));
    payload.insert(Arc::from("route_edge_ids"), V::List(Vec::new()));
    payload.insert(Arc::from("travel_minutes"), V::Int(route.minutes));
    payload.insert(Arc::from("start_minute"), V::Int(minute));
    let mut provenance = IndexMap::with_capacity(6);
    provenance.insert(Arc::from("source_type"), text("assumption"));
    provenance.insert(Arc::from("source_id"), text(MOVEMENT_SOURCE_ID));
    provenance.insert(Arc::from("policy_version"), text(POLICY_VERSION));
    provenance.insert(Arc::from("queue_phase"), text(phase));
    provenance.insert(Arc::from("queue_requested_at"), V::Int(task.start_minute));
    provenance.insert(Arc::from("fifo_position"), V::Int(fifo_position as i64));
    let Some(event) = EventRow::new(
        event_id.clone(),
        EventKind::Completion,
        due,
        0,
        sequence,
        Arc::from("agent_travel_complete"),
        payload,
        Some(task.id.clone()),
        Some(task.id.clone()),
        provenance,
    ) else {
        return Err(Error::unsupported("the event's values are not what an event takes"));
    };
    if taken {
        return Err(Error::unsupported("the event's identity is taken"));
    }
    let journey = Journey {
        event_id,
        task_id: task.id.clone(),
        purpose,
        facility_id: Some(Arc::from(facility_id)),
        destination_zone_id: zone.clone(),
        route_zone_ids: zone_ids,
        route_edge_ids: Vec::new(),
        route_points: route.path.iter().map(|point| Point { x: point.x, y: point.y }).collect(),
        started_minute: minute,
        due_minute: due,
        queue_id: fifo.map(|fifo| fifo.queue_id.clone()),
        support_request_id: None,
        support_staff_id: None,
    };
    let changes = start_on_path(row, rows, &route.agent_id, journey, &zone, &route.path, minute, route.minutes)?;
    Ok(Plan::Trip(Box::new(Trip { agent_id: route.agent_id, event, changes })))
}

/// The plan's writes: `world.metric_state[BLOCKED_METRIC] += 1.0` for a blocked one; for a trip,
/// the person queued at the facility, the event scheduled with its sequence, and the journey.
pub fn write_plan(world: &World, facility_id: &str, plan: Plan) -> WriteResult<()> {
    let trip = match plan {
        Plan::Nothing => return Ok(()),
        Plan::Blocked => return Ok(add_metric(world, BLOCKED_METRIC)?),
        Plan::Trip(trip) => trip,
    };
    let Trip { agent_id, event, changes } = *trip;
    {
        let mut rows = world.rows_mut()?;
        let Some(agent) = rows.agents.get(&agent_id) else {
            return Err(Error::unsupported("no such agent").into());
        };
        if agent.activity != Activity::Queued || agent.target_id.as_deref() != Some(facility_id) {
            if let Some(agent) = rows.agents.get_mut(&agent_id) {
                agent.activity = Activity::Queued;
                agent.target_id = Some(Arc::from(facility_id));
            }
        }
    }
    let events = Events::of(world)?;
    events.allocate_sequence()?;
    events.schedule(event)?;
    changes.apply(&mut *world.row_mut::<MovementRow>(KEY)?);
    Ok(())
}

/// `facility_queue_slot_route`: the route's minutes and its path, or None for no route.
#[allow(clippy::too_many_arguments)]
pub fn slot_route(
    geo: &Geo,
    row: &MovementRow,
    rows: &Entities,
    model: &Read<MovementModel>,
    agent_id: &str,
    facility: &FacilityRow,
    facility_id: &str,
    fifo: Option<&Fifo>,
) -> Result<Option<(i64, Arc<[Pt]>)>> {
    let Some(destination) = queue_slot_in(geo, row, rows, agent_id, facility_id, fifo.map(Fifo::slot_view))? else {
        return Ok(None);
    };
    let Some(agent) = rows.agents.get(agent_id) else {
        return Ok(None);
    };
    let start = position_point(row.positions.get(agent_id));
    if start.is_some_and(|start| !usable(start.x, start.y)) {
        return Err(Error::unsupported("a position is not a usable point"));
    }
    let mobility = mobility_text(agent);
    let Some(path) = geo.room_route(&facility.zone_id, start, destination, mobility).map_err(refused)? else {
        return Ok(None);
    };
    let mut minutes = 0;
    if path.len() >= 2 {
        let length = polyline_length(&path).ok_or_else(|| Error::unsupported("not a plain sum"))?;
        minutes = room_minutes(model, geo, rows, &agent.zone_id, mobility, length)?;
    }
    if path.len() < 2 || path.windows(2).all(|pair| distance(pair[0], pair[1]) <= 1e-8) {
        minutes = 0;
    }
    Ok(Some((minutes, path)))
}

/// What the walking kernels take besides the world: the floor plan and the movement model of the
/// configs, and whether the Python world holds the namespaces and `metric_state`.
pub struct Walk<'a> {
    pub geo: &'a Geo,
    pub model: &'a Read<MovementModel>,
    pub movement: Held,
    pub queues: Held,
    pub metric_state_held: bool,
}

/// What both walking kernels read before they plan: the FIFO, and the checks of the namespaces,
/// the events and the metrics, in the order the kernels made them.
fn walk_start<'w>(world: &'w World, walk: &Walk<'_>, queue_id: Option<&str>) -> Result<(Option<Fifo>, Events<'w>)> {
    if walk.geo.layout.is_none() {
        return Err(Error::unsupported("no floor plan"));
    }
    let fifo = match queue_id {
        Some(queue) => Some(Fifo::read(world, &walk.queues, walk.geo, queue)?),
        None => None,
    };
    if movement_row(world, &walk.movement)?.is_none() {
        return Err(Error::unsupported("the world holds another movement state"));
    }
    let events = Events::of(world)?;
    check_metrics(world, walk.metric_state_held)?;
    Ok((fifo, events))
}

/// `progress_physical_queue_positions` of a placed world (`"facility_geometry"` in the configs and
/// `movement_v1` in the state): the first person of the queue not in their place walks there.
pub fn positions(world: &World, walk: &Walk<'_>, facility_id: &str, resource: bool, minute: i64, queue_id: Option<&str>) -> WriteResult<bool> {
    let (fifo, events) = walk_start(world, walk, queue_id)?;
    let queue_name = fifo.as_ref().map(|fifo| &*fifo.queue_id);
    let service_point = fifo.as_ref().map(|fifo| &*fifo.service_point_id);
    let (geo, model) = (walk.geo, walk.model);
    let (result, plan) = {
        let Some(row) = movement_row(world, &walk.movement)? else {
            return Err(Error::unsupported("the world holds another movement state").into());
        };
        let rows = world.rows()?;
        (|| -> Result<(bool, Plan)> {
            let Some(facility) = rows.facilities.get(facility_id) else {
                return Ok((true, Plan::Nothing));
            };
            let queue: &[Arc<str>] = match &fifo {
                Some(fifo) => &fifo.members,
                None => members(facility, resource),
            };
            let head = queue.first();
            let head_walks = |row: &MovementRow| head.is_some_and(|head| fifo_trips(row, facility_id, queue_name).any(|(id, _)| id == head));
            if fifo_trips(&row, facility_id, queue_name).any(|(_, journey)| journey.purpose.ends_with("_position")) {
                return Ok((head_walks(&row), Plan::Nothing));
            }
            for (index, agent_id) in queue.iter().enumerate() {
                if index == 0 {
                    if head_walks(&row) {
                        continue;
                    }
                    let route = approach_point(geo, &row, &rows, model, agent_id, facility_id, service_point)?;
                    if route.is_some_and(|(minutes, path)| minutes <= 0 || path.len() < 2) {
                        continue;
                    }
                }
                let target = queue_slot_in(geo, &row, &rows, agent_id, facility_id, fifo.as_ref().map(Fifo::slot_view))?;
                let current = position_point(row.positions.get(&**agent_id));
                if current.is_some_and(|point| !usable(point.x, point.y)) {
                    return Err(Error::unsupported("a position is not a usable point"));
                }
                if let (Some(target), Some(current)) = (target, current) {
                    if distance(target, current) <= SLOT_TOLERANCE {
                        continue;
                    }
                }
                let route = slot_route(geo, &row, &rows, model, agent_id, facility, facility_id, fifo.as_ref())?;
                let Some((minutes, path)) = route.filter(|(_, path)| !path.is_empty()) else {
                    return Ok((index == 0, Plan::Blocked));
                };
                // `list.index`, which a person listed twice would make another place.
                let place = queue.iter().position(|other| other == agent_id).unwrap_or(index);
                let route = Route { agent_id: agent_id.clone(), index: place, minutes, path };
                let sequence = events.next_sequence()?;
                let taken = events.is_taken(&trip_id(sequence), Some(sequence))?;
                let plan = plan_trip(events.duration(), taken, &row, &rows, facility_id, resource, route, minute, "position", fifo.as_ref(), sequence)?;
                return Ok((index == 0, plan));
            }
            Ok((false, Plan::Nothing))
        })()?
    };
    write_plan(world, facility_id, plan)?;
    Ok(result)
}

/// `progress_physical_queue_head_to_service` of a placed world: false has written nothing, and a
/// refusal changes nothing.
pub fn head_to_service(world: &World, walk: &Walk<'_>, facility_id: &str, resource: bool, minute: i64, queue_id: Option<&str>) -> WriteResult<bool> {
    let (fifo, events) = walk_start(world, walk, queue_id)?;
    let queue_name = fifo.as_ref().map(|fifo| &*fifo.queue_id);
    let service_point = fifo.as_ref().map(|fifo| &*fifo.service_point_id);
    let (result, plan) = {
        let Some(row) = movement_row(world, &walk.movement)? else {
            return Err(Error::unsupported("the world holds another movement state").into());
        };
        let rows = world.rows()?;
        (|| -> Result<(bool, Plan)> {
            let Some(facility) = rows.facilities.get(facility_id) else {
                return Ok((true, Plan::Nothing));
            };
            let queue: &[Arc<str>] = match &fifo {
                Some(fifo) => &fifo.members,
                None => members(facility, resource),
            };
            let Some(agent_id) = queue.first() else {
                return Ok((false, Plan::Nothing));
            };
            if fifo_trips(&row, facility_id, queue_name).any(|(id, _)| id == agent_id) {
                return Ok((true, Plan::Nothing));
            }
            let Some((minutes, path)) = approach_point(walk.geo, &row, &rows, walk.model, agent_id, facility_id, service_point)? else {
                return Ok((true, Plan::Blocked));
            };
            if minutes <= 0 || path.len() < 2 {
                return Ok((false, Plan::Nothing));
            }
            let route = Route { agent_id: agent_id.clone(), index: 0, minutes, path };
            let sequence = events.next_sequence()?;
            let taken = events.is_taken(&trip_id(sequence), Some(sequence))?;
            Ok((true, plan_trip(events.duration(), taken, &row, &rows, facility_id, resource, route, minute, "service", fifo.as_ref(), sequence)?))
        })()?
    };
    write_plan(world, facility_id, plan)?;
    Ok(result)
}
