//! `_apply_effect`, `_route`, `_schedule_travel` and the water and meal service on the worlds of
//! `tests/integration/test_service_rules.py` (Issue #225, lane M6; moved from that file).
//!
//! The worlds are `tests/data/service_rules.json` (`tests/support/core_fixtures_service.py`): the
//! generated Scenario of the Project screen at minute 5 with its Run's policies, and the worlds the
//! pytest edited of it; the IDs the pytest picked in Python's order are the file's `inputs`. Each
//! case calls the core's step its PyO3 entry calls: `propose::apply_alone` (the propose phase's
//! `_apply_effect`), `travel::route` and `schedule_travel`, the service's `serve` (one pass of
//! `arbitrate`'s loop, as `serve_head`), `complete`, `request`, `append_outcome` and
//! `facilities_to_serve`.
//!
//! The pytest compared the world a step left with a digest of what the deleted Python rule left,
//! taken from Python's `repr` of the events (`world_digest`, `_is_python_s`). The core's World
//! does not write that `repr`, so the digests here are the first 16 hex digits of the sha256 of
//! the world's canonical text (`steps::Case::digest`), recorded from the core in #225 lane M6
//! (decision 1): on the same kernel, each pytest named in a test's doc comment passed with the
//! digests it had, and the PyO3 entry left the same canonical text as the core does here.

mod support;

use std::sync::Arc;

use serde_json::{json, Value};
use sheltersim_core::config::population::AccessibilityNeeds;
use sheltersim_core::config::SectionId;
use sheltersim_core::error::{Error, Outcome};
use sheltersim_core::events::writer::WriteError;
use sheltersim_core::host::Host;
use sheltersim_core::routes::Route;
use sheltersim_core::rules::proposals::Action;
use sheltersim_core::rules::service::{self, Minute, Request, Requested, Serve, Served};
use sheltersim_core::rules::travel::{self, Trip, Walking};
use sheltersim_core::state::resource_service::RequestRecord;
use sheltersim_core::world::WorldCell;

use support::steps::{self, applied, chained, row, row_mut, s, Case};
use support::TestHost;

const FILE: &str = "service_rules";

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn inputs(name: &str) -> Value {
    steps::inputs(FILE, name)
}

// ---- The steps ------------------------------------------------------------------------------------

/// `_schedule_travel` of a trip home, without the world's validation.
fn schedule(case: &mut Case, agent: &str, route: &Route, purpose: &str) -> Result<bool, Error> {
    let trip = Trip {
        agent_id: text(agent),
        route: route.clone(),
        points: Vec::new(),
        purpose,
        facility_id: None,
        minute: case.minute(),
        causation_id: None,
        support_request_id: None,
        support_staff_id: None,
    };
    case.tick(|tick| {
        let holding = tick.holding().unwrap();
        let movement = holding.held("movement_v1", "the world holds another movement state");
        let plan = tick.plan();
        let walking = Walking { movement: &movement, geo: plan.get() };
        match tick.cell.with(|world| travel::schedule_travel(world, walking, trip)).unwrap() {
            Ok(scheduled) => Ok(scheduled),
            Err(WriteError::Error(error)) => Err(error),
            Err(other) => panic!("{other:?}"),
        }
    })
}

/// One pass of `arbitrate`'s loop at a facility's shared queue (`serve_head`): the started
/// event's ID, or None.
fn serve(case: &mut Case, facility: &str, resource: &str) -> Result<Option<Arc<str>>, Error> {
    case.tick(|tick| {
        let manager = service::manager_in(tick)?;
        let minute = tick.minute;
        tick.host.read_queue_rows()?;
        let pending = tick.cell.with(|world| service::reserves_of(world))??;
        let walking = tick.walking(false)?;
        let walk = walking.as_ref().map(|walking| walking.walk());
        let pass = Serve { facility_id: facility, resource_id: resource, minute, queue_id: None, walk: walk.as_ref(), placement: walking.as_ref().map(|walking| &*walking.geo) };
        match tick.cell.with(|world| service::serve(world, &manager.manager(), &pass, &pending))? {
            Ok(Served::Started(event_id, balance)) => {
                tick.host.resources_changed(balance.index, balance.remaining)?;
                Ok(Some(event_id))
            }
            Ok(Served::Stopped(_)) => Ok(None),
            Err(failure) => Err(service::failure_of::<TestHost>(failure)),
        }
    })
}

/// `ResourceServiceManager(world).complete(event)` of the pending event with this ID.
fn complete(case: &mut Case, event_id: &str) -> Result<(), Error> {
    let doc = case.doc();
    let event = doc["event_queue"].as_array().unwrap().iter().find(|event| event["event_id"] == event_id).unwrap().clone();
    let row = support::texts::row(&event);
    case.tick(|tick| {
        let manager = service::manager_in(tick)?;
        match tick.cell.with(|world| service::complete(world, &manager.manager(), &row))? {
            Ok(balance) => tick.host.resources_changed(balance.index, balance.remaining),
            Err(failure) => Err(service::failure_of::<TestHost>(failure)),
        }
    })
}

/// `ResourceServiceManager(world).request(agent, facility, validate_state=False)`.
fn request(case: &mut Case, agent: &str, facility: &str) -> Result<Requested, Error> {
    case.tick(|tick| {
        let manager = service::manager_in(tick)?;
        let holding = tick.holding()?;
        let needs = tick.value_section(&holding, SectionId::AccessibilityNeeds, |configs| &configs.accessibility_needs, AccessibilityNeeds::parse)?;
        let pending = tick.cell.with(|world| service::reserves_of(world))??;
        let asked = Request { agent_id: agent, facility_id: facility, minute: Minute::Absent, queue_id: None, needs: &needs };
        service::request(tick.cell, tick.host, &manager.manager(), &asked, &pending, false)
    })
}

/// The resource manager's config value, as the Run read it.
fn config(case: &Case, key: &str) -> f64 {
    case.entry["run"]["policies"]["controller"]["attrs"]["resource_service"]["object"]["attrs"]["config"]["value"][key].as_f64().unwrap()
}

fn metric(doc: &Value, name: &str) -> f64 {
    doc["metric_state"].get(name).and_then(Value::as_f64).unwrap_or(0.0)
}

fn pending_count(doc: &Value) -> usize {
    doc["event_queue"].as_array().unwrap().len()
}

// ---- _apply_effect --------------------------------------------------------------------------------

/// `test_a_still_action_is_python_s` (pytest digests `1fecc84b22ca7b75`, `8dba68cbcb659dc2`,
/// `9dd05aad2d72d58b`).
#[test]
fn a_still_action_gives_up_the_target_and_waits() {
    let given = inputs("still_action");
    for (action, digest) in [(Action::Wait, "0be94bc83cc6e479"), (Action::Rest, "3ac2f1a446076bcd"), (Action::Sleep, "7b8bc81ba8da2bcf")] {
        let mut case = Case::of(FILE, s(&given["world"]));
        assert_eq!(case.apply(action, s(&given["agent"]), None).unwrap(), applied(None, 12, true), "{action:?}");
        assert_eq!(case.digest(), digest, "{action:?}");
    }
}

/// `test_a_social_action_schedules_python_s_event` (pytest digests `4a0541d31b600176`,
/// `2861f4df8b51b5a8`, `7ba195e90b4e41d3`).
#[test]
fn a_social_action_schedules_its_event() {
    let pair = &inputs("social_action")["pair"];
    for (action, digest) in [(Action::Socialize, "50a49d86a19143b2"), (Action::CareFamily, "f856f5147a71a969"), (Action::HelpOthers, "fa5a6ecdfe69df94")] {
        let mut case = Case::of(FILE, "minute_5");
        let pending = pending_count(&case.doc());
        assert_eq!(case.apply(action, s(&pair[0]), Some(s(&pair[1]))).unwrap(), applied(Some(s(&pair[1])), 10, true), "{action:?}");
        assert_eq!(pending_count(&case.doc()), pending + 1);
        assert_eq!(case.digest(), digest, "{action:?}");
    }
}

/// `test_the_config_s_minutes_and_their_end_of_the_run` (pytest digests `de59a5ea0e95b1e8`,
/// `72fe8db9763f1642`).
#[test]
fn the_config_s_minutes_and_their_end_of_the_run() {
    let given = inputs("config_s_minutes");
    let pairs = &given["pairs"];
    let mut case = Case::of(FILE, s(&given["world"]));
    assert_eq!(case.apply(Action::Socialize, s(&pairs[0][0]), Some(s(&pairs[0][1]))).unwrap(), applied(Some(s(&pairs[0][1])), 16, true));
    assert_eq!(case.digest(), "d35a5ea7795919d4");
    // Past the last minute there is no event, and the planned decision stands.
    case.edit(|doc| doc["model_configs"]["body_dynamics"]["social_interaction_minutes"] = doc["duration_minutes"].clone());
    assert_eq!(case.apply(Action::Socialize, s(&pairs[1][0]), Some(s(&pairs[1][1]))).unwrap(), applied(None, 12, false));
    assert_eq!(case.digest(), "55124a984f06b0af");
}

/// `test_without_a_config_the_default_minutes_are_python_s` (pytest digest `50325313983b94ac`).
#[test]
fn without_a_config_the_default_minutes() {
    let pair = &inputs("social_action")["pair"];
    let mut case = Case::of(FILE, s(&inputs("default_minutes")["world"]));
    assert_eq!(case.apply(Action::Socialize, s(&pair[0]), Some(s(&pair[1]))).unwrap(), applied(Some(s(&pair[1])), 10, true));
    assert_eq!(case.digest(), "b77a301f9e574d89");
}

/// `test_a_social_action_needs_someone_else_in_the_zone` (pytest digest `a8a42ffe67df1a3a` after
/// each step).
#[test]
fn a_social_action_needs_someone_else_in_the_zone() {
    let given = inputs("needs_someone_else");
    let mut case = Case::of(FILE, "minute_5");
    for target in given["targets"].as_array().unwrap() {
        assert_eq!(case.apply(Action::HelpOthers, s(&given["agent"]), target.as_str()).unwrap(), applied(None, 12, false), "{target}");
        assert_eq!(case.digest(), "4686d507aa38a03a");
    }
}

/// `test_a_held_bed_is_given_up_and_sleep_keeps_it`.
#[test]
fn a_held_bed_is_given_up_and_sleep_keeps_it() {
    let given = inputs("held_bed");
    let pair = &given["pair"];
    let mut case = Case::of(FILE, s(&given["world"]));
    case.apply(Action::Wait, s(&pair[0]), None).unwrap();
    case.apply(Action::Sleep, s(&pair[1]), None).unwrap();
    let reservations = &case.doc()["model_state"]["geometry_overlay_v1"]["bed_reservations"];
    assert_eq!(reservations.as_object().unwrap().keys().collect::<Vec<_>>(), vec![s(&pair[1])]);
}

/// `test_each_trip_action_from_each_zone` (pytest: eight digests of each action): each person is
/// moved to each zone in turn until a trip starts, as the pytest moved them; the target of each
/// step is the one the pytest listed.
#[test]
fn each_trip_action_from_each_zone() {
    let given = inputs("trip_action_from_each_zone");
    for (action, name, digest) in [(Action::RequestToilet, "request_toilet", "71bfda8567bf854f"), (Action::FetchWater, "fetch_water", "3ba2effc6801e384"), (Action::CollectMeal, "collect_meal", "ed578770e8974d1d")] {
        let mut case = Case::of(FILE, "minute_5");
        let mut lines = Vec::new();
        for step in given[name].as_array().unwrap() {
            let (agent, zone, target) = (s(&step[0]), s(&step[1]), s(&step[2]));
            case.edit(|doc| {
                row_mut(doc, "agents", agent)["zone_id"] = json!(zone);
                doc["model_state"]["movement_v1"]["positions"][agent]["zone_id"] = json!(zone);
            });
            assert_eq!(case.apply(action, agent, None).unwrap(), applied(Some(target), 12, true), "{step}");
            lines.push(case.digest());
        }
        assert_eq!(lines.len(), 8);
        assert_eq!(chained(&lines), digest, "{name}");
    }
}

/// `test_a_trip_past_the_end_of_the_run_is_refused_as_python_refuses` (pytest digests
/// `7e223ff83025985b` twice, `49966c9a404bdcb4`).
#[test]
fn a_trip_past_the_end_of_the_run_is_refused() {
    let given = inputs("trip_past_the_end");
    let mut case = Case::of(FILE, s(&given["world"]));
    let mut lines = Vec::new();
    for step in given["steps"].as_array().unwrap() {
        let action = Action::parse(s(&step[0])).unwrap();
        assert_eq!(case.apply(action, s(&step[1]), None).unwrap(), applied(None, 12, false), "{step}");
        lines.push(case.digest());
    }
    assert_eq!(lines[0], lines[1]);
    assert_eq!(chained(&lines), "8c00aa684b02cda8");
}

/// `test_no_known_facility_is_recorded_as_python_records` (pytest digests `de420a4402712c0d`,
/// `67380efd43798097` twice).
#[test]
fn no_known_facility_is_recorded() {
    let given = inputs("no_known_facility");
    let agent = s(&given["agent"]);
    let mut case = Case::of(FILE, s(&given["world"]));
    let mut lines = Vec::new();
    for action in [Action::FetchWater, Action::CollectMeal, Action::RequestToilet] {
        assert_eq!(case.apply(action, agent, None).unwrap(), applied(None, 12, false), "{action:?}");
        lines.push(case.digest());
    }
    assert_eq!(lines[1], lines[2]);
    assert_eq!(chained(&lines), "ed72d0d4f5841f69");
    let doc = case.doc();
    let history = doc["model_state"]["resource_service_v1"]["request_history"].as_array().unwrap();
    let reasons: Vec<&Value> = history[history.len() - 2..].iter().map(|record| &record["reason"]).collect();
    assert_eq!(reasons, [&json!("no_facility"), &json!("no_facility")]);
    assert!(metric(&doc, "resource_service_request_rejected_count") >= 2.0);
}

/// `test_the_lower_load_goes_before_the_shorter_walk`: the least load, then the shortest walk,
/// then the least ID (`_select_toilet` of 45a9bc9).
#[test]
fn the_lower_load_goes_before_the_shorter_walk() {
    let given = inputs("lower_load");
    let mut case = Case::of(FILE, s(&given["world"]));
    let done = case.apply(Action::RequestToilet, s(&given["agent"]), None).unwrap();
    assert_eq!(done.target.as_deref(), Some(s(&given["far"])));
}

/// `test_a_toilet_kind_beyond_ascii_is_folded_as_python_folds_it` (pytest digest
/// `068b0babfdc6141d`): `"toilet" in kind.casefold()`, where full-width letters fold to
/// full-width ones.
#[test]
fn a_toilet_kind_beyond_ascii_is_folded_as_python_folds_it() {
    let given = inputs("toilet_kind_beyond_ascii");
    let mut case = Case::of(FILE, s(&given["world"]));
    assert_eq!(case.apply(Action::RequestToilet, s(&given["agent"]), None).unwrap(), applied(Some("toilet_01"), 12, true));
    assert_eq!(case.digest(), "3470b3fa1e87508e");
}

/// `test_a_request_of_the_run_s_last_minute_is_closed_at_once` (pytest digests
/// `66c8a5870bf1b6f4`, `32f0d7926c7ef2c5`).
#[test]
fn a_request_of_the_run_s_last_minute_is_closed_at_once() {
    let given = inputs("request_of_the_last_minute");
    for (action, digest) in [(Action::SeekHelp, "7a316da76d11aba2"), (Action::SeekInformation, "e865309dcc2977f1")] {
        let mut case = Case::of(FILE, s(&given["world"]));
        let request = "support-5d7322b2c209034dac8ce21e";
        assert_eq!(case.apply(action, s(&given["agent"]), None).unwrap(), applied(Some(request), 12, false), "{action:?}");
        assert_eq!(row(&case.doc(), "support_requests", request)["resolved_minute"], json!(5));
        assert_eq!(case.digest(), digest, "{action:?}");
    }
}

// ---- _route and _schedule_travel ------------------------------------------------------------------

/// `test_every_route_is_python_s_for_every_mobility` (pytest digest `c5d1362fa0212f15` of the
/// routes' `repr`): from each zone to each, for the first person of each mobility.
#[test]
fn every_route_for_every_mobility() {
    let mut case = Case::of(FILE, "minute_5");
    let doc = case.doc();
    let zones: Vec<String> = doc["zones"].as_array().unwrap().iter().map(|zone| s(&zone["id"]).to_owned()).collect();
    let mut seen = Vec::new();
    let mut people = Vec::new();
    for agent in doc["agents"].as_array().unwrap() {
        let mobility = s(&agent["profile"]["mobility"]).to_owned();
        if !seen.contains(&mobility) {
            seen.push(mobility);
            people.push(s(&agent["profile"]["id"]).to_owned());
        }
    }
    let mut found = Vec::new();
    for agent in &people {
        for origin in &zones {
            for destination in &zones {
                let route = case.route(origin, destination, agent).expect("a route");
                found.push(format!("{} {} {:?} {:?} {}", route.origin, route.destination, route.zone_ids, route.edge_ids, route.minutes));
            }
        }
    }
    assert_eq!(found.len(), 192);
    assert_eq!(chained(&found), "ab7ea936fa50fb1e");
}

/// `test_an_equal_time_goes_to_the_lower_edge_id`: the graph's own travel times equal, so only the
/// IDs decide; as the Python rule found it.
#[test]
fn an_equal_time_goes_to_the_lower_edge_id() {
    let given = inputs("equal_time");
    let zones: Vec<&str> = given["zones"].as_array().unwrap().iter().map(s).collect();
    let mut case = Case::of(FILE, s(&given["world"]));
    let route = case.route(zones[0], zones[3], s(&given["agent"])).unwrap();
    assert_eq!(route.edge_ids, [text("a-c"), text("n-d")]);
    assert_eq!(route.zone_ids, [text(zones[0]), text(zones[2]), text(zones[3])]);
    assert_eq!(route.minutes, 4);
}

/// `test_a_trip_and_each_limit_are_python_s` (pytest digests `167bbd68b5a94255`,
/// `623e47f172d00aff`, `80054702e0e729ce`): the zone is full, counting who walks there; the trip
/// ends past the run; then it starts.
#[test]
fn a_trip_and_each_limit() {
    let given = inputs("trip");
    let (agent, destination) = (s(&given["agent"]), s(&given["destination"]));
    let mut case = Case::of(FILE, "minute_5");
    let origin = s(&row(&case.doc(), "agents", agent)["zone_id"]).to_owned();
    let route = case.route(&origin, destination, agent).unwrap();
    let capacity = row(&case.doc(), "zones", destination)["capacity"].clone();
    let mut lines = Vec::new();
    case.edit(|doc| row_mut(doc, "zones", destination)["capacity"] = json!(0));
    assert!(!schedule(&mut case, agent, &route, "return").unwrap());
    lines.push(case.digest());
    let duration = case.doc()["duration_minutes"].clone();
    case.edit(|doc| {
        row_mut(doc, "zones", destination)["capacity"] = capacity;
        doc["duration_minutes"] = json!(5 + route.minutes);
    });
    assert!(!schedule(&mut case, agent, &route, "return").unwrap());
    lines.push(case.digest());
    case.edit(|doc| doc["duration_minutes"] = duration);
    assert!(schedule(&mut case, agent, &route, "return").unwrap());
    assert_eq!(row(&case.doc(), "agents", agent)["activity"], json!("moving"));
    lines.push(case.digest());
    assert_eq!(chained(&lines), "2faa443c81665a5a");
}

/// `test_a_trip_refused_writes_nothing`: the Python rule's errors, raised before anything is
/// written, and a person the world does not have refused. The pytest keeps the refusal's
/// `KernelRefusal` of the PyO3 entry.
#[test]
fn a_trip_refused_writes_nothing() {
    let given = inputs("trip");
    let (agent, destination) = (s(&given["agent"]), s(&given["destination"]));
    let mut case = Case::of(FILE, "minute_5");
    let origin = s(&row(&case.doc(), "agents", agent)["zone_id"]).to_owned();
    let route = case.route(&origin, destination, agent).unwrap();
    let before = case.bytes();
    match schedule(&mut case, agent, &route, "staff_support_travel") {
        Err(Error::Value(text)) => assert_eq!(text, "staff support travel requires request and staff IDs"),
        other => panic!("{other:?}"),
    }
    match schedule(&mut case, s(&given["elsewhere"]), &route, "return") {
        Err(Error::Value(text)) => assert_eq!(text, "travel route origin does not match Agent zone"),
        other => panic!("{other:?}"),
    }
    assert!(matches!(schedule(&mut case, "nobody", &route, "return"), Err(Error::Unsupported(_))));
    assert!(case.bytes() == before);
}

// ---- arbitrate --------------------------------------------------------------------------------------

/// `test_a_head_at_the_point_starts_its_service` (pytest digest `2a3c0767c9e08409`).
#[test]
fn a_head_at_the_point_starts_its_service() {
    let given = inputs("at_water_station");
    let mut case = Case::of(FILE, s(&given["world"]));
    let event_id = serve(&mut case, s(&given["facility"]), "water_ml").unwrap().expect("a service started");
    let doc = case.doc();
    assert_eq!(row(&doc, "agents", s(&given["agent"]))["activity"], json!("in_service"));
    let event = doc["event_queue"].as_array().unwrap().iter().find(|event| event["event_id"] == *event_id).unwrap();
    assert_eq!(event["event_type"], json!("resource_service_complete"));
    assert!(metric(&doc, "water_service_wait_count") >= 1.0);
    assert_eq!(case.digest(), "5580fab0f3edca36");
}

/// `test_each_wait_is_python_s_record` (pytest digests: `capacity` `bb6a050583414052`, `staff`
/// `aea91f1779cd9538`, `stock` `83e5c9064839b2b3`, `run_end` `62eee99e27894a3c`, `water_outage`
/// `1b96482b22525348`): the head waits, its stall is recorded once, and the same wait the next
/// minute adds nothing.
#[test]
fn each_wait_is_recorded_once() {
    let given = inputs("each_wait");
    for (reason, digest) in [("capacity", "f2c3675464ad178f"), ("staff", "d4ef7c11e9941a00"), ("stock", "9badb6cecc2af52e"), ("run_end", "52ef8003baf26ebc"), ("water_outage", "8aedb553e056afd8")] {
        let case_inputs = &given[reason];
        let (agent, facility) = (s(&case_inputs["agent"]), s(&case_inputs["facility"]));
        let mut case = Case::of(FILE, s(&case_inputs["world"]));
        let counter = format!("resource_service_wait_{reason}_count");
        let count = metric(&case.doc(), &counter);
        assert_eq!(serve(&mut case, facility, "water_ml").unwrap(), None, "{reason}");
        let doc = case.doc();
        let state = &doc["model_state"]["resource_service_v1"];
        assert_eq!(state["stall_state"][facility], json!({"agent_id": agent, "reason": reason}));
        let history = state["queue_stall_history"].as_array().unwrap();
        assert_eq!(history.last().unwrap(), &json!({"minute": 5, "facility_id": facility, "agent_id": agent, "reason": reason}));
        assert_eq!(metric(&doc, &counter), count + 1.0);
        assert_eq!(case.digest(), digest, "{reason}");
        let before = case.bytes();
        assert_eq!(serve(&mut case, facility, "water_ml").unwrap(), None);
        assert!(case.bytes() == before, "{reason}");
    }
}

/// `test_a_taken_reservation_id_is_python_s_error`.
#[test]
fn a_taken_reservation_id_is_python_s_error() {
    let given = inputs("taken_reservation_id");
    let mut case = Case::of(FILE, s(&given["world"]));
    let before = case.bytes();
    match serve(&mut case, s(&given["facility"]), "water_ml") {
        Err(Error::Invariant(text)) => assert_eq!(text, format!("resource service task ID 'queue-task-{}' conflicts with an H01 reservation", s(&given["agent"]))),
        other => panic!("{other:?}"),
    }
    assert!(case.bytes() == before);
}

/// `test_only_the_queues_with_people_are_served`.
#[test]
fn only_the_queues_with_people_are_served() {
    let given = inputs("queues_with_people");
    let mut due = Vec::new();
    for name in given["worlds"].as_array().unwrap() {
        let mut case = Case::of(FILE, s(name));
        due.push(case.tick(|tick| {
            let holding = tick.holding().unwrap();
            let queues = holding.held("facility_queues_v1", "the world holds another queue state");
            tick.cell.with(|world| service::facilities_to_serve(world, &queues)).unwrap().unwrap()
        }));
    }
    assert_eq!(due[0], []);
    assert_eq!(due[1], [(text(s(&given["facility"])), "food_meal_units")]);
}

// ---- complete -----------------------------------------------------------------------------------------

/// `test_a_completion_relieves_python_s_need` (pytest digests: water `2a3c0767c9e08409` started,
/// `4f39327b201771c8` completed; meal `0d30579820e3bf95`, `46c48784b7891a9e`).
#[test]
fn a_completion_relieves_the_need() {
    for (kind, resource, need, relief, started, completed) in [
        ("at_water_station", "water_ml", "hydration_need", "hydration_relief", "5580fab0f3edca36", "8bd033243e885d37"),
        ("at_meal_distribution", "food_meal_units", "hunger", "hunger_relief", "7b9a213c72a5295d", "3cbd3713deb4e8b9"),
    ] {
        let given = inputs(kind);
        let (agent, facility) = (s(&given["agent"]), s(&given["facility"]));
        let mut case = Case::of(FILE, s(&given["world"]));
        let event_id = serve(&mut case, facility, resource).unwrap().unwrap();
        assert_eq!(case.digest(), started, "{kind}");
        case.edit(|doc| row_mut(doc, "agents", agent)["needs"][need] = json!(0.812345));
        let doc = case.doc();
        let event = doc["event_queue"].as_array().unwrap().iter().find(|event| event["event_id"] == *event_id).unwrap().clone();
        complete(&mut case, &event_id).unwrap();
        assert_eq!(case.digest(), completed, "{kind}");
        let doc = case.doc();
        let person = row(&doc, "agents", agent);
        assert_eq!(person["needs"][need].as_f64().unwrap(), f64::max(0.0, 0.812345 - config(&case, relief)));
        assert_eq!(person["activity"], json!("idle"));
        assert!(!row(&doc, "facilities", facility)["resource_active_agent_ids"].as_array().unwrap().contains(&json!(agent)));
        assert!(!doc["active_tasks"].as_array().unwrap().iter().any(|task| task["id"] == event["payload"]["task_id"]));
    }
}

/// `test_the_relief_is_read_at_its_value_and_stops_at_zero`: the manager's relief made 1% larger.
#[test]
fn the_relief_is_read_at_its_value() {
    let given = inputs("relief_at_its_value");
    let (agent, facility) = (s(&given["agent"]), s(&given["facility"]));
    let mut case = Case::of(FILE, s(&given["world"]));
    let event_id = serve(&mut case, facility, "water_ml").unwrap().unwrap();
    case.edit(|doc| row_mut(doc, "agents", agent)["needs"]["hydration_need"] = json!(0.5));
    complete(&mut case, &event_id).unwrap();
    let relief = config(&case, "hydration_relief");
    assert_eq!(row(&case.doc(), "agents", agent)["needs"]["hydration_need"].as_f64().unwrap(), f64::max(0.0, 0.5 - relief));
    assert_ne!(relief, config(&Case::of(FILE, "minute_5"), "hydration_relief"));
}

/// `test_a_completion_that_does_not_match_is_python_s_error`: its staff free, the reservation not
/// the service's, the person not served.
#[test]
fn a_completion_that_does_not_match_is_python_s_error() {
    let given = inputs("at_water_station");
    for change in ["busy_until", "active_reservation_ids", "resource_active_agent_ids"] {
        let mut case = Case::of(FILE, s(&given["world"]));
        let event_id = serve(&mut case, s(&given["facility"]), "water_ml").unwrap().unwrap();
        let doc = case.doc();
        let event = doc["event_queue"].as_array().unwrap().iter().find(|event| event["event_id"] == *event_id).unwrap().clone();
        case.edit(|doc| match change {
            "busy_until" => row_mut(doc, "staff", s(&event["payload"]["staff_id"]))["busy_until"] = json!(0),
            "active_reservation_ids" => doc["model_state"]["resource_service_v1"]["active_reservation_ids"] = json!([]),
            _ => row_mut(doc, "facilities", s(&event["payload"]["facility_id"]))["resource_active_agent_ids"] = json!([]),
        });
        let before = case.bytes();
        match complete(&mut case, &event_id) {
            Err(Error::Outcome(Outcome::Failed, text)) => assert_eq!(text, "resource service completion does not match its full reservation, slot, or staff", "{change}"),
            other => panic!("{change}: {other:?}"),
        }
        assert!(case.bytes() == before, "{change}");
    }
}

// ---- request and its record ---------------------------------------------------------------------------

/// `test_a_request_is_python_s` (pytest digest `22667c113d8b86dd`).
#[test]
fn a_request_queues_the_person() {
    let given = inputs("request");
    let (agent, facility) = (s(&given["agent"]), s(&given["facility"]));
    let mut case = Case::of(FILE, s(&given["world"]));
    let counter = case.doc()["id_counters"].get("resource_task").and_then(Value::as_i64).unwrap_or(0);
    let Requested::Queued(receipt) = request(&mut case, agent, facility).unwrap() else {
        panic!("refused");
    };
    let task_id = format!("resource-service-task-{counter}");
    assert_eq!((&*receipt.agent_id, &*receipt.facility_id, receipt.resource_id), (agent, facility, "water_ml"));
    assert_eq!((receipt.requested_at, &*receipt.queue_task_id), (5, task_id.as_str()));
    let doc = case.doc();
    assert_eq!(receipt.queue_position as usize, row(&doc, "facilities", facility)["resource_queue_agent_ids"].as_array().unwrap().len());
    assert_eq!(row(&doc, "agents", agent)["activity"], json!("queued"));
    assert_eq!(row(&doc, "active_tasks", &task_id)["kind"], json!("resource_service_queue"));
    assert_eq!(case.digest(), "e1dd052071d134fe");
}

/// `test_a_request_refused_has_python_s_text`: the kernel's text of the Python rule's ValueError,
/// before it writes anything.
#[test]
fn a_request_refused_has_python_s_text() {
    let cases = inputs("request_refused");
    for given in cases.as_array().unwrap() {
        let agent = s(&given["agent"]);
        let mut case = Case::of(FILE, s(&given["world"]));
        let before = case.bytes();
        let wanted = match s(&given["change"]) {
            "kitchen" => "Agent must arrive at the resource service facility before queueing".to_owned(),
            _ => format!("Agent '{agent}' cannot request resource service while active"),
        };
        match request(&mut case, agent, s(&given["facility"])).unwrap() {
            Requested::Refused(said) => assert_eq!(said, wanted),
            queued => panic!("{queued:?}"),
        }
        assert!(case.bytes() == before);
    }
    assert_eq!(cases.as_array().unwrap().len(), 2);
}

/// `test_a_record_of_a_request_is_python_s`: the record appended with the world's minute, and
/// nothing else written: the world the pytest made by appending it in Python.
#[test]
fn a_record_of_a_request() {
    let given = inputs("record_of_a_request");
    let options = &given["options"];
    let mut case = Case::of(FILE, s(&given["world"]));
    let optional = |key: &str| options[key].as_str().map(text);
    let record = RequestRecord {
        minute: 5,
        agent_id: text(s(&options["agent_id"])),
        action: text(s(&options["action"])),
        outcome: text(s(&options["outcome"])),
        reason: text(s(&options["reason"])),
        facility_id: optional("facility_id"),
        origin_zone_id: text(s(&options["origin_zone_id"])),
        detail: optional("detail"),
    };
    case.tick(|tick| {
        let holding = tick.holding().unwrap();
        let held = holding.held("resource_service_v1", "the world holds another service state");
        tick.cell.with(|world| service::append_outcome(world, &held, record)).unwrap().unwrap();
    });
    assert!(case.bytes() == Case::of(FILE, s(&given["after"])).bytes());
}
