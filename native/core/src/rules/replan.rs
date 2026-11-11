//! `_replan_journeys` of `experiments/phases/update.py`: the people whose route changed under them,
//! in the order the walk returned them (lane c6 of Issue #152).
//!
//! Each person is two steps. Their old trip stops (`stop`), and then a new route is planned from
//! where that left them (`second_step`), reading what the stop wrote: their zone, and that nobody
//! counts them as on the way. A refusal before the first stop says Unsupported, with nothing
//! changed; past it, a stop is written, and the PyO3 crate makes a refusal a RuntimeError. See
//! docs/rust-port/lanes/l3-movement.md.
//!
//! The old trip's event and a support's previous completion leave the world's queue through the
//! world's writer (`kit::Events::settle`), which gives back the serials of the slots it took out
//! for the PyO3 crate to let their Python objects go. A support whose staff the person can no
//! longer reach ends in a continuity receipt, written to the log here; projecting it into
//! `shared_operations_v1` reads the EventLog's records as Python holds them, so `second_step` hands
//! it back (`Second::Continuity`) and the PyO3 crate projects it, finishes the walk and counts the
//! replan (`after_continuity`), in the order the Python rule did.

use std::sync::Arc;

use indexmap::IndexMap;

use crate::config::movement::Movement;
use crate::entities::{Activity, RequestStatus};
use crate::error::{Error, Result};
use crate::events::writer::WriteResult;
use crate::events::{EventKind, EventRow, EventStatus};
use crate::geometry::Pt;
use crate::rules::care_receipts::continuity_record;
use crate::rules::kit::movement::KEY as MOVEMENT;
use crate::rules::kit::{self, add_metric, log_record, set_agent, Events, Held, Kept};
use crate::rules::movement;
use crate::rules::overlay::{self, Beds};
use crate::rules::travel::{self, approach_route, plan_travel, write_travel, Route, Routing, Trip, Walking};
use crate::run::services::Manager;
use crate::state::movement::{Journey, MovementRow};
use crate::value::V;
use crate::world::World;

const REPLAN: &str = "movement_replan_count";
const BLOCKED: &str = "movement_replan_blocked_count";
const STAFF_SUPPORT_SOURCE: &str = "ASM-STAFF-SUPPORT-LIFECYCLE-V1";

#[track_caller]
fn refuse<T>(reason: &'static str) -> Result<T> {
    Err(Error::unsupported(reason))
}

/// What a replan reads beside the world.
pub struct Replanning<'a> {
    pub routing: Routing<'a>,
    pub walking: Walking<'a>,
    /// What `reserve_sleep_bed` reads, and whether the world holds `geometry_overlay_v1`, whose bed
    /// a sleeper lets go.
    pub beds: Beds<'a>,
    /// The movement section of the configs, for the walk in a room.
    pub movement: &'a Kept<Movement>,
    /// `controller.staff_service`, for a support's policy version and cooldown.
    pub staff_service: &'a Manager,
}

/// The person's journey, where they have one and are a person of the world: else nothing to replan.
pub fn journey_of(world: &World, agent_id: &str) -> Result<Option<Journey>> {
    let journey = world.row::<MovementRow>(MOVEMENT)?.journeys.get(agent_id).cloned();
    let known = world.rows()?.agents.contains_key(agent_id);
    Ok(journey.filter(|_| known))
}

/// What the old trip's stop leaves: the event suppressed, the task gone, the person idle where
/// their position says, and no journey. Says Unsupported only before its first write. Returns the
/// serials of the queue's slots it took out.
pub fn stop(world: &World, agent_id: &Arc<str>, journey: &Journey, minute: i64) -> WriteResult<Vec<u64>> {
    let events = Events::of(world)?;
    let old = {
        let queue = world.queue()?;
        match queue.serial_of(&journey.event_id) {
            None => None,
            Some(serial) => match &queue.slot(serial).row {
                Some(row) => Some(row.clone()),
                None => return Err(Error::unsupported("the trip's event has no row").into()),
            },
        }
    };
    let zone = world.row::<MovementRow>(MOVEMENT)?.positions.get(&**agent_id).map(|position| position.zone_id.clone());
    if !world.rows()?.agents.contains_key(&**agent_id) {
        return Err(Error::unsupported("a row has no object").into());
    }

    // Write.
    let mut taken = Vec::new();
    if let Some(old) = old {
        taken.extend(events.settle(&old, minute, EventStatus::Suppressed, Some("the route changed before travel completion"))?.taken);
    }
    {
        let mut rows = world.rows_mut()?;
        rows.active_tasks.remove_row(&journey.task_id);
        if let Some(zone) = zone {
            if let Some(row) = rows.agents.get_mut(&**agent_id) {
                row.zone_id = zone;
            }
        }
        set_agent(&mut rows, agent_id, Activity::Idle, None)?;
    }
    movement::stop_journey(world, agent_id)?;
    Ok(taken)
}

/// What the second step leaves for the PyO3 crate.
pub enum Second {
    /// Done: the serials of the queue's slots it took out.
    Done(Vec<u64>),
    /// A support whose staff the person can no longer reach: its continuity receipt, recorded
    /// APPLIED at `minute`, is to be projected (`ingest_applied_shared_operation_records`), and then
    /// `after_continuity` finishes the person's walk.
    Continuity { event: EventRow, minute: i64, taken: Vec<u64> },
}

/// The second step: a new route from where the stop left them. Says Unsupported only before its
/// first write.
pub fn second_step(world: &World, replanning: &Replanning<'_>, agent_id: &Arc<str>, journey: &Journey, minute: i64) -> WriteResult<Second> {
    let step = Step { world, replanning, minute };
    match &*journey.purpose {
        "staff_support_travel" => match step.staff_support(agent_id, journey)? {
            // A support kept is no replan blocked.
            Support::Kept(taken) => {
                add_metric(world, REPLAN)?;
                Ok(Second::Done(taken))
            }
            Support::Lost { event, minute, taken } => Ok(Second::Continuity { event, minute, taken }),
        },
        "sleep_bed" => step.sleep_bed(agent_id, journey).map(|()| Second::Done(Vec::new())),
        _ => step.walk(agent_id, journey).map(|()| Second::Done(Vec::new())),
    }
}

/// After the continuity receipt of `Second::Continuity` and the end of the walk: the replan, which
/// kept no support, counted.
pub fn after_continuity(world: &World) -> Result<()> {
    add_metric(world, BLOCKED)?;
    add_metric(world, REPLAN)
}

enum Support {
    /// The support goes on, rescheduled: the serials of the queue's slots taken out.
    Kept(Vec<u64>),
    /// The staff's route is unavailable: the continuity receipt, written to the log.
    Lost { event: EventRow, minute: i64, taken: Vec<u64> },
}

struct Step<'a, 'r> {
    world: &'a World,
    replanning: &'a Replanning<'r>,
    minute: i64,
}

impl Step<'_, '_> {
    fn zone_of(&self, agent_id: &str) -> Result<Arc<str>> {
        match self.world.rows()?.agents.get(agent_id) {
            Some(agent) => Ok(agent.zone_id.clone()),
            None => refuse("no such agent"),
        }
    }

    fn travel(&self, events: &Events<'_>, agent_id: &Arc<str>, route: Route, points: Vec<Pt>, journey: &Journey) -> Result<Option<travel::PlannedTrip>> {
        plan_travel(
            self.world,
            events,
            self.replanning.walking,
            Trip {
                agent_id: agent_id.clone(),
                route,
                points,
                purpose: &journey.purpose,
                facility_id: journey.facility_id.clone(),
                minute: self.minute,
                causation_id: Some(journey.event_id.clone()),
                support_request_id: None,
                support_staff_id: None,
            },
        )
    }

    fn sleep_bed(&self, agent_id: &Arc<str>, journey: &Journey) -> WriteResult<()> {
        let world = self.world;
        let zone = self.zone_of(agent_id)?;
        let events = Events::of(world)?;
        let bed = overlay::plan_bed(world, &self.replanning.beds, agent_id)?;
        let trip = match &bed {
            Some(bed) if bed.travel_minutes > 0 => {
                let route = Route {
                    origin: zone.clone(),
                    destination: zone.clone(),
                    zone_ids: vec![zone.clone(), zone.clone()],
                    edge_ids: Vec::new(),
                    minutes: bed.travel_minutes,
                };
                self.travel(&events, agent_id, route, bed.path.to_vec(), journey)?
            }
            _ => None,
        };
        if let (Some(bed), Some(trip)) = (&bed, trip) {
            overlay::write_bed(world, self.replanning.beds.overlay, agent_id, bed)?;
            write_travel(world, &events, trip)?;
            return Ok(add_metric(world, REPLAN)?);
        }
        match &bed {
            Some(bed) if bed.travel_minutes <= 0 => {
                overlay::write_bed(world, self.replanning.beds.overlay, agent_id, bed)?;
                // The person is at their bed: their position says so, where the world has positions.
                if matches!(self.replanning.walking.movement, Held::Held) {
                    movement::place_at_point(world, agent_id, &zone, bed.point)?;
                }
            }
            _ => {
                add_metric(world, BLOCKED)?;
                overlay::release(world, self.replanning.beds.overlay, "bed_reservations", Some(agent_id))?;
            }
        }
        set_agent(&mut *world.rows_mut()?, agent_id, Activity::Sleeping, None)?;
        Ok(add_metric(world, REPLAN)?)
    }

    fn walk(&self, agent_id: &Arc<str>, journey: &Journey) -> WriteResult<()> {
        let world = self.world;
        let zone = self.zone_of(agent_id)?;
        let facility = {
            let rows = world.rows()?;
            journey.facility_id.as_ref().and_then(|id| rows.facilities.get(id).map(|facility| (id.clone(), facility.zone_id.clone())))
        };
        let mut target = journey.destination_zone_id.clone();
        if let Some((facility_id, facility_zone)) = facility {
            target = facility_zone;
            if zone == target {
                return self.within_the_room(agent_id, journey, &facility_id, &zone);
            }
        }
        let route = travel::route(world, &self.replanning.routing, &zone, &target, agent_id)?.filter(|route| !route.edge_ids.is_empty());
        match route {
            Some(route) => {
                // `_schedule_travel`, whose answer no one reads here.
                let events = Events::of(world)?;
                if let Some(trip) = self.travel(&events, agent_id, route, Vec::new(), journey)? {
                    write_travel(world, &events, trip)?;
                }
            }
            None => add_metric(world, BLOCKED)?,
        }
        Ok(add_metric(world, REPLAN)?)
    }

    fn within_the_room(&self, agent_id: &Arc<str>, journey: &Journey, facility_id: &str, zone: &Arc<str>) -> WriteResult<()> {
        let world = self.world;
        let found = approach_route(world, self.replanning.walking, self.replanning.movement, agent_id, facility_id)?.filter(|(_, points)| !points.is_empty());
        match found {
            Some((route, points)) if route.minutes > 0 && points.len() > 1 => {
                let events = Events::of(world)?;
                if let Some(trip) = self.travel(&events, agent_id, route, points, journey)? {
                    write_travel(world, &events, trip)?;
                }
            }
            Some((_, points)) => movement::place_in_room(world, agent_id, zone, points[points.len() - 1])?,
            None => add_metric(world, BLOCKED)?,
        }
        Ok(add_metric(world, REPLAN)?)
    }

    /// `_replan_staff_support_trip`: whether the person still has their support.
    fn staff_support(&self, agent_id: &Arc<str>, journey: &Journey) -> WriteResult<Support> {
        let world = self.world;
        let minute = self.minute;
        // The texts of the Python rule, which a Run's error keeps.
        let lost = || Error::value_error("replanned resident trip lost its assigned support task");
        let Some(request_id) = journey.support_request_id.clone() else {
            return Err(lost().into());
        };
        struct Found {
            staff_id: Arc<str>,
            staff_zone: Arc<str>,
            task_id: Arc<str>,
            start: i64,
            due: i64,
            timeout: i64,
        }
        let found = {
            let rows = world.rows()?;
            let Some(request) = rows.support_requests.get(&request_id) else {
                return Err(lost().into());
            };
            let (Some(staff_id), Some(task_id)) = (&request.assigned_staff_id, &request.task_id) else {
                return Err(lost().into());
            };
            let (Some(staff), Some(task)) = (rows.staff.get(staff_id), rows.active_tasks.get(task_id)) else {
                return Err(lost().into());
            };
            if request.status != RequestStatus::Assigned || &*task.kind != "staff_support" {
                return Err(lost().into());
            }
            Found {
                staff_id: staff_id.clone(),
                staff_zone: staff.zone_id.clone(),
                task_id: task_id.clone(),
                start: task.start_minute,
                due: task.due_minute,
                timeout: request.timeout_minute,
            }
        };
        let events = Events::of(world)?;
        // The completion of this task, which must be one and due with it.
        let mut completions = Vec::new();
        {
            let queue = world.queue()?;
            for (_, serial) in queue.ordered() {
                let Some(row) = &queue.slot(*serial).row else {
                    return Err(Error::unsupported("a pending event has no row").into());
                };
                let V::Map(payload) = &row.payload else {
                    continue;
                };
                let names_it = matches!(payload.get("task_id"), Some(V::Str(task)) if **task == *found.task_id);
                if &*row.event_type == "staff_support_complete" && names_it {
                    completions.push(row.clone());
                }
            }
        }
        if completions.len() != 1 || completions[0].due_minute != found.due {
            return Err(Error::value_error("replanned resident trip has inconsistent support completion").into());
        }
        let previous = completions.remove(0);
        let service_minutes = found.due - found.start;
        let zone = self.zone_of(agent_id)?;
        let route = travel::route(world, &self.replanning.routing, &zone, &found.staff_zone, agent_id)?;
        let route_minutes = route.as_ref().map_or(0, |route| route.minutes);
        let Some(due) = minute.checked_add(route_minutes).and_then(|at| at.checked_add(service_minutes)) else {
            return Err(Error::unsupported("a due minute out of range").into());
        };
        let mut can_schedule = route.is_some() && due < events.duration();
        let mut trip = None;
        if let Some(route) = route.filter(|route| can_schedule && route.minutes > 0) {
            trip = plan_travel(
                world,
                &events,
                self.replanning.walking,
                Trip {
                    agent_id: agent_id.clone(),
                    route,
                    points: Vec::new(),
                    purpose: "staff_support_travel",
                    facility_id: None,
                    minute,
                    causation_id: Some(journey.event_id.clone()),
                    support_request_id: Some(request_id.clone()),
                    support_staff_id: Some(found.staff_id.clone()),
                },
            )?;
            can_schedule = trip.is_some();
        }
        let service = kit::policy(self.replanning.staff_service.object())?;
        let mut taken = Vec::new();
        if can_schedule {
            let sequence = events.next_sequence()? + i64::from(trip.is_some());
            let policy = kit::exact_str(kit::policy(service.value("policy_version"))?)?;
            let event = reschedule_event(&request_id, agent_id, &found.staff_id, &found.task_id, service_minutes, due, sequence, journey, policy)?;
            // Write.
            match trip {
                Some(trip) => write_travel(world, &events, trip)?,
                None => set_agent(&mut *world.rows_mut()?, agent_id, Activity::SeekingHelp, None)?,
            }
            taken.extend(events.settle(&previous, minute, EventStatus::Suppressed, Some("support completion was rescheduled after a route change"))?.taken);
            events.allocate_sequence()?;
            {
                let mut rows = world.rows_mut()?;
                if let Some(task) = rows.active_tasks.get_mut(&found.task_id) {
                    task.start_minute = minute + route_minutes;
                    task.due_minute = due;
                }
                if let Some(staff) = rows.staff.get_mut(&found.staff_id) {
                    staff.busy_until = due;
                }
            }
            events.schedule(event)?;
            return Ok(Support::Kept(taken));
        }
        let cooldown = kit::exact_int(kit::policy(service.value("cooldown_minutes"))?)?;
        let Some(until) = minute.checked_add(cooldown) else {
            return Err(Error::unsupported("a minute out of range").into());
        };
        // Write.
        taken.extend(
            events.settle(&previous, minute, EventStatus::Suppressed, Some("support completion was cancelled because the staff route is unavailable"))?.taken,
        );
        let unresolved = found.timeout <= minute;
        {
            let mut rows = world.rows_mut()?;
            rows.active_tasks.remove_row(&found.task_id);
            if let Some(staff) = rows.staff.get_mut(&found.staff_id) {
                staff.work_queue.clear();
                staff.active_task_id = None;
                staff.busy_until = 0;
            }
            if let Some(request) = rows.support_requests.get_mut(&request_id) {
                request.assigned_staff_id = None;
                request.task_id = None;
                if unresolved {
                    request.status = RequestStatus::Unresolved;
                    request.resolved_minute = Some(minute);
                    request.cooldown_until = until;
                } else {
                    request.status = RequestStatus::Pending;
                }
            }
            if let Some(agent) = rows.agents.get_mut(&**agent_id) {
                agent.target_id = None;
                if unresolved {
                    agent.activity = Activity::Idle;
                    agent.support_request_id = None;
                    agent.support_cooldown_until = until;
                } else {
                    agent.activity = Activity::SeekingHelp;
                    agent.support_request_id = Some(request_id.clone());
                }
            }
        }
        if unresolved {
            add_metric(world, "support_unresolved_count")?;
        }
        let made = continuity_record(
            world,
            &request_id,
            agent_id,
            &found.task_id,
            &found.staff_id,
            if unresolved { "unresolved" } else { "requeued" },
            &journey.event_id,
            "route_unreachable",
        )?;
        log_record(world, &made.event, made.minute, EventStatus::Applied, Some(&made.detail))?;
        Ok(Support::Lost { event: made.event, minute: made.minute, taken })
    }
}

/// The new `staff_support_complete` of a support whose route changed.
#[allow(clippy::too_many_arguments)]
fn reschedule_event(
    request_id: &Arc<str>,
    agent_id: &Arc<str>,
    staff_id: &Arc<str>,
    task_id: &Arc<str>,
    service_minutes: i64,
    due: i64,
    sequence: i64,
    journey: &Journey,
    policy: &Arc<str>,
) -> Result<EventRow> {
    let mut payload: IndexMap<Arc<str>, V> = IndexMap::with_capacity(5);
    payload.insert(Arc::from("request_id"), V::Str(request_id.clone()));
    payload.insert(Arc::from("agent_id"), V::Str(agent_id.clone()));
    payload.insert(Arc::from("staff_id"), V::Str(staff_id.clone()));
    payload.insert(Arc::from("task_id"), V::Str(task_id.clone()));
    payload.insert(Arc::from("support_duration_minutes"), V::Int(service_minutes));
    let mut provenance: IndexMap<Arc<str>, V> = IndexMap::with_capacity(4);
    provenance.insert(Arc::from("source_type"), V::Str(Arc::from("assumption")));
    provenance.insert(Arc::from("source_id"), V::Str(Arc::from(STAFF_SUPPORT_SOURCE)));
    provenance.insert(Arc::from("policy_version"), V::Str(policy.clone()));
    provenance.insert(Arc::from("rescheduled_after_route_change"), V::Bool(true));
    EventRow::new(
        Arc::from(format!("staff-support-complete-{sequence}")),
        EventKind::Completion,
        due,
        0,
        sequence,
        Arc::from("staff_support_complete"),
        payload,
        Some(task_id.clone()),
        Some(journey.event_id.clone()),
        provenance,
    )
    .ok_or_else(|| Error::unsupported("the event's values are not what an event takes"))
}
