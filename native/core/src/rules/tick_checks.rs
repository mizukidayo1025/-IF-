//! The questions of the end-of-tick check (`rules::after_tick::Checks`) answered by the core, for a
//! world without Python (#250): `CoreChecks`. The native path answers them with the validators
//! Python registers (`state_mirror._RUST_VALIDATORS`, `body_state._ROWS_VALID`,
//! `common_operations._TICK_VALID`, `effects.validate_state`, `tick_validation.body_config_ids`),
//! which are the PyO3 crate's glue around the core's rules; here the same rules read the core's
//! world: the slots' rows, the entity rows, the queue's rows and the log.
//!
//! Each answer is yes only where the namespace's Python validator passes. Anything the core does
//! not hold as the rules read it (a namespace held as another row than its own, a tainted table, a
//! floor plan the host cannot give) is a no or a refusal, and the check then leaves the tick to the
//! whole validation (`validate::world`), which passes or gives Python's error. So a no never
//! changes an answer; it only costs time.
//!
//! Where the PyO3 crate reads a Python object instead of a row (the staff operations' active
//! records read the task, staff and edge objects and the pending events; the shared operations
//! read the log's records through `EventLog.find`), the rule is the same and reads the core's
//! rows; the rules on the namespaces' own rows are one function each, which both call.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::config::{SectionId, TickPlan};
use crate::entities::{Entities, RequestStatus};
use crate::error::{Error, Result};
use crate::events::EventRow;
use crate::geometry::Geo;
use crate::host::{Holding, Host, HostFailure};
use crate::row::{Row, ValueRow};
use crate::rules::after_tick::Checks;
use crate::rules::kit::{self, Held};
use crate::rules::movement::Validated;
use crate::rules::overlay::PLAN_UNREAD;
use crate::rules::tick_validation::{self as entity, Dynamics};
use crate::rules::{effects, facility_queues, movement, overlay, resources};
use crate::state::agent_dynamics::{self, AgentDynamicsRow};
use crate::state::body_state::{self, BodyRow};
use crate::state::information::{self, InformationRow};
use crate::state::psychological_distress::{self, DistressRow};
use crate::state::resource_ledger::{self, LedgerRow};
use crate::state::resource_service::{self, ServiceRow, ServiceWorld};
use crate::state::shared_operations::{self, CareStatus, SharedOperationsRow};
use crate::state::social_network::{self, SocialNetworkRow};
use crate::state::staff_dynamics::{self, StaffDynamicsRow};
use crate::state::staff_operations::{self, Active, StaffOperationsRow};
use crate::validate::View;
use crate::value::V;
use crate::world::{World, WorldCell};

const QUEUES: &str = "facility_queues_v1";
const MOVEMENT: &str = "movement_v1";
const OTHER_TREE: &str = "the world holds another tree than the namespace's";

#[track_caller]
fn refuse<T>(reason: &'static str) -> Result<T> {
    Err(Error::unsupported(reason))
}

/// The end-of-tick check's questions, answered from the core's world (module doc). `asked` keeps
/// what was asked, for the tests.
pub struct CoreChecks<'a, C: WorldCell, H: Host> {
    cell: &'a C,
    host: &'a mut H,
    holding: &'a Holding,
    plan: &'a TickPlan,
    /// The floor plan once read: None where the host could not give it.
    geo: Option<Option<Arc<Geo>>>,
    pub asked: Vec<String>,
}

impl<'a, C: WorldCell, H: Host> CoreChecks<'a, C, H> {
    /// The questions of one check of this world, whose `model_state` holds what `holding` says and
    /// whose configs give `plan`.
    pub fn new(cell: &'a C, host: &'a mut H, holding: &'a Holding, plan: &'a TickPlan) -> Self {
        Self { cell, host, holding, plan, geo: None, asked: Vec::new() }
    }

    fn held(&self, name: &str) -> Held {
        self.holding.held(name, OTHER_TREE)
    }

    /// The floor plan (`Geo::of`), read once; None where the host cannot give it.
    fn geo(&mut self) -> Option<Arc<Geo>> {
        if self.geo.is_none() {
            self.geo = Some(self.host.floor_plan().ok());
        }
        self.geo.clone().flatten()
    }

    fn with<R>(&self, f: impl FnOnce(&mut World) -> Result<R>) -> Result<R> {
        self.cell.with(f)?
    }

    /// The validator registered for the namespace, as `state_mirror._RUST_VALIDATORS` holds them.
    fn validator(&mut self, name: &str) -> Result<Option<bool>> {
        let held = self.held(name);
        let answer = match name {
            agent_dynamics::NAME => self.agent_dynamics(&held)?,
            entity::DYNAMICS => self.with(|world| population_dynamics(world, &held))?,
            entity::LIFECYCLE => self.with(|world| Ok(crate::validate::population_lifecycle::validate(&View::new(world, false, None)).is_ok()))?,
            entity::PRESENCE => {
                let lifecycle = self.held(entity::LIFECYCLE);
                self.with(|world| entity::validate_presence(world, &held, &lifecycle).map(|()| true))?
            }
            social_network::NAME => self.with(|world| {
                if absent(world, &held, social_network::NAME)? {
                    return Ok(true);
                }
                untouched::<SocialNetworkRow, _>(world, social_network::NAME, |row| row.is_valid())
            })?,
            staff_dynamics::NAME => self.with(|world| {
                if absent(world, &held, staff_dynamics::NAME)? {
                    return Ok(true);
                }
                let row = world.row::<StaffDynamicsRow>(staff_dynamics::NAME)?;
                Ok(row.accepts(&*world.rows()?))
            })?,
            staff_operations::NAME => self.with(|world| staff_operations_pass(world, &held))?,
            overlay::SPACES_KEY => self.with(|world| overlay::validate_spaces(world, &held))?,
            MOVEMENT => self.movement(&held)?,
            overlay::OVERLAY_KEY => self.overlay(&held)?,
            QUEUES => self.queues(&held)?,
            resource_service::NAME => self.with(|world| service_passes(world, self.holding, &held))?,
            resource_ledger::NAME => self.with(|world| {
                if absent(world, &held, resource_ledger::NAME)? {
                    return Ok(false);
                }
                let row = world.row::<LedgerRow>(resource_ledger::NAME)?;
                resources::ledger_passes(world, &row)
            })?,
            information::NAME => self.with(|world| {
                let duration = world.duration_minutes()?;
                let rows = entity::tables(world)?;
                let agents: HashSet<&str> = rows.agents.iter().map(|(id, _)| &**id).collect();
                let row = world.row::<InformationRow>(information::NAME)?;
                row.is_valid(&agents, duration, |zone| Ok::<bool, Error>(rows.zones.get(zone).is_some()))
            })?,
            psychological_distress::NAME => self.with(|world| Ok(world.row::<DistressRow>(psychological_distress::NAME)?.is_valid()))?,
            _ => return Ok(None),
        };
        Ok(Some(answer))
    }

    /// `validate_agent_dynamics_state` with the policy the configs select.
    fn agent_dynamics(&mut self, held: &Held) -> Result<bool> {
        if self.with(|world| absent(world, held, agent_dynamics::NAME))? {
            return Ok(true);
        }
        let policy = kit::kept_section_value(self.cell, self.host, self.holding, SectionId::AgentDynamicsPolicy, |configs| &configs.agent_dynamics_policy, crate::config::agent_dynamics::policy)
            .map_err(host_refusal)?;
        let policy = *kit::kept(&policy)?;
        self.with(|world| {
            let row = world.row::<AgentDynamicsRow>(agent_dynamics::NAME)?;
            Ok(row.accepts(policy, &*world.rows()?))
        })
    }

    /// `validate_movement_runtime_state`: the floor plan is read only for journeys in rooms.
    fn movement(&mut self, held: &Held) -> Result<bool> {
        if self.with(|world| absent(world, held, MOVEMENT))? {
            return Ok(true);
        }
        if !matches!(held, Held::Held) {
            return refuse(movement::OTHER_MOVEMENT);
        }
        let queues = self.held(QUEUES);
        let found = self.with(|world| movement::validate(world, &queues, None))?;
        let found = match found {
            Validated::NeedsGeo => {
                let Some(geo) = self.geo() else {
                    return Ok(false);
                };
                self.with(|world| movement::validate(world, &queues, Some(&geo)))?
            }
            found => found,
        };
        match found {
            Validated::Is(valid) => Ok(valid),
            Validated::NeedsGeo => refuse("the floor plan was asked for twice"),
        }
    }

    /// `validate_geometry_overlay_state`: the floor plan is read only where the rule reads it.
    fn overlay(&mut self, held: &Held) -> Result<bool> {
        match self.with(|world| overlay::check_overlay(world, held, None)) {
            Ok(()) => Ok(true),
            Err(Error::Unsupported(reason)) if !matches!(held, Held::Absent) && reason == PLAN_UNREAD => {
                let Some(geo) = self.geo() else {
                    return Ok(false);
                };
                self.with(|world| overlay::check_overlay(world, held, Some(&geo))).map(|()| true)
            }
            Err(error) => Err(error),
        }
    }

    /// `validate_facility_queue_state`.
    fn queues(&mut self, held: &Held) -> Result<bool> {
        let required = self.plan.independent_queues;
        match held {
            Held::Absent => return Ok(!required),
            Held::Other(reason) => return Err(Error::unsupported(*reason)),
            Held::Held => {}
        }
        if !required || !matches!(self.held(facility_queues::LEGACY_KEY), Held::Absent) {
            return Ok(false);
        }
        let Some(geo) = self.geo() else {
            return Ok(false);
        };
        let movement = self.held(MOVEMENT);
        self.with(|world| facility_queues::validate(world, &Held::Held, &geo, &movement))
    }
}

/// A host's error as a refusal: the whole validation then says what is wrong.
fn host_refusal<E: HostFailure>(error: E) -> Error {
    Error::unsupported(error.text())
}

/// `namespace_is_absent`: `model_state` has no such key, or holds None there.
fn absent(world: &World, held: &Held, name: &str) -> Result<bool> {
    match held {
        Held::Absent => Ok(true),
        Held::Other(reason) => Err(Error::unsupported(*reason)),
        Held::Held => {
            let Some(slot) = world.slots.get(name) else {
                return refuse("the namespace is not owned");
            };
            let slot = slot.try_borrow()?;
            match slot.row.as_deref() {
                None => refuse("the namespace has no row"),
                Some(row) => Ok(row.as_any().downcast_ref::<ValueRow>().is_some_and(|row| matches!(row.0, V::Null))),
            }
        }
    }
}

/// `f` on the row of a slot as its type, borrowed mutably without a write: what the rule keeps in
/// the row (which relationships it has checked) is no change of the state, so the slot keeps its
/// stamp and its tree, as the PyO3 crate's `with_row`.
fn untouched<T: Row, R>(world: &World, name: &str, f: impl FnOnce(&mut T) -> R) -> Result<R> {
    let slot = world.slots.get(name).ok_or_else(|| Error::unsupported("the namespace is not owned"))?;
    let mut slot = slot.try_borrow_mut()?;
    Ok(f(slot.row_mut::<T>()?))
}

/// `validate_population_dynamics_state`: the core's check of a state with no one pending and no
/// context, and else the rule itself.
fn population_dynamics(world: &World, held: &Held) -> Result<bool> {
    Ok(match entity::population_dynamics(world, held)? {
        Dynamics::Taken => true,
        Dynamics::Validate => crate::validate::population_dynamics::validate(&View::new(world, false, None)).is_ok(),
    })
}

/// What `ServiceRow::passes_rules` reads of a world without Python: the people here or archived by
/// the lifecycle state, and the zones.
struct Known<'a> {
    rows: &'a Entities,
    archived: Option<HashSet<Arc<str>>>,
}

impl ServiceWorld for Known<'_> {
    type Error = Error;

    fn person(&self, id: &str) -> Result<bool> {
        Ok(self.rows.agents.get(id).is_some() || self.archived.as_ref().is_some_and(|people| people.contains(id)))
    }

    fn zone(&self, id: &str) -> Result<bool> {
        Ok(self.rows.zones.get(id).is_some())
    }
}

/// The part of `WorldState.validate` a tick runs for `resource_service_v1`.
fn service_passes(world: &World, holding: &Holding, held: &Held) -> Result<bool> {
    if absent(world, held, resource_service::NAME)? {
        return Ok(false);
    }
    let row = world.row::<ServiceRow>(resource_service::NAME)?;
    // Where a facility has independent queues, each queue's head is a head of the facility.
    let mut heads: HashMap<Arc<str>, Vec<Arc<str>>> = HashMap::new();
    let queues = holding.held(QUEUES, OTHER_TREE);
    if !matches!(queues, Held::Absent) {
        if let Some(queues) = kit::held_row::<crate::state::facility_queues::QueuesRow>(world, QUEUES, &queues)? {
            for facility_id in row.stall_state.keys() {
                if let Some(found) = facility_queues::independent_heads(&queues, facility_id)? {
                    heads.insert(facility_id.clone(), found);
                }
            }
        }
    }
    // The keys of `population_lifecycle_v1.people`, as the value the world holds.
    let lifecycle = match holding.held(entity::LIFECYCLE, OTHER_TREE) {
        Held::Absent => None,
        Held::Other(reason) => return Err(Error::unsupported(reason)),
        Held::Held => world.slots.try_value(entity::LIFECYCLE)?,
    };
    let archived = match lifecycle.as_deref() {
        Some(V::Map(lifecycle)) => match lifecycle.get("people") {
            Some(V::Map(people)) => Some(people.keys().cloned().collect()),
            _ => None,
        },
        _ => None,
    };
    let minute = world.minute_index()?;
    let rows = entity::tables(world)?;
    row.passes_rules(&rows, &Known { rows: &rows, archived }, minute, &heads)
}

/// `validate_staff_operations_state`: the rules of the row, then those of each active record that
/// read its task, its staff member, the graph and the pending events.
fn staff_operations_pass(world: &World, held: &Held) -> Result<bool> {
    if absent(world, held, staff_operations::NAME)? {
        return Ok(true);
    }
    let rows = entity::tables(world)?;
    let zones: HashSet<String> = rows.zones.iter().map(|(id, _)| id.to_string()).collect();
    let row = world.row::<StaffOperationsRow>(staff_operations::NAME)?;
    let Some(active) = row.active_records(&zones) else {
        return Ok(false);
    };
    if !row.agrees_with(&rows, &zones) {
        return Ok(false);
    }
    drop(row);
    active_records_pass(world, &rows, &active)
}

/// The pending events in queue order, as rows.
fn pending_rows(world: &World) -> Result<Vec<Arc<EventRow>>> {
    let queue = world.queue()?;
    let mut rows = Vec::with_capacity(queue.ordered().len());
    for (_, serial) in queue.ordered() {
        let Some(row) = &queue.slot(*serial).row else {
            return refuse("an event in the queue is not a plain one");
        };
        rows.push(row.clone());
    }
    Ok(rows)
}

/// `event.payload.get(key)` as the rule compares it with a str or None: `same(Some(text))` for an
/// equal str, `same(None)` for None or no key.
fn payload_same(event: &EventRow, key: &str, expected: Option<&str>) -> Result<bool> {
    let V::Map(fields) = &event.payload else {
        return refuse("a payload is not a mapping");
    };
    Ok(match (fields.get(key), expected) {
        (None | Some(V::Null), None) => true,
        (Some(V::Str(text)), Some(expected)) => &**text == expected,
        _ => false,
    })
}

#[track_caller]
fn sum(first: i64, second: i64) -> Result<i64> {
    first.checked_add(second).ok_or_else(|| Error::unsupported("a route's minutes out of range"))
}

/// The rules of the active records that read the world (`validate_staff_operations_state`), from
/// the entity rows and the pending events' rows.
fn active_records_pass(world: &World, rows: &Entities, active: &[Active]) -> Result<bool> {
    let pending = pending_rows(world)?;
    for record in active {
        let Some(task) = rows.active_tasks.get(&record.task_id) else {
            return Ok(false);
        };
        let Some(member) = rows.staff.get(&record.staff_id) else {
            return Ok(false);
        };
        if task.staff_id.as_deref() != Some(&*record.staff_id)
            || member.active_task_id.as_deref() != Some(&*record.task_id)
            || member.work_queue.len() != 1
            || *member.work_queue[0] != *record.task_id
            || member.busy_until != record.due
            || rows.zones.get(&record.target_zone_id).is_none()
            || record.route_zones.iter().any(|zone| rows.zones.get(zone).is_none())
            || record.route_edges.iter().any(|edge| rows.zone_edges.get(edge).is_none())
            || record.target_id.as_ref().is_some_and(|target| rows.facilities.get(target).is_none())
        {
            return Ok(false);
        }
        let member_zone = &*member.zone_id;
        let travel_end = sum(record.started, record.travel_minutes)?;
        if record.working {
            if !record.route_zones.is_empty()
                || !record.route_edges.is_empty()
                || record.travel_due.is_some()
                || record.arrival != Some(travel_end)
                || member_zone != record.target_zone_id
            {
                return Ok(false);
            }
        } else {
            // route_is_valid
            if record.route_zones.len() != record.route_edges.len() + 1
                || record.route_edges.is_empty()
                || record.route_zones[0] != member_zone
                || record.route_zones.last().map(String::as_str) != Some(&*record.target_zone_id)
            {
                return Ok(false);
            }
            let mut total: i64 = 0;
            for (index, edge_id) in record.route_edges.iter().enumerate() {
                let Some(edge) = rows.zone_edges.get(edge_id) else {
                    return Ok(false);
                };
                if *edge.from_zone_id != record.route_zones[index] || *edge.to_zone_id != record.route_zones[index + 1] {
                    return Ok(false);
                }
                total = sum(total, edge.travel_minutes)?;
            }
            if total != record.travel_minutes || record.travel_due != Some(travel_end) || record.arrival.is_some() {
                return Ok(false);
            }
        }
        if record.due != sum(travel_end, record.duration)? {
            return Ok(false);
        }
        let (mut completions, mut arrivals) = (Vec::new(), Vec::new());
        for event in &pending {
            let is_completion = &*event.event_type == "staff_operation_complete";
            if !is_completion && &*event.event_type != "staff_operation_arrive" {
                continue;
            }
            if !payload_same(event, "task_id", Some(&record.task_id))? || !payload_same(event, "staff_id", Some(&record.staff_id))? {
                continue;
            }
            if is_completion {
                if event.due_minute == record.due {
                    completions.push(event);
                }
            } else {
                arrivals.push(event);
            }
        }
        let expected_arrivals = usize::from(!record.working);
        if completions.len() != 1
            || !payload_same(completions[0], "task_kind", Some(&record.task_kind))?
            || !payload_same(completions[0], "target_id", record.target_id.as_deref())?
            || arrivals.len() != expected_arrivals
        {
            return Ok(false);
        }
        if let Some(arrival) = arrivals.first() {
            if Some(arrival.due_minute) != record.travel_due || !payload_same(arrival, "target_zone_id", Some(&record.target_zone_id))? {
                return Ok(false);
            }
        }
    }
    Ok(true)
}

/// `_validate_staff_operation_receipt`'s reading of the retained completions, for a receipt of the
/// shared operations.
fn completion_agrees(world: &World, staff: &Held, receipt: &serde_json::Value, truncated: bool) -> Result<bool> {
    let task_id = receipt.get("task_id").and_then(serde_json::Value::as_str);
    let minute = receipt.get("minute").and_then(serde_json::Value::as_i64);
    let (Some(task_id), Some(minute)) = (task_id, minute) else {
        return refuse("a staff operation receipt lacks its task or minute");
    };
    if absent(world, staff, staff_operations::NAME)? {
        // No retained completions: the rule finds no match.
        return Ok(false);
    }
    Ok(world.row::<StaffOperationsRow>(staff_operations::NAME)?.completion_agrees(task_id, minute, receipt, truncated))
}

/// `validate_shared_operations_state` of a tick (no replay of the history), `_TICK_VALID`: the
/// recent receipts the core's rules appended and checked, each of an event the log holds; each
/// head of a request agreeing with its request; the staff member each assignment's event names.
fn shared_operations_pass(world: &World, held: &Held, staff: &Held) -> Result<bool> {
    if absent(world, held, shared_operations::NAME)? {
        return Ok(true);
    }
    let minute = world.minute_index()?;
    // (the request's staff member, the event that says who has it) of each head with one.
    let mut assigned: Vec<(Option<Arc<str>>, Arc<str>)> = Vec::new();
    let mut staff_receipts: Vec<Box<[u8]>> = Vec::new();
    let accepted = untouched::<SharedOperationsRow, _>(world, shared_operations::NAME, |row| -> Result<bool> {
        if !row.header_is_valid() || row.history.items.len() > shared_operations::MAX_HISTORY {
            return Ok(false);
        }
        let Some(recent) = row.history.recent_verified(minute) else {
            return Ok(false);
        };
        let log = world.log()?;
        let mut seen: Vec<&str> = Vec::new();
        for item in recent {
            let Some(event_id) = item.event_id.as_deref() else {
                return Ok(false);
            };
            if seen.contains(&event_id) || !log.has_event_id(event_id) {
                return Ok(false);
            }
            seen.push(event_id);
            if item.reads_staff_history {
                staff_receipts.push(item.bytes.clone());
            }
        }
        let rows = world.rows()?;
        for (id, head) in &row.support_requests.map {
            let Some(request) = rows.support_requests.get(id) else {
                return Ok(false);
            };
            let expected = if head.status == CareStatus::Requested { RequestStatus::Pending } else { RequestStatus::Assigned };
            if request.agent_id != head.agent_id || request.status != expected {
                return Ok(false);
            }
            if head.status != CareStatus::Requested {
                assigned.push((request.assigned_staff_id.clone(), head.latest_event_id.clone()));
            }
        }
        Ok(true)
    })??;
    if !accepted {
        return Ok(false);
    }
    let truncated = world.row::<SharedOperationsRow>(shared_operations::NAME)?.history_truncated;
    for bytes in &staff_receipts {
        let Ok(receipt) = serde_json::from_slice::<serde_json::Value>(bytes) else {
            return Ok(false);
        };
        if !completion_agrees(world, staff, &receipt, truncated)? {
            return Ok(false);
        }
    }
    for (staff, event_id) in assigned {
        let evidence = match crate::load::find_record(world, &event_id)? {
            None => None,
            Some(record) => {
                let V::Map(payload) = &record.event.payload else {
                    return refuse("a payload is not a mapping");
                };
                payload.get("staff_id").filter(|value| !matches!(value, V::Null)).cloned()
            }
        };
        let same = match (&evidence, &staff) {
            (None, None) => true,
            (Some(V::Str(value)), Some(staff)) => value == staff,
            _ => false,
        };
        if !same {
            return Ok(false);
        }
    }
    Ok(true)
}

impl<C: WorldCell, H: Host> Checks for CoreChecks<'_, C, H> {
    fn namespace(&mut self, name: &str) -> Result<Option<bool>> {
        self.asked.push(format!("namespace {name}"));
        self.validator(name)
    }

    fn body_rows(&mut self) -> Result<bool> {
        self.asked.push("body_rows".to_owned());
        let held = self.held(body_state::NAME);
        self.with(|world| {
            if absent(world, &held, body_state::NAME)? {
                return Ok(false);
            }
            let rows = world.rows()?;
            untouched::<BodyRow, _>(world, body_state::NAME, |row| row.rows_valid(&rows.agents))
        })
    }

    fn shared_operations(&mut self) -> Result<bool> {
        self.asked.push("shared_operations".to_owned());
        let (held, staff) = (self.held(shared_operations::NAME), self.held(staff_operations::NAME));
        self.with(|world| shared_operations_pass(world, &held, &staff))
    }

    fn effects(&mut self) -> Result<()> {
        self.asked.push("effects".to_owned());
        let program = self.held(effects::MARKER);
        effects::check_state(self.cell, self.host, &program, false).map_err(host_refusal)
    }

    fn body_config_ids(&mut self) -> Result<Vec<Arc<str>>> {
        self.asked.push("body_config_ids".to_owned());
        self.with(|world| {
            let config = world.row::<BodyRow>(body_state::NAME)?.config.clone();
            crate::validate::body_state::body_config_ids(&View::new(world, false, None), &config)
        })
    }
}

/// After the whole validation without the event history passed where the kernel's check refused:
/// what the Python rules tell the rows when they pass (`_tell_the_row_they_passed` of
/// `domain/common_operations.py`, `recent_passed`), so the next check reads the receipts they
/// read as checked.
pub fn validation_passed(world: &World) -> Result<()> {
    if world.slots.get(shared_operations::NAME).is_none() {
        return Ok(());
    }
    let minute = world.minute_index()?;
    match untouched::<SharedOperationsRow, _>(world, shared_operations::NAME, |row| row.history.recent_passed(minute)) {
        // A namespace held as another row than its own: there is nothing to tell (`except
        // native.Unsupported: pass`).
        Err(Error::Unsupported(_)) => Ok(()),
        other => other,
    }
}
