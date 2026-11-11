//! `validate_staff_operation_order` and `order_staff_operation` of `domain/staff_operations.py`: an
//! operator's order of one routed staff task, as an EffectProgram's `staff_task_order` gives it in the
//! tick (`effects.rs` `staff_task_order`, in its transaction), on the rows of a `World` (moved to the
//! core in P8 b2). The PyO3 crate's file of the same name reads what Python holds
//! (`staff_operation_events::Inputs`) and calls `order_operation`.
//!
//! The checks are the Python rule's, in its order, with its texts: each is raised before anything is
//! written. What the rule does not let Rust decide is Unsupported, before the first write too. The
//! writes are `order_staff_operation`'s, in its order: the row of `staff_operations_v1`, the task,
//! the staff member, the events and the staff positions (`staff_operation_events::snapshots`).
//! `validate_staff_operation_order` stays in Python for `validate_effect_program`, which checks a
//! program before it is scheduled (docs/rust-port/lanes/l8-texts-events.md).

use std::collections::HashSet;
use std::sync::Arc;

use indexmap::IndexMap;

use crate::entities::{Entities, TaskRow};
use crate::error::{Error, Result};
use crate::events::writer::WriteResult;
use crate::events::EventKind;
use crate::routes::Route;
use crate::row::ValueRow;
use crate::rules::kit::{self, Events, Held};
use crate::rules::staff_operation_events::{dynamics_available, relief_skills, snapshots, Inputs, FORMULA_UNREAD};
use crate::rules::travel::{self, ZONES_TAINTED};
use crate::state::body_state::{BodyRow, NAME as BODY};
use crate::state::effect_program::{EffectProgramRow, NAME as PROGRAM};
use crate::state::staff_operations::{OperationRecord, Phase, StaffOperationsRow, NAME};
use crate::value::V;
use crate::world::{World, MODEL_CONFIGS};

/// `_TASK_NAMES`.
fn task_name(kind: &str) -> &'static str {
    match kind {
        "cleaning" => "清掃",
        "maintenance" => "保守",
        "information_outreach" => "情報提供",
        "intake" => "受付",
        _ => "巡回",
    }
}

/// An order, as `order_staff_operation` takes it.
pub struct Order<'a> {
    pub staff_id: &'a str,
    pub task_kind: &'a str,
    pub target_zone_id: &'a str,
    pub target_id: Option<&'a str>,
    pub reason: &'a str,
    pub causation_event_id: &'a str,
    pub source_id: Option<&'a str>,
}

#[track_caller]
fn failed<T>(text: &str) -> Result<T> {
    Err(Error::failed(text))
}

/// `_missing_task_skill_message`: who could take the order instead, by ID.
fn missing_skill_message(rows: &Entities, staff_id: &str, kind: &str, skills: [&'static str; 2]) -> String {
    let mut qualified: Vec<String> = rows
        .staff
        .iter()
        .filter(|(_, other)| other.skills.0.iter().any(|skill| skills.contains(&&**skill)))
        .map(|(id, _)| id.to_string())
        .collect();
    qualified.sort_unstable();
    let mut sorted = skills;
    sorted.sort_unstable();
    let name = task_name(kind);
    let skill_list = sorted.join("・");
    if !qualified.is_empty() {
        return format!(
            "staff_lacks_task_skill: {staff_id}は{name}の技能（{skill_list}）を持ちません。技能を持つ職員（{}）に指示してください。",
            qualified.join("、")
        );
    }
    let advice = if kind == "maintenance" {
        "設備の修理は修理の手配（facility_repair）で指示するか、担当変更（set_staff_role）で職員に技能を与えてから指示してください。"
    } else {
        "担当変更（set_staff_role）で職員に技能を与えてから指示してください。"
    };
    format!("no_qualified_staff: {name}の技能（{skill_list}）を持つ職員がいません。{advice}")
}

/// What the checks found: the route and the task's minutes of work.
struct Checked {
    route: Route,
    duration: i64,
}

/// `validate_staff_operation_order(world, ..., minute=minute)`: the route, or the rule's text.
fn validate(world: &World, inputs: &Inputs<'_>, order: &Order<'_>, minute: i64) -> Result<Checked> {
    // `state is None or SHARED_OPERATIONS_KEY not in world.model_state`.
    if matches!(inputs.staff, Held::Absent) || inputs.shared_absent {
        return failed("staff task order requires staff and shared operation state");
    }
    let (idle, config) = {
        let Some(row) = kit::held_row::<StaffOperationsRow>(world, NAME, inputs.staff)? else {
            return failed("staff task order requires staff and shared operation state");
        };
        (row.staff.get(order.staff_id).map(|record| record.phase == Phase::Idle), row.config.clone())
    };
    let staff = world.rows()?.staff.get(order.staff_id).map(|staff| (staff.zone_id.clone(), staff.skills.0.clone(), staff.active_task_id.is_some()));
    let (Some((zone, skills, has_task)), Some(idle)) = (staff, idle) else {
        return failed("staff task order references unknown staff");
    };
    // Each test is read only if those before it passed, as `or` reads them.
    let busy = !idle
        || has_task
        || !crate::rules::staff_dynamics::with_staff_status(world, |status| crate::rules::staff_dynamics::is_available(status, order.staff_id))?
        || !dynamics_available(world, order.staff_id, minute)?;
    if busy {
        return failed("staff task order requires an idle, available, on-shift worker");
    }
    let Some(required) = relief_skills(order.task_kind) else {
        return failed("staff task order uses an unregistered task kind");
    };
    if !skills.iter().any(|skill| required.contains(&&**skill)) {
        let text = missing_skill_message(&*world.rows()?, order.staff_id, order.task_kind, required);
        return Err(Error::failed(text));
    }
    {
        let rows = world.entities.try_borrow()?;
        if rows.zones_tainted().is_some() {
            return Err(Error::unsupported(ZONES_TAINTED));
        }
        if !rows.zones.contains_key(order.target_zone_id) {
            return failed("staff task order references an unknown target zone");
        }
    }
    if order.task_kind == "maintenance" {
        // `world.facilities.get(target_id or "")`.
        let facility = world
            .rows()?
            .facilities
            .get(order.target_id.unwrap_or(""))
            .map(|facility| (facility.id.clone(), facility.zone_id.clone(), facility.working_units, facility.capacity));
        let Some((facility_id, facility_zone, working, capacity)) = facility else {
            return failed("maintenance order must target a facility in its selected zone");
        };
        if &*facility_zone != order.target_zone_id {
            return failed("maintenance order must target a facility in its selected zone");
        }
        // `_maintenance_blocked`: the outage, then the modifiers.
        if working >= capacity
            || kit::active_outage(world.slots.try_value(crate::rules::world_view::INFRASTRUCTURE)?.as_deref(), &facility_id)?
            || kit::has_active_modifier(kit::held_row::<EffectProgramRow>(world, PROGRAM, inputs.program)?.as_deref(), &facility_id)
        {
            return failed("maintenance order has no currently repairable facility unit");
        }
    } else if order.target_id.is_some() {
        return failed("only maintenance orders accept a target facility");
    }
    if matches!(order.task_kind, "information_outreach" | "patrol") {
        let anyone = world.rows()?.agents.iter().any(|(_, agent)| &*agent.zone_id == order.target_zone_id);
        if !anyone {
            return failed("staff support order requires residents in its target zone");
        }
        if order.task_kind == "information_outreach" {
            // `world.model_state.get("body_state_v1", {}).get("agents")`.
            let held = kit::held_row::<BodyRow>(world, BODY, inputs.body)?.is_some_and(|body| !body.agents.is_empty());
            if !held {
                return failed("information outreach requires the body-state policy");
            }
        }
    }
    if order.task_kind == "intake" {
        let population = world.slots.try_value(kit::DYNAMICS)?;
        let waiting = matches!(population.as_deref(), Some(V::Map(population)) if population.get("pending_arrivals").is_some_and(V::is_truthy));
        if !waiting {
            return failed("intake order requires pending arrivals");
        }
    }
    // `ZoneGraph.from_states(world.zones, world.zone_edges)` has no edge capacities, so the loads it is
    // given turn no edge down; they are read as the rule reads them.
    let graph = travel::world_graph(world)?;
    let geometry = match &world.row::<ValueRow>(MODEL_CONFIGS)?.0 {
        V::Map(configs) => configs.contains_key("facility_geometry"),
        _ => return Err(Error::unsupported("a value is not a plain dict")),
    };
    let closed = if geometry { graph.edges_closed_by(travel::plan(inputs.geo)?, "independent") } else { HashSet::new() };
    travel::passage_edge_loads(world, inputs.movement, inputs.staff, minute)?;
    let Some(route) = graph.shortest(&closed, None, &zone, &Arc::from(order.target_zone_id))? else {
        return failed("staff task order has no reachable route");
    };
    let duration = {
        let V::Map(config) = &config else {
            return Err(Error::unsupported("the config is not a map"));
        };
        let Some(V::Map(durations)) = config.get("task_duration_minutes") else {
            return Err(Error::unsupported("the task durations are not a map"));
        };
        match durations.get(order.task_kind) {
            Some(V::Int(minutes)) => *minutes,
            _ => return Err(Error::unsupported("a task duration is not an int")),
        }
    };
    let run_end = world.duration_minutes()?;
    if i128::from(minute) + i128::from(route.minutes) + i128::from(duration) >= i128::from(run_end) {
        return failed("staff task order would finish beyond the Run boundary");
    }
    Ok(Checked { route, duration })
}

fn text(value: &str) -> V {
    V::Str(Arc::from(value))
}

/// `order_staff_operation`: the task's ID. Its checks raise before any change; past them, the writes
/// are the rule's, in its order.
pub fn order_operation(world: &World, inputs: &Inputs<'_>, order: &Order<'_>) -> WriteResult<String> {
    let minute = world.minute_index()?;
    let Checked { route, duration } = validate(world, inputs, order, minute)?;
    let due = minute + route.minutes + duration;
    let run_end = world.duration_minutes()?;
    // As the checks have it, with the same minute: never reached.
    if due >= run_end {
        return Err(Error::failed("staff task order would finish beyond the Run boundary").into());
    }
    let Some(formula) = inputs.formula else {
        return Err(Error::unsupported(FORMULA_UNREAD).into());
    };
    let (formula_source, formula_policy, _) = formula.provenance();
    let (formula_source, formula_policy): (Arc<str>, Arc<str>) = (Arc::from(formula_source), Arc::from(formula_policy));
    // `source_id or formula_source_id`.
    let event_source: Arc<str> = match order.source_id {
        Some(source) if !source.is_empty() => Arc::from(source),
        _ => formula_source.clone(),
    };
    let events = Events::of(world)?;
    let travel = route.minutes;
    let arrival = minute + travel;
    let target = order.target_id.map_or(V::Null, text);

    // Write.
    let sequence = events.allocate_sequence()?;
    let task_id: Arc<str> = Arc::from(format!("staff-operation-{sequence}"));
    let (zones, edges) = if travel != 0 { (route.zone_ids.clone(), route.edge_ids.clone()) } else { (Vec::new(), Vec::new()) };
    let record = OperationRecord {
        phase: if travel != 0 { Phase::Travel } else { Phase::Working },
        task_id: Some(task_id.clone()),
        task_kind: Some(Arc::from(order.task_kind)),
        target_id: order.target_id.map(Arc::from),
        target_zone_id: Some(Arc::from(order.target_zone_id)),
        reason: Some(Arc::from(order.reason)),
        started_minute: Some(minute),
        travel_due_minute: if travel != 0 { Some(arrival) } else { None },
        arrival_minute: if travel != 0 { None } else { Some(arrival) },
        due_minute: Some(due),
        route_zone_ids: zones,
        route_edge_ids: edges,
        travel_minutes: travel,
        causation_event_ids: vec![Arc::from(order.causation_event_id)],
    };
    let staff_key: Arc<str> = Arc::from(order.staff_id);
    world.row_mut::<StaffOperationsRow>(NAME)?.staff.insert(staff_key.clone(), record);
    {
        let mut rows = world.rows_mut()?;
        rows.active_tasks.insert(
            task_id.clone(),
            TaskRow {
                id: task_id.clone(),
                kind: Arc::from("staff_operation"),
                agent_id: None,
                resource_id: None,
                start_minute: minute,
                due_minute: due,
                staff_id: Some(staff_key.clone()),
            },
        );
        let staff = rows.staff.get_mut(order.staff_id).ok_or_else(|| Error::Runtime("the staff member left under the plan".to_owned()))?;
        staff.active_task_id = Some(task_id.clone());
        staff.work_queue = vec![task_id.clone()];
        staff.busy_until = due;
    }
    let provenance = || {
        let mut provenance = kit::assumption(V::Str(formula_source.clone()), &formula_policy);
        provenance.insert(Arc::from("reason"), text(order.reason));
        provenance
    };
    let causation: Arc<str> = Arc::from(order.causation_event_id);
    if travel != 0 {
        let mut payload = IndexMap::with_capacity(3);
        payload.insert(Arc::from("staff_id"), text(order.staff_id));
        payload.insert(Arc::from("task_id"), V::Str(task_id.clone()));
        payload.insert(Arc::from("target_zone_id"), text(order.target_zone_id));
        let event = kit::new_event(kit::NewEvent {
            event_id: Arc::from(format!("staff-operation-arrive-{sequence}")),
            kind: EventKind::Completion,
            due_minute: arrival,
            priority: 5,
            sequence,
            event_type: Arc::from("staff_operation_arrive"),
            payload,
            source_id: Some(event_source.clone()),
            causation_id: Some(causation.clone()),
            provenance: provenance(),
        })?;
        events.schedule(event)?;
    }
    let completion = events.allocate_sequence()?;
    let mut payload = IndexMap::with_capacity(5);
    payload.insert(Arc::from("staff_id"), text(order.staff_id));
    payload.insert(Arc::from("task_id"), V::Str(task_id.clone()));
    payload.insert(Arc::from("task_kind"), text(order.task_kind));
    payload.insert(Arc::from("target_id"), target);
    payload.insert(Arc::from("target_zone_id"), text(order.target_zone_id));
    let event = kit::new_event(kit::NewEvent {
        event_id: Arc::from(format!("staff-operation-complete-{completion}")),
        kind: EventKind::Completion,
        due_minute: due,
        priority: 5,
        sequence: completion,
        event_type: Arc::from("staff_operation_complete"),
        payload,
        source_id: Some(event_source),
        causation_id: Some(causation),
        provenance: provenance(),
    })?;
    events.schedule(event)?;
    if !matches!(inputs.movement, Held::Absent) {
        snapshots(world, inputs, minute + 1)?;
    }
    Ok(task_id.to_string())
}
