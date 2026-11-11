//! The staffed water and meal service of `domain/resource_service.py` (lane c3 of Issue #152): the
//! queue's head starts its service (`serve`), a completion consumes the reservation (`complete`), a
//! person queues (`request`), and a service is cancelled or interrupted (`cancel`, `interrupt`).
//! Each kernel plans with nothing changed and then writes, on the world's rows alone: the entities,
//! `resource_service_v1`, `resource_ledger_v1`, `facility_queues_v1`, `metric_state`, `id_counters`,
//! the event queue and the EventLog. An attached tree follows its row when the borrow ends (lane
//! P8-0c), so a tainted namespace is refused.
//!
//! The PyO3 crate's file of the same name reads first what only Python holds (the manager's config,
//! whether the world holds each namespace, the floor plan, the pending reserve events, an answer of
//! a tainted `staff_operations_v1`) and turns what a kernel returns into Python: the new
//! `world.resources` of an operation the ledger took (`Applied`), and the `ResourceLedgerError` text
//! of one it turned down (`Failure::Ledger`), which names a value by Python's `repr`. The checks and
//! their texts are the Python rule's, in its order.

use std::sync::{Arc, LazyLock};

use indexmap::IndexMap;

use crate::cell::{ShardMut, ShardRef};
use crate::config::population::AccessibilityNeeds;
use crate::config::read;
use crate::entities::{Activity, TaskRow};
use crate::error::{Error, Result};
use crate::events::writer::{WriteError, WriteResult};
use crate::events::{is_stable_id, EventKind, EventRow, EventStatus};
use crate::geometry::Geo;
use crate::host::Host;
use crate::mathx::py_max;
use crate::row::{map_set, ValueRow};
use crate::rules::facility_queues::{self as fifo, point_of, position_in, request_of_task};
use crate::rules::kit::movement::config_read;
use crate::rules::kit::{add_metric, check_metric_state, held_row, kept, metrics_of, namespace_absent, payload, set_agent, Events, Held, Kept, NoTree};
use crate::rules::queues::{self as queues, Walk};
use crate::rules::resources::{apply_planned, Applied, Fail, LedgerFault, Operation};
use crate::rules::staff_dynamics;
use crate::state::facility_queues::QueueRequest;
use crate::state::resource_ledger::{resource_index, LedgerRow, NAME as LEDGER_KEY};
use crate::state::resource_service::{RequestRecord, ServiceRow, Stall, StallRecord, NAME as KEY};
use crate::text::py_repr;
use crate::value::V;
use crate::world::{set_id_counter, World, WorldCell, ID_COUNTERS};

const OPERATIONAL_KEY: &str = "operational_events_v1";
pub const WATER: &str = "water_ml";
pub const FOOD: &str = "food_meal_units";
/// The tasks `_agent_has_exclusive_task` looks for.
const EXCLUSIVE_TASKS: [&str; 8] = [
    "toilet_queue",
    "toilet_service",
    "resource_service_queue",
    "resource_service_water",
    "resource_service_food",
    "travel_to_toilet",
    "travel_to_resource",
    "travel_return",
];
/// The fields of a completion's payload, as `_assert_service_event_matches_task` wants them.
const COMPLETION_FIELDS: [&str; 7] = ["facility_id", "agent_id", "task_id", "resource_id", "reservation_id", "quantity", "staff_id"];

/// The fixed strings of the events, each shared by all their rows.
struct Names {
    facility_id: Arc<str>,
    agent_id: Arc<str>,
    task_id: Arc<str>,
    resource_id: Arc<str>,
    reservation_id: Arc<str>,
    quantity: Arc<str>,
    staff_id: Arc<str>,
    warehouse_id: Arc<str>,
    source_type: Arc<str>,
    source_id: Arc<str>,
    policy_version: Arc<str>,
    action: Arc<str>,
    service_minutes: Arc<str>,
    completion_type: Arc<str>,
    water: Arc<str>,
    food: Arc<str>,
    service_scheduled: Arc<str>,
}

static NAMES: LazyLock<Names> = LazyLock::new(|| Names {
    facility_id: Arc::from("facility_id"),
    agent_id: Arc::from("agent_id"),
    task_id: Arc::from("task_id"),
    resource_id: Arc::from("resource_id"),
    reservation_id: Arc::from("reservation_id"),
    quantity: Arc::from("quantity"),
    staff_id: Arc::from("staff_id"),
    warehouse_id: Arc::from("warehouse_id"),
    source_type: Arc::from("source_type"),
    source_id: Arc::from("source_id"),
    policy_version: Arc::from("policy_version"),
    action: Arc::from("action"),
    service_minutes: Arc::from("service_minutes"),
    completion_type: Arc::from("resource_service_complete"),
    water: Arc::from(WATER),
    food: Arc::from(FOOD),
    service_scheduled: Arc::from("service_scheduled"),
});

/// `resource_for_facility`.
pub fn resource_of(kind: &str) -> Option<&'static str> {
    match kind {
        "water_station" => Some(WATER),
        "meal_distribution" => Some(FOOD),
        _ => None,
    }
}

// ---- What the PyO3 crate reads first -------------------------------------------------------------

/// What a kernel takes of the manager and of the Python world, read before it borrows the world.
pub struct Manager<'a> {
    /// The manager's `config`: its dict, or the copy `ResourceServiceManager(world)` makes of the
    /// world's `model_configs["resource_service"]`.
    pub config: &'a IndexMap<Arc<str>, V>,
    /// Whether the world holds `resource_service_v1`, `resource_ledger_v1` and `facility_queues_v1`.
    pub service: Held,
    pub ledger: Held,
    pub queues: Held,
    /// The floor plan (`Geo::of`).
    pub geo: &'a Geo,
    /// Whether `world.metric_state` is its slot's tree.
    pub metric_state_held: bool,
}

impl Manager<'_> {
    fn field(&self, name: &str) -> Result<&V> {
        self.config.get(name).ok_or_else(|| Error::unsupported("the config lacks a field"))
    }

    fn text(&self, name: &str) -> Result<Arc<str>> {
        config_read(read::text(self.field(name)?).cloned())
    }

    fn integer(&self, name: &str) -> Result<i64> {
        config_read(read::int(self.field(name)?))
    }

    fn float(&self, name: &str) -> Result<f64> {
        config_read(read::float(self.field(name)?))
    }

    /// `config["water_ml_per_service"]` or the meal's.
    fn quantity(&self, resource_id: &str) -> Result<i64> {
        self.integer(if resource_id == WATER { "water_ml_per_service" } else { "food_meal_units_per_service" })
    }

    /// `resource_service_v1` as the PyO3 crate's `held_mirror` reads it, with its row.
    fn service<'w>(&self, world: &'w World) -> Result<ShardRef<'w, ServiceRow>> {
        held_service(world, &self.service)
    }

    /// Its row, to write: the slot is marked, so an attached tree follows it.
    fn service_mut<'w>(&self, world: &'w World) -> Result<ShardMut<'w, ServiceRow>> {
        drop(self.service(world)?);
        world.row_mut::<ServiceRow>(KEY)
    }

    /// `resource_ledger_v1`, with its row.
    fn ledger<'w>(&self, world: &'w World) -> Result<ShardRef<'w, LedgerRow>> {
        match held_row::<LedgerRow>(world, LEDGER_KEY, &self.ledger)? {
            Some(row) => Ok(row),
            None => Err(Error::unsupported("the world holds another ledger")),
        }
    }
}

/// `resource_service_v1` with its row: the world must hold it, untainted.
fn held_service<'w>(world: &'w World, held: &Held) -> Result<ShardRef<'w, ServiceRow>> {
    match held_row::<ServiceRow>(world, KEY, held)? {
        Some(row) => Ok(row),
        None => Err(Error::unsupported("the world holds another service state")),
    }
}

/// The `reservation_id` of a pending `resource_reserve` event, in the queue's order: what
/// `_reservation_id_is_in_use` reads of each. The PyO3 crate reads them from the queue's events.
#[derive(Clone, Debug, PartialEq)]
pub enum Pending {
    Id(Arc<str>),
    /// A value that is not a str: never equal to an ID.
    Other,
    /// No `reservation_id`.
    Missing,
}

/// Whether a pending `resource_reserve` event names this reservation.
fn pending_reserves(pending: &[Pending], reservation_id: &str) -> Result<bool> {
    for event in pending {
        match event {
            Pending::Id(id) if &**id == reservation_id => return Ok(true),
            Pending::Id(_) | Pending::Other => {}
            Pending::Missing => return Err(Error::unsupported("a pending reservation has no ID")),
        }
    }
    Ok(false)
}

// ---- Errors ----------------------------------------------------------------------------------------

/// Which operation the ledger turned down, whose rule wraps the ledger's text in its own error.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wrap {
    Reserve,
    Consume,
    Release,
}

impl Wrap {
    /// The rule's error around the ledger's text.
    pub fn error(self, text: &str) -> Error {
        match self {
            Wrap::Reserve => Error::invariant(format!("resource reservation failed atomically: {text}")),
            Wrap::Consume => Error::failed(format!("resource consume rejected: {text}")),
            Wrap::Release => Error::failed(format!("resource reservation release rejected: {text}")),
        }
    }
}

/// Why a kernel stopped: what the world or a write said, or the ledger's `ResourceLedgerError`
/// inside the rule's `except ValueError as exc`, after the sequences the Python rule had taken by
/// then. The PyO3 crate writes the ledger's text (it may name a value by Python's `repr`).
#[derive(Debug)]
pub enum Failure {
    Write(WriteError),
    Ledger(LedgerFault, Wrap),
}

impl From<Error> for Failure {
    fn from(error: Error) -> Self {
        Failure::Write(WriteError::Error(error))
    }
}

impl From<WriteError> for Failure {
    fn from(error: WriteError) -> Self {
        Failure::Write(error)
    }
}

pub type Outcome<T> = std::result::Result<T, Failure>;

/// The ledger's answer to a planned operation: the ledger's refusal takes the sequences the Python
/// rule had taken by then (`taken`), and anything else goes on as it is, with nothing taken.
fn ledger_answer(world: &mut World, planned: &Operation<'_>, taken: std::ops::RangeInclusive<i64>, wrap: Wrap) -> Outcome<Applied> {
    match apply_planned(world, planned) {
        Ok(applied) => Ok(applied),
        Err(Fail::Error(error)) => Err(error.into()),
        Err(Fail::Ledger(fault)) => {
            let events = Events::of(world)?;
            for _ in taken {
                events.allocate_sequence()?;
            }
            Err(Failure::Ledger(fault, wrap))
        }
    }
}

/// A write's error where the caller has only the core's errors: a duplicate event as the
/// `RuntimeError` it never is in a world the checks let through.
pub(crate) fn plain(error: WriteError) -> Error {
    match error {
        WriteError::Error(error) => error,
        WriteError::Duplicate(text) => Error::Runtime(text),
    }
}

// ---- Shared pieces -----------------------------------------------------------------------------

/// What `_provenance` and `_audit_event` read of the config.
struct Source {
    source_type: Arc<str>,
    source_id: Arc<str>,
    policy_version: Arc<str>,
}

impl Source {
    fn of(manager: &Manager<'_>) -> Result<Self> {
        let source = Self { source_type: manager.text("source_type")?, source_id: manager.text("source_id")?, policy_version: manager.text("policy_version")? };
        if !is_stable_id(&source.source_id) {
            return Err(Error::unsupported("the source is for the Python rule to judge"));
        }
        Ok(source)
    }

    fn provenance(&self, action: V) -> IndexMap<Arc<str>, V> {
        let names = &*NAMES;
        let mut provenance = IndexMap::with_capacity(5);
        provenance.insert(names.source_type.clone(), V::Str(self.source_type.clone()));
        provenance.insert(names.source_id.clone(), V::Str(self.source_id.clone()));
        provenance.insert(names.policy_version.clone(), V::Str(self.policy_version.clone()));
        provenance.insert(names.action.clone(), action);
        provenance
    }
}

/// `_audit_event` from the sequence given: the row of the first sequence whose identity is free.
/// The Python rule allocates each sequence it tries, so the caller allocates up to the row's. A
/// pending event's sequence is below the counter (`schedule_event` keeps it so), so only the IDs and
/// the log's sequences can be taken.
fn audit_row(
    events: &Events<'_>,
    source: &Source,
    operation: &'static str,
    first: i64,
    minute: i64,
    payload: IndexMap<Arc<str>, V>,
    causation_id: Arc<str>,
) -> Result<EventRow> {
    let mut sequence = first;
    let event_id: Arc<str> = loop {
        let event_id: Arc<str> = Arc::from(format!("resource-{operation}-{sequence}"));
        if !events.is_taken(&event_id, Some(sequence))? {
            break event_id;
        }
        sequence = sequence.checked_add(1).ok_or_else(|| Error::unsupported("a sequence past 64 bits"))?;
    };
    let provenance = source.provenance(V::Str(Arc::from(operation)));
    EventRow::new(
        event_id,
        EventKind::Endogenous,
        minute,
        0,
        sequence,
        Arc::from(format!("resource_{operation}")),
        payload,
        Some(source.source_id.clone()),
        Some(causation_id),
        provenance,
    )
    .ok_or_else(|| Error::unsupported("the event's values are not what an event takes"))
}

fn operation_payload(warehouse_id: &Arc<str>, resource_id: &Arc<str>, reservation_id: &Arc<str>, agent_id: &Arc<str>, quantity: i64) -> IndexMap<Arc<str>, V> {
    let names = &*NAMES;
    let mut payload = IndexMap::with_capacity(5);
    payload.insert(names.warehouse_id.clone(), V::Str(warehouse_id.clone()));
    payload.insert(names.resource_id.clone(), V::Str(resource_id.clone()));
    payload.insert(names.reservation_id.clone(), V::Str(reservation_id.clone()));
    payload.insert(names.agent_id.clone(), V::Str(agent_id.clone()));
    payload.insert(names.quantity.clone(), V::Int(quantity));
    payload
}

/// The record of `record_queue_transition`, changing nothing: `sequence` is the one it will take.
#[allow(clippy::too_many_arguments)]
pub(crate) fn transition_row(
    events: &Events<'_>,
    request: &QueueRequest,
    transition: &str,
    task_id: &str,
    wait_minutes: Option<i64>,
    causation_id: Option<&str>,
    queue_position: Option<i64>,
    capacity: Option<i64>,
    sequence: i64,
    minute: i64,
) -> Result<EventRow> {
    let taken = events.is_realized(&fifo::transition_id(sequence), Some(sequence))?;
    let payload = fifo::transition_payload(taken, request, transition, task_id, wait_minutes, causation_id, queue_position, capacity, sequence, minute)?;
    fifo::transition_event(payload, causation_id, sequence, minute)
}

/// The request of a task the queues hold, with its place in the list it is not removed from.
pub(crate) fn finishing_request(world: &World, held: &Held, task_id: &str) -> Result<Option<(QueueRequest, Option<i64>)>> {
    let Some(row) = fifo::queues(world, held)? else {
        return Ok(None);
    };
    let Some(request) = request_of_task(&row, task_id).cloned() else {
        return Ok(None);
    };
    let Some(queue) = row.queues.get(&*request.queue_id) else {
        return Err(Error::unsupported("no such queue"));
    };
    // In the list it isn't removed from, the record gives its place.
    let position = if &*request.status == "queued" { None } else { position_in(queue, &request.request_id) };
    Ok(Some((request, position)))
}

/// `list.remove(item)` of a list of IDs on a row, with Python's error for an item it lacks.
pub(crate) fn remove_from(list: &mut Vec<Arc<str>>, item: &str) -> Result<()> {
    match list.iter().position(|known| &**known == item) {
        Some(place) => {
            list.remove(place);
            Ok(())
        }
        None => Err(Error::Value("list.remove(x): x not in list".to_owned())),
    }
}

/// `del world.active_tasks[task_id]`.
pub(crate) fn remove_task(world: &World, task_id: &str) -> Result<()> {
    if world.rows_mut()?.active_tasks.remove_row(task_id) {
        Ok(())
    } else {
        Err(Error::Key(task_id.to_owned()))
    }
}

/// `_release_staff`.
pub(crate) fn release_staff(world: &World, staff_id: &str) -> Result<()> {
    let mut rows = world.rows_mut()?;
    let Some(row) = rows.staff.get_mut(staff_id) else {
        return Err(Error::unsupported("no such staff"));
    };
    row.work_queue.clear();
    row.active_task_id = None;
    row.busy_until = 0;
    Ok(())
}

/// `_record_queue_wait`: the wait counted under `key` (`toilet_wait`, `water_service_wait`, ...).
pub(crate) fn record_wait(world: &World, key: &str, wait: i64) -> Result<()> {
    let mut row = metrics_of(world)?;
    let (count, total, maximum) = (format!("{key}_count"), format!("{key}_total_minutes"), format!("{key}_max_minutes"));
    let value = row.get(&*count).copied().unwrap_or(0.0) + 1.0;
    let Ok(()) = map_set(&mut row, &NoTree, &count, value);
    let value = row.get(&*total).copied().unwrap_or(0.0) + wait as f64;
    let Ok(()) = map_set(&mut row, &NoTree, &total, value);
    let value = py_max(row.get(&*maximum).copied().unwrap_or(0.0), wait as f64);
    let Ok(()) = map_set(&mut row, &NoTree, &maximum, value);
    Ok(())
}

/// `_record_queue_stall`.
fn record_stall(world: &World, manager: &Manager<'_>, minute: i64, facility_id: &str, agent_id: &Arc<str>, reason: &str) -> Result<()> {
    let stall = Stall { agent_id: agent_id.clone(), reason: Arc::from(reason) };
    {
        let mut row = manager.service_mut(world)?;
        if row.stall_state.get(facility_id).is_some_and(|known| known.agent_id == stall.agent_id && known.reason == stall.reason) {
            return Ok(());
        }
        let facility: Arc<str> = Arc::from(facility_id);
        row.stall_state.insert(facility.clone(), stall);
        row.queue_stall_history.push(StallRecord { minute, facility_id: facility, agent_id: agent_id.clone(), reason: Arc::from(reason) });
    }
    add_metric(world, &format!("resource_service_wait_{reason}_count"))
}

/// `_clear_queue_stall`.
fn clear_stall(world: &World, manager: &Manager<'_>, facility_id: &str) -> Result<()> {
    let held = manager.service(world)?.stall_state.get(facility_id).is_some();
    if held {
        manager.service_mut(world)?.stall_state.shift_remove(facility_id);
    }
    Ok(())
}

/// `water_service_blocked`, of `operational_events_v1`.
fn water_is_blocked(world: &World, facility_id: &str, zone_id: &str) -> Result<bool> {
    let state = world.slots.try_value(OPERATIONAL_KEY)?;
    let outages = match state.as_deref() {
        None | Some(V::Null) => return Ok(false),
        Some(V::Map(state)) => match state.get("water_outages") {
            Some(V::Map(outages)) => outages,
            Some(_) => return Err(Error::unsupported("a value is not a plain dict")),
            None => return Err(Error::unsupported("no water_outages")),
        },
        Some(_) => return Err(Error::unsupported("a value is not a plain dict")),
    };
    for outage in outages.values() {
        let V::Map(outage) = outage else {
            return Err(Error::unsupported("a value is not a plain dict"));
        };
        let field = |name: &str| outage.get(name).ok_or_else(|| Error::unsupported("an outage lacks a field"));
        match field("active")? {
            V::Bool(true) => {}
            V::Bool(false) => continue,
            _ => return Err(Error::unsupported("active is not a bool")),
        }
        let kind = field("target_kind")?;
        let target = field("target_id")?;
        let (V::Str(kind), V::Str(target)) = (kind, target) else {
            return Err(Error::unsupported("a value is not a str"));
        };
        if (&**kind == "zone" && &**target == zone_id) || (&**kind == "facility" && &**target == facility_id) {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `physical_service_capacity`, for a facility's own queue: one at a facility with a place on the
/// floor plan of a placed world (`placement`: the plan, where the world holds `movement_v1` and
/// its configs have `facility_geometry`).
pub fn service_capacity(placement: Option<&Geo>, facility_id: &str, configured: i64) -> i64 {
    let placed = placement.is_some_and(|geo| geo.layout.as_ref().is_some_and(|layout| layout.facility_points.contains_key(facility_id)));
    if placed {
        configured.min(1)
    } else {
        configured
    }
}

/// `staff_dynamics_is_available` of one member: `idle` holds the answers the PyO3 crate read of a
/// tainted `staff_operations_v1`. None asks for the member's.
fn available_now(world: &World, staff_id: &str, minute: i64, idle: &[(Arc<str>, bool)]) -> Result<Option<bool>> {
    if namespace_absent(world, crate::state::staff_dynamics::NAME)? {
        return Ok(Some(true));
    }
    let given = idle.iter().find(|(id, _)| &**id == staff_id).map(|(_, idle)| *idle);
    staff_dynamics::is_available_now(world, staff_id, minute, given)
}

/// The first free member in ID order who is available now, as `staff_is_available` and
/// `staff_dynamics_is_available` say: `Err(id)` asks for the answer of a tainted
/// `staff_operations_v1` for that member.
pub(crate) fn first_available(world: &World, mut candidates: Vec<Arc<str>>, minute: i64, idle: &[(Arc<str>, bool)]) -> Result<std::result::Result<Option<Arc<str>>, Arc<str>>> {
    candidates.sort_unstable_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    // `staff_status(world)`, read once before the members, as the rule reads it.
    staff_dynamics::with_staff_status(world, |_| Ok(()))?;
    for candidate in candidates {
        if !staff_dynamics::with_staff_status(world, |status| staff_dynamics::is_available(status, &candidate))? {
            continue;
        }
        match available_now(world, &candidate, minute, idle)? {
            Some(true) => return Ok(Ok(Some(candidate))),
            Some(false) => {}
            None => return Ok(Err(candidate)),
        }
    }
    Ok(Ok(None))
}

// ---- arbitrate -----------------------------------------------------------------------------------

/// The facilities `arbitrate` has anything to do at, in ID order, as `(facility, resource_id)`:
/// someone in their own list, or in one of their independent queues. An empty queue does nothing.
pub fn facilities_to_serve(world: &World, queues: &Held) -> Result<Vec<(Arc<str>, &'static str)>> {
    let waiting: Vec<Arc<str>> = match fifo::queues(world, queues)? {
        Some(row) => row.queues.values().filter(|queue| !queue.queued_request_ids.is_empty()).map(|queue| queue.facility_id.clone()).collect(),
        None => Vec::new(),
    };
    let rows = world.rows()?;
    let mut due: Vec<(Arc<str>, &'static str)> = rows
        .facilities
        .iter()
        .filter(|(id, facility)| !facility.resource_queue_agent_ids.is_empty() || waiting.contains(id))
        .filter_map(|(id, facility)| resource_of(&facility.kind).map(|resource| (id.clone(), resource)))
        .collect();
    due.sort_unstable_by(|first, second| first.0.as_bytes().cmp(second.0.as_bytes()));
    Ok(due)
}

/// One pass of `arbitrate`'s loop after the positions: a facility's own queue, or one independent
/// queue of the facility.
pub struct Serve<'a> {
    pub facility_id: &'a str,
    pub resource_id: &'a str,
    pub minute: i64,
    pub queue_id: Option<&'a str>,
    /// The walk of `progress_physical_queue_head_to_service`, where the world is placed.
    pub walk: Option<&'a Walk<'a>>,
    /// The plan `physical_service_capacity` reads, where the world is placed.
    pub placement: Option<&'a Geo>,
    /// The answers of a tainted `staff_operations_v1` read so far.
    pub idle: &'a [(Arc<str>, bool)],
}

/// What a pass did.
#[derive(Debug, PartialEq)]
pub enum Served {
    /// The head's service started: the completion's ID, and the reservation's new balance.
    Started(Arc<str>, Applied),
    /// Nothing more this pass: the head waits (an independent queue's head says why), or it still
    /// walks to the service point.
    Stopped(Option<(Arc<str>, &'static str)>),
    /// Nothing written: the answer of a tainted `staff_operations_v1` for this member is needed.
    NeedsIdle(Arc<str>),
}

/// The head of an independent queue, read when a pass starts.
struct IndependentHead {
    request: QueueRequest,
    active_count: i64,
    point_capacity: i64,
    required_skills: Vec<Arc<str>>,
}

/// The start of a service, planned with nothing changed.
struct Start {
    completion: EventRow,
    completion_sequence: i64,
    audit: EventRow,
    /// The independent queue's request and its `service_started` record.
    transition: Option<(Arc<str>, EventRow)>,
    index: usize,
    warehouse: Arc<str>,
    source_id: Arc<str>,
    agent_id: Arc<str>,
    task_id: Arc<str>,
    staff_id: Arc<str>,
    quantity: i64,
    due: i64,
    wait: i64,
}

/// `_serve_head` and `_serve_independent_head`.
pub fn serve(world: &mut World, manager: &Manager<'_>, serve: &Serve<'_>, pending: &[Pending]) -> Outcome<Served> {
    let start = match plan_serve(world, manager, serve, pending)? {
        Ok(start) => start,
        Err(done) => return Ok(done),
    };
    // The ledger says last whether it takes it. It changes nothing when it doesn't, and the Python
    // rule had taken the completion's and the audit's sequences then.
    let applied = ledger_answer(
        world,
        &Operation {
            operation: "reserve",
            warehouse_id: &start.warehouse,
            resource_id: serve.resource_id,
            index: start.index,
            quantity: start.quantity,
            beyond: false,
            reservation_id: Some(&start.task_id),
            agent_id: Some(&start.agent_id),
            source_id: &start.source_id,
            event_id: &start.audit.event_id,
        },
        start.completion_sequence..=start.audit.sequence,
        Wrap::Reserve,
    )?;
    let completion_id = start.completion.event_id.clone();
    write_start(world, manager, serve, start)?;
    Ok(Served::Started(completion_id, applied))
}

/// `_serve_head`'s checks and plan, with nothing changed but what ends the pass: `Err` is the pass's
/// end (a recorded wait, a walk to the service point, or a question).
fn plan_serve(world: &World, manager: &Manager<'_>, serve: &Serve<'_>, pending: &[Pending]) -> Result<std::result::Result<Start, Served>> {
    let facility_name = serve.facility_id;
    let Some(index) = resource_index(serve.resource_id) else {
        return Err(Error::unsupported("an unknown resource"));
    };
    if serve.queue_id.is_none() && fifo::queues(world, &manager.queues)?.is_some_and(|row| fifo::has_independent(&row, facility_name)) {
        return Err(Error::unsupported("a facility with independent queues"));
    }
    let is_water = serve.resource_id == WATER;
    let minute = serve.minute;
    if minute != world.minute_index()? {
        return Err(Error::unsupported("another minute"));
    }
    manager.service(world)?;
    manager.ledger(world)?;
    check_metric_state(world, manager.metric_state_held)?;
    let events = Events::of(world)?;
    let head = match serve.queue_id {
        Some(queue_name) => {
            let Some(point) = point_of(manager.geo, queue_name) else {
                return Err(Error::unsupported("a queue with no signed point"));
            };
            let Some(row) = fifo::queues(world, &manager.queues)? else {
                return Err(Error::unsupported("no queue state"));
            };
            let Some(queue) = row.queues.get(queue_name) else {
                return Err(Error::unsupported("no such queue"));
            };
            let Some(first) = queue.queued_request_ids.first() else {
                return Err(Error::unsupported("the queue is empty"));
            };
            let Some(request) = row.requests.get(&**first) else {
                return Err(Error::unsupported("a queue lists an unknown request"));
            };
            if &*point.facility_id != facility_name {
                return Err(Error::unsupported("a queue's point is another facility's"));
            }
            Some(IndependentHead {
                request: request.clone(),
                active_count: queue.active_request_ids.len() as i64,
                point_capacity: point.service_capacity,
                required_skills: point.required_staff_skills.clone(),
            })
        }
        None => None,
    };

    // `_queue_task` and what is read of the facility.
    let (agent_id, task_id, task_start, zone_id, configured, active_count) = {
        let rows = world.rows()?;
        let Some(facility) = rows.facilities.get(facility_name) else {
            return Err(Error::unsupported("no such facility"));
        };
        let (agent_id, found) = match &head {
            Some(head) => {
                // `world.active_tasks.get(request["queue_task_id"])`, of the queue's kind.
                let Some(task) = rows.active_tasks.get(&head.request.queue_task_id).filter(|task| &*task.kind == "resource_service_queue") else {
                    return Err(Error::invariant("independent resource queue lost its active task"));
                };
                if !facility.resource_queue_agent_ids.contains(&head.request.agent_id) {
                    return Err(Error::unsupported("a request is missing from the facility's list"));
                }
                (&head.request.agent_id, Some((task.id.clone(), task.start_minute)))
            }
            None => {
                let Some(agent_id) = facility.resource_queue_agent_ids.first() else {
                    return Err(Error::unsupported("the queue is empty"));
                };
                let mut found = None;
                let mut count = 0;
                for (_, task) in rows.active_tasks.iter() {
                    if &*task.kind == "resource_service_queue" && task.agent_id.as_deref() == Some(&**agent_id) && task.resource_id.as_deref() == Some(facility_name) {
                        count += 1;
                        found = Some((task.id.clone(), task.start_minute));
                    }
                }
                if count != 1 {
                    return Err(Error::invariant(format!("queued Agent {} needs exactly one resource queue task", py_repr(agent_id))));
                }
                (agent_id, found)
            }
        };
        let Some((task_id, start)) = found else {
            return Err(Error::unsupported("a person with no queue task"));
        };
        (agent_id.clone(), task_id, start, facility.zone_id.clone(), facility.capacity.min(facility.working_units), facility.resource_active_agent_ids.len() as i64)
    };
    if !is_stable_id(&task_id) || !is_stable_id(&agent_id) {
        return Err(Error::unsupported("an ID is for the Python rule to judge"));
    }
    let (used, warehouse, available) = {
        let row = manager.ledger(world)?;
        let account = row.accounts.of(index);
        (
            row.used_reservation_ids.contains(&task_id),
            row.warehouse_id.clone(),
            account.remaining.checked_sub(account.reserved).ok_or_else(|| Error::unsupported("a balance past 64 bits"))?,
        )
    };
    // `_assert_service_task_id_available`: at once for a facility's own queue, and just before the
    // completion's identity for an independent one, as the Python rules check it.
    let taken_id = used || pending_reserves(pending, &task_id)?;
    let id_is_taken = || Error::invariant(format!("resource service task ID {} conflicts with an H01 reservation", py_repr(&task_id)));
    if taken_id && head.is_none() {
        return Err(id_is_taken());
    }

    let stop = |reason: &'static str| -> Result<std::result::Result<Start, Served>> {
        match (&head, serve.queue_id) {
            (Some(head), Some(queue_name)) => {
                fifo::write_stall(world, queue_name, &head.request.request_id, if reason == "water_outage" { "outage" } else { reason })?;
                Ok(Err(Served::Stopped(Some((agent_id.clone(), reason)))))
            }
            _ => {
                record_stall(world, manager, minute, facility_name, &agent_id, reason)?;
                Ok(Err(Served::Stopped(None)))
            }
        }
    };
    if is_water && water_is_blocked(world, facility_name, &zone_id)? {
        return stop("water_outage");
    }
    let at_capacity = match &head {
        Some(head) => head.active_count >= head.point_capacity || active_count >= configured,
        None => active_count >= service_capacity(serve.placement, facility_name, configured),
    };
    if at_capacity {
        return stop("capacity");
    }

    // The first free person in ID order.
    let skill = manager.text(if is_water { "water_staff_skill" } else { "meal_staff_skill" })?;
    let candidates: Vec<Arc<str>> = world
        .rows()?
        .staff
        .iter()
        .filter(|(_, person)| {
            person.zone_id == zone_id
                && person.skills.0.iter().any(|known| *known == skill)
                && head.as_ref().is_none_or(|head| head.required_skills.iter().all(|wanted| person.skills.0.contains(wanted)))
                && person.active_task_id.is_none()
        })
        .map(|(_, person)| person.id.clone())
        .collect();
    let staff_id = match first_available(world, candidates, minute, serve.idle)? {
        Ok(Some(staff_id)) => staff_id,
        Ok(None) => return stop("staff"),
        Err(asked) => return Ok(Err(Served::NeedsIdle(asked))),
    };
    if !is_stable_id(&staff_id) {
        return Err(Error::unsupported("an ID is for the Python rule to judge"));
    }
    let quantity = manager.quantity(serve.resource_id)?;
    if available < quantity {
        return stop("stock");
    }
    if quantity <= 0 {
        return Err(Error::unsupported("a quantity that is not positive"));
    }
    let service_minutes = manager.integer("service_minutes")?;
    let Some(due) = minute.checked_add(service_minutes) else {
        return Err(Error::unsupported("a due minute out of range"));
    };
    if due >= events.duration() {
        return stop("run_end");
    }
    // Writes only what ends this pass.
    if let Some(walk) = serve.walk {
        if queues::head_to_service(world, walk, facility_name, true, minute, serve.queue_id).map_err(plain)? {
            return Ok(Err(Served::Stopped(None)));
        }
    }

    // The start of the service: everything here is checked before the first write.
    let names = &*NAMES;
    let source = Source::of(manager)?;
    let resource_arc = if is_water { names.water.clone() } else { names.food.clone() };
    let completion_sequence = events.next_sequence()?;
    let completion_id: Arc<str> = Arc::from(format!("resource-service-complete-{completion_sequence}"));
    if taken_id {
        return Err(id_is_taken());
    }
    // `_ensure_event_identity_free`.
    if events.is_taken(&completion_id, Some(completion_sequence))? {
        return Err(Error::invariant(format!("resource event identity {} is not unique", py_repr(&completion_id))));
    }
    let mut payload = IndexMap::with_capacity(7);
    payload.insert(names.facility_id.clone(), V::Str(Arc::from(facility_name)));
    payload.insert(names.agent_id.clone(), V::Str(agent_id.clone()));
    payload.insert(names.task_id.clone(), V::Str(task_id.clone()));
    payload.insert(names.resource_id.clone(), V::Str(resource_arc.clone()));
    payload.insert(names.reservation_id.clone(), V::Str(task_id.clone()));
    payload.insert(names.quantity.clone(), V::Int(quantity));
    payload.insert(names.staff_id.clone(), V::Str(staff_id.clone()));
    let mut provenance = source.provenance(V::Str(names.service_scheduled.clone()));
    provenance.insert(names.service_minutes.clone(), V::Int(service_minutes));
    if let Some(queue_name) = serve.queue_id {
        provenance.insert(Arc::from("queue_id"), V::Str(Arc::from(queue_name)));
    }
    let Some(completion) = EventRow::new(
        completion_id.clone(),
        EventKind::Completion,
        due,
        0,
        completion_sequence,
        names.completion_type.clone(),
        payload,
        Some(task_id.clone()),
        Some(task_id.clone()),
        provenance,
    ) else {
        return Err(Error::unsupported("the event's values are not what an event takes"));
    };
    let audit = audit_row(&events, &source, "reserve", completion_sequence + 1, minute, operation_payload(&warehouse, &resource_arc, &task_id, &agent_id, quantity), task_id.clone())?;
    let wait = minute.checked_sub(task_start).ok_or_else(|| Error::unsupported("a start out of range"))?;
    if wait.abs() >= 1 << 53 {
        return Err(Error::unsupported("a wait too long for a float"));
    }
    // `record_queue_transition` after the start: the record takes the next sequence.
    let transition = match &head {
        Some(head) => {
            let row = transition_row(&events, &head.request, "service_started", &task_id, Some(wait), Some(&completion_id), None, Some(head.point_capacity), audit.sequence + 1, minute)?;
            Some((head.request.request_id.clone(), row))
        }
        None => None,
    };
    Ok(Ok(Start { completion, completion_sequence, audit, transition, index, warehouse, source_id: source.source_id, agent_id, task_id, staff_id, quantity, due, wait }))
}

/// `_serve_head`'s writes, once the ledger took the reservation.
fn write_start(world: &World, manager: &Manager<'_>, serve: &Serve<'_>, start: Start) -> WriteResult<()> {
    let Start { completion, completion_sequence, audit, transition, agent_id, task_id, staff_id, due, wait, .. } = start;
    let minute = serve.minute;
    let facility_id = serve.facility_id;
    let events = Events::of(world)?;
    for _ in completion_sequence..=audit.sequence {
        events.allocate_sequence()?;
    }
    events.record(&audit, minute, EventStatus::Applied, None)?;
    {
        let mut row = manager.service_mut(world)?;
        row.active_reservation_ids.push(task_id.clone());
        // `list.sort()` of strs, which orders them by code point, as their UTF-8 bytes do.
        row.active_reservation_ids.sort_unstable_by(|first, second| first.as_bytes().cmp(second.as_bytes()));
    }
    clear_stall(world, manager, facility_id)?;
    {
        let mut rows = world.rows_mut()?;
        let Some(facility) = rows.facilities.get_mut(facility_id) else {
            return Err(Error::unsupported("no such facility").into());
        };
        if serve.queue_id.is_some() {
            remove_from(&mut facility.resource_queue_agent_ids, &agent_id)?;
        } else {
            facility.resource_queue_agent_ids.remove(0);
        }
        facility.resource_active_agent_ids.push(agent_id.clone());
        let kind = if serve.resource_id == WATER { "resource_service_water" } else { "resource_service_food" };
        // The queue task's key, in its place.
        rows.active_tasks.insert(
            task_id.clone(),
            TaskRow {
                id: task_id.clone(),
                kind: Arc::from(kind),
                agent_id: Some(agent_id.clone()),
                resource_id: Some(Arc::from(facility_id)),
                start_minute: minute,
                due_minute: due,
                staff_id: Some(staff_id.clone()),
            },
        );
        let Some(row) = rows.staff.get_mut(&staff_id) else {
            return Err(Error::unsupported("no such staff").into());
        };
        row.active_task_id = Some(task_id.clone());
        row.busy_until = due;
        row.work_queue = vec![task_id.clone()];
        set_agent(&mut rows, &agent_id, Activity::InService, Some(facility_id))?;
    }
    events.schedule(completion)?;
    if let (Some(queue_name), Some((request_id, transition))) = (serve.queue_id, &transition) {
        fifo::write_start(world, request_id, queue_name, &task_id)?;
        fifo::write_transition(&events, transition, minute)?;
    }
    record_wait(world, if serve.resource_id == WATER { "water_service_wait" } else { "meal_service_wait" }, wait)?;
    Ok(())
}

/// `world.model_state["resource_service_v1"]["stall_state"].get(facility_id)`, from the row: the
/// stall a pass of an independent facility starts from.
pub fn previous_stall(world: &World, service: &Held, facility_id: &str) -> Result<Option<Stall>> {
    if *service == Held::Absent {
        return Err(Error::Key(KEY.to_owned()));
    }
    Ok(held_service(world, service)?.stall_state.get(facility_id).cloned())
}

/// `_record_independent_queue_stall`: keep a still-waiting head's record, else record the first
/// stalled queue's head.
pub fn record_independent_stall(world: &World, manager: &Manager<'_>, facility_id: &str, stalls: &[(Arc<str>, Arc<str>)], previous: Option<&Stall>) -> Result<()> {
    let Some((agent_id, reason)) = stalls.first() else {
        return Ok(());
    };
    if let Some(previous) = previous {
        if stalls.iter().any(|(agent, reason)| *agent == previous.agent_id && *reason == previous.reason) {
            manager.service_mut(world)?.stall_state.insert(Arc::from(facility_id), previous.clone());
            return Ok(());
        }
    }
    manager.service(world)?;
    check_metric_state(world, manager.metric_state_held)?;
    record_stall(world, manager, world.minute_index()?, facility_id, agent_id, reason)
}

// ---- complete ------------------------------------------------------------------------------------

/// `value == text` in Python, of a payload's value and an optional str.
fn same_text(value: &V, text: Option<&str>) -> bool {
    match (value, text) {
        (V::Str(value), Some(text)) => &**value == text,
        (V::Null, None) => true,
        _ => false,
    }
}

/// `value == number` in Python, of a payload's value and an int.
fn same_int(value: &V, number: i64) -> Result<bool> {
    Ok(match value {
        V::Int(value) => *value == number,
        V::Bool(flag) => i64::from(*flag) == number,
        V::Float(value) => {
            if !(-(1 << 53)..=1 << 53).contains(&number) {
                return Err(Error::unsupported("an int a float can't hold exactly"));
            }
            *value == number as f64
        }
        _ => false,
    })
}

static NULL: V = V::Null;

/// `event.payload.get(key)`: Null when it is missing, as `get` says None.
fn payload_get<'a>(event: &'a EventRow, key: &str) -> &'a V {
    payload(event, key).unwrap_or(&NULL)
}

/// `payload.get(key)` when it is a str.
pub(crate) fn payload_text(event: &EventRow, key: &str) -> Option<Arc<str>> {
    match payload(event, key) {
        Some(V::Str(text)) => Some(text.clone()),
        _ => None,
    }
}

/// `set(payload) == set(names)`.
fn payload_fields_are(event: &EventRow, names: &[&str]) -> bool {
    match &event.payload {
        V::Map(payload) => payload.len() == names.len() && payload.keys().all(|key| names.contains(&&**key)),
        _ => names.is_empty(),
    }
}

/// What `complete` writes, planned with nothing changed.
struct Completion {
    resource_id: &'static str,
    index: usize,
    is_water: bool,
    task_id: Arc<str>,
    agent_id: Arc<str>,
    staff_id: Arc<str>,
    facility_id: Arc<str>,
    warehouse: Arc<str>,
    source_id: Arc<str>,
    quantity: i64,
    relieved: f64,
    consume_sequence: i64,
    audit: EventRow,
    finished: Option<(QueueRequest, EventRow)>,
    minute: i64,
}

/// `ResourceServiceManager.complete`: the consumed reservation's new balance. Its checks are the
/// Python rule's, in its order, with its texts.
pub fn complete(world: &mut World, manager: &Manager<'_>, event: &EventRow) -> Outcome<Applied> {
    let plan = plan_complete(world, manager, event)?;
    let applied = ledger_answer(
        world,
        &Operation {
            operation: "consume",
            warehouse_id: &plan.warehouse,
            resource_id: plan.resource_id,
            index: plan.index,
            quantity: plan.quantity,
            beyond: false,
            reservation_id: Some(&plan.task_id),
            agent_id: Some(&plan.agent_id),
            source_id: &plan.source_id,
            event_id: &plan.audit.event_id,
        },
        plan.consume_sequence..=plan.audit.sequence,
        Wrap::Consume,
    )?;
    write_complete(world, manager, plan)?;
    Ok(applied)
}

fn plan_complete(world: &World, manager: &Manager<'_>, event: &EventRow) -> Result<Completion> {
    let Some(task_arc) = payload_text(event, "task_id") else {
        return Err(Error::failed("resource completion requires a task_id"));
    };
    let task_id = &*task_arc;
    manager.service(world)?;
    let task = world.rows()?.active_tasks.get(task_id).cloned();
    let Some(task) = task else {
        if manager.service(world)?.cancelled_task_ids.iter().any(|id| &**id == task_id) {
            return Err(Error::suppressed(format!("resource service task {} was cancelled", py_repr(task_id))));
        }
        return Err(Error::failed("resource completion does not match an active task"));
    };

    // `_assert_service_event_matches_task`.
    if event.kind != EventKind::Completion || &*event.event_type != "resource_service_complete" || !payload_fields_are(event, &COMPLETION_FIELDS) {
        return Err(Error::failed("resource completion payload or event kind is invalid"));
    }
    let given_resource = payload_get(event, "resource_id");
    let mut matches = matches!(&*task.kind, "resource_service_water" | "resource_service_food")
        && task.due_minute == event.due_minute
        && same_text(payload_get(event, "agent_id"), task.agent_id.as_deref())
        && same_text(payload_get(event, "staff_id"), task.staff_id.as_deref())
        && same_text(payload_get(event, "facility_id"), task.resource_id.as_deref())
        && same_text(payload_get(event, "reservation_id"), Some(&task.id));
    // `facilities[task.resource_id or ""]`, read only when all of the above match.
    let mut facility_view = None;
    if matches {
        let found = world.rows()?.facilities.get(task.resource_id.as_deref().unwrap_or("")).map(|facility| (facility.kind.clone(), facility.resource_active_agent_ids.clone()));
        let Some((kind, active)) = found else {
            return Err(Error::unsupported("no such facility"));
        };
        matches = same_text(given_resource, resource_of(&kind));
        facility_view = Some((kind, active));
    }
    if !matches {
        return Err(Error::failed("resource completion does not match its active task"));
    }
    let Some((kind, active_ids)) = facility_view else {
        return Err(Error::unsupported("no facility was read"));
    };
    // The resource is the facility's: `accounts[None]` would be a KeyError.
    let Some(resource_id) = resource_of(&kind) else {
        return Err(Error::unsupported("not a resource facility"));
    };
    let Some(index) = resource_index(resource_id) else {
        return Err(Error::unsupported("an unknown resource"));
    };
    let is_water = resource_id == WATER;
    let expected = manager.quantity(resource_id)?;
    let given_quantity = payload_get(event, "quantity");
    let staff_view = world.rows()?.staff.get(task.staff_id.as_deref().unwrap_or("")).map(|staff| (staff.active_task_id.clone(), staff.busy_until));
    let (reserved, warehouse) = {
        let row = manager.ledger(world)?;
        let reserved = row.accounts.of(index).reservations.get(&*task.id).is_some_and(|found| {
            found.reservation_id == task.id && &*found.resource_id == resource_id && Some(&found.agent_id) == task.agent_id.as_ref() && found.quantity == expected
        });
        (reserved, row.warehouse_id.clone())
    };
    let owned = manager.service(world)?.active_reservation_ids.iter().any(|id| *id == task.id);
    let full = task.agent_id.as_ref().is_some_and(|agent| active_ids.contains(agent))
        && staff_view.as_ref().is_some_and(|(active, busy)| active.as_deref() == Some(&*task.id) && *busy == task.due_minute)
        && same_int(given_quantity, expected)?
        && matches!(given_quantity, V::Int(_) | V::Big(_))
        && reserved
        && owned;
    if !full {
        return Err(Error::failed("resource service completion does not match its full reservation, slot, or staff"));
    }

    // What the writes below read, which the checks above have matched.
    let (Some(agent_id), Some(staff_id), Some(facility_id)) = (task.agent_id.clone(), task.staff_id.clone(), task.resource_id.clone()) else {
        return Err(Error::unsupported("the task's ownership is incomplete"));
    };
    // `payload["quantity"]`, which consumes: the service's quantity, as the checks above say.
    let quantity = match given_quantity {
        V::Int(given) if *given == expected => *given,
        V::Big(_) => return Err(Error::unsupported("an int is too large")),
        _ => return Err(Error::unsupported("not the service's quantity")),
    };
    if *task.id != *task_id || !is_stable_id(task_id) || !is_stable_id(&agent_id) {
        return Err(Error::unsupported("an ID is for the Python rule to judge"));
    }
    let event_id = event.event_id.clone();
    if !is_stable_id(&event_id) {
        return Err(Error::unsupported("an ID is for the Python rule to judge"));
    }
    if quantity <= 0 {
        return Err(Error::unsupported("not a positive quantity"));
    }
    let relief = manager.float(if is_water { "hydration_relief" } else { "hunger_relief" })?;
    check_metric_state(world, manager.metric_state_held)?;
    let events = Events::of(world)?;
    let minute = world.minute_index()?;
    let need = {
        let rows = world.rows()?;
        let Some(agent) = rows.agents.get(&agent_id) else {
            return Err(Error::unsupported("no such agent"));
        };
        let need = if is_water { agent.needs.hydration_need } else { agent.needs.hunger };
        if !need.is_finite() || !relief.is_finite() {
            return Err(Error::unsupported("a need is not finite"));
        }
        need
    };

    let source = Source::of(manager)?;
    let resource_arc = if is_water { NAMES.water.clone() } else { NAMES.food.clone() };
    let consume_sequence = events.next_sequence()?;
    let audit = audit_row(&events, &source, "consume", consume_sequence, minute, operation_payload(&warehouse, &resource_arc, &task_arc, &agent_id, quantity), event_id.clone())?;
    // `finish_service` and `record_queue_transition`, with the record's next sequence.
    let finished = match finishing_request(world, &manager.queues, task_id)? {
        Some((request, position)) => {
            let task_start = match world.rows()?.active_tasks.get(task_id) {
                Some(task) => task.start_minute,
                None => return Err(Error::unsupported("no active task")),
            };
            let wait = task_start.checked_sub(request.requested_minute).ok_or_else(|| Error::unsupported("a wait out of range"))?;
            let capacity = point_of(manager.geo, &request.queue_id).map(|point| point.service_capacity);
            let row = transition_row(&events, &request, "completed", task_id, Some(wait), Some(&event_id), position, capacity, audit.sequence + 1, minute)?;
            Some((request, row))
        }
        None => None,
    };
    manager.service(world)?;
    Ok(Completion {
        resource_id,
        index,
        is_water,
        task_id: task_arc.clone(),
        agent_id,
        staff_id,
        facility_id,
        warehouse,
        source_id: source.source_id,
        quantity,
        relieved: py_max(0.0, need - relief),
        consume_sequence,
        audit,
        finished,
        minute,
    })
}

fn write_complete(world: &World, manager: &Manager<'_>, plan: Completion) -> WriteResult<()> {
    let minute = plan.minute;
    let events = Events::of(world)?;
    for _ in plan.consume_sequence..=plan.audit.sequence {
        events.allocate_sequence()?;
    }
    events.record(&plan.audit, minute, EventStatus::Applied, None)?;
    remove_from(&mut manager.service_mut(world)?.active_reservation_ids, &plan.task_id)?;
    {
        let mut rows = world.rows_mut()?;
        let Some(row) = rows.agents.get_mut(&plan.agent_id) else {
            return Err(Error::unsupported("no such agent").into());
        };
        if plan.is_water {
            row.needs.hydration_need = plan.relieved;
        } else {
            row.needs.hunger = plan.relieved;
        }
    }
    add_metric(world, if plan.is_water { "water_service_completed_count" } else { "meal_service_completed_count" })?;
    if let Some((request, row)) = &plan.finished {
        fifo::write_finish(world, request)?;
        fifo::write_transition(&events, row, minute)?;
    }
    {
        let mut rows = world.rows_mut()?;
        let Some(facility) = rows.facilities.get_mut(&plan.facility_id) else {
            return Err(Error::unsupported("no such facility").into());
        };
        remove_from(&mut facility.resource_active_agent_ids, &plan.agent_id)?;
    }
    release_staff(world, &plan.staff_id)?;
    remove_task(world, &plan.task_id)?;
    set_agent(&mut *world.rows_mut()?, &plan.agent_id, Activity::Idle, None)?;
    add_metric(world, "resource_service_completed_count")?;
    Ok(())
}

// ---- request -------------------------------------------------------------------------------------

/// `record_request_outcome`: the record at the end of the service's `request_history`.
pub fn append_outcome(world: &World, service: &Held, record: RequestRecord) -> Result<()> {
    drop(held_service(world, service)?);
    world.row_mut::<ServiceRow>(KEY)?.request_history.push(record);
    Ok(())
}

/// The `minute` argument of `request`: none (or None), or an int and no bool, or anything else.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Minute {
    Absent,
    Int(i64),
    NotAnInt,
}

/// What `request` takes besides the manager.
pub struct Request<'a> {
    pub agent_id: &'a str,
    pub facility_id: &'a str,
    pub minute: Minute,
    /// The given `queue_id`, unless None.
    pub queue_id: Option<&'a str>,
    /// The population's accessibility needs (`choose_queue_id`'s), as the PyO3 crate read them.
    pub needs: &'a Kept<AccessibilityNeeds>,
}

/// `ResourceRequestReceipt`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Receipt {
    pub agent_id: Arc<str>,
    pub facility_id: Arc<str>,
    pub resource_id: &'static str,
    pub requested_at: i64,
    pub queue_task_id: Arc<str>,
    pub queue_position: i64,
}

/// What `request` did: the receipt, or the text of the `ValueError` the Python rule raises when the
/// queue turns the request down, having written what it writes before that.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Requested {
    Queued(Receipt),
    Refused(String),
}

/// The checks `request` makes before the world's validation: the minute, then what the kernel
/// writes to.
fn request_checks(world: &World, manager: &Manager<'_>, request: &Request<'_>) -> Result<Option<String>> {
    let world_minute = world.minute_index()?;
    match request.minute {
        Minute::Absent => {}
        Minute::NotAnInt => return Ok(Some("resource request minute must be an integer".to_owned())),
        Minute::Int(given) if given != world_minute => return Ok(Some("resource request minute must match WorldState".to_owned())),
        Minute::Int(_) => {}
    }
    world.rows()?;
    manager.service(world)?;
    Events::of(world)?;
    manager.ledger(world)?;
    world.id_counter("resource_task")?;
    Ok(None)
}

/// `ResourceServiceManager.request`: the world's validation before and after (`validates`), through
/// the host, with the world let go of around each call.
pub fn request<C: WorldCell, H: Host>(
    cell: &C,
    host: &mut H,
    manager: &Manager<'_>,
    request: &Request<'_>,
    pending: &[Pending],
    validates: bool,
) -> std::result::Result<Requested, H::Error> {
    if let Some(text) = cell.with(|world| request_checks(world, manager, request))?? {
        return Ok(Requested::Refused(text));
    }
    if validates {
        host.validate_world()?;
    }
    let requested = cell.with(|world| queue_up(world, manager, request, pending))??;
    if validates && matches!(requested, Requested::Queued(_)) {
        host.validate_world()?;
    }
    Ok(requested)
}

/// `request`'s rule without the world's validation: the checks of the person and the facility, in
/// the Python rule's order, then the queue.
pub fn queue_up(world: &World, manager: &Manager<'_>, request: &Request<'_>, pending: &[Pending]) -> Result<Requested> {
    if let Some(text) = request_checks(world, manager, request)? {
        return Ok(Requested::Refused(text));
    }
    let world_minute = world.minute_index()?;
    let (agent_name, facility_name) = (request.agent_id, request.facility_id);
    let mut counter = world.id_counter("resource_task")?.unwrap_or(0);
    // The Python rule's checks of the person and the facility, in its order.
    enum Turned {
        UnknownAgent,
        NotAFacility,
        Elsewhere,
        Active,
        Exclusive,
    }
    let checked = {
        let rows = world.rows()?;
        (|| {
            let Some(agent) = rows.agents.get(agent_name) else {
                return Err(Turned::UnknownAgent);
            };
            let Some((facility, resource)) = rows.facilities.get(facility_name).and_then(|facility| resource_of(&facility.kind).map(|resource| (facility, resource))) else {
                return Err(Turned::NotAFacility);
            };
            if agent.zone_id != facility.zone_id {
                return Err(Turned::Elsewhere);
            }
            if !matches!(agent.activity, Activity::Idle | Activity::Resting | Activity::Sleeping) {
                return Err(Turned::Active);
            }
            if rows.active_tasks.iter().any(|(_, task)| task.agent_id.as_deref() == Some(agent_name) && EXCLUSIVE_TASKS.contains(&&*task.kind)) {
                return Err(Turned::Exclusive);
            }
            for (_, other) in rows.facilities.iter() {
                let lists = [&other.queue_agent_ids, &other.active_agent_ids, &other.resource_queue_agent_ids, &other.resource_active_agent_ids];
                if lists.iter().any(|list| list.iter().any(|id| &**id == agent_name)) {
                    return Err(Turned::Exclusive);
                }
            }
            Ok((agent.zone_id.clone(), resource, facility.resource_queue_agent_ids.len() as i64 + 1))
        })()
    };
    let (zone, resource, shared_tail) = match checked {
        Ok(found) => found,
        Err(turned) => {
            return Ok(Requested::Refused(match turned {
                Turned::UnknownAgent => format!("unknown Agent {}", py_repr(agent_name)),
                Turned::NotAFacility => format!("{} is not a resource service facility", py_repr(facility_name)),
                Turned::Elsewhere => "Agent must arrive at the resource service facility before queueing".to_owned(),
                Turned::Active => format!("Agent {} cannot request resource service while active", py_repr(agent_name)),
                Turned::Exclusive => format!("Agent {} already has an exclusive task", py_repr(agent_name)),
            }))
        }
    };
    let independent = fifo::queues(world, &manager.queues)?.is_some_and(|row| fifo::has_independent(&row, facility_name));
    if request.queue_id.is_some() && !independent {
        return Err(Error::unsupported("a queue ID of a facility without independent queues"));
    }
    let chosen_queue: Option<Arc<str>> = match (independent, request.queue_id) {
        (false, _) => None,
        (true, Some(queue)) if !queue.is_empty() => Some(Arc::from(queue)),
        (true, _) => choose_queue(world, &manager.queues, manager.geo, facility_name, agent_name, request.needs)?,
    };
    let point = match &chosen_queue {
        Some(queue) => point_of(manager.geo, queue).filter(|point| &*point.facility_id == facility_name),
        None => None,
    };
    let task_id = next_queue_task_id(world, manager, pending, &mut counter)?;
    if independent && point.is_none() {
        // The Python rule takes the task's ID first, then finds no queue for the person.
        set_id_counter(&mut *world.row_mut::<ValueRow>(ID_COUNTERS)?, "resource_task", counter)?;
        return Ok(Requested::Refused("no accessible independent resource service queue is available".to_owned()));
    }
    let queue_position = match &chosen_queue {
        Some(queue) => match fifo::queues(world, &manager.queues)? {
            Some(row) => match row.queues.get(&**queue) {
                Some(queue) => queue.queued_request_ids.len() as i64 + 1,
                None => return Err(Error::unsupported("no such queue")),
            },
            None => return Err(Error::unsupported("no queue state")),
        },
        None => shared_tail,
    };
    let action = if resource == WATER { "fetch_water" } else { "collect_meal" };
    let service_kind = if resource == WATER { "water" } else { "meal" };
    let events = Events::of(world)?;
    let transition = match point {
        Some(point) => {
            let known = fifo::queues(world, &manager.queues)?.map(|row| row.requests.contains_key(&*task_id));
            if known != Some(false) {
                return Err(Error::unsupported("the request's ID is in use"));
            }
            let request = QueueRequest {
                request_id: task_id.clone(),
                queue_id: point.queue_id.clone(),
                service_point_id: point.id.clone(),
                facility_id: point.facility_id.clone(),
                agent_id: Arc::from(agent_name),
                service_kind: Arc::from(service_kind),
                requested_minute: world_minute,
                status: Arc::from("queued"),
                queue_task_id: task_id.clone(),
                active_task_id: task_id.clone(),
            };
            let sequence = events.next_sequence()?;
            Some(transition_row(&events, &request, "queued", &task_id, None, None, Some(queue_position), Some(point.service_capacity), sequence, world_minute)?)
        }
        None => None,
    };

    // Write.
    let agent_arc: Arc<str> = Arc::from(agent_name);
    let facility_arc: Arc<str> = Arc::from(facility_name);
    let written = (|| -> WriteResult<()> {
        set_id_counter(&mut *world.row_mut::<ValueRow>(ID_COUNTERS)?, "resource_task", counter)?;
        {
            let mut rows = world.rows_mut()?;
            let Some(facility) = rows.facilities.get_mut(facility_name) else {
                return Err(Error::unsupported("no such facility").into());
            };
            facility.resource_queue_agent_ids.push(agent_arc.clone());
        }
        if let Some(point) = point {
            fifo::write_enqueue(world, point, &task_id, &task_id, agent_name, service_kind, world_minute)?;
        }
        {
            let mut rows = world.rows_mut()?;
            rows.active_tasks.insert(
                task_id.clone(),
                TaskRow {
                    id: task_id.clone(),
                    kind: Arc::from("resource_service_queue"),
                    agent_id: Some(agent_arc.clone()),
                    resource_id: Some(facility_arc.clone()),
                    start_minute: world_minute,
                    due_minute: world_minute,
                    staff_id: None,
                },
            );
            set_agent(&mut rows, agent_name, Activity::Queued, Some(facility_name))?;
        }
        if let Some(transition) = &transition {
            fifo::write_transition(&events, transition, world_minute)?;
        }
        append_outcome(
            world,
            &manager.service,
            RequestRecord {
                minute: world_minute,
                agent_id: agent_arc.clone(),
                action: Arc::from(action),
                outcome: Arc::from("queued"),
                reason: Arc::from("arrived_at_facility"),
                facility_id: Some(facility_arc.clone()),
                origin_zone_id: zone,
                detail: None,
            },
        )?;
        Ok(())
    })();
    written.map_err(plain)?;
    Ok(Requested::Queued(Receipt {
        agent_id: agent_arc,
        facility_id: facility_arc,
        resource_id: resource,
        requested_at: world_minute,
        queue_task_id: task_id,
        queue_position,
    }))
}

/// `choose_queue_id`, with the population's accessibility needs as the PyO3 crate read them.
pub(crate) fn choose_queue(world: &World, queues: &Held, geo: &Geo, facility_id: &str, agent_id: &str, needs: &Kept<AccessibilityNeeds>) -> Result<Option<Arc<str>>> {
    if *queues == Held::Absent {
        return Ok(None);
    }
    let needs = config_read(kept(needs)?.of(agent_id))?;
    fifo::choose_queue(world, queues, geo, facility_id, agent_id, needs)
}

/// `_next_queue_task_id`: the ID, with `counter` left after it.
fn next_queue_task_id(world: &World, manager: &Manager<'_>, pending: &[Pending], counter: &mut i64) -> Result<Arc<str>> {
    loop {
        let candidate = format!("resource-service-task-{counter}");
        *counter = counter.checked_add(1).ok_or_else(|| Error::unsupported("a counter past 64 bits"))?;
        let is_task = world.rows()?.active_tasks.get(&candidate).is_some();
        let is_used = manager.ledger(world)?.used_reservation_ids.contains(&candidate);
        if !is_task && !is_used && !pending_reserves(pending, &candidate)? {
            return Ok(Arc::from(candidate));
        }
    }
}

// ---- cancel and interrupt_for_staff -------------------------------------------------------------

/// What `cancel` releases of a service under way.
struct Releasing {
    resource_id: &'static str,
    index: usize,
    staff_id: Arc<str>,
    warehouse: Arc<str>,
    source_id: Arc<str>,
    /// The first sequence `_audit_event` tried: it took each one up to the audit's.
    first: i64,
    audit: EventRow,
}

struct CancelPlan {
    task_id: Arc<str>,
    agent_id: Arc<str>,
    facility_id: Arc<str>,
    releasing: Option<Releasing>,
    finishing: Option<(QueueRequest, EventRow)>,
    sequences: i64,
}

/// `cancel` of a task, changing nothing, with the sequences it takes from `sequence`. What it
/// would turn down is the Python rule's, with its text.
fn plan_cancel(world: &World, manager: &Manager<'_>, event_id: &Arc<str>, task_id: &str, interrupted: bool, sequence: i64) -> Result<CancelPlan> {
    manager.service(world)?;
    let events = Events::of(world)?;
    manager.ledger(world)?;
    check_metric_state(world, manager.metric_state_held)?;
    let minute = world.minute_index()?;
    // The Python rule's checks, in its order, with its texts.
    if manager.service(world)?.cancelled_task_ids.iter().any(|id| &**id == task_id) {
        return Err(Error::suppressed(format!("resource service task {} was already cancelled", py_repr(task_id))));
    }
    enum Turned {
        NoQueueEntry,
        NotAResourceFacility,
        NoSlot,
    }
    #[allow(clippy::type_complexity)]
    let checked: std::result::Result<(TaskRow, Arc<str>, Option<(&'static str, bool)>), Turned> = {
        let rows = world.rows()?;
        let Some(task) = rows.active_tasks.get(task_id).filter(|task| matches!(&*task.kind, "resource_service_queue" | "resource_service_water" | "resource_service_food")) else {
            return Err(Error::failed("resource cancellation does not match an active task"));
        };
        // `agents[task.agent_id or ""]` and `facilities[task.resource_id or ""]`: a KeyError is the
        // Python rule's.
        let (Some(agent), Some(facility)) = (rows.agents.get(task.agent_id.as_deref().unwrap_or("")), rows.facilities.get(task.resource_id.as_deref().unwrap_or(""))) else {
            return Err(Error::unsupported("the task's person or facility is gone"));
        };
        let agent_id = agent.profile.id.clone();
        if &*task.kind == "resource_service_queue" {
            if facility.resource_queue_agent_ids.contains(&agent_id) {
                Ok((task.clone(), agent_id, None))
            } else {
                Err(Turned::NoQueueEntry)
            }
        } else {
            match resource_of(&facility.kind) {
                None => Err(Turned::NotAResourceFacility),
                Some(_) if !facility.resource_active_agent_ids.contains(&agent_id) => Err(Turned::NoSlot),
                Some(resource) => {
                    let staff_matches = rows
                        .staff
                        .get(task.staff_id.as_deref().unwrap_or(""))
                        .is_some_and(|staff| staff.active_task_id.as_deref() == Some(&*task.id) && staff.work_queue.len() == 1 && staff.work_queue[0] == task.id);
                    Ok((task.clone(), agent_id, Some((resource, staff_matches))))
                }
            }
        }
    };
    let (task, agent_id, active) = match checked {
        Ok(found) => found,
        Err(turned) => {
            return Err(Error::failed(match turned {
                Turned::NoQueueEntry => "queued resource Agent has no matching queue entry",
                Turned::NotAResourceFacility => "resource service task references a non-resource facility",
                Turned::NoSlot => "active resource Agent has no matching facility slot",
            }))
        }
    };
    let active = match active {
        None => None,
        Some((resource, staff_matches)) => {
            let Some(index) = resource_index(resource) else {
                return Err(Error::unsupported("an unknown resource"));
            };
            let reserved = manager.ledger(world)?.accounts.of(index).reservations.get(&*task.id).is_some_and(|found| found.agent_id == agent_id && &*found.resource_id == resource);
            let owned = manager.service(world)?.active_reservation_ids.iter().any(|id| *id == task.id);
            if !staff_matches || !reserved || !owned {
                return Err(Error::failed("active resource cancellation does not match reservation, slot, and staff"));
            }
            let Some(staff_id) = task.staff_id.clone() else {
                return Err(Error::unsupported("the service has no staff"));
            };
            Some((resource, index, staff_id))
        }
    };
    // What the writes read: the task's own ID, its person's, and its facility's.
    let Some(facility_id) = task.resource_id.clone() else {
        return Err(Error::unsupported("the task has incomplete ownership"));
    };
    if *task.id != *task_id || task.agent_id.as_ref() != Some(&agent_id) {
        return Err(Error::unsupported("the task's IDs are for the Python rule to judge"));
    }
    let task_arc: Arc<str> = Arc::from(task_id);
    let mut next = sequence;
    let releasing = match active {
        None => None,
        Some((resource_id, index, staff_id)) => {
            let warehouse = manager.ledger(world)?.warehouse_id.clone();
            let source = Source::of(manager)?;
            let resource_arc = if resource_id == WATER { NAMES.water.clone() } else { NAMES.food.clone() };
            let mut payload = IndexMap::with_capacity(4);
            payload.insert(NAMES.warehouse_id.clone(), V::Str(warehouse.clone()));
            payload.insert(NAMES.resource_id.clone(), V::Str(resource_arc));
            payload.insert(NAMES.reservation_id.clone(), V::Str(task_arc.clone()));
            payload.insert(NAMES.agent_id.clone(), V::Str(agent_id.clone()));
            let audit = audit_row(&events, &source, "release", next, minute, payload, event_id.clone())?;
            let first = next;
            next = audit.sequence + 1;
            Some(Releasing { resource_id, index, staff_id, warehouse, source_id: source.source_id, first, audit })
        }
    };
    let finishing = match finishing_request(world, &manager.queues, task_id)? {
        None => None,
        Some((request, position)) => {
            let capacity = point_of(manager.geo, &request.queue_id).map(|point| point.service_capacity);
            let transition = if interrupted { "interrupted" } else { "cancelled" };
            let record = transition_row(&events, &request, transition, task_id, None, Some(event_id), position, capacity, next, minute)?;
            next += 1;
            Some((request, record))
        }
    };
    Ok(CancelPlan { task_id: task_arc, agent_id, facility_id, releasing, finishing, sequences: next - sequence })
}

/// `cancel`'s release in the ledger, which alone can still say no. `taken` is the first sequence
/// the Python rule had taken by then: the interruption's cancellation, or the first the audit tried.
fn apply_release(world: &mut World, plan: &CancelPlan, taken: Option<i64>) -> Outcome<Option<Applied>> {
    let Some(releasing) = &plan.releasing else {
        return Ok(None);
    };
    ledger_answer(
        world,
        &Operation {
            operation: "release",
            warehouse_id: &releasing.warehouse,
            resource_id: releasing.resource_id,
            index: releasing.index,
            quantity: 0,
            beyond: false,
            reservation_id: Some(&plan.task_id),
            agent_id: Some(&plan.agent_id),
            source_id: &releasing.source_id,
            event_id: &releasing.audit.event_id,
        },
        taken.unwrap_or(releasing.first)..=releasing.audit.sequence,
        Wrap::Release,
    )
    .map(Some)
}

fn write_cancel(world: &World, manager: &Manager<'_>, plan: &CancelPlan) -> WriteResult<()> {
    let events = Events::of(world)?;
    let minute = world.minute_index()?;
    match &plan.releasing {
        None => {
            {
                let mut rows = world.rows_mut()?;
                let Some(facility) = rows.facilities.get_mut(&plan.facility_id) else {
                    return Err(Error::Runtime("a row has no object".to_owned()).into());
                };
                remove_from(&mut facility.resource_queue_agent_ids, &plan.agent_id)?;
            }
            clear_stall(world, manager, &plan.facility_id)?;
        }
        Some(releasing) => {
            for _ in releasing.first..=releasing.audit.sequence {
                events.allocate_sequence()?;
            }
            events.record(&releasing.audit, minute, EventStatus::Applied, None)?;
            remove_from(&mut manager.service_mut(world)?.active_reservation_ids, &plan.task_id)?;
            {
                let mut rows = world.rows_mut()?;
                let Some(facility) = rows.facilities.get_mut(&plan.facility_id) else {
                    return Err(Error::Runtime("a row has no object".to_owned()).into());
                };
                remove_from(&mut facility.resource_active_agent_ids, &plan.agent_id)?;
            }
            release_staff(world, &releasing.staff_id)?;
        }
    }
    if let Some((request, record)) = &plan.finishing {
        fifo::write_finish(world, request)?;
        fifo::write_transition(&events, record, minute)?;
    }
    remove_task(world, &plan.task_id)?;
    set_agent(&mut *world.rows_mut()?, &plan.agent_id, Activity::Idle, None)?;
    manager.service_mut(world)?.cancelled_task_ids.push(plan.task_id.clone());
    add_metric(world, "resource_service_cancelled_count")?;
    Ok(())
}

/// `ResourceServiceManager.cancel`: the reservation's new balance, if a service under way released one.
pub fn cancel(world: &mut World, manager: &Manager<'_>, event: &EventRow) -> Outcome<Option<Applied>> {
    let Some(task_id) = payload_text(event, "task_id").filter(|task_id| !task_id.is_empty()) else {
        return Err(Error::failed("resource cancellation requires a task_id").into());
    };
    let typed = payload_fields_are(event, &["task_id", "reason"])
        && match payload_get(event, "reason") {
            V::Str(reason) => matches!(&**reason, "operator_cancelled" | "facility_stopped" | "staff_unavailable"),
            // `in` a set hashes the value: a list or an object is a TypeError.
            V::List(_) | V::Map(_) => return Err(Error::unsupported("the reason is not hashable").into()),
            _ => false,
        };
    if !typed {
        return Err(Error::failed("resource cancellation payload is not a typed cancellation").into());
    }
    let interrupted = matches!(payload_get(event, "reason"), V::Str(reason) if &**reason == "staff_unavailable");
    let sequence = Events::of(world)?.next_sequence()?;
    let plan = plan_cancel(world, manager, &event.event_id, &task_id, interrupted, sequence)?;
    let applied = apply_release(world, &plan, None)?;
    write_cancel(world, manager, &plan)?;
    Ok(applied)
}

/// What `interrupt_for_staff` found the member serving: the task, its person, the facility, the
/// person's zone and the resource.
type Interrupted = (Arc<str>, Arc<str>, Arc<str>, Arc<str>, &'static str);

/// `interrupt_for_staff`'s checks of the member's service, in the Python rule's order, with its texts.
fn interrupted_service(world: &World, staff_id: &str) -> Result<Option<Interrupted>> {
    let rows = world.rows()?;
    let mut tasks = rows
        .active_tasks
        .iter()
        .filter(|(_, task)| task.staff_id.as_deref() == Some(staff_id) && matches!(&*task.kind, "resource_service_water" | "resource_service_food"))
        .map(|(_, task)| task);
    let Some(task) = tasks.next() else {
        return Ok(None);
    };
    if tasks.next().is_some() {
        return Err(Error::failed("unavailable staff owns multiple resource services"));
    }
    let (Some(agent_id), Some(facility_id)) = (&task.agent_id, &task.resource_id) else {
        return Err(Error::failed("interrupted resource service has incomplete ownership"));
    };
    let inconsistent = || Error::failed("interrupted resource task has inconsistent state");
    let (Some(agent), Some(facility), Some(staff)) = (rows.agents.get(agent_id), rows.facilities.get(facility_id), rows.staff.get(staff_id)) else {
        return Err(inconsistent());
    };
    if agent.zone_id != facility.zone_id
        || agent.activity != Activity::InService
        || staff.active_task_id.as_deref() != Some(&*task.id)
        || staff.work_queue.len() != 1
        || staff.work_queue[0] != task.id
        || staff.busy_until != task.due_minute
        || !facility.resource_active_agent_ids.contains(&agent.profile.id)
    {
        return Err(inconsistent());
    }
    let Some(resource) = resource_of(&facility.kind) else {
        return Err(Error::failed("interrupted resource service has an invalid facility"));
    };
    // The keys the rule goes on with must be the IDs it reads from the rows.
    if agent.profile.id != *agent_id || facility.id != *facility_id {
        return Err(Error::unsupported("a row's ID is not its key"));
    }
    Ok(Some((task.id.clone(), agent.profile.id.clone(), facility_id.clone(), agent.zone_id.clone(), resource)))
}

/// `interrupt_for_staff`: the reservation's new balance, if the member served anyone. `cause` is
/// the causation event's ID, None where it is not a str.
pub fn interrupt(world: &mut World, manager: &Manager<'_>, pending: &[Pending], staff_id: &str, cause: Option<&Arc<str>>) -> Outcome<Option<Applied>> {
    let Some((task_id, agent_id, facility_id, zone_id, resource)) = interrupted_service(world, staff_id)? else {
        return Ok(None);
    };
    // `accounts[resource_id]["reservations"].get(task.id)`, and the service's own list.
    let Some(index) = resource_index(resource) else {
        return Err(Error::unsupported("an unknown resource").into());
    };
    let reserved = manager.ledger(world)?.accounts.of(index).reservations.get(&*task_id).is_some_and(|found| found.agent_id == agent_id && &*found.resource_id == resource);
    let owned = manager.service(world)?.active_reservation_ids.iter().any(|id| *id == task_id);
    if !reserved || !owned {
        return Err(Error::failed("interrupted service reservation is inconsistent").into());
    }
    let prior = fifo::queues(world, &manager.queues)?.and_then(|row| request_of_task(&row, &task_id).cloned());
    let events = Events::of(world)?;
    manager.ledger(world)?;
    manager.service(world)?;
    let mut counter = world.id_counter("resource_task")?.unwrap_or(0);
    let queue_task_id = next_queue_task_id(world, manager, pending, &mut counter)?;
    let minute = world.minute_index()?;
    let queued_at = manager
        .service(world)?
        .request_history
        .iter()
        .rev()
        .find(|record| record.agent_id == agent_id && record.facility_id.as_deref() == Some(&*facility_id) && &*record.outcome == "queued")
        .map_or(minute, |record| record.minute);
    let sequence = events.next_sequence()?;
    let cancel_id: Arc<str> = Arc::from(format!("resource-service-cancel-{sequence}"));
    if events.is_taken(&cancel_id, Some(sequence))? {
        return Err(Error::unsupported("the event's identity is taken").into());
    }
    let source = Source::of(manager)?;
    let mut provenance = source.provenance(V::Str(Arc::from("staff_interrupted")));
    provenance.insert(NAMES.staff_id.clone(), V::Str(Arc::from(staff_id)));
    let mut payload = IndexMap::with_capacity(2);
    payload.insert(NAMES.task_id.clone(), V::Str(task_id.clone()));
    payload.insert(Arc::from("reason"), V::Str(Arc::from("staff_unavailable")));
    let Some(cause) = cause.cloned() else {
        return Err(Error::unsupported("a value is not a str").into());
    };
    let Some(cancellation) = EventRow::new(
        cancel_id.clone(),
        EventKind::Endogenous,
        minute,
        0,
        sequence,
        Arc::from("resource_service_cancel"),
        payload,
        Some(source.source_id.clone()),
        Some(cause.clone()),
        provenance,
    ) else {
        return Err(Error::unsupported("the event's values are not what an event takes").into());
    };
    let plan = plan_cancel(world, manager, &cancel_id, &task_id, true, sequence + 1)?;
    if plan.releasing.is_none() {
        return Err(Error::unsupported("the service has no reservation to release").into());
    }
    let requeue = match &prior {
        None => None,
        Some(prior) => {
            let Some(point) = point_of(manager.geo, &prior.queue_id) else {
                return Err(Error::unsupported("no such service point").into());
            };
            let fits = fifo::queues(world, &manager.queues)?.map(|row| !row.requests.contains_key(&*queue_task_id) && row.queues.contains_key(&*prior.queue_id));
            if fits != Some(true) {
                return Err(Error::unsupported("the queue is not installed or the request's ID is in use").into());
            }
            let request = QueueRequest {
                request_id: queue_task_id.clone(),
                queue_id: point.queue_id.clone(),
                service_point_id: point.id.clone(),
                facility_id: point.facility_id.clone(),
                agent_id: agent_id.clone(),
                service_kind: prior.service_kind.clone(),
                requested_minute: queued_at,
                status: Arc::from("queued"),
                queue_task_id: queue_task_id.clone(),
                active_task_id: queue_task_id.clone(),
            };
            // First in its queue by then.
            let record = transition_row(&events, &request, "requeued", &queue_task_id, None, Some(&cause), Some(1), Some(point.service_capacity), sequence + 1 + plan.sequences, minute)?;
            Some((point, record))
        }
    };
    drop(events);
    let applied = apply_release(world, &plan, Some(sequence))?;

    // Write.
    let world = &*world;
    let events = Events::of(world)?;
    events.allocate_sequence()?;
    write_cancel(world, manager, &plan)?;
    events.record(&cancellation, minute, EventStatus::Applied, None)?;
    set_agent(&mut *world.rows_mut()?, &agent_id, Activity::Queued, Some(&facility_id))?;
    set_id_counter(&mut *world.row_mut::<ValueRow>(ID_COUNTERS)?, "resource_task", counter)?;
    {
        let mut rows = world.rows_mut()?;
        rows.active_tasks.insert(
            queue_task_id.clone(),
            TaskRow {
                id: queue_task_id.clone(),
                kind: Arc::from("resource_service_queue"),
                agent_id: Some(agent_id.clone()),
                resource_id: Some(facility_id.clone()),
                start_minute: queued_at,
                due_minute: queued_at,
                staff_id: None,
            },
        );
        let Some(facility) = rows.facilities.get_mut(&facility_id) else {
            return Err(Error::Runtime("a row has no object".to_owned()).into());
        };
        facility.resource_queue_agent_ids.insert(0, agent_id.clone());
    }
    if let (Some(prior), Some((point, record))) = (&prior, &requeue) {
        fifo::write_enqueue(world, point, &queue_task_id, &queue_task_id, &agent_id, &prior.service_kind, queued_at)?;
        fifo::write_to_head(world, &prior.queue_id, &queue_task_id)?;
        fifo::write_transition(&events, record, minute)?;
    }
    clear_stall(world, manager, &facility_id)?;
    append_outcome(
        world,
        &manager.service,
        RequestRecord {
            minute,
            agent_id: agent_id.clone(),
            action: Arc::from(if resource == WATER { "fetch_water" } else { "collect_meal" }),
            outcome: Arc::from("queued"),
            reason: Arc::from("arrived_at_facility"),
            facility_id: Some(facility_id.clone()),
            origin_zone_id: zone_id,
            detail: Some(Arc::from("service_requeued_after_staff_unavailable")),
        },
    )?;
    Ok(applied)
}
