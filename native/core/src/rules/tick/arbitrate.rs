//! The arbitrate phase (`_run_arbitrate_phase` of `experiments/phases/arbitrate.py`): the staff's
//! fatigue and operations, the toilets' and the resource service's queues (`toilet::arbitrate`,
//! `service::arbitrate`, which were the driver's loops), the assignment of support with its
//! receipts, the staff's operations, the complaints and the relief.
//!
//! The queue steps each loop takes (`prepare_facility_queue`, the walks of a placed world, the
//! independent queues' checks) are here, read as their PyO3 entries read them
//! (`native/src/rules/queues.rs`, `facility_queues.rs`).

use std::sync::Arc;

use crate::config::movement::{Movement, EXACT_INT};
use crate::config::queues::{QueueConfig, QueuePolicy};
use crate::config::{FaultClass, SectionId};
use crate::error::Error;
use crate::events::writer::{WriteError, WriteResult};
use crate::geometry::Geo;
use crate::host::{Holding, Host};
use crate::routes::Which;
use crate::rules::kit::Held;
use crate::rules::overlay::PLAN_UNREAD;
use crate::rules::queues::{self, LeaveInputs, Prepare, Prepared, Walk};
use crate::rules::staff::{self, Assigned, Full, Inputs, Operations, Routed, Service, Step as Resolver, Travel};
use crate::rules::staff_operation_events::{self as operations, FORMULA_UNREAD, SNAPSHOTS, SNAPSHOT_PLAN_UNREAD};
use crate::rules::{facility_queues as fifo, support};
use crate::text::py_repr;
use crate::world::{WorldCell, METRIC_STATE};

use super::propose::{graph, refused, router};
use super::read::{is_refusal, Plan};
use super::{Labelled, RunCell, Step, Tick, RUN_TICK};

const OTHER_DOCUMENT: &str = "the world holds another document than the row's";
const MOVEMENT: &str = "movement_v1";
const QUEUES: &str = "facility_queues_v1";
const SERVICE: &str = "resource_service_v1";

/// The arbitrate phase.
pub fn arbitrate<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>) -> Step<(), H::Error> {
    staff_dynamics(tick).at("staff_dynamics.update")?;
    update_operations(tick).at("staff.update_operations")?;
    // `_arbitrate_services`: the driver refused another class of manager before the tick began.
    crate::rules::toilet::arbitrate(tick)?;
    let policies = tick.policies.clone();
    let none = policies.services.resource.is_none().map_err(|fault| tick.policy_error(&["resource_service"], &fault)).bare()?;
    if !none {
        crate::rules::service::arbitrate(tick)?;
    }
    // `_assign_staff_support`.
    let assigned = assign_pending(tick).at("staff.assign_pending")?;
    if !assigned.is_empty() {
        // The kernel's receipts, as the step's kernel (`segments.py`) writes them.
        record_assigned(tick, &assigned).at("support.assign_staff_support")?;
    }
    assign_operations(tick).at("staff.assign_operations")?;
    // Its decision is the policy's, called through `consider_complaint_response`.
    consider_due_complaints(tick).at("staff.consider_due_complaints")?;
    relief(tick).at("staff.relief")
}

// ---- The staff ------------------------------------------------------------------------------------

/// A namespace as the staff rules take it: absent where `model_state` has no such key or None
/// there (`held` of the staff glue).
fn staff_held(holding: &Holding, name: &str, other: &'static str) -> crate::error::Result<Held> {
    match holding.is_absent(name) {
        Ok(true) => Ok(Held::Absent),
        Ok(false) => Ok(holding.held(name, other)),
        Err(reason) => Err(Error::unsupported(reason)),
    }
}

/// What the staff rules read of the world besides the rows (`Staff::of`).
pub struct Staff {
    operations: Held,
    dynamics: Held,
    body: Held,
    shared: Held,
    metric_state_held: bool,
}

impl Staff {
    pub fn of(holding: &Holding) -> crate::error::Result<Self> {
        Ok(Self {
            operations: staff_held(holding, "staff_operations_v1", OTHER_DOCUMENT)?,
            dynamics: staff_held(holding, "staff_dynamics_v1", OTHER_DOCUMENT)?,
            body: staff_held(holding, "body_state_v1", OTHER_DOCUMENT)?,
            shared: staff_held(holding, "shared_operations_v1", "a value is not a plain dict")?,
            metric_state_held: holding.field(METRIC_STATE),
        })
    }

    pub fn inputs<'a>(&'a self, tasks: Option<&'a operations::Inputs<'a>>) -> Inputs<'a> {
        Inputs { operations: &self.operations, dynamics: &self.dynamics, body: &self.body, shared: &self.shared, metric_state_held: self.metric_state_held, tasks }
    }
}

/// What the rules of a task read (`Gathered` of the staff operation events' glue): the floor plan
/// where `geo`, the formula where `formula`, each kept with what its read raised.
pub struct Gathered<E> {
    staff: Held,
    movement: Held,
    body: Held,
    program: Held,
    shared_absent: bool,
    plan: Option<Plan<E>>,
    formula: Option<std::result::Result<Arc<crate::config::staff::StaffOperations>, E>>,
}

impl<E: crate::host::HostFailure + From<Error>> Gathered<E> {
    pub fn inputs(&self) -> operations::Inputs<'_> {
        operations::Inputs {
            staff: &self.staff,
            movement: &self.movement,
            body: &self.body,
            program: &self.program,
            shared_absent: self.shared_absent,
            geo: self.plan.as_ref().and_then(|plan| plan.get()),
            formula: self.formula.as_ref().and_then(|formula| formula.as_ref().ok()).map(|formula| &**formula),
        }
    }

    /// The core's result, with what a read raised where the core read what it could not get.
    pub fn finish<T>(self, result: std::result::Result<T, E>) -> std::result::Result<T, E> {
        let Err(error) = result else {
            return result;
        };
        let Some(text) = is_refusal(&error) else {
            return Err(error);
        };
        let plan = self.plan.and_then(|plan| plan.0.err());
        let formula = self.formula.and_then(Result::err);
        match (&*text, plan, formula) {
            (PLAN_UNREAD, Some(read), _) => Err(read),
            (SNAPSHOT_PLAN_UNREAD, Some(read), _) => Err(refused(SNAPSHOTS, read)),
            (FORMULA_UNREAD, _, Some(read)) => Err(read),
            _ => Err(error),
        }
    }
}

impl<C: WorldCell, H: Host, R: RunCell> Tick<'_, C, H, R> {
    /// `Gathered::of(world, geo, formula)`.
    pub fn gathered(&mut self, holding: &Holding, geo: bool, formula: bool) -> std::result::Result<Gathered<H::Error>, H::Error> {
        let staff = holding.held("staff_operations_v1", OTHER_DOCUMENT);
        let movement = holding.held(MOVEMENT, "the world holds another movement state");
        let body = holding.held("body_state_v1", "the world holds another body state");
        let program = holding.held("effect_program_v1", "the world holds another effect_program_v1");
        let shared_absent = !holding.keys.contains_key("shared_operations_v1");
        let plan = if geo { Some(self.plan()) } else { None };
        let formula = if formula {
            Some(match self.host_section(holding, SectionId::StaffOperations, |configs| &configs.staff_operations)? {
                Ok(formula) => Ok(formula),
                Err(fault) => Err(self.raise(SectionId::StaffOperations, &fault)),
            })
        } else {
            None
        };
        Ok(Gathered { staff, movement, body, program, shared_absent, plan, formula })
    }

    /// The staff rules' write result: a step's refusal where the floor plan could not be read is
    /// what reading it raised, as that step's `KernelRefusal` (`finish` of the staff glue).
    fn staff_written<T>(&mut self, result: WriteResult<T>) -> std::result::Result<T, H::Error> {
        let result = match result {
            Err(WriteError::Error(Error::Refused { label, reason })) if reason == PLAN_UNREAD || reason == SNAPSHOT_PLAN_UNREAD => match self.host.floor_plan() {
                Err(read) => {
                    return Err(match is_refusal(&read) {
                        Some(reason) => Error::Refused { label, reason: reason.into() }.into(),
                        None => read,
                    })
                }
                Ok(_) => Err(WriteError::Error(Error::Refused { label, reason })),
            },
            other => other,
        };
        self.written(result)
    }

    /// Let go the objects of the queue's slots the core took out.
    fn forget(&mut self, taken: &[u64]) -> std::result::Result<(), H::Error> {
        self.host.slots_changed(taken)
    }
}

/// `update_staff_dynamics`.
fn staff_dynamics<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>) -> std::result::Result<(), H::Error> {
    let holding = tick.holding()?;
    if tick.is_absent(&holding, crate::state::staff_dynamics::NAME)? {
        return Ok(());
    }
    let formula = tick.value_now(&holding, SectionId::StaffFatigue, |configs| &configs.staff_fatigue, crate::config::staff::StaffFatigue::parse)?;
    let minute = tick.minute;
    tick.world(|world| crate::rules::staff_dynamics::update(world, minute, &formula))
}

/// `update_staff_operations`.
fn update_operations<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>) -> std::result::Result<(), H::Error> {
    let holding = tick.holding()?;
    if tick.is_absent(&holding, "staff_operations_v1")? {
        return Ok(());
    }
    let staff = Staff::of(&holding)?;
    let minute = tick.minute;
    tick.world(|world| staff::update_operations(world, &staff.inputs(None), minute))
}

/// The staff service's parameters as the Run read them (`assign_pending_of_run`): a read that
/// raised is raised again by reading the service again (policy E14).
fn service_of<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>) -> std::result::Result<Service, H::Error> {
    let policies = tick.policies.clone();
    let read = policies.services.staff.object().and_then(|service| crate::run::services::staff_service(service));
    match read {
        Ok(service) => Ok(service),
        Err(fault) if fault.is_refusal() => Err(Error::unsupported(fault.text.to_string()).into()),
        Err(_) => {
            for attr in crate::run::services::STAFF_SPEC.attrs {
                tick.host.read_again(crate::host::Again::Policy(&["staff_service", attr.name]))?;
            }
            Err(Error::Runtime("a fault of the staff service did not come again".to_owned()).into())
        }
    }
}

/// `_assign_staff_support`'s `assign_pending`, with the service as the Run read it: the plain case
/// first, and where a request may be assigned, the queue's rows, the floor plan and the
/// controller's travel.
fn assign_pending<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>) -> std::result::Result<Vec<Arc<str>>, H::Error> {
    let service = service_of(tick)?;
    let holding = tick.holding()?;
    let staff = Staff::of(&holding)?;
    let minute = tick.minute;
    let mut taken = Vec::new();
    let first = tick.cell.with(|world| staff::assign_pending(world, &staff.inputs(None), &service, minute, None, &mut taken)).map_err(H::Error::from)?;
    match tick.staff_written(first)? {
        Assigned::Done(assigned) => Ok(assigned),
        Assigned::NeedsAll => {
            tick.host.read_queue_rows()?;
            let tasks = tick.gathered(&holding, true, false)?;
            let graph = match service.resolver {
                Resolver::Own => Some(graph(tick)?),
                _ => None,
            };
            let plan = tick.plan();
            let section = tick.value_section(&holding, SectionId::Movement, |configs| &configs.movement, Movement::parse)?;
            let movement = holding.held(MOVEMENT, "the world holds another movement state");
            let loads = holding.held("staff_operations_v1", OTHER_DOCUMENT);
            let travel = graph.as_ref().map(|graph| Travel {
                routing: crate::rules::travel::Routing { graph, geo: plan.get(), movement: &section, staff: &loads },
                walking: crate::rules::travel::Walking { movement: &movement, geo: plan.get() },
            });
            let full = Full { travel };
            let task_inputs = tasks.inputs();
            let inputs = staff.inputs(Some(&task_inputs));
            let result = tick.cell.with(|world| staff::assign_pending(world, &inputs, &service, minute, Some(&full), &mut taken)).map_err(H::Error::from)?;
            tick.forget(&taken)?;
            let result = tick.staff_written(result);
            drop(plan);
            match tasks.finish(result)? {
                Assigned::Done(assigned) => Ok(assigned),
                Assigned::NeedsAll => Err(Error::Runtime("the assignment asked for its inputs again".to_owned()).into()),
            }
        }
    }
}

/// `_record_assigned_support`: the receipts of the requests just assigned.
fn record_assigned<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>, assigned: &[Arc<str>]) -> std::result::Result<(), H::Error> {
    let holding = tick.holding()?;
    let shared = match holding.is_absent("shared_operations_v1") {
        Ok(true) if holding.keys.contains_key("shared_operations_v1") => Held::Other(support::NAMESPACE_IS_NONE),
        Ok(true) => Held::Absent,
        Ok(false) => holding.held("shared_operations_v1", support::OTHER_DOCUMENT),
        Err(reason) => Held::Other(reason),
    };
    let body_present = !tick.is_absent(&holding, crate::state::body_state::NAME)?;
    let inputs = support::Inputs { shared, body_present, metric_state_held: holding.field(METRIC_STATE) };
    let written = tick.cell.with(|world| support::record_assigned(world, &inputs, assigned)).map_err(H::Error::from)?;
    tick.written(written)
}

/// `assign_staff_operations` with the Run's staff router.
fn assign_operations<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>) -> std::result::Result<(), H::Error> {
    let holding = tick.holding()?;
    if tick.is_absent(&holding, "staff_operations_v1")? {
        return Ok(());
    }
    let minute = tick.minute;
    let known = tick.value_section(&holding, SectionId::StaffOperationsKnown, |configs| &configs.staff_operations_known, crate::config::staff::known_staff_operations)?;
    let staff = Staff::of(&holding)?;
    let first = tick.cell.with(|world| staff::assign_operations(world, &staff.inputs(None), &known, minute, None)).map_err(H::Error::from)?;
    match tick.staff_written(first)? {
        Operations::Done(_) => Ok(()),
        Operations::NeedsRoutes => {
            tick.host.read_queue_rows()?;
            let tasks = tick.gathered(&holding, true, true)?;
            let graph = router(tick, Which::Staff)?;
            let movement = holding.held(MOVEMENT, "the world holds another movement state");
            let loads = holding.held("staff_operations_v1", OTHER_DOCUMENT);
            let routed = Routed { router: &graph, movement: &movement, loads_staff: &loads };
            let task_inputs = tasks.inputs();
            let inputs = staff.inputs(Some(&task_inputs));
            let result = tick.cell.with(|world| staff::assign_operations(world, &inputs, &known, minute, Some(&routed))).map_err(H::Error::from)?;
            let result = tick.staff_written(result);
            match tasks.finish(result)? {
                Operations::Done(_) => Ok(()),
                Operations::NeedsRoutes => Err(Error::Runtime("the tasks asked for their routes again".to_owned()).into()),
            }
        }
    }
}

/// `_consider_due_complaints`: the policy's threshold as the Run read it, its decision through the
/// host.
fn consider_due_complaints<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>) -> std::result::Result<(), H::Error> {
    let policies = tick.policies.clone();
    let threshold = match policies.social.complaint_wait_minutes() {
        Ok(threshold) => threshold,
        Err(fault) if fault.is_refusal() => return Err(Error::unsupported(fault.text.to_string()).into()),
        Err(_) => {
            tick.host.read_again(crate::host::Again::Policy(&["social_policy", "limits", "complaint_wait_minutes"]))?;
            return Err(Error::Runtime("a fault of the social policy did not come again".to_owned()).into());
        }
    };
    let Some(threshold) = threshold else {
        return Ok(());
    };
    staff::consider_due_complaints(tick.cell, tick.host, Some(threshold), tick.minute)
}

/// `schedule_staff_operation_relief`.
fn relief<C: WorldCell, H: Host, R: RunCell>(tick: &mut Tick<'_, C, H, R>) -> std::result::Result<(), H::Error> {
    let holding = tick.holding()?;
    if tick.is_absent(&holding, "staff_operations_v1")? {
        return Ok(());
    }
    let minute = tick.minute;
    tick.host.read_queue_rows()?;
    let staff = Staff::of(&holding)?;
    let handoffs = tick.cell.with(|world| staff::relief(world, &staff.inputs(None), minute)).map_err(H::Error::from)?;
    tick.written(handoffs).map(drop)
}

// ---- The queue steps of the services' loops -----------------------------------------------------------

/// A minute a float holds exactly (`plain_minute` of the queues' glue).
fn plain_minute(minute: i64) -> crate::error::Result<i64> {
    if !(0..EXACT_INT).contains(&minute) {
        return Err(Error::unsupported("not a plain minute"));
    }
    Ok(minute)
}

/// `repr(kind)`, where the rule turns the kind down and its error names it.
fn kind_repr(kind: &str) -> Option<String> {
    match queues::service_of(kind) {
        Some(_) => None,
        None => Some(py_repr(kind)),
    }
}

impl<C: WorldCell, H: Host, R: RunCell> Tick<'_, C, H, R> {
    /// `create_queue_policy` of the configs (`policy_of` of the queues' glue).
    fn queue_policy(&mut self, holding: &Holding) -> std::result::Result<QueuePolicy, H::Error> {
        let config = self.value_now(holding, SectionId::FacilityQueues, |configs| &configs.facility_queues, QueueConfig::parse)?;
        let selected = match self.host_section(holding, SectionId::Selection, |configs| &configs.selection)? {
            Ok(selection) => selection.get("queue").cloned(),
            Err(fault) if fault.is_refusal() => return Err(Error::unsupported("not a selection").into()),
            Err(fault) => return Err(self.raise(SectionId::Selection, &fault)),
        };
        let need_weighted = match &selected {
            Some(model) => match &**model {
                "need-weighted-physical-queue-v1" => true,
                "strict-fifo-queue-v1" => false,
                _ => return Err(Error::unsupported("another queue model").into()),
            },
            None => config.has_geometry,
        };
        match config.policy(need_weighted) {
            Ok(policy) => Ok(*policy),
            Err(fault) if fault.class == FaultClass::Value => Err(Error::Value(fault.text.to_string()).into()),
            Err(fault) => Err(crate::rules::kit::kept::<()>(&Err(fault.clone())).err().unwrap_or_else(|| Error::Runtime("a fault read as a value".to_owned())).into()),
        }
    }

    /// The movement section, raised at once (`movement_of`).
    fn movement_now(&mut self, holding: &Holding) -> std::result::Result<Arc<Movement>, H::Error> {
        self.value_now(holding, SectionId::Movement, |configs| &configs.movement, Movement::parse)
    }

    /// What those who give up need (`Leave::read`).
    fn leave(&mut self, holding: &Holding, moving: &Held, independent: bool) -> std::result::Result<Leave, H::Error> {
        let metric_state_held = holding.field(METRIC_STATE);
        let movement = self.movement_now(holding)?;
        let placed = !matches!(moving, Held::Absent) && movement.has_geometry;
        let geo = if placed || independent { Some(self.host.floor_plan()?) } else { None };
        Ok(Leave { movement, geo, metric_state_held })
    }

    /// `prepare_facility_queue`.
    pub fn prepare_queue(&mut self, facility: &str, resource: bool) -> Step<(), H::Error> {
        self.prepare_shared(facility, resource).at("queues.prepare_facility_queue")
    }

    fn prepare_shared(&mut self, facility: &str, resource: bool) -> std::result::Result<(), H::Error> {
        let Some(kind) = self.world(|world| queues::queued_kind(world, facility, resource))? else {
            return Ok(());
        };
        let minute = plain_minute(self.minute)?;
        let holding = self.holding()?;
        let policy = self.queue_policy(&holding)?;
        let movement = holding.held(MOVEMENT, "the world holds another movement state");
        let repr = kind_repr(&kind);
        let (queues, service) = match resource {
            true => (holding.held(QUEUES, "the world holds another queue state"), holding.held(SERVICE, "the world holds another service state")),
            false => (Held::Absent, Held::Absent),
        };
        let prepare = Prepare { resource, minute, policy, movement, queues, service, kind_repr: repr.as_deref() };
        let mut result = self.cell.with(|world| queues::prepare(world, facility, &prepare, None)).map_err(H::Error::from)?;
        if matches!(result, Ok(Prepared::NeedsLeaveInputs)) {
            let leave = self.leave(&holding, &prepare.movement, false)?;
            result = self.cell.with(|world| queues::prepare(world, facility, &prepare, Some(&leave.inputs()))).map_err(H::Error::from)?;
        }
        self.written(result).map(drop)
    }

    /// `prepare_independent_facility_queue`.
    pub fn prepare_independent(&mut self, queue: &str, resource: bool) -> Step<(), H::Error> {
        self.prepare_one(queue, resource).at("queues.prepare_independent_facility_queue")
    }

    fn prepare_one(&mut self, queue: &str, resource: bool) -> std::result::Result<(), H::Error> {
        let minute = plain_minute(self.minute)?;
        let holding = self.holding()?;
        let queues_held = holding.held(QUEUES, "the world holds another queue state");
        let movement = holding.held(MOVEMENT, "the world holds another movement state");
        let found = self.world(|world| queues::independent_queue(world, queue, &queues_held, &movement))?;
        let Some((facility, kind)) = found else {
            return Ok(());
        };
        let policy = self.queue_policy(&holding)?;
        let repr = kind.as_deref().and_then(kind_repr);
        let mut prepare = Prepare { resource, minute, policy, movement, queues: queues_held, service: Held::Absent, kind_repr: repr.as_deref() };
        let first = self.cell.with(|world| queues::prepare_independent(world, queue, &facility, &prepare, None)).map_err(H::Error::from)?;
        if self.written(first)? == Prepared::Done {
            return Ok(());
        }
        // Someone gives up: a resource queue's stall is read once each has left.
        if resource {
            prepare.service = holding.held(SERVICE, "the world holds another service state");
        }
        let leave = self.leave(&holding, &prepare.movement, true)?;
        let done = self.cell.with(|world| queues::prepare_independent(world, queue, &facility, &prepare, Some(&leave.inputs()))).map_err(H::Error::from)?;
        self.written(done).map(drop)
    }

    /// The movement section of a placed world (`movement_v1` in the state and `facility_geometry`
    /// in the configs), or None: the walking kernels do nothing then (`placed`).
    fn placed(&mut self, holding: &Holding) -> std::result::Result<Option<Arc<Movement>>, H::Error> {
        if !holding.table {
            return Err(Error::unsupported(super::read::NOT_PLAIN).into());
        }
        if !holding.keys.contains_key(MOVEMENT) {
            return Ok(None);
        }
        let movement = self.movement_now(holding)?;
        Ok(movement.has_geometry.then_some(movement))
    }

    /// The floor plan of a placed world, which `physical_service_capacity` reads, or None.
    pub fn placement(&mut self) -> std::result::Result<Option<Arc<Geo>>, H::Error> {
        let holding = self.holding()?;
        match self.placed(&holding)? {
            Some(_) => Ok(Some(self.host.floor_plan()?)),
            None => Ok(None),
        }
    }

    /// What the walking kernels read of a placed world (`Walking::read`), or None.
    pub fn walking(&mut self, independent: bool) -> std::result::Result<Option<Walking>, H::Error> {
        let holding = self.holding()?;
        let Some(movement) = self.placed(&holding)? else {
            return Ok(None);
        };
        self.walking_of(&holding, movement, independent).map(Some)
    }

    fn walking_of(&mut self, holding: &Holding, movement: Arc<Movement>, independent: bool) -> std::result::Result<Walking, H::Error> {
        let geo = self.host.floor_plan()?;
        let queues = if independent { holding.held(QUEUES, "the world holds another queue state") } else { Held::Absent };
        let moving = holding.held(MOVEMENT, "the world holds another movement state");
        Ok(Walking { movement, geo, moving, queues, metric_state_held: holding.field(METRIC_STATE) })
    }

    /// `progress_physical_queue_positions` (`head` false) or `progress_physical_queue_head_to_service`.
    pub fn progress(&mut self, facility: &str, resource: bool, head: bool, queue: Option<&str>) -> Step<bool, H::Error> {
        let label = if head { "queues.progress_physical_queue_head_to_service" } else { "queues.progress_physical_queue_positions" };
        self.walk_queue(facility, resource, head, queue).at(label)
    }

    fn walk_queue(&mut self, facility: &str, resource: bool, head: bool, queue: Option<&str>) -> std::result::Result<bool, H::Error> {
        let holding = self.holding()?;
        let Some(movement) = self.placed(&holding)? else {
            return Ok(false);
        };
        let minute = plain_minute(self.minute)?;
        let walking = self.walking_of(&holding, movement, queue.is_some())?;
        let kernel = if head { queues::head_to_service } else { queues::positions };
        let result = self.cell.with(|world| kernel(world, &walking.walk(), facility, resource, minute, queue)).map_err(H::Error::from)?;
        self.written(result)
    }

    /// The queues of a facility with independent ones, or None (`independent_queues`).
    pub fn independent_queues(&mut self, facility: &str) -> Step<Option<Vec<Arc<str>>>, H::Error> {
        let holding = self.holding().bare()?;
        let held = holding.held(QUEUES, "the world holds another queue state");
        if matches!(held, Held::Absent) {
            return Ok(None);
        }
        let has = self.world(|world| Ok(fifo::queues(world, &held)?.is_some_and(|row| fifo::has_independent(&row, facility)))).bare()?;
        if !has {
            return Ok(None);
        }
        let geo = self.host.floor_plan().bare()?;
        Ok(Some(fifo::queue_ids(&geo, facility).into_iter().map(Arc::from).collect()))
    }

    /// `raise StateInvariantError(f"{label} queue {queue_id!r} lost its signed point")`, as needed.
    pub fn check_signed_point(&mut self, queue: &str, label: &str) -> Step<(), H::Error> {
        let geo = self.host.floor_plan().bare()?;
        if fifo::point_of(&geo, queue).is_some() {
            return Ok(());
        }
        Err(Error::Invariant(format!("{label} queue {} lost its signed point", py_repr(queue))).into()).bare()
    }

    /// Whether the independent queue has anyone in it.
    pub fn queue_has_people(&mut self, queue: &str) -> Step<bool, H::Error> {
        let holding = self.holding().bare()?;
        let held = holding.held(QUEUES, "the world holds another queue state");
        self.world(|world| {
            let Some(row) = fifo::queues(world, &held)? else {
                return Err(Error::unsupported("no queue state"));
            };
            match row.queues.get(queue) {
                Some(queue) => Ok(!queue.queued_request_ids.is_empty()),
                None => Err(Error::unsupported("no such queue")),
            }
        })
        .map(|found| found)
        .bare()
    }

    /// The label of an error of the driver's own, in a loop.
    pub fn run_tick_label(&self) -> &'static str {
        RUN_TICK
    }
}

/// What those who give up need.
struct Leave {
    movement: Arc<Movement>,
    geo: Option<Arc<Geo>>,
    metric_state_held: bool,
}

impl Leave {
    fn inputs(&self) -> LeaveInputs<'_> {
        LeaveInputs { has_geometry: self.movement.has_geometry, model: &self.movement.model, geo: self.geo.as_deref(), metric_state_held: self.metric_state_held }
    }
}

/// What the walking kernels read of a placed world.
pub struct Walking {
    movement: Arc<Movement>,
    pub geo: Arc<Geo>,
    moving: Held,
    queues: Held,
    metric_state_held: bool,
}

impl Walking {
    pub fn walk(&self) -> Walk<'_> {
        Walk { geo: &self.geo, model: &self.movement.model, movement: self.moving.clone(), queues: self.queues.clone(), metric_state_held: self.metric_state_held }
    }
}
