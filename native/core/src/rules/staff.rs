//! `domain/staff.py` and `domain/staff_operations.py` on the rows of a `World` (moved to the core
//! in P8 b3): the support requests' assignment (`StaffService.assign_pending`), the complaints that
//! come due (`_consider_due_complaints`), what the metrics read of each open request
//! (`_support_wait_snapshot`), the sanitation of the zones (`update_staff_operations`), the routed
//! tasks (`assign_staff_operations`) and their relief at a shift's boundary
//! (`schedule_staff_operation_relief`). The PyO3 crate's file of the same name reads what Python
//! holds (whether `model_state` holds each namespace, the floor plan, the sections of the configs,
//! the service's parameters, the router a caller gave) and calls these.
//!
//! Each plans with nothing changed and then writes: the entities, `staff_operations_v1`,
//! `metric_state`, the event counter, the queue and the log are written as their rows (the objects
//! and an attached tree follow them when the world is given back, lanes 6-1 and P8-0c).
//!
//! The service's route and trip are the controller's own `_route` and
//! `_schedule_staff_support_travel`, which are the core's `travel::route` and
//! `travel::schedule_travel`: they are called directly, in the order the Python rule called them
//! (a route records its capacity waits, a trip schedules its event), and their refusal is the
//! `KernelRefusal` their steps raised (`ROUTE`, `SCHEDULE`). Another route resolver or travel
//! scheduler than the controller's own is refused before anything is written. The decision on a
//! complaint is the policy's, through `Host::consider_complaint_response`, with the world given
//! back during the call (rule R6).

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::Arc;

use indexmap::IndexMap;
use rustc_hash::FxHashMap;

use crate::cell::ShardRef;
use crate::config::movement::Movement;
use crate::config::staff::StaffOperations;
use crate::entities::{Activity, RequestStatus, TaskRow};
use crate::error::{Error, Result};
use crate::events::writer::{WriteError, WriteResult};
use crate::events::{EventKind, EventStatus};
use crate::geometry::Geo;
use crate::host::Host;
use crate::mathx::{py_fsum, py_max, py_round_int};
use crate::row::{map_set, ValueRow};
use crate::routes::{EdgeLoads, Route, Router};
use crate::rules::kit::{self, Events, Held, Kept, NewEvent, NoTree};
use crate::rules::movement;
use crate::rules::staff_dynamics::{self, on_shift};
use crate::rules::staff_operation_events::{self as operations, relief_skills, Stopping, FORMULA_UNREAD};
use crate::rules::travel::{self, Routing, Trip, Walking, ZONES_TAINTED};
use crate::state::body_state::{BodyRow, NAME as BODY_STATE};
use crate::state::effect_program::{EffectProgramRow, NAME as PROGRAM};
use crate::state::shared_operations::{SharedOperationsRow, NAME as SHARED_OPERATIONS};
use crate::state::staff_dynamics::{StaffDynamicsRow, NAME as STAFF_DYNAMICS};
use crate::state::staff_operations::{OperationRecord, Phase, StaffOperationsRow, NAME};
use crate::text::g_text;
use crate::value::V;
use crate::world::{World, WorldCell, MODEL_CONFIGS};

const SUPPORT_SOURCE: &str = "ASM-STAFF-SUPPORT-LIFECYCLE-V1";
const PREEMPTION_SOURCE: &str = "ASM-URGENT-STAFF-SUPPORT-PREEMPTION-V1";
const PREEMPTION_POLICY: &str = "urgent-staff-support-preemption-v1";
const PREEMPTION_EVENT_SOURCE: &str = "urgent-resident-support-preemption-v1";
const PREEMPTION_DETAIL: &str = "A priority-1 support request preempted a routed background staff task.";
const RELIEF_SOURCE: &str = "ASM-STAFF-RELIEF-V1";
const RELIEF_POLICY: &str = "same-site-staff-relief-v1";
const INFRASTRUCTURE: &str = "infrastructure_v1";
const DEFAULT_KIND: &str = "emotional_support";
const WAIT_REASON: &str = "no_available_qualified_staff";
const NO_QUALIFIED_METRIC: &str = "support_no_qualified_worker_request_minutes";
const ATTEMPT_METRIC: &str = "support_assignment_metrics_minute";
const BUSY_METRIC: &str = "support_wait_staff_busy_request_minutes";
const OFF_SHIFT_METRIC: &str = "support_wait_staff_off_shift_request_minutes";
const PROMOTED_METRIC: &str = "support_fairness_promoted_assignment_count";
const CLEANING_BELOW: f64 = 0.85;
const OUTREACH_AT_LEAST: f64 = 0.45;
const PATROL_AT_LEAST: f64 = 0.6;
/// Above this an int does not always fit a float.
const FLOAT_EXACT_INT: i64 = 1 << 53;
/// The kinds of routed task, in the order `assign_staff_operations` reads their durations.
const TASK_KINDS: [&str; 5] = ["cleaning", "maintenance", "information_outreach", "intake", "patrol"];

/// The label of the `KernelRefusal` of the controller's `_route`.
pub const ROUTE: &str = "travel.route";
/// The label of the `KernelRefusal` of the controller's `_schedule_staff_support_travel`.
pub const SCHEDULE: &str = "travel.schedule_support_travel";
/// The label of the `KernelRefusal` of `interrupt_staff_operation`.
pub const INTERRUPT: &str = "staff_operation_events.interrupt";
/// The refusal of a route resolver that is not the controller's own `_route`.
pub const OTHER_RESOLVER: &str = "another route resolver";
/// The refusal of a travel scheduler that is not the controller's own `_schedule_staff_support_travel`.
pub const OTHER_SCHEDULER: &str = "another travel scheduler";

/// `_CARE_SKILL_ALTERNATIVES.get(kind, {"support"})`.
fn required_skills(kind: &str) -> &'static [&'static str] {
    match kind {
        "emotional_support" => &["support"],
        "medical_assessment" => &["medical", "nursing"],
        "medication_support" => &["medication", "nursing"],
        "caregiver_assistance" => &["caregiver", "support"],
        "dependent_assistance" => &["caregiver", "support", "nursing"],
        "privacy_accommodation" => &["privacy", "support"],
        "mobility_assistance" => &["mobility", "mobility_assistance", "support"],
        "hygiene_support" => &["hygiene", "support"],
        "language_access" => &["interpreter", "language_access"],
        _ => &["support"],
    }
}

// ---- What the rules read of the Python world -----------------------------------------------------

/// What the staff rules read of the Python world, read by the PyO3 crate before it borrows the
/// world. Each namespace is `Absent` where `model_state` has no such key or None there, and `Other`
/// where it holds another document than the row's, which a rule refuses where it reads it.
pub struct Inputs<'a> {
    /// `staff_operations_v1`.
    pub operations: &'a Held,
    /// `staff_dynamics_v1`.
    pub dynamics: &'a Held,
    /// `body_state_v1`.
    pub body: &'a Held,
    /// `shared_operations_v1`, whose heads say each request's kind of care.
    pub shared: &'a Held,
    /// Whether `world.metric_state` is still its slot's tree.
    pub metric_state_held: bool,
    /// What the rules of a task read (`staff_operation_events`): the interruption of a preempted
    /// task, the staff positions after a start, the facilities' modifiers, the routed formula. None
    /// where the entry reads none of them.
    pub tasks: Option<&'a operations::Inputs<'a>>,
}

impl Inputs<'_> {
    fn operations_row<'w>(&self, world: &'w World) -> Result<Option<ShardRef<'w, StaffOperationsRow>>> {
        kit::held_row::<StaffOperationsRow>(world, NAME, self.operations)
    }

    /// The row of `staff_operations_v1` of a rule that read the namespace's presence first.
    fn operations_held<'w>(&self, world: &'w World) -> Result<ShardRef<'w, StaffOperationsRow>> {
        self.operations_row(world)?.ok_or_else(|| Error::Runtime("staff_operations_v1 left under the rule".to_owned()))
    }

    fn tasks(&self) -> Result<&operations::Inputs<'_>> {
        self.tasks.ok_or_else(|| Error::Runtime("the inputs of the tasks were not given".to_owned()))
    }

    fn dynamics_row<'w>(&self, world: &'w World) -> Result<Option<ShardRef<'w, StaffDynamicsRow>>> {
        kit::held_row::<StaffDynamicsRow>(world, STAFF_DYNAMICS, self.dynamics)
    }
}

/// The IDs of `world.zones`, in its order.
fn world_zones(world: &World) -> Result<Vec<Arc<str>>> {
    let rows = world.entities.try_borrow()?;
    if rows.zones_tainted().is_some() {
        return Err(Error::unsupported(ZONES_TAINTED));
    }
    Ok(rows.zones.iter().map(|(id, _)| id.clone()).collect())
}

// ---- What the rules read about the staff ---------------------------------------------------------

/// What `staff_is_available` and `staff_dynamics_*` answer for one member at one minute, and what
/// `support_wait_reason_codes` reads from the same records.
struct Worker {
    id: Arc<str>,
    zone: Arc<str>,
    skills: Vec<Arc<str>>,
    has_task: bool,
    /// `staff_is_available`.
    available: bool,
    /// `staff_dynamics_on_shift`.
    on_shift: bool,
    /// `staff_dynamics_is_available`.
    can_start: bool,
    /// The `rest_until_minute` of its `staff_dynamics_v1` record, if it has one.
    rest_until: Option<i64>,
    /// The phase of its `staff_operations_v1` record is not idle.
    on_operation: bool,
}

/// Every staff member in the order of `world.staff`.
fn workers_at(world: &World, inputs: &Inputs<'_>, minute: i64) -> Result<Vec<Worker>> {
    let mut workers: Vec<Worker> = world
        .rows()?
        .staff
        .iter()
        .map(|(id, row)| Worker {
            id: id.clone(),
            zone: row.zone_id.clone(),
            skills: row.skills.0.clone(),
            has_task: row.active_task_id.is_some(),
            available: true,
            on_shift: true,
            can_start: true,
            rest_until: None,
            on_operation: false,
        })
        .collect();
    if let Some(row) = inputs.operations_row(world)? {
        for worker in &mut workers {
            worker.on_operation = row.staff.get(&worker.id).is_some_and(|record| record.phase != Phase::Idle);
        }
    }
    let has_dynamics = match inputs.dynamics_row(world)? {
        Some(row) => {
            for worker in &mut workers {
                let record = row.staff.get(&worker.id);
                worker.rest_until = record.map(|record| record.rest_until_minute);
                worker.on_shift = match record {
                    Some(record) => on_shift(&row.config, &worker.id, minute)? && minute >= record.rest_until_minute,
                    None => false,
                };
            }
            true
        }
        None => false,
    };
    staff_dynamics::with_staff_status(world, |status| {
        for worker in &mut workers {
            worker.available = staff_dynamics::is_available(status, &worker.id)?;
            worker.can_start = !has_dynamics || (worker.on_shift && worker.available && !worker.on_operation);
        }
        Ok(())
    })?;
    Ok(workers)
}

/// The kind of each request as `_support_kind` reads it, from the heads of `shared_operations_v1`:
/// `emotional_support` for a request without a head.
struct CareKinds(Option<FxHashMap<Arc<str>, Arc<str>>>);

impl CareKinds {
    fn of(world: &World, inputs: &Inputs<'_>) -> Result<Self> {
        let Some(row) = kit::held_row::<SharedOperationsRow>(world, SHARED_OPERATIONS, inputs.shared)? else {
            return Ok(Self(None));
        };
        Ok(Self(Some(row.support_requests.map.iter().map(|(id, head)| (id.clone(), head.support_kind.clone())).collect())))
    }

    fn kind_of(&self, id: &str) -> Arc<str> {
        match self.0.as_ref().and_then(|kinds| kinds.get(id)) {
            Some(kind) => kind.clone(),
            None => Arc::from(DEFAULT_KIND),
        }
    }
}

/// `model_state["llm"]["support_priorities"]`, where it is a non-empty dict: what
/// `_support_priority(world, id)` reads.
fn priorities_of(world: &World) -> Result<Option<IndexMap<Arc<str>, V>>> {
    let Some(state) = world.slots.try_value("llm")? else {
        return Ok(None);
    };
    let V::Map(state) = &*state else {
        return Ok(None);
    };
    let Some(V::Map(priorities)) = state.get("support_priorities") else {
        return Ok(None);
    };
    Ok((!priorities.is_empty()).then(|| priorities.clone()))
}

/// `_support_priority`: an int above 0 is urgent; a bool or another value is 0.
fn priority_of(priorities: Option<&IndexMap<Arc<str>, V>>, id: &str) -> Result<i64> {
    match priorities.and_then(|priorities| priorities.get(id)) {
        Some(V::Int(value)) => Ok(*value),
        Some(V::Big(_)) => Err(Error::unsupported("an int is too large")),
        _ => Ok(0),
    }
}

fn qualifies(worker: &Worker, kind: &str) -> bool {
    let required = required_skills(kind);
    worker.skills.iter().any(|skill| required.contains(&&**skill))
}

/// Of the qualified staff of a kind: none at all, one has a task, one is not working now. None if
/// one could take the request.
fn kind_exposure(workers: &[Worker], kind: &str) -> Option<(bool, bool, bool)> {
    let (mut any, mut busy, mut off_shift) = (false, false, false);
    for worker in workers {
        if qualifies(worker, kind) {
            if !worker.has_task && worker.available && worker.can_start {
                return None;
            }
            any = true;
            busy |= worker.has_task;
            off_shift |= !worker.on_shift || !worker.available;
        }
    }
    Some((!any, busy, off_shift))
}

/// `metric_state[name] = metric_state.get(name, 0.0) + count`, of the row.
fn add_to_metric(world: &World, name: &str, count: f64) -> Result<()> {
    let mut metrics = kit::metric_entries_of(world, &[name])?;
    let sum = metrics.get(name).copied().unwrap_or(0.0) + count;
    let Ok(()) = map_set(&mut metrics, &NoTree, name, sum);
    Ok(())
}

/// `metric_state[name] = value`, of the row.
fn set_metric(world: &World, name: &str, value: f64) -> Result<()> {
    let mut metrics = kit::metric_entries_of(world, &[name])?;
    let Ok(()) = map_set(&mut metrics, &NoTree, name, value);
    Ok(())
}

// ---- StaffService.assign_pending -----------------------------------------------------------------

/// What a route resolver or a travel scheduler of the service is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// None: no route is asked (the request is served where it stands), and no trip made.
    None,
    /// The controller's own step, which is the core's.
    Own,
    /// Another callable, which the core does not run.
    Other,
}

/// What `StaffService` gives `assign_pending`.
#[derive(Clone, Debug, PartialEq)]
pub struct Service {
    /// `policy_version`, the provenance of the support events.
    pub policy: Arc<str>,
    pub task_minutes: i64,
    pub fairness_after: i64,
    pub resolver: Step,
    pub scheduler: Step,
}

/// What the controller's own route and trip read beside the world.
#[derive(Clone, Copy)]
pub struct Travel<'a> {
    pub routing: Routing<'a>,
    pub walking: Walking<'a>,
}

/// One pending request, in the assignment's order.
struct Open {
    id: Arc<str>,
    agent_id: Arc<str>,
    requested: i64,
    kind: Arc<str>,
    priority: i64,
    same_reason: bool,
    same_minute: bool,
}

/// The minute's exposure of the plain case, summed as the Python rule sums it.
struct Exposure {
    no_qualified: f64,
    busy: f64,
    off_shift: f64,
}

/// What `assign_pending` read before it writes.
struct PendingPlan {
    minute: i64,
    ordered: Vec<Open>,
    workers: Vec<Worker>,
    record_metrics: bool,
    /// The plain case: every request stays pending, with the minute's exposure.
    plain: Option<Exposure>,
}

/// What `assign_pending` led to.
#[derive(Debug, PartialEq, Eq)]
pub enum Assigned {
    /// The IDs of the requests assigned, in order.
    Done(Vec<Arc<str>>),
    /// A request may be assigned: nothing is written, and the call again with what a request's
    /// assignment reads (`Full`) assigns.
    NeedsAll,
}

/// What the assignment of a request reads beside the plain case: the queue's rows and the floor
/// plan in the inputs of the tasks (`Inputs::tasks`, for an interrupted task), and the travel's
/// inputs where the service's route is the controller's.
#[derive(Clone, Copy)]
pub struct Full<'a> {
    pub travel: Option<Travel<'a>>,
}

/// The pending requests of the world (in the assignment's order with `fairness`), and whether
/// every one of them stays pending.
fn plan_pending(world: &World, inputs: &Inputs<'_>, fairness: Option<i64>, minute: i64) -> Result<PendingPlan> {
    let kinds = CareKinds::of(world, inputs)?;
    let priorities = priorities_of(world)?;
    let mut ordered = Vec::new();
    for (id, row) in world.rows()?.support_requests.iter() {
        if row.status != RequestStatus::Pending {
            continue;
        }
        ordered.push(Open {
            id: id.clone(),
            agent_id: row.agent_id.clone(),
            requested: row.requested_minute,
            kind: kinds.kind_of(id),
            priority: priority_of(priorities.as_ref(), id)?,
            same_reason: row.last_assignment_wait_reason_code.as_deref() == Some(WAIT_REASON),
            same_minute: row.last_assignment_attempt_minute == Some(minute),
        });
    }
    let workers = workers_at(world, inputs, minute)?;
    kit::check_metric_state(world, inputs.metric_state_held)?;
    let record_metrics = world.metric_state()?.get(ATTEMPT_METRIC).copied().unwrap_or(-1.0) != minute as f64;
    // The plain case: no request has a qualified free worker or a priority.
    let mut plain = Some(Exposure { no_qualified: 0.0, busy: 0.0, off_shift: 0.0 });
    let mut exposures: Vec<(Arc<str>, Option<(bool, bool, bool)>)> = Vec::new();
    for open in &ordered {
        let exposure = match exposures.iter().find(|(kind, _)| *kind == open.kind) {
            Some((_, exposure)) => *exposure,
            None => {
                let exposure = kind_exposure(&workers, &open.kind);
                exposures.push((open.kind.clone(), exposure));
                exposure
            }
        };
        let (Some(exposure), Some(sums), false) = (exposure, plain.as_mut(), open.priority > 0) else {
            plain = None;
            break;
        };
        sums.no_qualified += f64::from(u8::from(exposure.0));
        sums.busy += f64::from(u8::from(exposure.1));
        sums.off_shift += f64::from(u8::from(exposure.2));
    }
    if plain.is_some() && minute.abs() >= FLOAT_EXACT_INT {
        return Err(Error::unsupported("the minute does not fit a float"));
    }
    // `_support_assignment_order`: urgent or aged past fairness first, then by minute and ID.
    if let Some(fairness) = fairness {
        let key = |open: &Open| (-i64::from(open.priority > 0 || minute - open.requested >= fairness), open.requested);
        ordered.sort_by(|first, second| key(first).cmp(&key(second)).then_with(|| first.id.cmp(&second.id)));
    }
    Ok(PendingPlan { minute, ordered, workers, record_metrics, plain })
}

/// The writes of the plain case: each request records this attempt and its reason, and the
/// minute's exposure is counted once per minute.
fn write_plain(world: &World, plan: &PendingPlan, exposure: &Exposure) -> Result<()> {
    if plan.record_metrics {
        add_to_metric(world, NO_QUALIFIED_METRIC, exposure.no_qualified)?;
    }
    if plan.ordered.iter().any(|open| !(open.same_minute && open.same_reason)) {
        let mut rows = world.rows_mut()?;
        let reason: Arc<str> = Arc::from(WAIT_REASON);
        for open in &plan.ordered {
            if open.same_minute && open.same_reason {
                continue;
            }
            let Some(row) = rows.support_requests.get_mut(&open.id) else {
                return Err(Error::unsupported("a request has no row"));
            };
            row.last_assignment_attempt_minute = Some(plan.minute);
            if !open.same_reason {
                row.last_assignment_wait_reason_code = Some(reason.clone());
            }
        }
    }
    if plan.record_metrics {
        for (metric, count) in [(BUSY_METRIC, exposure.busy), (OFF_SHIFT_METRIC, exposure.off_shift)] {
            if count != 0.0 {
                add_to_metric(world, metric, count)?;
            }
        }
        set_metric(world, ATTEMPT_METRIC, plan.minute as f64)?;
    }
    Ok(())
}

#[track_caller]
fn left(what: &str) -> Error {
    Error::Runtime(format!("{what} left its row under the plan"))
}

/// `request.last_assignment_wait_reason_code = reason`.
fn set_reason(world: &World, request_id: &str, reason: &str) -> Result<()> {
    let mut rows = world.rows_mut()?;
    let row = rows.support_requests.get_mut(request_id).ok_or_else(|| left("a request"))?;
    row.last_assignment_wait_reason_code = Some(Arc::from(reason));
    Ok(())
}

/// The zones of a person and of a staff member now.
fn zones_of(world: &World, agent_id: &Arc<str>, staff_id: &str) -> Result<(Arc<str>, Arc<str>)> {
    let rows = world.rows()?;
    let Some(agent) = rows.agents.get(agent_id) else {
        // `world.agents[agent_id]`.
        return Err(Error::Key(agent_id.to_string()));
    };
    let staff = rows.staff.get(staff_id).ok_or_else(|| left("a staff member"))?;
    Ok((agent.zone_id.clone(), staff.zone_id.clone()))
}

/// A refusal of the controller's step as its `KernelRefusal`; any other error as it is.
fn step_error(label: &'static str, error: WriteError) -> WriteError {
    match error {
        WriteError::Error(error) => WriteError::Error(Error::refused(label, error)),
        duplicate => duplicate,
    }
}

/// `route_resolver(origin, destination, agent_id)`, which is the controller's `_route`.
fn resolve(world: &World, travel: Option<&Travel<'_>>, origin: &Arc<str>, destination: &Arc<str>, agent_id: &str) -> WriteResult<Option<Route>> {
    let travel = travel.ok_or_else(|| Error::Runtime("the route's inputs were not given".to_owned()))?;
    travel::route(world, &travel.routing, origin, destination, agent_id).map_err(|error| step_error(ROUTE, error))
}

/// `StaffService.assign_pending(minute, validate_state=False)`: the IDs of the requests assigned.
/// A minute other than the world's raises the rule's marked ValueError first. A world whose every
/// pending request stays pending records the attempt and the minute's exposure; otherwise, without
/// `full`, nothing is written and `NeedsAll` asks for it. Another route resolver or travel
/// scheduler is refused there, before anything is written.
pub fn assign_pending(world: &World, inputs: &Inputs<'_>, service: &Service, minute: i64, full: Option<&Full<'_>>, taken: &mut Vec<u64>) -> WriteResult<Assigned> {
    if minute != world.minute_index()? {
        // The rule's first check, before any change; marked, so that the step's catch lets it by.
        return Err(Error::MarkedValue("staff assignment minute must match WorldState".to_owned()).into());
    }
    let plan = plan_pending(world, inputs, Some(service.fairness_after), minute)?;
    if let Some(exposure) = &plan.plain {
        write_plain(world, &plan, exposure)?;
        return Ok(Assigned::Done(Vec::new()));
    }
    match (service.resolver, service.scheduler) {
        (Step::Other, _) => return Err(Error::unsupported(OTHER_RESOLVER).into()),
        (Step::Own, Step::Other) => return Err(Error::unsupported(OTHER_SCHEDULER).into()),
        _ => {}
    }
    let Some(full) = full else {
        return Ok(Assigned::NeedsAll);
    };
    let travel = match (service.resolver, &full.travel) {
        (Step::Own, None) => return Err(Error::Runtime("the travel's inputs were not given".to_owned()).into()),
        (_, travel) => travel.as_ref(),
    };
    let events = Events::of(world)?;
    assign_work(world, inputs, service, travel, &events, plan, taken).map(Assigned::Done)
}

/// The plain case of `assign_pending` alone: where every pending request stays pending at this
/// minute, its writes (each attempt and the minute's exposure), and true; else false with nothing
/// written. A minute other than the world's is not the plain case.
pub fn assign_plain(world: &World, inputs: &Inputs<'_>, minute: i64) -> Result<bool> {
    if minute != world.minute_index()? {
        return Ok(false);
    }
    let plan = plan_pending(world, inputs, None, minute)?;
    let Some(exposure) = &plan.plain else {
        return Ok(false);
    };
    write_plain(world, &plan, exposure)?;
    Ok(true)
}

/// `assign_pending` where a request may be taken.
fn assign_work(
    world: &World,
    inputs: &Inputs<'_>,
    service: &Service,
    travel: Option<&Travel<'_>>,
    events: &Events<'_>,
    plan: PendingPlan,
    taken: &mut Vec<u64>,
) -> WriteResult<Vec<Arc<str>>> {
    let PendingPlan { minute: at, ordered, workers, record_metrics, .. } = plan;
    // Sorted by ID: the workers free to start.
    let mut free: Vec<usize> = (0..workers.len()).filter(|index| !workers[*index].has_task && workers[*index].available && workers[*index].can_start).collect();
    free.sort_by(|first, second| workers[*first].id.cmp(&workers[*second].id));
    if record_metrics {
        let no_qualified = ordered.iter().filter(|open| !workers.iter().any(|worker| qualifies(worker, &open.kind))).count();
        add_to_metric(world, NO_QUALIFIED_METRIC, no_qualified as f64)?;
    }
    let (mut busy, mut off_shift) = (0_i64, 0_i64);
    let duration = world.duration_minutes()?;
    let task_minutes = service.task_minutes;
    let mut assigned: Vec<Arc<str>> = Vec::new();
    for open in ordered {
        {
            let mut rows = world.rows_mut()?;
            let row = rows.support_requests.get_mut(&open.id).ok_or_else(|| left("a request"))?;
            row.last_assignment_attempt_minute = Some(at);
            row.last_assignment_wait_reason_code = Some(Arc::from(WAIT_REASON));
        }
        let mut person = free.iter().copied().find(|index| qualifies(&workers[*index], &open.kind));
        if person.is_none() && open.priority > 0 {
            person = preempt_worker(world, inputs, service, travel, events, &workers, &open, at, duration, taken)?;
            if let Some(index) = person {
                free.push(index);
            }
        }
        let Some(person) = person else {
            if record_metrics {
                let rows = world.rows()?;
                let (mut any_busy, mut any_away) = (false, false);
                for worker in &workers {
                    if qualifies(worker, &open.kind) {
                        any_busy |= rows.staff.get(&worker.id).is_some_and(|staff| staff.active_task_id.is_some());
                        any_away |= !worker.on_shift || !worker.available;
                    }
                }
                busy += i64::from(any_busy);
                off_shift += i64::from(any_away);
            }
            continue;
        };
        let staff_id = workers[person].id.clone();
        let mut route = None;
        let mut travel_minutes = 0_i64;
        if service.resolver != Step::None {
            let (agent_zone, staff_zone) = zones_of(world, &open.agent_id, &staff_id)?;
            route = resolve(world, travel, &agent_zone, &staff_zone, &open.agent_id)?;
            let Some(found) = &route else {
                set_reason(world, &open.id, "route_unavailable_at_assignment")?;
                continue;
            };
            travel_minutes = found.minutes;
        } else if !world.rows()?.agents.contains_key(&open.agent_id) {
            return Err(Error::Key(open.agent_id.to_string()).into());
        }
        let due = at + travel_minutes + task_minutes;
        if due >= duration {
            set_reason(world, &open.id, "insufficient_run_time")?;
            continue;
        }
        let sequence = events.allocate_sequence()?;
        if travel_minutes > 0 {
            let scheduled = match (service.scheduler, route, travel) {
                (Step::Own, Some(route), Some(travel)) => {
                    let trip = Trip {
                        agent_id: open.agent_id.clone(),
                        route,
                        points: Vec::new(),
                        purpose: "staff_support_travel",
                        facility_id: None,
                        minute: at,
                        causation_id: Some(open.id.clone()),
                        support_request_id: Some(open.id.clone()),
                        support_staff_id: Some(staff_id.clone()),
                    };
                    travel::schedule_travel(world, travel.walking, trip).map_err(|error| step_error(SCHEDULE, error))?
                }
                _ => false,
            };
            if !scheduled {
                set_reason(world, &open.id, "resident_travel_schedule_rejected")?;
                continue;
            }
        }
        let task_id: Arc<str> = Arc::from(format!("staff-support-{sequence}"));
        let promoted = open.priority == 0 && at - open.requested >= service.fairness_after;
        let mut payload = IndexMap::with_capacity(5);
        payload.insert(Arc::from("request_id"), V::Str(open.id.clone()));
        payload.insert(Arc::from("agent_id"), V::Str(open.agent_id.clone()));
        payload.insert(Arc::from("staff_id"), V::Str(staff_id.clone()));
        payload.insert(Arc::from("task_id"), V::Str(task_id.clone()));
        payload.insert(Arc::from("support_duration_minutes"), V::Int(task_minutes));
        let mut provenance = kit::assumption(V::Str(Arc::from(SUPPORT_SOURCE)), &service.policy);
        provenance.insert(Arc::from("assignment_wait_minutes"), V::Int(at - open.requested));
        provenance.insert(Arc::from("fairness_promoted"), V::Bool(promoted));
        let event = kit::new_event(NewEvent {
            event_id: Arc::from(format!("staff-support-complete-{sequence}")),
            kind: EventKind::Completion,
            due_minute: due,
            priority: 0,
            sequence,
            event_type: Arc::from("staff_support_complete"),
            payload,
            source_id: Some(task_id.clone()),
            causation_id: Some(open.id.clone()),
            provenance,
        })?;
        {
            let mut rows = world.rows_mut()?;
            rows.active_tasks.insert(
                task_id.clone(),
                TaskRow {
                    id: task_id.clone(),
                    kind: Arc::from("staff_support"),
                    agent_id: Some(open.agent_id.clone()),
                    resource_id: None,
                    start_minute: at + travel_minutes,
                    due_minute: due,
                    staff_id: Some(staff_id.clone()),
                },
            );
            let staff = rows.staff.get_mut(&staff_id).ok_or_else(|| left("a staff member"))?;
            staff.active_task_id = Some(task_id.clone());
            staff.work_queue = vec![task_id.clone()];
            staff.busy_until = due;
            let request = rows.support_requests.get_mut(&open.id).ok_or_else(|| left("a request"))?;
            request.status = RequestStatus::Assigned;
            request.assigned_staff_id = Some(staff_id.clone());
            request.task_id = Some(task_id.clone());
            request.last_assignment_wait_reason_code = None;
        }
        events.schedule(event)?;
        if promoted {
            add_to_metric(world, PROMOTED_METRIC, 1.0)?;
        }
        assigned.push(open.id.clone());
        free.retain(|index| *index != person);
    }
    if record_metrics {
        for (metric, count) in [(BUSY_METRIC, busy), (OFF_SHIFT_METRIC, off_shift)] {
            if count != 0 {
                add_to_metric(world, metric, count as f64)?;
            }
        }
        set_metric(world, ATTEMPT_METRIC, at as f64)?;
    }
    Ok(assigned)
}

/// `_preempt_background_staff`: frees one qualified routed worker for an urgent request.
#[allow(clippy::too_many_arguments)]
fn preempt_worker(
    world: &World,
    inputs: &Inputs<'_>,
    service: &Service,
    travel: Option<&Travel<'_>>,
    events: &Events<'_>,
    workers: &[Worker],
    open: &Open,
    at: i64,
    duration: i64,
    taken: &mut Vec<u64>,
) -> WriteResult<Option<usize>> {
    // `model_state.get(NAME)`: no preemption without the namespace.
    let Some(phases) = inputs.operations_row(world)?.map(|row| row.staff.iter().map(|(id, record)| (id.clone(), record.phase)).collect::<FxHashMap<_, _>>()) else {
        return Ok(None);
    };
    let mut order: Vec<usize> = (0..workers.len()).collect();
    order.sort_by(|first, second| workers[*first].id.cmp(&workers[*second].id));
    let mut candidates = Vec::new();
    for index in order {
        let worker = &workers[index];
        let has_task = world.rows()?.staff.get(&worker.id).is_some_and(|staff| staff.active_task_id.is_some());
        // `records.get(staff_id, {}).get("phase") == "idle"`.
        let idle = phases.get(&worker.id) == Some(&Phase::Idle);
        if !qualifies(worker, &open.kind) || !has_task || idle || !worker.available || !worker.on_shift {
            continue;
        }
        if service.resolver != Step::None {
            let (zone, staff_zone) = zones_of(world, &open.agent_id, &worker.id)?;
            let route = resolve(world, travel, &zone, &staff_zone, &open.agent_id)?;
            if route.is_none_or(|route| at + route.minutes + service.task_minutes >= duration) {
                continue;
            }
        }
        candidates.push(index);
    }
    let Some(chosen) = candidates.first().copied() else {
        return Ok(None);
    };
    let worker = &workers[chosen];
    // `records[staff_id]["task_id"]`.
    let task_id = {
        let row = inputs.operations_held(world)?;
        let Some(record) = row.staff.get(&worker.id) else {
            return Err(Error::Key(worker.id.to_string()).into());
        };
        record.task_id.clone()
    };
    let sequence = events.allocate_sequence()?;
    let mut payload = IndexMap::with_capacity(4);
    payload.insert(Arc::from("staff_id"), V::Str(worker.id.clone()));
    payload.insert(Arc::from("task_id"), task_id.map_or(V::Null, V::Str));
    payload.insert(Arc::from("request_id"), V::Str(open.id.clone()));
    payload.insert(Arc::from("reason_code"), V::Str(Arc::from("urgent_resident_support")));
    let event = kit::new_event(NewEvent {
        event_id: Arc::from(format!("staff-operation-preempted-{sequence}")),
        kind: EventKind::Endogenous,
        due_minute: at,
        priority: -10,
        sequence,
        event_type: Arc::from("staff_operation_preempted"),
        payload,
        source_id: Some(Arc::from(PREEMPTION_EVENT_SOURCE)),
        causation_id: Some(open.id.clone()),
        provenance: kit::assumption(V::Str(Arc::from(PREEMPTION_SOURCE)), PREEMPTION_POLICY),
    })?;
    events.record(&event, at, EventStatus::Applied, Some(PREEMPTION_DETAIL))?;
    // Its refusal is the `KernelRefusal` of `interrupt_staff_operation`, past the record above.
    let stopping = Stopping { event_id: &event.event_id, due_minute: event.due_minute };
    let interrupted = operations::interrupt(world, inputs.tasks()?, &worker.id, &stopping, None, taken).map_err(|error| step_error(INTERRUPT, error))?;
    if !interrupted {
        return Err(Error::Value("urgent support preemption lost its active staff operation".to_owned()).into());
    }
    Ok(Some(chosen))
}

// ---- _consider_due_complaints --------------------------------------------------------------------

/// `_consider_due_complaints`: each pending request that has waited exactly `threshold` minutes, in
/// ID order, goes to the policy (`Host::consider_complaint_response`), with the world given back
/// during the call. A decision before one may have settled it: each is read again first. None for
/// the threshold is a Run without a social policy, which does nothing.
pub fn consider_due_complaints<C: WorldCell, H: Host>(cell: &C, host: &mut H, threshold: Option<i64>, minute: i64) -> std::result::Result<(), H::Error> {
    let Some(threshold) = threshold else {
        return Ok(());
    };
    let is_due = |requested: i64| requested.checked_add(threshold) == Some(minute);
    let mut due: Vec<Arc<str>> = cell.with(|world| -> Result<Vec<Arc<str>>> {
        Ok(world
            .rows()?
            .support_requests
            .iter()
            .filter(|(_, row)| row.status == RequestStatus::Pending && is_due(row.requested_minute))
            .map(|(id, _)| id.clone())
            .collect())
    })??;
    due.sort_unstable();
    for request_id in due {
        let agent_id = cell.with(|world| -> Result<Option<Arc<str>>> {
            let rows = world.rows()?;
            let Some(row) = rows.support_requests.get(&request_id) else {
                return Err(Error::Runtime("a request left its row in a complaint's decision".to_owned()));
            };
            Ok((row.status == RequestStatus::Pending && is_due(row.requested_minute)).then(|| row.agent_id.clone()))
        })??;
        let Some(agent_id) = agent_id else {
            continue;
        };
        host.consider_complaint_response(&agent_id, &request_id)?;
    }
    Ok(())
}

// ---- What the metrics read of each open request --------------------------------------------------

/// What `_support_wait_snapshot` needs of the qualified staff of one kind.
struct KindWait {
    kind: Arc<str>,
    qualified: bool,
    /// The sorted blockers of `support_wait_reason_codes` that come from the staff.
    blockers: Vec<&'static str>,
    /// `sorted({person.zone_id for person in _route_candidates(...)})`.
    destinations: Vec<Arc<str>>,
}

fn kind_wait(workers: &[Worker], kind: &Arc<str>, sample: i64) -> KindWait {
    let mut qualified = false;
    let mut unavailable = false;
    let mut staff_busy = false;
    let mut other_operation = false;
    let mut resting = false;
    let mut off_shift = false;
    let mut destinations = BTreeSet::new();
    for worker in workers {
        if !qualifies(worker, kind) {
            continue;
        }
        qualified = true;
        if !worker.has_task && worker.available && worker.can_start && worker.on_shift {
            destinations.insert(worker.zone.clone());
        }
        unavailable |= !worker.available;
        staff_busy |= worker.has_task;
        other_operation |= worker.on_operation;
        if worker.rest_until.is_some_and(|until| until > sample) {
            resting = true;
        } else if !worker.on_shift {
            off_shift = true;
        }
    }
    // Sorted, as `tuple(sorted(blockers))`.
    let mut blockers = Vec::with_capacity(5);
    for (blocked, name) in [
        (staff_busy, "staff_busy"),
        (off_shift, "staff_off_shift"),
        (other_operation, "staff_on_other_operation"),
        (resting, "staff_resting"),
        (unavailable, "staff_unavailable"),
    ] {
        if blocked {
            blockers.push(name);
        }
    }
    KindWait { kind: kind.clone(), qualified, blockers, destinations: destinations.into_iter().collect() }
}

/// What the routes of a wait read beside the world, read by the PyO3 crate only where a request's
/// kind has free staff (`open_waits` returns None without them).
#[derive(Clone, Copy)]
pub struct WaitRoutes<'a> {
    /// `staff_operations_v1` as the edge loads read it (`travel::active_edge_loads`); the queue's
    /// rows are read before.
    pub loads_staff: &'a Held,
    /// The floor plan; None where the PyO3 crate could not read it.
    pub geo: Option<&'a Arc<Geo>>,
    /// The movement section of the configs (`config::movement`).
    pub movement: &'a Kept<Movement>,
    /// `model_configs["staff"].get("support_task_minutes", 10)`.
    pub support_minutes: &'a Kept<i64>,
}

/// What `_support_route_wait_reasons` reads once for all the requests of a snapshot.
struct RouteReads {
    graph: Router,
    loads: EdgeLoads,
    has_plan: bool,
    service_minutes: i64,
    duration: i64,
    closed: Vec<(&'static str, Arc<HashSet<Arc<str>>>)>,
}

impl RouteReads {
    fn of(world: &World, inputs: &WaitRoutes<'_>, minute: i64) -> Result<Self> {
        let service_minutes = *kit::kept(inputs.support_minutes)?;
        let has_plan = match &world.row::<ValueRow>(MODEL_CONFIGS)?.0 {
            V::Map(configs) => configs.contains_key("facility_geometry"),
            _ => return Err(Error::unsupported("a value is not a plain dict")),
        };
        Ok(Self {
            graph: travel::world_graph(world)?,
            loads: travel::active_edge_loads(world, inputs.loads_staff, minute)?,
            has_plan,
            service_minutes,
            duration: world.duration_minutes()?,
            closed: Vec::new(),
        })
    }

    fn closed_to(&mut self, geo: Option<&Arc<Geo>>, mobility: &'static str) -> Result<Arc<HashSet<Arc<str>>>> {
        if let Some((_, closed)) = self.closed.iter().find(|(name, _)| *name == mobility) {
            return Ok(closed.clone());
        }
        let closed = if self.has_plan { self.graph.edges_closed_by(travel::plan(geo)?, mobility) } else { HashSet::new() };
        let closed = Arc::new(closed);
        self.closed.push((mobility, closed.clone()));
        Ok(closed)
    }
}

/// `route_with_movement_time` of a route's minutes.
fn movement_minutes(world: &World, inputs: &WaitRoutes<'_>, agent_id: &str, route: &Route) -> Result<i64> {
    let section = kit::kept(inputs.movement)?;
    if !section.times_routes() {
        return Ok(route.minutes);
    }
    let model = kit::movement::config_read(section.model.clone())?;
    if route.minutes.abs() >= FLOAT_EXACT_INT {
        return Err(Error::unsupported("the route's minutes are too large"));
    }
    let geo = travel::plan(inputs.geo)?;
    let zones: Vec<&str> = route.zone_ids.iter().map(|id| &**id).collect();
    let edges: Vec<&str> = route.edge_ids.iter().map(|id| &**id).collect();
    movement::movement_minutes(world, geo, &model, agent_id, &zones, &edges, route.minutes)
}

/// `_support_route_wait_reasons`, of a person with candidates: the reasons, and whether a route is
/// feasible.
fn route_wait_reasons(world: &World, inputs: &WaitRoutes<'_>, reads: &mut RouteReads, agent_id: &str, destinations: &[Arc<str>], minute: i64) -> Result<(BTreeSet<&'static str>, bool)> {
    let (zone, mobility) = {
        let rows = world.rows()?;
        let Some(agent) = rows.agents.get(agent_id) else {
            return Err(Error::unsupported("no such agent"));
        };
        (agent.zone_id.clone(), kit::mobility_text(agent))
    };
    let closed = reads.closed_to(inputs.geo, mobility)?;
    let mut reasons = BTreeSet::new();
    let mut feasible = false;
    for destination in destinations {
        let Some(route) = reads.graph.shortest(&closed, Some(&reads.loads), &zone, destination)? else {
            let free = reads.graph.shortest(&closed, None, &zone, destination)?;
            reasons.insert(if free.is_some() { "route_capacity_full" } else { "no_route_to_qualified_staff" });
            continue;
        };
        let minutes = movement_minutes(world, inputs, agent_id, &route)?;
        let rows = world.rows()?;
        if rows.zones_tainted().is_some() {
            return Err(Error::unsupported(ZONES_TAINTED));
        }
        let Some(place) = rows.zones.get(destination) else {
            return Err(Error::unsupported("no such zone"));
        };
        if *destination != zone {
            if let Some(capacity) = place.capacity {
                let (mut occupied, mut incoming) = (0_i64, 0_i64);
                for (_, person) in rows.agents.iter() {
                    occupied += i64::from(person.zone_id == *destination);
                    incoming += i64::from(person.activity == Activity::Moving && person.target_id.as_ref() == Some(destination));
                }
                if occupied + incoming >= capacity {
                    reasons.insert("staff_destination_full");
                    continue;
                }
            }
        }
        if minute.saturating_add(minutes).saturating_add(reads.service_minutes) >= reads.duration {
            reasons.insert("insufficient_run_time");
            continue;
        }
        feasible = true;
    }
    Ok((reasons, feasible))
}

/// The reason codes of an open request.
enum WaitReason {
    /// `waits[index]`, whose qualified staff are none of them free.
    OfKind(usize),
    One(&'static str),
    /// What the routes of the free ones add.
    Routed(Vec<Arc<str>>),
}

/// What the metrics read of one open request, by its place in the list given.
pub struct OpenWait {
    /// The request's place in the list given.
    pub index: usize,
    pub agent: Arc<str>,
    pub waited: i64,
    reason: WaitReason,
}

/// The requests of `_support_wait_snapshot`, in the order given.
pub struct OpenWaits {
    pub requests: Vec<OpenWait>,
    waits: Vec<KindWait>,
}

impl OpenWaits {
    /// `support_wait_reason_codes` of a request.
    pub fn reasons_of<'a>(&'a self, request: &'a OpenWait) -> Vec<&'a str> {
        match &request.reason {
            WaitReason::OfKind(kind) => {
                let wait = &self.waits[*kind];
                if !wait.qualified {
                    vec!["no_qualified_staff"]
                } else if wait.blockers.is_empty() {
                    vec!["no_route_to_qualified_staff"]
                } else {
                    wait.blockers.clone()
                }
            }
            WaitReason::One(reason) => vec![*reason],
            WaitReason::Routed(reasons) => reasons.iter().map(|reason| &**reason).collect(),
        }
    }

    /// Whether no staff member is qualified for its kind.
    pub fn has_no_qualified_staff(&self, request: &OpenWait) -> bool {
        matches!(request.reason, WaitReason::OfKind(kind) if !self.waits[kind].qualified)
    }
}

/// `support_wait_reason_codes` of a pending request whose kind has free staff, past its routes:
/// None where its routes are to be read and `routes` is None.
#[allow(clippy::too_many_arguments)]
fn routed_reasons(
    world: &World,
    staff: &Inputs<'_>,
    routes: Option<&WaitRoutes<'_>>,
    reads: &mut Option<RouteReads>,
    wait: &KindWait,
    agent_exists: bool,
    agent_id: &str,
    last_attempt: Option<i64>,
    last_reason: Option<&Arc<str>>,
    sample: i64,
) -> Result<Option<Vec<Arc<str>>>> {
    let (route_reasons, feasible) = if agent_exists {
        let Some(routes) = routes else {
            return Ok(None);
        };
        if reads.is_none() {
            *reads = Some(RouteReads::of(world, routes, sample)?);
        }
        let Some(known) = reads.as_mut() else {
            return Err(Error::unsupported("the routes were not read"));
        };
        route_wait_reasons(world, routes, known, agent_id, &wait.destinations, sample)?
    } else {
        (BTreeSet::new(), false)
    };
    let mut blockers: BTreeSet<&str> = wait.blockers.iter().copied().collect();
    blockers.extend(route_reasons);
    if !blockers.is_empty() {
        return Ok(Some(blockers.into_iter().map(Arc::from).collect()));
    }
    if !feasible {
        return Ok(Some(vec![Arc::from("no_route_to_qualified_staff")]));
    }
    if last_attempt == Some(sample) {
        return Ok(Some(vec![match last_reason {
            Some(reason) if !reason.is_empty() => reason.clone(),
            _ => Arc::from("assignment_failure_reason_not_recorded"),
        }]));
    }
    kit::check_metric_state(world, staff.metric_state_held)?;
    let attempted = world.metric_state()?.get(ATTEMPT_METRIC).copied().unwrap_or(-1.0) == sample as f64;
    Ok(Some(vec![Arc::from(if attempted { "assignment_failure_reason_not_recorded" } else { "assignment_not_attempted_at_sampled_minute" })]))
}

/// `support_waited_minutes` and `support_wait_reason_codes` of each request of these IDs, or with
/// `only_open` of each one that is pending or assigned. None, where a request's routes are to be
/// read and `routes` is None: the call again with them answers. Nothing is written.
pub fn open_waits(world: &World, staff: &Inputs<'_>, routes: Option<&WaitRoutes<'_>>, ids: &[Arc<str>], sample: i64, only_open: bool) -> Result<Option<OpenWaits>> {
    let kinds = CareKinds::of(world, staff)?;
    let mut waits: Vec<KindWait> = Vec::new();
    let mut workers: Option<Vec<Worker>> = None;
    let mut reads: Option<RouteReads> = None;
    let mut requests: Vec<OpenWait> = Vec::with_capacity(ids.len());
    for (index, id) in ids.iter().enumerate() {
        let (agent, status, moving, waited, agent_exists, last_attempt, last_reason) = {
            let rows = world.rows()?;
            let Some(request) = rows.support_requests.get(id) else {
                return Err(Error::unsupported("a request has no row"));
            };
            let agent = rows.agents.get(&request.agent_id);
            let moving = agent.is_some_and(|agent| agent.activity == Activity::Moving);
            let mut endpoint = sample;
            if request.status == RequestStatus::Assigned {
                let task = rows.active_tasks.get(request.task_id.as_deref().unwrap_or(""));
                if let (Some(_), false, Some(task)) = (agent, moving, task) {
                    endpoint = sample.min(task.start_minute);
                }
            }
            let Some(waited) = endpoint.checked_sub(request.requested_minute) else {
                return Err(Error::unsupported("the wait does not fit an i64"));
            };
            (
                request.agent_id.clone(),
                request.status,
                moving,
                waited.max(0),
                agent.is_some(),
                request.last_assignment_attempt_minute,
                request.last_assignment_wait_reason_code.clone(),
            )
        };
        if only_open && !matches!(status, RequestStatus::Pending | RequestStatus::Assigned) {
            continue;
        }
        let reason = match status {
            RequestStatus::Assigned => WaitReason::One(if moving { "resident_travel" } else { "support_in_progress" }),
            RequestStatus::Pending => {
                let kind = kinds.kind_of(id);
                let index = match waits.iter().position(|wait| wait.kind == kind) {
                    Some(index) => index,
                    None => {
                        if workers.is_none() {
                            workers = Some(workers_at(world, staff, sample)?);
                        }
                        waits.push(kind_wait(workers.as_deref().unwrap_or(&[]), &kind, sample));
                        waits.len() - 1
                    }
                };
                if waits[index].destinations.is_empty() {
                    WaitReason::OfKind(index)
                } else {
                    match routed_reasons(world, staff, routes, &mut reads, &waits[index], agent_exists, &agent, last_attempt, last_reason.as_ref(), sample)? {
                        Some(reasons) => WaitReason::Routed(reasons),
                        None => return Ok(None),
                    }
                }
            }
            _ => WaitReason::One("request_closed"),
        };
        requests.push(OpenWait { index, agent, waited, reason });
    }
    Ok(Some(OpenWaits { requests, waits }))
}

// ---- update_staff_operations ---------------------------------------------------------------------

/// The value under `name` of a config document.
fn config_item<'a>(config: &'a V, name: &str) -> Result<&'a V> {
    match config {
        V::Map(config) => config.get(name).ok_or_else(|| Error::unsupported("the config lacks a key")),
        _ => Err(Error::unsupported("the config is not a map")),
    }
}

/// `config[table].get(zone, {}).get(name)`. A table that is `required` is read as `config[table]`.
fn zone_override<'a>(config: &'a V, table: &str, zone: &str, name: &str, required: bool) -> Result<Option<&'a V>> {
    let V::Map(document) = config else {
        return Err(Error::unsupported("the config is not a map"));
    };
    let Some(table) = document.get(table) else {
        return if required { Err(Error::unsupported("the config lacks a table")) } else { Ok(None) };
    };
    let V::Map(table) = table else {
        return Err(Error::unsupported("a table of the config is not a map"));
    };
    match table.get(zone) {
        None => Ok(None),
        Some(V::Map(entry)) => Ok(entry.get(name)),
        Some(_) => Err(Error::unsupported("an entry of the config is not a map")),
    }
}

/// `load * rate * elapsed` of `update_staff_operations`, from the left, as the float Python takes
/// off the sanitation. `canonical_staff_operations_config` takes an int rate too (0 or 1 in its
/// range): its product is an int, exact, which the float subtraction then reads as its float.
fn used_sanitation(rate: &V, load: i64, elapsed: i64) -> Result<f64> {
    match rate {
        V::Float(rate) => Ok(load as f64 * rate * elapsed as f64),
        V::Int(rate) => {
            let used = i128::from(load) * i128::from(*rate) * i128::from(elapsed);
            if used.unsigned_abs() > FLOAT_EXACT_INT as u128 {
                return Err(Error::unsupported("the sanitation used does not fit a float"));
            }
            Ok(used as f64)
        }
        _ => Err(Error::unsupported("a rate is not a number")),
    }
}

/// A threshold a comparison with a float reads: both types compare as their value does.
fn threshold(value: Option<&V>, default: f64) -> Result<f64> {
    match value {
        None => Ok(default),
        Some(V::Float(number)) => Ok(*number),
        Some(V::Int(number)) if number.abs() < FLOAT_EXACT_INT => Ok(*number as f64),
        Some(_) => Err(Error::unsupported("a threshold is not a number")),
    }
}

/// `update_staff_operations`: each zone's sanitation falls with who is in it. Nothing without the
/// namespace; a zone whose record does not change is not written.
pub fn update_operations(world: &World, inputs: &Inputs<'_>, minute: i64) -> Result<()> {
    if matches!(inputs.operations, Held::Absent) {
        return Ok(());
    }
    let zones = world_zones(world)?;
    let loads: Vec<i64> = {
        let rows = world.rows()?;
        let mut loads: FxHashMap<&str, i64> = FxHashMap::default();
        for (_, agent) in rows.agents.iter() {
            *loads.entry(&agent.zone_id).or_insert(0) += 1;
        }
        zones.iter().map(|zone| loads.get(&**zone).copied().unwrap_or(0)).collect()
    };
    let plan = {
        let row = inputs.operations_held(world)?;
        let mut plan = Vec::with_capacity(zones.len());
        let global = config_item(&row.config, "sanitation_use_per_person_minute")?;
        for (zone_id, load) in zones.iter().zip(&loads) {
            let Some((index, _, record)) = row.zones.get_full(zone_id) else {
                return Err(Error::unsupported("a zone has no record"));
            };
            let Some(elapsed) = minute.checked_sub(record.updated_minute) else {
                return Err(Error::unsupported("the time since the update does not fit an i64"));
            };
            let elapsed = elapsed.max(0);
            if elapsed >= FLOAT_EXACT_INT {
                return Err(Error::unsupported("the time since the update does not fit a float"));
            }
            let rate = zone_override(&row.config, "effects_by_zone", zone_id, "sanitation_use_per_person_minute", true)?.unwrap_or(global);
            let sanitation = py_max(0.0, record.sanitation - used_sanitation(rate, *load, elapsed)?);
            if record.sanitation.to_bits() != sanitation.to_bits() || record.updated_minute != minute {
                plan.push((index, sanitation));
            }
        }
        plan
    };
    if plan.is_empty() {
        return Ok(());
    }
    let mut row = world.row_mut::<StaffOperationsRow>(NAME)?;
    for (index, sanitation) in plan {
        let (_, record) = row.zones.get_index_mut(index).ok_or_else(|| Error::unsupported("a zone has no record"))?;
        record.sanitation = sanitation;
        record.updated_minute = minute;
    }
    Ok(())
}

// ---- assign_staff_operations ---------------------------------------------------------------------

/// Both rules of the staff's tasks round each member's fatigue on their way, which raises for this.
fn every_fatigue_is_finite(world: &World, inputs: &Inputs<'_>) -> Result<()> {
    match inputs.dynamics_row(world)? {
        Some(row) if !row.staff.values().all(|record| record.fatigue.is_finite()) => Err(Error::unsupported("a fatigue is not finite")),
        _ => Ok(()),
    }
}

/// The people of a zone: their stress, and the uncertainty of those with a body record.
#[derive(Default)]
struct ZonePeople {
    stress: Vec<f64>,
    uncertainty: Vec<f64>,
}

/// Who is in each zone, in the order of the zones' IDs.
fn zone_people(world: &World, inputs: &Inputs<'_>) -> Result<BTreeMap<Arc<str>, ZonePeople>> {
    let body = kit::held_row::<BodyRow>(world, BODY_STATE, inputs.body)?;
    let rows = world.rows()?;
    let mut people: BTreeMap<Arc<str>, ZonePeople> = BTreeMap::new();
    for (id, agent) in rows.agents.iter() {
        let zone = match people.get_mut(&agent.zone_id) {
            Some(zone) => zone,
            None => people.entry(agent.zone_id.clone()).or_default(),
        };
        zone.stress.push(agent.stress);
        if let Some(record) = body.as_ref().and_then(|body| body.agents.get(id)) {
            zone.uncertainty.push(record.uncertainty);
        }
    }
    Ok(people)
}

/// `math.fsum(values) / len(values)`, of at least one value.
fn mean_of(values: &[f64], refusal: &'static str) -> Result<f64> {
    match py_fsum(values) {
        Some(sum) => Ok(sum / values.len() as f64),
        None => Err(Error::unsupported(refusal)),
    }
}

fn is_free_for_a_task(worker: &Worker) -> bool {
    !worker.has_task && !worker.on_operation && worker.available && worker.can_start
}

fn has_skill(worker: &Worker, skills: &[&str]) -> bool {
    worker.skills.iter().any(|skill| skills.contains(&&**skill))
}

const CLEANING_SKILLS: [&str; 2] = ["cleaning", "sanitation"];
const MAINTENANCE_SKILLS: [&str; 2] = ["maintenance", "repair"];
const OUTREACH_SKILLS: [&str; 2] = ["support", "information"];
const PATROL_SKILLS: [&str; 2] = ["support", "patrol"];

/// Whether a free worker has a task to take: what the tick reads before it routes anything.
fn a_task_is_open(world: &World, inputs: &Inputs<'_>, row: &StaffOperationsRow, workers: &[Worker]) -> Result<bool> {
    let free: Vec<&Worker> = workers.iter().filter(|worker| is_free_for_a_task(worker)).collect();
    let has = |skills: &[&str]| free.iter().any(|worker| has_skill(worker, skills));
    let (cleaners, repairers, outreach, patrol) = (has(&CLEANING_SKILLS), has(&MAINTENANCE_SKILLS), has(&OUTREACH_SKILLS), has(&PATROL_SKILLS));
    // `state["staff"][staff_id]` raises for one without a record.
    if workers.iter().any(|worker| !worker.has_task && !row.staff.contains_key(&worker.id)) {
        return Err(Error::unsupported("a staff member has no record"));
    }
    let reserved = |kind: &str, zone: &str| {
        row.staff.values().any(|record| {
            record.phase != Phase::Idle && record.task_kind.as_deref() == Some(kind) && record.target_zone_id.as_deref() == Some(zone) && record.target_id.is_none()
        })
    };
    if cleaners {
        for (zone, record) in &row.zones {
            let below = threshold(zone_override(&row.config, "thresholds_by_zone", zone, "cleaning_sanitation_below", false)?, CLEANING_BELOW)?;
            if record.sanitation < below && !reserved("cleaning", zone) {
                return Ok(true);
            }
        }
    }
    if repairers && world.rows()?.facilities.iter().any(|(_, facility)| facility.capacity > facility.working_units) {
        return Ok(true);
    }
    if !outreach && !patrol {
        return Ok(false);
    }
    let people = zone_people(world, inputs)?;
    if outreach {
        for (zone, group) in &people {
            if group.uncertainty.is_empty() {
                continue;
            }
            let mean = mean_of(&group.uncertainty, "an uncertainty is not finite")?;
            let at_least = threshold(zone_override(&row.config, "thresholds_by_zone", zone, "outreach_uncertainty_at_least", false)?, OUTREACH_AT_LEAST)?;
            if mean >= at_least && !reserved("information_outreach", zone) {
                return Ok(true);
            }
        }
    }
    if patrol {
        for zone in world_zones(world)? {
            let Some(group) = people.get(&zone) else {
                continue;
            };
            let mean = mean_of(&group.stress, "a stress is not finite")?;
            let at_least = threshold(zone_override(&row.config, "thresholds_by_zone", &zone, "patrol_stress_at_least", false)?, PATROL_AT_LEAST)?;
            if mean >= at_least && !reserved("patrol", &zone) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// A task a worker could take, with what `_TASK_SKILLS` asks of them.
struct OpenTask {
    urgency: f64,
    kind: &'static str,
    zone: Arc<str>,
    target: Option<Arc<str>>,
    reason: String,
    skills: &'static [&'static str; 2],
}

/// One start of `assign_staff_operations`, nothing changed yet.
struct PlannedOperation {
    worker: usize,
    kind: &'static str,
    zone: Arc<str>,
    target: Option<Arc<str>>,
    reason: String,
    route: Route,
    due: i64,
}

/// `(recent, total, round(1000 * fatigue))` of each member by ID: how both rules rotate the staff.
fn workload_of(world: &World, inputs: &Inputs<'_>, row: &StaffOperationsRow, workers: &[Worker], at: i64) -> Result<Vec<(i64, i64, i64)>> {
    let cutoff = 0.max(at - 240);
    let mut counts: FxHashMap<&str, (i64, i64)> = FxHashMap::default();
    for item in &row.history {
        let entry = counts.entry(&item.staff_id).or_insert((0, 0));
        entry.0 += i64::from(item.complete_minute >= cutoff);
        entry.1 += 1;
    }
    let dynamics = inputs.dynamics_row(world)?;
    let mut workload = Vec::with_capacity(workers.len());
    for worker in workers {
        let rounded = match dynamics.as_ref().and_then(|dynamics| dynamics.staff.get(&worker.id)) {
            Some(record) => py_round_int(1000.0 * record.fatigue).ok_or_else(|| Error::unsupported("a fatigue does not round"))?,
            None => 0,
        };
        let (recent, total) = counts.get(&*worker.id).copied().unwrap_or((0, 0));
        workload.push((recent, total, rounded));
    }
    Ok(workload)
}

/// The tasks `assign_staff_operations` would list for someone with every skill, in its order,
/// without the ones `reserved_operations` takes.
fn open_tasks(world: &World, inputs: &Inputs<'_>, row: &StaffOperationsRow, workers: &[Worker]) -> Result<Vec<OpenTask>> {
    let eligible = |skills: &[&str]| workers.iter().any(|worker| is_free_for_a_task(worker) && has_skill(worker, skills));
    let mut tasks = Vec::new();
    if eligible(&CLEANING_SKILLS) {
        // `sorted(state["zones"].items())`.
        let mut zones: Vec<(&Arc<str>, f64)> = row.zones.iter().map(|(zone, record)| (zone, record.sanitation)).collect();
        zones.sort_by(|first, second| first.0.cmp(second.0));
        for (zone, sanitation) in zones {
            let below = threshold(zone_override(&row.config, "thresholds_by_zone", zone, "cleaning_sanitation_below", false)?, CLEANING_BELOW)?;
            if sanitation < below {
                tasks.push(OpenTask {
                    urgency: 1.0 - sanitation,
                    kind: "cleaning",
                    zone: zone.clone(),
                    target: None,
                    reason: format!("sanitation_index_below_{}", g_text(below)?),
                    skills: &CLEANING_SKILLS,
                });
            }
        }
    }
    if eligible(&MAINTENANCE_SKILLS) {
        // `sorted(world.facilities.values(), key=id)`.
        let mut short: Vec<(Arc<str>, Arc<str>, i64)> = world
            .rows()?
            .facilities
            .iter()
            .filter(|(_, facility)| facility.capacity > facility.working_units)
            .map(|(id, facility)| (id.clone(), facility.zone_id.clone(), facility.capacity - facility.working_units))
            .collect();
        short.sort_by(|first, second| first.0.cmp(&second.0));
        for (id, zone, deficit) in short {
            if deficit >= FLOAT_EXACT_INT {
                return Err(Error::unsupported("a deficit does not fit a float"));
            }
            let outage = kit::active_outage(world.slots.try_value(INFRASTRUCTURE)?.as_deref(), &id)?;
            if !outage && !kit::has_active_modifier(kit::held_row::<EffectProgramRow>(world, PROGRAM, inputs.tasks()?.program)?.as_deref(), &id) {
                tasks.push(OpenTask {
                    urgency: deficit as f64,
                    kind: "maintenance",
                    zone,
                    target: Some(id),
                    reason: "facility_working_units_below_capacity".to_owned(),
                    skills: &MAINTENANCE_SKILLS,
                });
            }
        }
    }
    let outreach = eligible(&OUTREACH_SKILLS);
    let patrol = eligible(&PATROL_SKILLS);
    if outreach || patrol {
        let people = zone_people(world, inputs)?;
        if outreach {
            for (zone, group) in &people {
                if group.uncertainty.is_empty() {
                    continue;
                }
                let mean = mean_of(&group.uncertainty, "an uncertainty is not finite")?;
                let at_least = threshold(zone_override(&row.config, "thresholds_by_zone", zone, "outreach_uncertainty_at_least", false)?, OUTREACH_AT_LEAST)?;
                if mean >= at_least {
                    tasks.push(OpenTask {
                        urgency: mean,
                        kind: "information_outreach",
                        zone: zone.clone(),
                        target: None,
                        reason: format!("mean_information_uncertainty_at_least_{}", g_text(at_least)?),
                        skills: &OUTREACH_SKILLS,
                    });
                }
            }
        }
        if patrol {
            let mut zones = world_zones(world)?;
            zones.sort();
            for zone in zones {
                let Some(group) = people.get(&zone) else {
                    continue;
                };
                let mean = mean_of(&group.stress, "a stress is not finite")?;
                let at_least = threshold(zone_override(&row.config, "thresholds_by_zone", &zone, "patrol_stress_at_least", false)?, PATROL_AT_LEAST)?;
                if mean >= at_least {
                    tasks.push(OpenTask {
                        urgency: mean * 0.5,
                        kind: "patrol",
                        reason: format!("mean_zone_stress_at_least_{}", g_text(at_least)?),
                        zone,
                        target: None,
                        skills: &PATROL_SKILLS,
                    });
                }
            }
        }
    }
    Ok(tasks)
}

/// `formula.rank_key(...)` compared as Python compares the tuples. The minutes fit a float.
fn ranks_above(first: (f64, f64, &OpenTask), second: (f64, f64, &OpenTask)) -> bool {
    let text = |task: &OpenTask| (task.kind, task.zone.clone(), task.target.clone().unwrap_or_else(|| Arc::from("")));
    if first.0 != second.0 {
        return first.0 > second.0;
    }
    if first.1 != second.1 {
        return first.1 > second.1;
    }
    text(first.2) > text(second.2)
}

/// What `assign_staff_operations` reads only where a free worker has a task to take.
#[derive(Clone, Copy)]
pub struct Routed<'a> {
    /// The staff's router: the Run's, or the graph of the `SpaceRouter` a Python caller gave.
    pub router: &'a Router,
    /// Whether the world holds `movement_v1` and `staff_operations_v1`, as the passages' loads read
    /// them (`travel::passage_edge_loads`).
    pub movement: &'a Held,
    pub loads_staff: &'a Held,
}

/// What `assign_staff_operations` led to.
#[derive(Debug, PartialEq, Eq)]
pub enum Operations {
    /// The new task IDs.
    Done(Vec<Arc<str>>),
    /// A free worker has a task to take: nothing is written, and the call again with `Routed`
    /// (and the formula in the inputs) routes and starts them.
    NeedsRoutes,
}

/// `assign_staff_operations`: nothing without the namespace; the plain tick where no free worker has
/// a task to take changes nothing and reads no route.
pub fn assign_operations(world: &World, inputs: &Inputs<'_>, known: &Kept<()>, minute: i64, routed: Option<&Routed<'_>>) -> WriteResult<Operations> {
    if matches!(inputs.operations, Held::Absent) {
        return Ok(Operations::Done(Vec::new()));
    }
    kit::kept(known)?;
    let workers = workers_at(world, inputs, minute)?;
    {
        let row = inputs.operations_held(world)?;
        every_fatigue_is_finite(world, inputs)?;
        if !a_task_is_open(world, inputs, &row, &workers)? {
            return Ok(Operations::Done(Vec::new()));
        }
    }
    let Some(routed) = routed else {
        return Ok(Operations::NeedsRoutes);
    };
    let starts = plan_operations(world, inputs, minute, routed, &workers)?;
    write_operations(world, inputs, minute, starts, &workers)
}

/// `assign_staff_operations`, changing nothing: who starts what, and on which route.
fn plan_operations(world: &World, inputs: &Inputs<'_>, at: i64, routed: &Routed<'_>, workers: &[Worker]) -> Result<Vec<PlannedOperation>> {
    let formula: &StaffOperations = inputs.tasks()?.formula.ok_or_else(|| Error::unsupported(FORMULA_UNREAD))?;
    let travel_first = formula.travel_first();
    let mut loads = travel::passage_edge_loads(world, routed.movement, routed.loads_staff, at)?;
    let duration = world.duration_minutes()?;
    let (tasks, workload, durations, mut reserved) = {
        let row = inputs.operations_held(world)?;
        let mut durations = Vec::with_capacity(TASK_KINDS.len());
        for kind in TASK_KINDS {
            let V::Map(table) = config_item(&row.config, "task_duration_minutes")? else {
                return Err(Error::unsupported("the task durations are not a map"));
            };
            let Some(V::Int(minutes)) = table.get(kind) else {
                return Err(Error::unsupported("a task duration is not an int"));
            };
            durations.push((kind, *minutes));
        }
        let reserved: HashSet<(Arc<str>, Arc<str>, Option<Arc<str>>)> = row
            .staff
            .values()
            .filter(|record| record.phase != Phase::Idle)
            .filter_map(|record| Some((record.task_kind.clone()?, record.target_zone_id.clone()?, record.target_id.clone())))
            .collect();
        (open_tasks(world, inputs, &row, workers)?, workload_of(world, inputs, &row, workers, at)?, durations, reserved)
    };
    let mut order: Vec<usize> = (0..workers.len()).collect();
    order.sort_by(|first, second| (workload[*first], &workers[*first].id).cmp(&(workload[*second], &workers[*second].id)));
    let nothing_closed = HashSet::new();
    let mut starts = Vec::new();
    for index in order {
        let worker = &workers[index];
        if !is_free_for_a_task(worker) {
            continue;
        }
        let mut best: Option<(f64, f64, &OpenTask, Route)> = None;
        for task in &tasks {
            if !has_skill(worker, task.skills) || reserved.contains(&(Arc::from(task.kind), task.zone.clone(), task.target.clone())) {
                continue;
            }
            let Some(route) = routed.router.shortest(&nothing_closed, Some(&loads), &worker.zone, &task.zone)? else {
                continue;
            };
            if route.minutes >= FLOAT_EXACT_INT {
                return Err(Error::unsupported("a route's minutes do not fit a float"));
            }
            let (first, second) = if travel_first { (-route.minutes as f64, task.urgency) } else { (task.urgency, -route.minutes as f64) };
            // `max` keeps the first of equal keys.
            if best.as_ref().is_none_or(|known| ranks_above((first, second, task), (known.0, known.1, known.2))) {
                best = Some((first, second, task, route));
            }
        }
        let Some((_, _, task, route)) = best else {
            continue;
        };
        let Some((_, work)) = durations.iter().find(|(kind, _)| *kind == task.kind) else {
            return Err(Error::unsupported("a task without a duration"));
        };
        let Some(due) = at.checked_add(route.minutes).and_then(|arrival| arrival.checked_add(*work)) else {
            return Err(Error::unsupported("a due minute out of range"));
        };
        if due >= duration {
            continue;
        }
        for edge in &route.edge_ids {
            *loads.entry(edge.clone()).or_insert(0) += 1;
        }
        reserved.insert((Arc::from(task.kind), task.zone.clone(), task.target.clone()));
        starts.push(PlannedOperation { worker: index, kind: task.kind, zone: task.zone.clone(), target: task.target.clone(), reason: task.reason.clone(), route, due });
    }
    Ok(starts)
}

fn text_or_none(text: Option<&Arc<str>>) -> V {
    text.map_or(V::Null, |text| V::Str(text.clone()))
}

/// What `assign_staff_operations` writes, in its order.
fn write_operations(world: &World, inputs: &Inputs<'_>, at: i64, starts: Vec<PlannedOperation>, workers: &[Worker]) -> WriteResult<Operations> {
    let formula: &StaffOperations = inputs.tasks()?.formula.ok_or_else(|| Error::unsupported(FORMULA_UNREAD))?;
    let (source, policy, event_source) = formula.provenance();
    let events = Events::of(world)?;
    let mut assigned = Vec::with_capacity(starts.len());
    for start in starts {
        let worker = &workers[start.worker];
        let travel = start.route.minutes;
        let arrival = at + travel;
        let sequence = events.allocate_sequence()?;
        let task_id: Arc<str> = Arc::from(format!("staff-operation-{sequence}"));
        let (zones, edges) = if travel != 0 { (start.route.zone_ids.clone(), start.route.edge_ids.clone()) } else { (Vec::new(), Vec::new()) };
        let record = OperationRecord {
            phase: if travel != 0 { Phase::Travel } else { Phase::Working },
            task_id: Some(task_id.clone()),
            task_kind: Some(Arc::from(start.kind)),
            target_id: start.target.clone(),
            target_zone_id: Some(start.zone.clone()),
            reason: Some(Arc::from(start.reason.as_str())),
            started_minute: Some(at),
            travel_due_minute: (travel != 0).then_some(arrival),
            arrival_minute: (travel == 0).then_some(arrival),
            due_minute: Some(start.due),
            route_zone_ids: zones,
            route_edge_ids: edges,
            travel_minutes: travel,
            causation_event_ids: Vec::new(),
        };
        world.row_mut::<StaffOperationsRow>(NAME)?.staff.insert(worker.id.clone(), record);
        {
            let mut rows = world.rows_mut()?;
            rows.active_tasks.insert(
                task_id.clone(),
                TaskRow {
                    id: task_id.clone(),
                    kind: Arc::from("staff_operation"),
                    agent_id: None,
                    resource_id: None,
                    start_minute: at,
                    due_minute: start.due,
                    staff_id: Some(worker.id.clone()),
                },
            );
            let staff = rows.staff.get_mut(&worker.id).ok_or_else(|| left("a staff member"))?;
            staff.active_task_id = Some(task_id.clone());
            staff.work_queue = vec![task_id.clone()];
            staff.busy_until = start.due;
        }
        if travel != 0 {
            let mut payload = IndexMap::with_capacity(3);
            payload.insert(Arc::from("staff_id"), V::Str(worker.id.clone()));
            payload.insert(Arc::from("task_id"), V::Str(task_id.clone()));
            payload.insert(Arc::from("target_zone_id"), V::Str(start.zone.clone()));
            events.schedule(kit::new_event(NewEvent {
                event_id: Arc::from(format!("staff-operation-arrive-{sequence}")),
                kind: EventKind::Completion,
                due_minute: arrival,
                priority: 5,
                sequence,
                event_type: Arc::from("staff_operation_arrive"),
                payload,
                source_id: Some(Arc::from(event_source)),
                causation_id: None,
                provenance: kit::assumption(V::Str(Arc::from(source)), policy),
            })?)?;
        }
        let completion = events.allocate_sequence()?;
        let mut payload = IndexMap::with_capacity(5);
        payload.insert(Arc::from("staff_id"), V::Str(worker.id.clone()));
        payload.insert(Arc::from("task_id"), V::Str(task_id.clone()));
        payload.insert(Arc::from("task_kind"), V::Str(Arc::from(start.kind)));
        payload.insert(Arc::from("target_id"), text_or_none(start.target.as_ref()));
        payload.insert(Arc::from("target_zone_id"), V::Str(start.zone.clone()));
        let mut provenance = kit::assumption(V::Str(Arc::from(source)), policy);
        provenance.insert(Arc::from("reason"), V::Str(Arc::from(start.reason.as_str())));
        events.schedule(kit::new_event(NewEvent {
            event_id: Arc::from(format!("staff-operation-complete-{completion}")),
            kind: EventKind::Completion,
            due_minute: start.due,
            priority: 5,
            sequence: completion,
            event_type: Arc::from("staff_operation_complete"),
            payload,
            source_id: Some(Arc::from(event_source)),
            causation_id: None,
            provenance,
        })?)?;
        assigned.push(task_id);
    }
    let tasks = inputs.tasks()?;
    if !assigned.is_empty() && !matches!(tasks.movement, Held::Absent) {
        operations::snapshots(world, tasks, at + 1)?;
    }
    Ok(Operations::Done(assigned))
}

// ---- schedule_staff_operation_relief -------------------------------------------------------------

/// One handoff of `schedule_staff_operation_relief`, nothing changed yet.
struct PlannedRelief {
    task_id: Arc<str>,
    previous: Arc<str>,
    replacement: Arc<str>,
}

/// `event.payload.get("task_id")` of each `staff_operation_relief` in the queue: the tasks already
/// handed. The PyO3 crate reads the rows of the events Python scheduled before it calls.
fn handed_tasks(world: &World) -> Result<HashSet<Arc<str>>> {
    let queue = world.queue()?;
    let mut handed = HashSet::new();
    for (_, serial) in queue.ordered() {
        let Some(row) = &queue.slot(*serial).row else {
            return Err(Error::unsupported("an event in the queue is not a plain one"));
        };
        if &*row.event_type != "staff_operation_relief" {
            continue;
        }
        if let Some(V::Str(task)) = kit::payload(row, "task_id") {
            handed.insert(task.clone());
        }
    }
    Ok(handed)
}

/// `schedule_staff_operation_relief`, changing nothing.
fn plan_relief(world: &World, inputs: &Inputs<'_>, at: i64, next: i64, duration: i64) -> Result<Vec<PlannedRelief>> {
    let handed = handed_tasks(world)?;
    let workers = workers_at(world, inputs, next)?;
    let row = inputs.operations_held(world)?;
    let workload = workload_of(world, inputs, &row, &workers, at)?;
    let mut operations: Vec<(&Arc<str>, &OperationRecord)> = row.staff.iter().collect();
    operations.sort_by(|first, second| first.0.cmp(second.0));
    let mut plans = Vec::new();
    for (staff_id, operation) in operations {
        if operation.phase == Phase::Idle {
            continue;
        }
        let Some(task_id) = &operation.task_id else {
            return Err(Error::unsupported("a staff member's task has no ID"));
        };
        if handed.contains(task_id) {
            continue;
        }
        let Some(worker) = workers.iter().find(|worker| worker.id == *staff_id) else {
            return Err(Error::unsupported("a record of no staff member"));
        };
        if worker.available && worker.on_shift {
            continue;
        }
        let holds = world.rows()?.active_tasks.get(&**task_id).is_some_and(|task| task.staff_id.as_ref() == Some(staff_id));
        if !holds {
            continue;
        }
        let Some(due) = operation.due_minute else {
            return Err(Error::unsupported("a staff member's task has no due minute"));
        };
        if due <= next {
            continue;
        }
        let (Some(target), Some(kind)) = (&operation.target_zone_id, &operation.task_kind) else {
            return Err(Error::unsupported("a staff member's task has no target"));
        };
        let V::Map(durations) = config_item(&row.config, "task_duration_minutes")? else {
            return Err(Error::unsupported("the task durations are not a map"));
        };
        let Some(V::Int(work)) = durations.get(&**kind) else {
            return Err(Error::unsupported("a task duration is not an int"));
        };
        let Some(after) = next.checked_add(*work) else {
            return Err(Error::unsupported("a due minute out of range"));
        };
        if after >= duration {
            continue;
        }
        let Some(required) = relief_skills(kind) else {
            return Err(Error::unsupported("an unknown kind of task"));
        };
        let mut replacement: Option<usize> = None;
        for (index, candidate) in workers.iter().enumerate() {
            if candidate.id == *staff_id || candidate.zone != *target || !has_skill(candidate, &required) || candidate.has_task {
                continue;
            }
            let Some(record) = row.staff.get(&candidate.id) else {
                return Err(Error::unsupported("a staff member has no record"));
            };
            if record.phase != Phase::Idle || !candidate.available || !candidate.can_start {
                continue;
            }
            if replacement.is_none_or(|known| (workload[index], &candidate.id) < (workload[known], &workers[known].id)) {
                replacement = Some(index);
            }
        }
        if let Some(index) = replacement {
            plans.push(PlannedRelief { task_id: task_id.clone(), previous: staff_id.clone(), replacement: workers[index].id.clone() });
        }
    }
    Ok(plans)
}

/// `schedule_staff_operation_relief`: a replacement already at the task takes it over at the shift's
/// boundary. With no staff member on a task there is nothing to relieve. The IDs of the events.
pub fn relief(world: &World, inputs: &Inputs<'_>, minute: i64) -> WriteResult<Vec<Arc<str>>> {
    if matches!(inputs.operations, Held::Absent) {
        return Ok(Vec::new());
    }
    let duration = world.duration_minutes()?;
    let Some(next) = minute.checked_add(1) else {
        return Err(Error::unsupported("the minute does not fit an i64").into());
    };
    if next >= duration {
        return Ok(Vec::new());
    }
    let any_task = inputs.operations_held(world)?.staff.values().any(|record| record.phase != Phase::Idle);
    every_fatigue_is_finite(world, inputs)?;
    if !any_task {
        return Ok(Vec::new());
    }
    let plans = plan_relief(world, inputs, minute, next, duration)?;
    let events = Events::of(world)?;
    let mut handoffs = Vec::with_capacity(plans.len());
    for plan in plans {
        let sequence = events.allocate_sequence()?;
        let event_id: Arc<str> = Arc::from(format!("staff-operation-relief-{sequence}"));
        let mut payload = IndexMap::with_capacity(4);
        payload.insert(Arc::from("task_id"), V::Str(plan.task_id));
        payload.insert(Arc::from("previous_staff_id"), V::Str(plan.previous));
        payload.insert(Arc::from("replacement_staff_id"), V::Str(plan.replacement));
        payload.insert(Arc::from("reason_code"), V::Str(Arc::from("shift_or_availability_boundary")));
        events.schedule(kit::new_event(NewEvent {
            event_id: event_id.clone(),
            kind: EventKind::Endogenous,
            due_minute: next,
            priority: -5,
            sequence,
            event_type: Arc::from("staff_operation_relief"),
            payload,
            source_id: Some(Arc::from(RELIEF_POLICY)),
            causation_id: None,
            provenance: kit::assumption(V::Str(Arc::from(RELIEF_SOURCE)), RELIEF_POLICY),
        })?)?;
        handoffs.push(event_id);
    }
    Ok(handoffs)
}
