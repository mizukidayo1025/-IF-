//! `validate_facility_queue_state(world)` of `domain/facility_queues.py`, with Python's errors and
//! texts (lane E of #250, #350). Raw errors: `validate_simulator_extensions` passes them on.
//!
//! `independent_queues_required`, `is_legacy_queue_checkpoint` and `request_for_agent` are kernels
//! in Python (`native/src/rules/facility_queues.rs`); here they read the world's values the same way.

use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::Arc;

use indexmap::IndexMap;

use super::geometry_overlay::parsed_geometry;
use super::movement::py::{cmp_i64, eq, has_exactly, in_keys, in_texts, is_exact_int, is_text, key_of, repeats, Held, Key};

/// `world.model_state.get(...)`: the tree of the world, its lists `TrackedList`.
const HELD: Held = Held::Tracked;
use super::{repr, View};
use crate::error::{Error, Result};
use crate::state::facility_queues::QueuesRow;
use crate::text::py_repr;
use crate::value::V;

const KEY: &str = "facility_queues_v1";
const LEGACY_KEY: &str = "facility_queues_legacy_mode_v1";
const POLICY: &str = "independent-facility-queues-v1";
const MAX_REQUESTS: usize = 100_000;
const ROW_FIELDS: [&str; 5] = ["facility_id", "service_point_id", "queued_request_ids", "active_request_ids", "last_stall"];
const STALL_REASONS: [&str; 6] = ["capacity", "staff", "stock", "outage", "route", "run_end"];

type Map = IndexMap<Arc<str>, V>;

/// A signed service point, as the rule reads it.
struct Point {
    id: V,
    facility_id: Arc<str>,
    capacity: V,
    staff_skills: Vec<Arc<str>>,
}

fn text(map: &Map, name: &str) -> Arc<str> {
    match map.get(name) {
        Some(V::Str(text)) => text.clone(),
        _ => Arc::from(""),
    }
}

/// `service_points(world)`: the Scenario's own (`parsed_facility_geometry`), by queue ID; none
/// without a config dict.
fn service_points(view: &View<'_>) -> Result<IndexMap<Arc<str>, Point>> {
    let raw = match view.config("facility_geometry")? {
        Some(raw @ V::Map(_)) => raw,
        _ => return Ok(IndexMap::new()),
    };
    let dump = parsed_geometry(&raw)?;
    let mut out = IndexMap::new();
    if let V::Map(geometry) = &dump {
        if let Some(V::List(points)) = geometry.get("service_points") {
            for point in points {
                let V::Map(point) = point else { continue };
                let staff_skills = match point.get("access") {
                    Some(V::Map(access)) => match access.get("required_staff_skills") {
                        Some(V::List(skills)) => skills.iter().filter_map(|skill| if let V::Str(skill) = skill { Some(skill.clone()) } else { None }).collect(),
                        _ => Vec::new(),
                    },
                    _ => Vec::new(),
                };
                let entry = Point {
                    id: point.get("id").cloned().unwrap_or(V::Null),
                    facility_id: text(point, "facility_id"),
                    capacity: point.get("service_capacity").cloned().unwrap_or(V::Int(0)),
                    staff_skills,
                };
                out.insert(text(point, "queue_id"), entry);
            }
        }
    }
    Ok(out)
}

/// `independent_queues_required(world.model_configs)`: a service point whose queue is not its
/// facility's ID, or a facility with two points; False without a config dict.
fn independent_required(view: &View<'_>) -> Result<bool> {
    let points = match view.config("facility_geometry")? {
        Some(V::Map(_)) => service_points(view)?,
        _ => return Ok(false),
    };
    let mut facilities = HashSet::new();
    Ok(points.iter().any(|(queue_id, point)| *queue_id != point.facility_id || !facilities.insert(point.facility_id.clone())))
}

/// `is_legacy_queue_checkpoint(world)`.
fn is_legacy(view: &View<'_>, required: bool) -> Result<bool> {
    if !required {
        return Ok(false);
    }
    if view.state(KEY)?.is_some_and(|state| !matches!(*state, V::Null)) {
        return Ok(false);
    }
    let Some(mode) = view.state(LEGACY_KEY)? else {
        return Ok(false);
    };
    let mut expected = Map::new();
    expected.insert(Arc::from("schema_version"), V::Int(1));
    expected.insert(Arc::from("mode"), V::Str(Arc::from("pre_independent_queue_checkpoint")));
    expected.insert(Arc::from("policy_version"), V::Str(Arc::from("facility_fifo_legacy-v1")));
    Ok(eq(&mode, &V::Map(expected)))
}

/// The check, as `validate` (`validate/mod.rs`) calls it in Python's order.
pub fn validate(view: &View<'_>) -> Result<()> {
    let state = view.state(KEY)?;
    let required = independent_required(view)?;
    let state = match state {
        Some(state) if !matches!(*state, V::Null) => state,
        _ => {
            if required && !is_legacy(view, required)? {
                return Err(Error::Value("independent service points require facility_queues_v1 state".to_owned()));
            }
            return Ok(());
        }
    };
    if view.contains(LEGACY_KEY)? {
        return Err(Error::Value("legacy facility queue mode cannot be mixed with independent queue state".to_owned()));
    }
    if !required {
        return Err(Error::Value("facility_queues_v1 has no matching independent service-point config".to_owned()));
    }
    let schema = || Error::Value("facility_queues_v1 has an invalid exact schema".to_owned());
    let V::Map(map) = &*state else {
        return Err(schema());
    };
    if !has_exactly(map, &["schema_version", "policy_version", "queues", "requests"]) || !eq(&map["schema_version"], &V::Int(1)) || !is_text(&map["policy_version"], POLICY) {
        return Err(schema());
    }
    let (V::Map(queues), V::Map(requests)) = (&map["queues"], &map["requests"]) else {
        return Err(schema());
    };
    if requests.len() > MAX_REQUESTS {
        return Err(schema());
    }
    let expected = service_points(view)?;
    if queues.len() != expected.len() || queues.keys().any(|id| !expected.contains_key(id)) {
        return Err(Error::Value("facility_queues_v1 queue IDs do not match the signed service points".to_owned()));
    }
    let minute = view.minute_index()?;
    let rows = view.rows()?;
    let mut active_agents: HashSet<Arc<str>> = HashSet::new();
    let mut assigned: HashSet<Key> = HashSet::new();
    for (queue_id, row) in queues {
        let point = &expected[queue_id];
        let invalid = || Err(Error::Value(format!("facility queue {} is invalid", py_repr(queue_id))));
        let V::Map(row) = row else {
            return invalid();
        };
        if !has_exactly(row, &ROW_FIELDS) || !is_text(&row["facility_id"], &point.facility_id) || !eq(&row["service_point_id"], &point.id) {
            return invalid();
        }
        let (V::List(queued), V::List(active)) = (&row["queued_request_ids"], &row["active_request_ids"]) else {
            return invalid();
        };
        if repeats(queued, HELD)? || repeats(active, HELD)? {
            return invalid();
        }
        let queued_keys: HashSet<Key> = queued.iter().map(|id| key_of(id, HELD)).collect::<Result<_>>()?;
        if active.iter().map(|id| key_of(id, HELD)).collect::<Result<Vec<_>>>()?.iter().any(|key| queued_keys.contains(key)) {
            return invalid();
        }
        if cmp_i64(&point.capacity, active.len() as i64) == Some(Ordering::Less) {
            return Err(Error::Value(format!("facility queue {} exceeds its signed concurrent capacity", py_repr(queue_id))));
        }
        let stall = &row["last_stall"];
        if !matches!(stall, V::Null) && !stall_valid(stall)? {
            return Err(Error::Value(format!("facility queue {} stall state is invalid", py_repr(queue_id))));
        }
        for request_id in queued.iter().chain(active) {
            assigned.insert(key_of(request_id, HELD)?);
            let is_queued = queued.iter().any(|id| eq(id, request_id));
            let request = match request_id {
                V::Str(id) => requests.get(&**id),
                _ => None,
            };
            let Some(V::Map(request)) = request else {
                return Err(request_invalid(request_id));
            };
            if !request_valid(request, request_id, queue_id, point, is_queued, minute, &|value| in_keys(value, HELD, |id| rows.agents.contains_key(id)))? {
                return Err(request_invalid(request_id));
            }
            let V::Str(agent_id) = &request["agent_id"] else {
                unreachable!("an Agent's ID is a str");
            };
            if !active_agents.insert(agent_id.clone()) {
                return Err(Error::Value("an Agent cannot occupy two independent service queues".to_owned()));
            }
            let facility = rows.facilities.get(&point.facility_id).ok_or_else(|| Error::Key(point.facility_id.to_string()))?;
            let agent = rows.agents.get(agent_id).ok_or_else(|| Error::Key(agent_id.to_string()))?;
            let kind = &request["service_kind"];
            let expected_kind = if is_text(kind, "water") {
                Some("water_station")
            } else if is_text(kind, "meal") {
                Some("meal_distribution")
            } else {
                None
            };
            if expected_kind.is_some_and(|wanted| &*facility.kind != wanted) {
                return Err(Error::Value("facility queue service kind does not match its resource facility".to_owned()));
            }
            let resource = is_text(kind, "water") || is_text(kind, "meal");
            let at_facility = agent.target_id.as_deref() == Some(&*point.facility_id);
            if is_queued {
                let task = match &request["queue_task_id"] {
                    V::Str(id) => rows.active_tasks.get(id),
                    _ => None,
                };
                let matches = task.is_some_and(|task| {
                    matches!(&*task.kind, "toilet_queue" | "resource_service_queue")
                        && task.agent_id.as_deref() == Some(&**agent_id)
                        && task.resource_id.as_deref() == Some(&*point.facility_id)
                        && agent.activity.text() == "queued"
                        && at_facility
                });
                if !matches {
                    return Err(Error::Value("queued service request has no matching task and Agent state".to_owned()));
                }
                let mirror = if resource { &facility.resource_queue_agent_ids } else { &facility.queue_agent_ids };
                if !mirror.iter().any(|id| id == agent_id) {
                    return Err(Error::Value("independent queue request is missing from the facility projection".to_owned()));
                }
            } else {
                let task = match &request["active_task_id"] {
                    V::Str(id) => rows.active_tasks.get(id),
                    _ => None,
                };
                let Some(task) = task.filter(|task| {
                    task.agent_id.as_deref() == Some(&**agent_id)
                        && task.resource_id.as_deref() == Some(&*point.facility_id)
                        && matches!(&*task.kind, "toilet_service" | "resource_service_water" | "resource_service_food")
                        && agent.activity.text() == "in_service"
                        && at_facility
                }) else {
                    return Err(Error::Value("active service request has no matching service task and Agent state".to_owned()));
                };
                if is_text(kind, "toilet") && (!point.staff_skills.is_empty() || task.staff_id.is_some()) {
                    let member = rows.staff.get(task.staff_id.as_deref().unwrap_or(""));
                    let reserved = !point.staff_skills.is_empty()
                        && member.is_some_and(|member| {
                            point.staff_skills.iter().all(|skill| member.skills.0.contains(skill))
                                && member.zone_id == facility.zone_id
                                && member.active_task_id.as_deref() == Some(&*task.id)
                                && member.work_queue.len() == 1
                                && member.work_queue[0] == task.id
                                && member.busy_until == task.due_minute
                        });
                    if !reserved {
                        return Err(Error::Value("staff-required toilet service has no matching skilled staff reservation".to_owned()));
                    }
                }
                let mirror = if resource { &facility.resource_active_agent_ids } else { &facility.active_agent_ids };
                if !mirror.iter().any(|id| id == agent_id) {
                    return Err(Error::Value("active independent service is missing from the facility projection".to_owned()));
                }
            }
        }
    }
    if requests.len() != assigned.len() || requests.keys().any(|id| !assigned.contains(&Key::Text(id.clone()))) {
        return Err(Error::Value("facility_queues_v1 contains an unassigned or terminal request".to_owned()));
    }
    drop(rows);
    queue_journeys(view, requests)
}

fn request_invalid(request_id: &V) -> Error {
    Error::Value(format!("facility queue request {} is invalid", repr(request_id)))
}

fn stall_valid(stall: &V) -> Result<bool> {
    let V::Map(stall) = stall else {
        return Ok(false);
    };
    if !has_exactly(stall, &["minute", "request_id", "reason"]) || !is_exact_int(&stall["minute"]) || cmp_i64(&stall["minute"], 0) == Some(Ordering::Less) {
        return Ok(false);
    }
    if !matches!(stall["request_id"], V::Str(_)) {
        return Ok(false);
    }
    in_texts(&stall["reason"], HELD, &STALL_REASONS)
}

/// The long condition on a request, in Python's order.
#[allow(clippy::too_many_arguments)]
fn request_valid(request: &Map, request_id: &V, queue_id: &str, point: &Point, is_queued: bool, minute: i64, agent_known: &dyn Fn(&V) -> Result<bool>) -> Result<bool> {
    let get = |name: &str| request.get(name).unwrap_or(&V::Null);
    if !eq(get("request_id"), request_id) || !is_text(get("queue_id"), queue_id) || !is_text(get("facility_id"), &point.facility_id) || !eq(get("service_point_id"), &point.id) {
        return Ok(false);
    }
    if !agent_known(get("agent_id"))? || !in_texts(get("service_kind"), HELD, &["toilet", "water", "meal"])? {
        return Ok(false);
    }
    let requested = get("requested_minute");
    if !is_exact_int(requested) || cmp_i64(requested, 0) == Some(Ordering::Less) || cmp_i64(requested, minute) == Some(Ordering::Greater) {
        return Ok(false);
    }
    if !is_text(get("status"), if is_queued { "queued" } else { "active" }) {
        return Ok(false);
    }
    Ok(matches!(get("queue_task_id"), V::Str(_)) && matches!(get("active_task_id"), V::Str(_)))
}

/// Each movement journey to a queue has its queued request (`request_for_agent`).
fn queue_journeys(view: &View<'_>, requests: &Map) -> Result<()> {
    let Some(movement) = view.state("movement_v1")? else {
        return Ok(());
    };
    let V::Map(movement) = &*movement else {
        return Ok(());
    };
    let journeys = match movement.get("journeys") {
        None => return Ok(()),
        Some(V::Map(journeys)) => journeys,
        Some(_) => return Err(Error::unsupported("AttributeError: movement_v1's journeys is not a dict")),
    };
    for (agent_id, journey) in journeys {
        let V::Map(journey) = journey else {
            return Err(Error::unsupported("AttributeError: a journey is not a dict"));
        };
        let queue_id = match journey.get("queue_id") {
            None | Some(V::Null) => continue,
            Some(V::Str(queue_id)) => queue_id,
            Some(_) => return Err(Error::unsupported("request_for_agent refused: a queue ID is not a str")),
        };
        // The kernel reads the namespace's row: a state no row holds (one the typed row refuses)
        // is a refusal in Python, which the core's lenient loader holds as a value.
        if view.row::<QueuesRow>(KEY)?.is_none() {
            return Err(Error::Refused { label: "facility_queues.request_for_agent".to_owned(), reason: "the namespace has no row".into() });
        }
        let request = requests.values().find_map(|request| match request {
            V::Map(request) if is_text(request.get("agent_id").unwrap_or(&V::Null), agent_id) && is_text(request.get("queue_id").unwrap_or(&V::Null), queue_id) => Some(request),
            _ => None,
        });
        if !request.is_some_and(|request| is_text(request.get("status").unwrap_or(&V::Null), "queued")) {
            return Err(Error::Value("queue movement journey has no matching queued request".to_owned()));
        }
    }
    Ok(())
}
