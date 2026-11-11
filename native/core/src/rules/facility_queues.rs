//! `domain/facility_queues.py`: the independent FIFO of each service point, on the row of
//! `facility_queues_v1` (lane c1 of Issue #152).
//!
//! The kernels read the row and write the row alone (`World::row_mut`): an attached namespace's
//! tree follows the row when the PyO3 crate gives the world back (lane P8-0c), and a detached one
//! has no tree. Where the Python rule raised its own ValueError, the kernel raises it with the
//! same text, before any write; what else it doesn't take says Unsupported before any write. The
//! pydantic contract of a transition's receipt (`FacilityQueueTransitionReceipt`), which only ever
//! saw the payload the kernel made, is a Rust check of the same fields (`receipt_takes`), whose
//! refusal keeps its text.
//!
//! What the PyO3 crate hands in: whether the Python world holds the namespace (`Held`), the floor
//! plan (`Geo`), what the configs say (a person's accessibility needs).

use std::collections::HashSet;
use std::sync::Arc;

use indexmap::IndexMap;

use crate::cell::{ShardMut, ShardRef};
use crate::entities::Activity;
use crate::error::{Error, Result};
use crate::events::writer::WriteResult;
use crate::events::{EventKind, EventRow, EventStatus};
use crate::geometry::{is_identifier, Geo, ServicePoint};
use crate::row::IdMap;
use crate::rules::kit::movement::KEY as MOVEMENT_KEY;
use crate::rules::kit::{held_row, mobility_text, Events, Held};
use crate::state::facility_queues::{QueueRequest, QueueRow, QueueStall, QueuesRow};
use crate::state::movement::MovementRow;
use crate::value::V;
use crate::world::World;

pub use crate::state::facility_queues::NAME as STATE_KEY;
pub const LEGACY_KEY: &str = "facility_queues_legacy_mode_v1";
pub const POLICY_VERSION: &str = "independent-facility-queues-v1";
pub const SOURCE_ID: &str = "ASM-FACILITY-QUEUES-V1";
pub const MAX_REQUESTS: usize = 100_000;
pub const NOT_INSTALLED_OR_TAKEN: &str = "service queue is not installed or request ID is already in use";

// ---- Reading -------------------------------------------------------------------------------------

/// The row, or None for a world without the namespace.
#[track_caller]
pub fn queues<'w>(world: &'w World, held: &Held) -> Result<Option<ShardRef<'w, QueuesRow>>> {
    held_row::<QueuesRow>(world, STATE_KEY, held)
}

/// `facility_has_independent_queues`.
pub fn has_independent(row: &QueuesRow, facility_id: &str) -> bool {
    row.queues.values().any(|queue| &*queue.facility_id == facility_id)
}

/// `queue_agent_ids`.
pub fn members_of(row: &QueuesRow, queue_id: &str) -> Result<Vec<Arc<str>>> {
    let Some(queue) = row.queues.get(queue_id) else {
        return Ok(Vec::new());
    };
    queue
        .queued_request_ids
        .iter()
        .map(|id| match row.requests.get(&**id) {
            Some(request) => Ok(request.agent_id.clone()),
            None => Err(Error::unsupported("a queue lists an unknown request")),
        })
        .collect()
}

/// `request_for_agent`.
pub fn request_of<'a>(row: &'a QueuesRow, agent_id: &str, queue_id: Option<&str>) -> Option<&'a QueueRequest> {
    row.requests.values().find(|request| &*request.agent_id == agent_id && queue_id.is_none_or(|queue| &*request.queue_id == queue))
}

/// `request_for_task`.
pub fn request_of_task<'a>(row: &'a QueuesRow, task_id: &str) -> Option<&'a QueueRequest> {
    row.requests.get(task_id).or_else(|| row.requests.values().find(|request| &*request.active_task_id == task_id))
}

/// `point_for_queue`.
pub fn point_of<'a>(geo: &'a Geo, queue_id: &str) -> Option<&'a ServicePoint> {
    geo.layout.as_ref()?.signed_service_points.iter().find(|point| &*point.queue_id == queue_id)
}

/// `queued_ids.index(request_id) + 1`, or None.
pub fn position_in(queue: &QueueRow, request_id: &str) -> Option<i64> {
    queue.queued_request_ids.iter().position(|id| &**id == request_id).map(|place| place as i64 + 1)
}

/// The heads of the facility's independent queues, or None for a facility without any.
pub fn independent_heads(row: &QueuesRow, facility_id: &str) -> Result<Option<Vec<Arc<str>>>> {
    if !has_independent(row, facility_id) {
        return Ok(None);
    }
    let mut heads = Vec::new();
    for queue in row.queues.values() {
        if &*queue.facility_id != facility_id {
            continue;
        }
        if let Some(head) = queue.queued_request_ids.first() {
            let Some(request) = row.requests.get(&**head) else {
                return Err(Error::unsupported("a queue lists an unknown request"));
            };
            heads.push(request.agent_id.clone());
        }
    }
    Ok(Some(heads))
}

/// `independent_heads` of a world: None also without the namespace.
pub fn heads_in(world: &World, held: &Held, facility_id: &str) -> Result<Option<Vec<Arc<str>>>> {
    match queues(world, held)? {
        Some(row) => independent_heads(&row, facility_id),
        None => Ok(None),
    }
}

/// `queue_head_agent_ids`: the heads of the independent queues, else the first of the facility's
/// shared resource queue.
pub fn head_agents(world: &World, held: &Held, facility_id: &str) -> Result<Vec<Arc<str>>> {
    if let Some(heads) = heads_in(world, held, facility_id)? {
        return Ok(heads);
    }
    let rows = world.rows()?;
    let Some(facility) = rows.facilities.get(facility_id) else {
        return Err(Error::unsupported("no such facility"));
    };
    Ok(facility.resource_queue_agent_ids.first().cloned().into_iter().collect())
}

/// `queue_ids_for_facility`: sorted by bytes.
pub fn queue_ids<'a>(geo: &'a Geo, facility_id: &str) -> Vec<&'a str> {
    let mut ids: Vec<&str> = geo
        .layout
        .iter()
        .flat_map(|layout| layout.signed_service_points.iter())
        .filter(|point| &*point.facility_id == facility_id)
        .map(|point| &*point.queue_id)
        .collect();
    ids.sort_unstable_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    ids
}

/// `create_facility_queue_state`, once the configs require independent queues: a queue per signed
/// point, by queue ID.
pub fn create_state(geo: &Geo) -> Result<QueuesRow> {
    let Some(layout) = &geo.layout else {
        return Err(Error::unsupported("no floor plan"));
    };
    let mut points: Vec<&ServicePoint> = layout.signed_service_points.iter().collect();
    points.sort_by(|first, second| first.queue_id.as_bytes().cmp(second.queue_id.as_bytes()));
    let mut queues = IdMap::default();
    for point in points {
        queues.insert(
            point.queue_id.clone(),
            QueueRow {
                facility_id: point.facility_id.clone(),
                service_point_id: point.id.clone(),
                queued_request_ids: Vec::new(),
                active_request_ids: Vec::new(),
                last_stall: None,
            },
        );
    }
    Ok(QueuesRow { schema_version: 1, policy_version: Arc::from(POLICY_VERSION), queues, requests: IdMap::default() })
}

/// `choose_queue_id`: among the facility's signed points open to someone of these needs, the queue
/// with the fewest requests, then the smaller ID. None without the namespace or the floor plan.
/// `needs` are the person's `accessibility_needs` in the configs.
pub fn choose_queue(world: &World, held: &Held, geo: &Geo, facility_id: &str, agent_id: &str, needs: &[Arc<str>]) -> Result<Option<Arc<str>>> {
    let loads: Vec<(Arc<str>, i64)> = {
        let Some(row) = queues(world, held)? else {
            return Ok(None);
        };
        row.queues.iter().map(|(id, queue)| (id.clone(), (queue.queued_request_ids.len() + queue.active_request_ids.len()) as i64)).collect()
    };
    let mut tags: HashSet<&str> = needs.iter().map(|need| &**need).collect();
    let rows = world.rows()?;
    if let Some(agent) = rows.agents.get(agent_id) {
        tags.insert(mobility_text(agent));
    }
    let Some(layout) = &geo.layout else {
        return Ok(None);
    };
    let mut best: Option<(i64, &Arc<str>)> = None;
    for point in &layout.signed_service_points {
        if &*point.facility_id != facility_id {
            continue;
        }
        if &*point.accessibility == "restricted" && !point.required_access_tags.iter().all(|tag| tags.contains(&**tag)) {
            continue;
        }
        let Some((_, load)) = loads.iter().find(|(id, _)| *id == point.queue_id) else {
            return Err(Error::unsupported("a signed point has no queue"));
        };
        if best.is_none_or(|(known, known_id)| (*load, point.queue_id.as_bytes()) < (known, known_id.as_bytes())) {
            best = Some((*load, &point.queue_id));
        }
    }
    Ok(best.map(|(_, id)| id.clone()))
}

// ---- Writing the row -----------------------------------------------------------------------------

/// `enqueue`'s writes, after its check: the request at the end of its point's queue.
pub fn enqueue_row(
    row: &mut QueuesRow,
    point: &ServicePoint,
    request_id: &str,
    task_id: &str,
    agent_id: &str,
    service_kind: &str,
    requested_minute: i64,
) -> Result<()> {
    if !row.queues.contains_key(&*point.queue_id) {
        return Err(Error::unsupported("no such queue"));
    }
    let id: Arc<str> = Arc::from(request_id);
    let task: Arc<str> = Arc::from(task_id);
    let record = QueueRequest {
        request_id: id.clone(),
        queue_id: point.queue_id.clone(),
        service_point_id: point.id.clone(),
        facility_id: point.facility_id.clone(),
        agent_id: Arc::from(agent_id),
        service_kind: Arc::from(service_kind),
        requested_minute,
        status: Arc::from("queued"),
        queue_task_id: task.clone(),
        active_task_id: task,
    };
    // `requests[request_id] = record`: a key it has keeps its place.
    row.requests.insert(id.clone(), record);
    if let Some(queue) = row.queues.get_mut(&*point.queue_id) {
        queue.queued_request_ids.push(id);
    }
    Ok(())
}

/// `start_service`'s writes, the request being one the queue holds: it goes from the queued to the
/// active list, and the queue's stall is cleared.
pub fn start_row(row: &mut QueuesRow, request_id: &str, queue_id: &str, active_task_id: &str) -> Result<()> {
    if !row.requests.contains_key(request_id) {
        return Err(Error::unsupported("no such request"));
    }
    let Some(queue) = row.queues.get_mut(queue_id) else {
        return Err(Error::unsupported("no such queue"));
    };
    // `list.remove`, which the callers have checked holds it.
    let Some(place) = queue.queued_request_ids.iter().position(|id| &**id == request_id) else {
        return Err(Error::Value("list.remove(x): x not in list".to_owned()));
    };
    let id = queue.queued_request_ids.remove(place);
    queue.active_request_ids.push(id);
    queue.last_stall = None;
    if let Some(request) = row.requests.get_mut(request_id) {
        request.status = Arc::from("active");
        request.active_task_id = Arc::from(active_task_id);
    }
    Ok(())
}

/// `finish_service`'s writes, for the request `request_of_task` found: it leaves its list and the
/// state, and the queue's stall is cleared.
pub fn finish_row(row: &mut QueuesRow, request: &QueueRequest) -> Result<()> {
    let Some(queue) = row.queues.get_mut(&*request.queue_id) else {
        return Err(Error::unsupported("no such queue"));
    };
    let list = if &*request.status == "queued" { &mut queue.queued_request_ids } else { &mut queue.active_request_ids };
    if let Some(place) = list.iter().position(|id| *id == request.request_id) {
        list.remove(place);
    }
    queue.last_stall = None;
    row.requests.shift_remove(&*request.request_id);
    Ok(())
}

/// `queued.remove(request_id)` and `queued.insert(0, request_id)` of a request just queued: it goes
/// back to the head of its queue.
pub fn to_head_row(row: &mut QueuesRow, queue_id: &str, request_id: &str) -> Result<()> {
    let Some(queue) = row.queues.get_mut(queue_id) else {
        return Err(Error::Key(queue_id.to_owned()));
    };
    let Some(place) = queue.queued_request_ids.iter().position(|id| &**id == request_id) else {
        return Err(Error::Value("list.remove(x): x not in list".to_owned()));
    };
    let id = queue.queued_request_ids.remove(place);
    queue.queued_request_ids.insert(0, id);
    Ok(())
}

/// `mark_stall`'s write: the queue's `last_stall`.
pub fn stall_row(row: &mut QueuesRow, queue_id: &str, stall: QueueStall) -> Result<()> {
    match row.queues.get_mut(queue_id) {
        Some(queue) => {
            queue.last_stall = Some(stall);
            Ok(())
        }
        None => Err(Error::unsupported("no such queue")),
    }
}

/// The row of the world, to write: its slot is marked, so an attached tree follows it.
fn row_mut(world: &World) -> Result<ShardMut<'_, QueuesRow>> {
    world.row_mut::<QueuesRow>(STATE_KEY)
}

/// `enqueue`'s writes, on the world's row.
pub fn write_enqueue(
    world: &World,
    point: &ServicePoint,
    request_id: &str,
    task_id: &str,
    agent_id: &str,
    service_kind: &str,
    requested_minute: i64,
) -> Result<()> {
    enqueue_row(&mut *row_mut(world)?, point, request_id, task_id, agent_id, service_kind, requested_minute)
}

/// `start_service`'s writes, on the world's row.
pub fn write_start(world: &World, request_id: &str, queue_id: &str, active_task_id: &str) -> Result<()> {
    start_row(&mut *row_mut(world)?, request_id, queue_id, active_task_id)
}

/// `finish_service`'s writes, on the world's row.
pub fn write_finish(world: &World, request: &QueueRequest) -> Result<()> {
    finish_row(&mut *row_mut(world)?, request)
}

/// The request back at the head of its queue, on the world's row.
pub fn write_to_head(world: &World, queue_id: &str, request_id: &str) -> Result<()> {
    to_head_row(&mut *row_mut(world)?, queue_id, request_id)
}

/// `mark_stall`'s write at the world's minute, on its row.
pub fn write_stall(world: &World, queue_id: &str, request_id: &str, reason: &str) -> Result<()> {
    let stall = QueueStall { minute: world.minute_index()?, request_id: Arc::from(request_id), reason: Arc::from(reason) };
    stall_row(&mut *row_mut(world)?, queue_id, stall)
}

// ---- The record of a transition ------------------------------------------------------------------

fn text(value: &str) -> V {
    V::Str(Arc::from(value))
}

/// The ID of the transition record of this sequence.
pub fn transition_id(sequence: i64) -> Arc<str> {
    Arc::from(format!("facility-queue-transition-{sequence}"))
}

/// What `FacilityQueueTransitionReceipt.model_validate` takes of the payload `transition_payload`
/// makes: its IDs are `Identifier`s, its kinds the contract's, its counts not negative (a place
/// from 1), the request not from the future, the wait within the time since it, and a queued or
/// requeued request with its place.
#[allow(clippy::too_many_arguments)]
pub fn receipt_takes(
    event_id: &str,
    request: &QueueRequest,
    transition: &str,
    task_id: &str,
    wait_minutes: Option<i64>,
    causation_id: Option<&str>,
    queue_position: Option<i64>,
    capacity: Option<i64>,
    minute: i64,
) -> bool {
    let ids = [event_id, &request.request_id, &request.queue_id, &request.service_point_id, &request.facility_id, &request.agent_id, task_id];
    let requested = request.requested_minute;
    ids.iter().all(|id| is_identifier(id))
        && causation_id.is_none_or(is_identifier)
        && matches!(&*request.service_kind, "toilet" | "water" | "meal")
        && matches!(transition, "queued" | "service_started" | "completed" | "cancelled" | "interrupted" | "requeued" | "abandoned")
        && minute >= 0
        && requested >= 0
        && queue_position.is_none_or(|place| place >= 1)
        && capacity.is_none_or(|capacity| capacity >= 0)
        && wait_minutes.is_none_or(|wait| wait >= 0)
        && requested <= minute
        && wait_minutes.is_none_or(|wait| wait <= minute - requested)
        && (queue_position.is_some() || !matches!(transition, "queued" | "requeued"))
}

/// What a transition's record says, in the receipt's order, changing nothing. `taken`: the log
/// holds a record of its ID or its sequence already.
#[allow(clippy::too_many_arguments)]
pub fn transition_payload(
    taken: bool,
    request: &QueueRequest,
    transition: &str,
    task_id: &str,
    wait_minutes: Option<i64>,
    causation_id: Option<&str>,
    queue_position: Option<i64>,
    capacity: Option<i64>,
    sequence: i64,
    minute: i64,
) -> Result<IndexMap<Arc<str>, V>> {
    if taken {
        return Err(Error::unsupported("the record's identity is taken"));
    }
    let event_id = transition_id(sequence);
    if !receipt_takes(&event_id, request, transition, task_id, wait_minutes, causation_id, queue_position, capacity, minute) {
        return Err(Error::unsupported("the receipt's contract refuses it"));
    }
    let option_int = |value: Option<i64>| value.map_or(V::Null, V::Int);
    let mut payload: IndexMap<Arc<str>, V> = IndexMap::with_capacity(18);
    payload.insert(Arc::from("schema_version"), V::Int(1));
    payload.insert(Arc::from("record_type"), text("operations"));
    payload.insert(Arc::from("event_type"), text("facility_queue_transition"));
    payload.insert(Arc::from("event_id"), V::Str(event_id));
    payload.insert(Arc::from("request_id"), V::Str(request.request_id.clone()));
    payload.insert(Arc::from("queue_id"), V::Str(request.queue_id.clone()));
    payload.insert(Arc::from("service_point_id"), V::Str(request.service_point_id.clone()));
    payload.insert(Arc::from("facility_id"), V::Str(request.facility_id.clone()));
    payload.insert(Arc::from("agent_id"), V::Str(request.agent_id.clone()));
    payload.insert(Arc::from("service_kind"), V::Str(request.service_kind.clone()));
    payload.insert(Arc::from("transition"), text(transition));
    payload.insert(Arc::from("minute"), V::Int(minute));
    payload.insert(Arc::from("requested_minute"), V::Int(request.requested_minute));
    payload.insert(Arc::from("task_id"), text(task_id));
    payload.insert(Arc::from("queue_position"), option_int(queue_position));
    payload.insert(Arc::from("service_capacity_concurrent"), option_int(capacity));
    payload.insert(Arc::from("wait_minutes"), option_int(wait_minutes));
    payload.insert(Arc::from("causation_id"), causation_id.map_or(V::Null, text));
    Ok(payload)
}

/// The record of `record_queue_transition` of a payload `transition_payload` made.
pub fn transition_event(payload: IndexMap<Arc<str>, V>, causation_id: Option<&str>, sequence: i64, minute: i64) -> Result<EventRow> {
    let mut provenance: IndexMap<Arc<str>, V> = IndexMap::with_capacity(3);
    provenance.insert(Arc::from("source_type"), text("assumption"));
    provenance.insert(Arc::from("source_id"), text(SOURCE_ID));
    provenance.insert(Arc::from("policy_version"), text(POLICY_VERSION));
    EventRow::new(
        transition_id(sequence),
        EventKind::Endogenous,
        minute,
        0,
        sequence,
        Arc::from("facility_queue_transition"),
        payload,
        Some(Arc::from(POLICY_VERSION)),
        causation_id.map(Arc::from),
        provenance,
    )
    .ok_or_else(|| Error::unsupported("the record's values are not what an event takes"))
}

/// The record `transition_event` made, in the log, with the sequence it takes.
pub fn write_transition(events: &Events<'_>, row: &EventRow, minute: i64) -> WriteResult<()> {
    events.allocate_sequence()?;
    events.record(row, minute, EventStatus::Applied, None)
}

/// What `record_queue_transition` reads before it writes.
pub struct Recording {
    pub sequence: i64,
    pub minute: i64,
    pub payload: IndexMap<Arc<str>, V>,
}

/// `record_queue_transition`, changing nothing: the payload of the record, with the place of the
/// request in its queue, the point's capacity and the sequence it will take.
#[allow(clippy::too_many_arguments)]
pub fn plan_record(
    world: &World,
    held: &Held,
    geo: &Geo,
    request: &QueueRequest,
    transition: &str,
    task_id: &str,
    wait_minutes: Option<i64>,
    causation_id: Option<&str>,
) -> Result<Recording> {
    let events = Events::of(world)?;
    let minute = world.minute_index()?;
    let position = {
        let Some(row) = queues(world, held)? else {
            return Err(Error::unsupported("no queue state"));
        };
        let Some(queue) = row.queues.get(&*request.queue_id) else {
            return Err(Error::unsupported("no such queue"));
        };
        position_in(queue, &request.request_id)
    };
    let capacity = point_of(geo, &request.queue_id).map(|point| point.service_capacity);
    let sequence = events.next_sequence()?;
    let taken = events.is_realized(&transition_id(sequence), Some(sequence))?;
    let payload = transition_payload(taken, request, transition, task_id, wait_minutes, causation_id, position, capacity, sequence, minute)?;
    Ok(Recording { sequence, minute, payload })
}

/// `record_queue_transition`'s writes, of what `plan_record` read.
pub fn write_record(world: &World, recording: Recording, causation_id: Option<&str>) -> WriteResult<()> {
    let events = Events::of(world)?;
    let row = transition_event(recording.payload, causation_id, recording.sequence, recording.minute)?;
    write_transition(&events, &row, recording.minute)
}

// ---- validate_facility_queue_state ---------------------------------------------------------------

/// The checks of one listed request, false where the Python rule raises.
#[allow(clippy::too_many_arguments)]
fn request_passes(
    rows: &crate::entities::Entities,
    row: &QueuesRow,
    point: &ServicePoint,
    queue_id: &Arc<str>,
    request_id: &Arc<str>,
    is_queued: bool,
    minute: i64,
    people: &mut HashSet<Arc<str>>,
) -> bool {
    let Some(request) = row.requests.get(&**request_id) else {
        return false;
    };
    let (Some(agent), Some(facility)) = (rows.agents.get(&request.agent_id), rows.facilities.get(&point.facility_id)) else {
        return false;
    };
    if request.request_id != *request_id
        || request.queue_id != *queue_id
        || request.facility_id != point.facility_id
        || request.service_point_id != point.id
        || !matches!(&*request.service_kind, "toilet" | "water" | "meal")
        || !(0..=minute).contains(&request.requested_minute)
        || &*request.status != (if is_queued { "queued" } else { "active" })
        || !people.insert(request.agent_id.clone())
    {
        return false;
    }
    let resource = matches!(&*request.service_kind, "water" | "meal");
    match &*request.service_kind {
        "water" if &*facility.kind != "water_station" => return false,
        "meal" if &*facility.kind != "meal_distribution" => return false,
        _ => {}
    }
    let agent_at = agent.target_id.as_deref() == Some(&*point.facility_id);
    if is_queued {
        let Some(task) = rows.active_tasks.get(&request.queue_task_id) else {
            return false;
        };
        let mirror_list = if resource { &facility.resource_queue_agent_ids } else { &facility.queue_agent_ids };
        return matches!(&*task.kind, "toilet_queue" | "resource_service_queue")
            && task.agent_id.as_ref() == Some(&request.agent_id)
            && task.resource_id.as_deref() == Some(&*point.facility_id)
            && agent.activity == Activity::Queued
            && agent_at
            && mirror_list.contains(&request.agent_id);
    }
    let Some(task) = rows.active_tasks.get(&request.active_task_id) else {
        return false;
    };
    let mirror_list = if resource { &facility.resource_active_agent_ids } else { &facility.active_agent_ids };
    if !matches!(&*task.kind, "toilet_service" | "resource_service_water" | "resource_service_food")
        || task.agent_id.as_ref() != Some(&request.agent_id)
        || task.resource_id.as_deref() != Some(&*point.facility_id)
        || agent.activity != Activity::InService
        || !agent_at
        || !mirror_list.contains(&request.agent_id)
    {
        return false;
    }
    if &*request.service_kind == "toilet" && (!point.required_staff_skills.is_empty() || task.staff_id.is_some()) {
        let Some(staff) = task.staff_id.as_deref().and_then(|id| rows.staff.get(id)) else {
            return false;
        };
        if point.required_staff_skills.is_empty()
            || !point.required_staff_skills.iter().all(|skill| staff.skills.0.contains(skill))
            || staff.zone_id != facility.zone_id
            || staff.active_task_id.as_deref() != Some(&*task.id)
            || staff.work_queue.len() != 1
            || staff.work_queue[0] != task.id
            || staff.busy_until != task.due_minute
        {
            return false;
        }
    }
    true
}

/// `validate_facility_queue_state` of a world that holds the namespace and requires independent
/// queues, without the legacy marker: true only for what the Python rule passes. `movement` is
/// whether the world holds `movement_v1`.
pub fn validate(world: &World, held: &Held, geo: &Geo, movement: &Held) -> Result<bool> {
    let Some(layout) = &geo.layout else {
        return Ok(false);
    };
    let minute = world.minute_index()?;
    let Some(row) = queues(world, held)? else {
        return Ok(false);
    };
    if row.schema_version != 1
        || &*row.policy_version != POLICY_VERSION
        || row.requests.len() > MAX_REQUESTS
        || row.queues.len() != layout.signed_service_points.len()
    {
        return Ok(false);
    }
    {
        let rows = world.rows()?;
        let mut people: HashSet<Arc<str>> = HashSet::new();
        let mut listed = 0;
        for (queue_id, queue) in row.queues.iter() {
            let Some(point) = layout.signed_service_points.iter().find(|point| point.queue_id == *queue_id) else {
                return Ok(false);
            };
            let queued: HashSet<&str> = queue.queued_request_ids.iter().map(|id| &**id).collect();
            let active: HashSet<&str> = queue.active_request_ids.iter().map(|id| &**id).collect();
            if queue.facility_id != point.facility_id
                || queue.service_point_id != point.id
                || queued.len() != queue.queued_request_ids.len()
                || active.len() != queue.active_request_ids.len()
                || !queued.is_disjoint(&active)
                || queue.active_request_ids.len() as i64 > point.service_capacity
            {
                return Ok(false);
            }
            if let Some(stall) = &queue.last_stall {
                if stall.minute < 0 || !matches!(&*stall.reason, "capacity" | "staff" | "stock" | "outage" | "route" | "run_end") {
                    return Ok(false);
                }
            }
            for request_id in queue.queued_request_ids.iter().chain(&queue.active_request_ids) {
                listed += 1;
                let is_queued = queued.contains(&**request_id);
                if !request_passes(&rows, &row, point, queue_id, request_id, is_queued, minute, &mut people) {
                    return Ok(false);
                }
            }
        }
        if listed != row.requests.len() {
            return Ok(false);
        }
    }
    // The journeys: each with a queue walks for a queued request of that queue.
    let Some(moving) = held_row::<MovementRow>(world, MOVEMENT_KEY, movement)? else {
        return Ok(true);
    };
    for (agent_id, journey) in moving.journeys.iter() {
        let Some(queue_id) = &journey.queue_id else {
            continue;
        };
        if request_of(&row, agent_id, Some(queue_id)).is_none_or(|request| &*request.status != "queued") {
            return Ok(false);
        }
    }
    Ok(true)
}
