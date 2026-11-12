//! The texts of the water and meal service's outcomes, of a request its queue turns down, and of
//! the toilet completion's and the ledger handler's wraps of their rules' errors (Issue #225, lane
//! M2; moved from `tests/integration/test_resource_service_texts.py`).
//!
//! `service::complete`, `cancel` and `interrupt` raise `EventSuppressed` and
//! `EventApplicationError`, whose texts the dispatcher saves as an event's `detail`; a request the
//! queue turns down is the text of a `ValueError` the propose step saves as the request's `detail`.
//! Each case builds a state that reaches one check, and the core must raise the class and text the
//! Python rule raised there (written down from it before it went: lane L10, `kernel_only`) and
//! leave the world's canonical bytes as they were.
//!
//! The worlds are `tests/data/resource_service_texts.json` (`tests/support/core_fixtures_texts.py`):
//! `serving` is h00_small_replay with a water station and a meal point, A001 being served water at
//! minute 0; `visiting` is the gate's generated Scenario at the first minute someone is served at a
//! toilet; each with the Run's policies (the manager's config, the dispatch table). The edits of
//! each case are made here, as the pytest file made them in Python. The pytest file keeps one test
//! of each Python entry: its request is the one no independent queue takes, on a floor plan of its
//! own. The propose step's request at once the queue turns down is in `requests_at_once.rs`.

mod support;

use std::sync::Arc;

use serde_json::{json, Value};
use sheltersim_core::config::population::AccessibilityNeeds;
use sheltersim_core::config::SectionId;
use sheltersim_core::entities::{Activity, TaskRow};
use sheltersim_core::error::Error;
use sheltersim_core::events::EventStatus;
use sheltersim_core::host::Host;
use sheltersim_core::rules::handlers::{self, LedgerIn, OPERATIONAL};
use sheltersim_core::rules::kit;
use sheltersim_core::rules::resources::Fail;
use sheltersim_core::rules::service::{self, Minute, Request, Requested};
use sheltersim_core::rules::tick::Tick;
use sheltersim_core::rules::{dispatch, toilet};
use sheltersim_core::state::resource_ledger::{LedgerRow, NAME as LEDGER};
use sheltersim_core::state::resource_service::{ServiceRow, NAME as SERVICE};
use sheltersim_core::text::py_repr;
use sheltersim_core::world::{Fit, World, WorldCell, METRIC_STATE};

use support::cases;
use support::dumps::Dumps;
use support::load::{self, Loaded};
use support::texts::{self, changed, DROP};
use support::TestHost;

const FILE: &str = "resource_service_texts";
const WATER: &str = "water_station_1";

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

/// A world of the fixture, its Run and a host with its sections and floor plan.
struct Case {
    entry: Value,
    loaded: Loaded,
    host: TestHost,
}

fn case(name: &str) -> Case {
    let entry = cases::entry(FILE, name);
    let loaded = load::loaded(&entry);
    let host = load::host(&entry);
    Case { entry, loaded, host }
}

impl Case {
    fn edit(&self, edit: impl FnOnce(&mut World)) {
        self.loaded.cell.with(edit).unwrap();
    }

    fn bytes(&self) -> String {
        texts::canonical_of(&self.loaded.cell, &self.entry)
    }

    /// What `rule` raised on the world, which it must leave as it was (`_says`).
    fn says<T>(&mut self, rule: impl FnOnce(&mut Self) -> Result<T, Error>) -> (&'static str, String) {
        let before = self.bytes();
        let said = texts::outcome(rule(self));
        assert!(self.bytes() == before, "the world changed: {said:?}");
        said
    }

    /// The completion A001's service waits for.
    fn completion(&self) -> Value {
        texts::pending_id(&self.entry, self.entry["completion_id"].as_str().unwrap())
    }

    fn task_id(&self) -> String {
        self.completion()["payload"]["task_id"].as_str().unwrap().to_owned()
    }

    fn staff_id(&self) -> String {
        self.completion()["payload"]["staff_id"].as_str().unwrap().to_owned()
    }

    /// `ResourceServiceManager(world).complete(event)`.
    fn complete(&mut self, event: &Value) -> Result<(), Error> {
        let manager = service::manager_parts(&self.loaded.cell, &mut self.host, &self.loaded.policies)?;
        let row = texts::row(event);
        self.loaded.cell.with(|world| service::complete(world, &manager.manager(), &row)).unwrap().map(drop).map_err(service::failure_of::<TestHost>)
    }

    /// `ResourceServiceCancellationHandler(ResourceServiceManager(world)).apply(world, event)`.
    fn cancel(&mut self, event: &Value) -> Result<(), Error> {
        self.loaded.cell.with(|world| handlers::service_cancellation(world)).unwrap()?;
        let manager = service::manager_parts(&self.loaded.cell, &mut self.host, &self.loaded.policies)?;
        let row = texts::row(event);
        self.loaded.cell.with(|world| service::cancel(world, &manager.manager(), &row)).unwrap().map(drop).map_err(service::failure_of::<TestHost>)
    }

    /// `ResourceServiceManager(world).interrupt_for_staff(staff_id, causation_event=...)`, the
    /// cause a `staff_unavailable` event.
    fn interrupt(&mut self, staff_id: &str) -> Result<(), Error> {
        let manager = service::manager_parts(&self.loaded.cell, &mut self.host, &self.loaded.policies)?;
        let cause = text("test-staff-unavailable");
        self.loaded
            .cell
            .with(|world| {
                let pending = service::reserves_of(world)?;
                service::interrupt(world, &manager.manager(), &pending, staff_id, Some(&cause)).map(drop).map_err(service::failure_of::<TestHost>)
            })
            .unwrap()
    }

    /// `ResourceServiceManager(world).request(agent_id, facility_id, minute=...,
    /// validate_state=...)`.
    fn requested(&mut self, agent_id: &str, facility_id: &str, minute: Minute, validates: bool) -> Result<Requested, Error> {
        let manager = service::manager_parts(&self.loaded.cell, &mut self.host, &self.loaded.policies)?;
        let holding = kit::holding(&self.loaded.cell, &mut self.host)?;
        let needs = kit::kept_section_value(&self.loaded.cell, &mut self.host, &holding, SectionId::AccessibilityNeeds, |configs| &configs.accessibility_needs, AccessibilityNeeds::parse)?;
        let pending = self.loaded.cell.with(|world| service::reserves_of(world)).unwrap()?;
        let request = Request { agent_id, facility_id, minute, queue_id: None, needs: &needs };
        service::request(&self.loaded.cell, &mut self.host, &manager.manager(), &request, &pending, validates)
    }

    /// A request the queue turns down, without the world's validation: the `ValueError`'s text.
    fn request(&mut self, agent_id: &str, facility_id: &str, minute: Minute) -> Result<(), Error> {
        match self.requested(agent_id, facility_id, minute, false)? {
            Requested::Refused(text) => Err(Error::Value(text)),
            queued => panic!("queued: {queued:?}"),
        }
    }

    /// A request to the water station at minute 0, which queues: its task.
    fn request_queued(&mut self, agent_id: &str) -> String {
        match self.requested(agent_id, WATER, Minute::Int(0), true).unwrap() {
            Requested::Queued(receipt) => receipt.queue_task_id.to_string(),
            refused => panic!("refused: {refused:?}"),
        }
    }
}

/// `_event` of the pytest file: an event of the test's source.
fn event(event_type: &str, payload: Value, kind: &str, due: i64, sequence: i64) -> Value {
    json!({
        "event_id": format!("test-{}", event_type.replace('_', "-")),
        "kind": kind,
        "due_minute": due,
        "priority": 0,
        "sequence": sequence,
        "event_type": event_type,
        "payload": payload,
        "source_id": "SRC-TEST",
        "causation_id": null,
        "provenance": {"source_type": "assumption", "source_id": "SRC-TEST"},
    })
}

fn the_task_s_reservation_is(world: &mut World, task_id: &str, agent_id: &str) {
    let mut ledger = world.row_mut::<LedgerRow>(LEDGER).unwrap();
    ledger.accounts.water_ml.reservations.get_mut(task_id).unwrap().agent_id = text(agent_id);
}

fn the_service_does_not_own(world: &mut World, task_id: &str) {
    world.row_mut::<ServiceRow>(SERVICE).unwrap().active_reservation_ids.retain(|id| &**id != task_id);
}

fn the_service_cancelled(world: &mut World, task_id: &str) {
    world.row_mut::<ServiceRow>(SERVICE).unwrap().cancelled_task_ids.push(text(task_id));
}

/// The loaded worlds write the canonical text Python wrote of them, by which each case sees that its
/// world did not change.
#[test]
fn the_loaded_worlds_write_python_s_canonical_text() {
    for name in ["serving", "visiting"] {
        let case = case(name);
        let python = Dumps::default().unicode().compact().sorted().text(&load::v(&case.entry["world"]));
        assert!(case.bytes() == python, "{name}");
    }
}

// ---- complete ------------------------------------------------------------------------------------

#[test]
fn a_completion_s_texts_are_python_s() {
    let full = "resource service completion does not match its full reservation, slot, or staff";
    let invalid = "resource completion payload or event kind is invalid";
    let unmatched = "resource completion does not match its active task";
    let cases: Vec<(Value, Value, &str)> = vec![
        (json!({"task_id": 7}), json!({}), "resource completion requires a task_id"),
        (json!({"task_id": DROP}), json!({}), "resource completion requires a task_id"),
        (json!({"task_id": "no-such-task"}), json!({}), "resource completion does not match an active task"),
        (json!({}), json!({"kind": "exogenous"}), invalid),
        (json!({}), json!({"event_type": "resource_service_completed"}), invalid),
        (json!({"extra": 1}), json!({}), invalid),
        (json!({"staff_id": DROP}), json!({}), invalid),
        (json!({}), json!({"due_minute": 3}), unmatched),
        (json!({"agent_id": "A002"}), json!({}), unmatched),
        (json!({"agent_id": null}), json!({}), unmatched),
        (json!({"staff_id": "S-MEAL"}), json!({}), unmatched),
        (json!({"facility_id": "meal_distribution_1"}), json!({}), unmatched),
        (json!({"reservation_id": "other-reservation"}), json!({}), unmatched),
        (json!({"resource_id": "food_meal_units"}), json!({}), unmatched),
        (json!({"quantity": 1}), json!({}), full),
        (json!({"quantity": 250.0}), json!({}), full),
        (json!({"quantity": true}), json!({}), full),
        (json!({"quantity": "250"}), json!({}), full),
    ];
    for (payload, fields, expected) in cases {
        let mut case = case("serving");
        let event = changed(&case.completion(), payload.clone(), fields.clone());
        let said = case.says(|case| case.complete(&event));
        assert_eq!(said, ("EventApplicationError", expected.to_owned()), "{payload} {fields}");
    }
}

#[test]
fn a_completion_of_a_cancelled_task_is_suppressed_with_python_s_text() {
    let mut case = case("serving");
    case.edit(|world| the_service_cancelled(world, "resource-service-task-gone"));
    let event = changed(&case.completion(), json!({"task_id": "resource-service-task-gone"}), json!({}));
    let said = case.says(|case| case.complete(&event));
    assert_eq!(said, ("EventSuppressed", "resource service task 'resource-service-task-gone' was cancelled".to_owned()));
}

#[test]
fn a_completion_whose_service_state_differs_is_python_s_to_fail() {
    for spoil in ["slot", "staff", "staff_busy", "reservation", "owned"] {
        let mut case = case("serving");
        let (task_id, staff_id) = (case.task_id(), case.staff_id());
        case.edit(|world| match spoil {
            "slot" => world.rows_mut().unwrap().facilities.get_mut(WATER).unwrap().resource_active_agent_ids.retain(|id| &**id != "A001"),
            "staff" => world.rows_mut().unwrap().staff.get_mut(&staff_id).unwrap().active_task_id = None,
            "staff_busy" => world.rows_mut().unwrap().staff.get_mut(&staff_id).unwrap().busy_until += 1,
            "reservation" => the_task_s_reservation_is(world, &task_id, "A002"),
            _ => the_service_does_not_own(world, &task_id),
        });
        let event = case.completion();
        let said = case.says(|case| case.complete(&event));
        assert_eq!(said, ("EventApplicationError", "resource service completion does not match its full reservation, slot, or staff".to_owned()), "{spoil}");
    }
}

/// `ResourceServiceCompletionHandler`'s own check, in the dispatch phase: a world whose version was
/// set back reaches it before the manager's, and the phase saves the failed record with its text.
#[test]
fn a_completion_in_a_world_not_v6_fails_with_the_handler_s_text() {
    let mut case = case("serving");
    let event = case.completion();
    let event_id = event["event_id"].as_str().unwrap().to_owned();
    let due_now = texts::row(&changed(&event, json!({}), json!({"due_minute": 0})));
    case.edit(|world| {
        let mut queue = world.queue_mut().unwrap();
        queue.take_slot(&event_id).unwrap();
        assert_eq!(queue.schedule_row(due_now), 0);
        drop(queue);
        world.scalars.get_mut().schema_version = Fit::Ok(5);
    });
    let mut tick = Tick {
        cell: &case.loaded.cell,
        host: &mut case.host,
        run: &case.loaded.run,
        policies: case.loaded.policies.clone(),
        minute: 0,
        seed: case.loaded.seed.clone(),
        neighbors: case.loaded.neighbors.clone(),
    };
    dispatch::run(&mut tick).unwrap();
    let record = case
        .loaded
        .cell
        .with(|world| {
            let log = world.log().unwrap();
            (0..log.len()).map(|index| serde_json::from_slice::<Value>(log.written(index).unwrap()).unwrap()).find(|record| record["event"]["event_id"] == event_id.as_str())
        })
        .unwrap()
        .expect("the completion's record");
    assert_eq!((&record["status"], &record["detail"]), (&json!(EventStatus::Failed.as_str()), &json!("resource service completion requires WorldState v6")));
}

// ---- cancel --------------------------------------------------------------------------------------

fn cancellation(payload: Value) -> Value {
    event("resource_service_cancel", payload, "intervention", 0, 900_001)
}

#[test]
fn a_cancellation_s_texts_are_python_s() {
    let requires = "resource cancellation requires a task_id";
    let untyped = "resource cancellation payload is not a typed cancellation";
    let cases: Vec<(Value, &str)> = vec![
        (json!({"reason": "operator_cancelled"}), requires),
        (json!({"task_id": "", "reason": "operator_cancelled"}), requires),
        (json!({"task_id": 3, "reason": "operator_cancelled"}), requires),
        (json!({"task_id": "T"}), untyped),
        (json!({"task_id": "T", "reason": "operator_cancelled", "note": "x"}), untyped),
        (json!({"task_id": "T", "reason": "someone_said_so"}), untyped),
        (json!({"task_id": "T", "reason": null}), untyped),
        (json!({"task_id": "T", "reason": 4}), untyped),
        (json!({"task_id": "T", "reason": "operator_cancelled"}), "resource cancellation does not match an active task"),
    ];
    for (payload, expected) in cases {
        let mut case = case("serving");
        let event = cancellation(payload.clone());
        let said = case.says(|case| case.cancel(&event));
        assert_eq!(said, ("EventApplicationError", expected.to_owned()), "{payload}");
    }
}

#[test]
fn a_cancellation_of_a_cancelled_task_is_suppressed_with_python_s_text() {
    let mut case = case("serving");
    let task_id = case.task_id();
    case.edit(|world| the_service_cancelled(world, &task_id));
    let event = cancellation(json!({"task_id": task_id, "reason": "facility_stopped"}));
    let said = case.says(|case| case.cancel(&event));
    assert_eq!(said, ("EventSuppressed", format!("resource service task {} was already cancelled", py_repr(&task_id))));
}

#[test]
fn a_cancellation_whose_task_differs_is_python_s_to_fail() {
    let mismatch = "active resource cancellation does not match reservation, slot, and staff";
    let cases = [
        ("kind", "resource cancellation does not match an active task"),
        ("queue_entry", "queued resource Agent has no matching queue entry"),
        ("facility", "resource service task references a non-resource facility"),
        ("slot", "active resource Agent has no matching facility slot"),
        ("staff", mismatch),
        ("work_queue", mismatch),
        ("reservation", mismatch),
        ("owned", mismatch),
    ];
    for (spoil, expected) in cases {
        let mut case = case("serving");
        let (mut task_id, staff_id) = (case.task_id(), case.staff_id());
        let toilet = case.entry["toilet"].as_str().unwrap().to_owned();
        if spoil == "queue_entry" {
            // A2 queues and then leaves the facility's list behind its task's back.
            case.edit(|world| {
                let zone = world.rows().unwrap().facilities.get(WATER).unwrap().zone_id.clone();
                world.rows_mut().unwrap().agents.get_mut("A002").unwrap().zone_id = zone;
            });
            task_id = case.request_queued("A002");
            case.edit(|world| world.rows_mut().unwrap().facilities.get_mut(WATER).unwrap().resource_queue_agent_ids.retain(|id| &**id != "A002"));
        }
        case.edit(|world| match spoil {
            "kind" => world.rows_mut().unwrap().active_tasks.get_mut(&task_id).unwrap().kind = text("toilet_service"),
            "facility" => world.rows_mut().unwrap().active_tasks.get_mut(&task_id).unwrap().resource_id = Some(text(&toilet)),
            "slot" => world.rows_mut().unwrap().facilities.get_mut(WATER).unwrap().resource_active_agent_ids.retain(|id| &**id != "A001"),
            "staff" => world.rows_mut().unwrap().staff.get_mut(&staff_id).unwrap().active_task_id = None,
            "work_queue" => world.rows_mut().unwrap().staff.get_mut(&staff_id).unwrap().work_queue.push(text("another")),
            "reservation" => the_task_s_reservation_is(world, &task_id, "A002"),
            "owned" => the_service_does_not_own(world, &task_id),
            _ => {}
        });
        let event = cancellation(json!({"task_id": task_id, "reason": "operator_cancelled"}));
        let said = case.says(|case| case.cancel(&event));
        assert_eq!(said, ("EventApplicationError", expected.to_owned()), "{spoil}");
    }
}

// ---- interrupt_for_staff -------------------------------------------------------------------------

#[test]
fn an_interruption_s_texts_are_python_s() {
    let inconsistent = "interrupted resource task has inconsistent state";
    let reservation = "interrupted service reservation is inconsistent";
    let cases = [
        ("several", "unavailable staff owns multiple resource services"),
        ("ownership", "interrupted resource service has incomplete ownership"),
        ("activity", inconsistent),
        ("busy", inconsistent),
        ("facility", "interrupted resource service has an invalid facility"),
        ("reservation", reservation),
        ("owned", reservation),
    ];
    for (spoil, expected) in cases {
        let mut case = case("serving");
        let (task_id, staff_id) = (case.task_id(), case.staff_id());
        let toilet = case.entry["toilet"].as_str().unwrap().to_owned();
        case.edit(|world| match spoil {
            "several" => {
                let task = TaskRow {
                    id: text("second-service"),
                    kind: text("resource_service_food"),
                    agent_id: Some(text("A002")),
                    resource_id: Some(text("meal_distribution_1")),
                    start_minute: 0,
                    due_minute: 2,
                    staff_id: Some(text(&staff_id)),
                };
                world.rows_mut().unwrap().active_tasks.insert(text("second-service"), task);
            }
            "ownership" => world.rows_mut().unwrap().active_tasks.get_mut(&task_id).unwrap().agent_id = None,
            "activity" => world.rows_mut().unwrap().agents.get_mut("A001").unwrap().activity = Activity::Idle,
            "busy" => world.rows_mut().unwrap().staff.get_mut(&staff_id).unwrap().busy_until += 1,
            "facility" => {
                let mut rows = world.rows_mut().unwrap();
                assert_eq!(rows.facilities.get(&toilet).unwrap().zone_id, rows.agents.get("A001").unwrap().zone_id);
                rows.facilities.get_mut(&toilet).unwrap().resource_active_agent_ids.push(text("A001"));
                rows.active_tasks.get_mut(&task_id).unwrap().resource_id = Some(text(&toilet));
            }
            "reservation" => the_task_s_reservation_is(world, &task_id, "A002"),
            _ => the_service_does_not_own(world, &task_id),
        });
        let said = case.says(|case| case.interrupt(&staff_id));
        assert_eq!(said, ("EventApplicationError", expected.to_owned()), "{spoil}");
    }
}

/// The cancellation inside raises its own text; the core decides before it writes.
#[test]
fn an_interruption_of_a_cancelled_task_is_the_cancellation_s_text() {
    let mut case = case("serving");
    let (task_id, staff_id) = (case.task_id(), case.staff_id());
    case.edit(|world| the_service_cancelled(world, &task_id));
    let said = case.says(|case| case.interrupt(&staff_id));
    assert_eq!(said, ("EventSuppressed", format!("resource service task {} was already cancelled", py_repr(&task_id))));
}

// ---- A request the queue turns down ------------------------------------------------------------

#[test]
fn a_request_s_refusal_texts_are_python_s() {
    let while_active = |agent: &str| format!("Agent {} cannot request resource service while active", py_repr(agent));
    let exclusive = |agent: &str| format!("Agent {} already has an exclusive task", py_repr(agent));
    let cases: Vec<(&str, &str, Minute, bool, String)> = vec![
        ("A001", WATER, Minute::Int(1), false, "resource request minute must match WorldState".to_owned()),
        // `minute=True` and `minute="0"`.
        ("A001", WATER, Minute::NotAnInt, false, "resource request minute must be an integer".to_owned()),
        ("nobody", WATER, Minute::Absent, false, "unknown Agent 'nobody'".to_owned()),
        ("A002", "nowhere", Minute::Absent, false, "'nowhere' is not a resource service facility".to_owned()),
        ("A002", "toilet_1", Minute::Absent, false, "'toilet_1' is not a resource service facility".to_owned()),
        ("A003", WATER, Minute::Absent, false, "Agent must arrive at the resource service facility before queueing".to_owned()),
        ("A002", WATER, Minute::Absent, false, while_active("A002")),
        ("A001", "meal_distribution_1", Minute::Absent, false, while_active("A001")),
        ("A003", "meal_distribution_1", Minute::Absent, true, exclusive("A003")),
        ("A004", "meal_distribution_1", Minute::Absent, false, exclusive("A004")),
    ];
    for (agent_id, facility_id, minute, walking, expected) in cases {
        let mut case = case("serving");
        let toilet = case.entry["toilet"].as_str().unwrap().to_owned();
        case.edit(|world| {
            let mut rows = world.rows_mut().unwrap();
            let zone = rows.facilities.get(WATER).unwrap().zone_id.clone();
            let a002 = rows.agents.get_mut("A002").unwrap();
            a002.zone_id = zone.clone();
            a002.activity = Activity::Moving;
            rows.agents.get_mut("A004").unwrap().zone_id = zone.clone();
            rows.facilities.get_mut(&toilet).unwrap().queue_agent_ids.push(text("A004"));
            assert_ne!(rows.agents.get("A003").unwrap().zone_id, zone);
            if walking {
                rows.agents.get_mut("A003").unwrap().zone_id = zone;
                let walk = TaskRow {
                    id: text("walk-home"),
                    kind: text("travel_return"),
                    agent_id: Some(text("A003")),
                    resource_id: None,
                    start_minute: 0,
                    due_minute: 3,
                    staff_id: None,
                };
                rows.active_tasks.insert(text("walk-home"), walk);
            }
        });
        let before = case.bytes();
        let refused = case.request(agent_id, facility_id, minute);
        assert!(matches!(&refused, Err(Error::Value(text)) if *text == expected), "{agent_id} {facility_id}: {refused:?}");
        assert!(case.bytes() == before, "{agent_id} {facility_id}");
    }
}

// ---- The wraps: a toilet visit's completion, and the ledger --------------------------------------

#[test]
fn a_toilet_completion_wraps_python_s_text() {
    let cases = [
        ("facility", "'{water}' is not a toilet Facility"),
        ("unknown_facility", "'nowhere' is not a toilet Facility"),
        ("agent", "Agent 'nobody' does not exist"),
        ("kind", "toilet service task '{task}' is not active"),
        ("other_agent", "toilet completion payload does not match its active task"),
        ("activity", "Agent '{agent}' is not using Facility '{toilet}'"),
        ("slot", "Agent '{agent}' must occupy exactly one toilet service slot"),
    ];
    for (spoil, expected) in cases {
        let mut case = case("visiting");
        let task_id = case.entry["task_id"].as_str().unwrap().to_owned();
        let water = case.entry["water"].as_str().unwrap().to_owned();
        let (agent_id, toilet, due, other) = case
            .loaded
            .cell
            .with(|world| {
                let rows = world.rows().unwrap();
                let task = rows.active_tasks.get(&task_id).unwrap();
                let agent_id = task.agent_id.clone().unwrap();
                let mut agents: Vec<Arc<str>> = rows.agents.iter().map(|(id, _)| id.clone()).collect();
                agents.sort();
                let other = agents.into_iter().find(|id| *id != agent_id).unwrap();
                (agent_id, task.resource_id.clone().unwrap(), task.due_minute, other)
            })
            .unwrap();
        let mut payload = json!({"facility_id": &*toilet, "agent_id": &*agent_id, "task_id": &task_id});
        match spoil {
            "facility" => payload["facility_id"] = json!(water),
            "unknown_facility" => payload["facility_id"] = json!("nowhere"),
            "agent" => payload["agent_id"] = json!("nobody"),
            "other_agent" => payload["agent_id"] = json!(&*other),
            _ => {}
        }
        case.edit(|world| match spoil {
            "kind" => world.rows_mut().unwrap().active_tasks.get_mut(&task_id).unwrap().kind = text("toilet_queue"),
            "activity" => world.rows_mut().unwrap().agents.get_mut(&agent_id).unwrap().activity = Activity::Idle,
            "slot" => world.rows_mut().unwrap().facilities.get_mut(&toilet).unwrap().active_agent_ids.push(agent_id.clone()),
            _ => {}
        });
        let sequence = case.loaded.cell.with(|world| world.id_counter("event").unwrap().unwrap()).unwrap() + 50;
        let row = texts::row(&event("toilet_service_complete", payload, "completion", due, sequence));
        let said = case.says(|case| {
            let holding = kit::holding(&case.loaded.cell, &mut case.host)?;
            let queues = holding.held("facility_queues_v1", "the world holds another queue state");
            let body = holding.held("body_state_v1", "the world holds another body state");
            let geo = case.host.floor_plan()?;
            let metric_state_held = holding.field(METRIC_STATE);
            case.loaded.cell.with(|world| toilet::complete(world, &row, &queues, &geo, &body, metric_state_held)).unwrap().map(drop).map_err(Error::from)
        });
        let expected = expected.replace("{water}", &water).replace("{task}", &task_id).replace("{agent}", &agent_id).replace("{toilet}", &toilet);
        assert_eq!(said, ("EventApplicationError", expected), "{spoil}");
    }
}

#[test]
fn the_ledger_handler_wraps_python_s_text() {
    let cases: Vec<(&str, Value, &str)> = vec![
        ("loss", json!({"warehouse_id": "warehouse-h02", "resource_id": "water_ml", "quantity": 900}), "resource loss exceeds unreserved remaining inventory"),
        (
            "reserve",
            json!({"warehouse_id": "warehouse-h02", "resource_id": "water_ml", "reservation_id": "R-1", "agent_id": "A002", "quantity": 900}),
            "resource reservation exceeds available inventory",
        ),
        (
            "release",
            json!({"warehouse_id": "warehouse-h02", "resource_id": "water_ml", "reservation_id": "R-unknown", "agent_id": "A002"}),
            "unknown resource reservation 'R-unknown'",
        ),
        ("delivery", json!({"warehouse_id": "elsewhere", "resource_id": "water_ml", "quantity": 5}), "unknown resource warehouse ID 'elsewhere'"),
        ("delivery", json!({"warehouse_id": "warehouse-h02", "resource_id": "water_ml", "quantity": 0}), "resource quantity must be a positive integer"),
    ];
    for (operation, payload, expected) in cases {
        let mut case = case("serving");
        let sequence = case.loaded.cell.with(|world| world.id_counter("event").unwrap().unwrap()).unwrap() + 50;
        let row = texts::row(&event(&format!("resource_{operation}"), payload, "exogenous", 0, sequence));
        let said = case.says(|case| {
            let holding = kit::holding(&case.loaded.cell, &mut case.host)?;
            let operational = holding.held(OPERATIONAL, "the world holds another operational state");
            let service = holding.held("resource_service_v1", "the world holds another service state");
            let ledger = holding.held("resource_ledger_v1", "the world holds another ledger");
            let inputs = LedgerIn { operational: &operational, service: &service, ledger: &ledger };
            match case.loaded.cell.with(|world| handlers::resource_ledger(world, &inputs, &row, operation)).unwrap() {
                Ok(_) => Ok(()),
                // The handler's `except ResourceLedgerError as exc: raise EventApplicationError(str(exc))`.
                Err(Fail::Ledger(fault)) => Err(Error::failed(fault.text(service::repr_of))),
                Err(Fail::Error(error)) => Err(error),
            }
        });
        assert_eq!(said, ("EventApplicationError", expected.to_owned()), "{operation}");
    }
}
