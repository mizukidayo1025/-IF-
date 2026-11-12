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
//! does not write that `repr`, so the digests here are of each step's answer and the world's
//! canonical parts after it (`steps::Case::answered`, `support/answers.rs`), recorded from the core
//! in #225 lane M6 (decision 1): on the same kernel, each pytest named in a test's doc comment
//! passed with the digests it had, and the PyO3 entry left the same canonical text as the core does
//! here.

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

use support::steps::{self, applied, row, row_mut, s, Case};
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
    let result = case.tick(|tick| {
        let holding = tick.holding().unwrap();
        let movement = holding.held("movement_v1", "the world holds another movement state");
        let plan = tick.plan();
        let walking = Walking { movement: &movement, geo: plan.get() };
        match tick.cell.with(|world| travel::schedule_travel(world, walking, trip)).unwrap() {
            Ok(scheduled) => Ok(scheduled),
            Err(WriteError::Error(error)) => Err(error),
            Err(other) => panic!("{other:?}"),
        }
    });
    case.answered(&result);
    result
}

/// One pass of `arbitrate`'s loop at a facility's shared queue (`serve_head`): the started
/// event's ID, or None.
fn serve(case: &mut Case, facility: &str, resource: &str) -> Result<Option<Arc<str>>, Error> {
    let result = case.tick(|tick| {
        let manager = service::manager_in(tick)?;
        let minute = tick.minute;
        tick.host.read_queue_rows()?;
        let pending = tick.cell.with(|world| service::reserves_of(world))??;
        let walking = tick.walking(false)?;
        let walk = walking.as_ref().map(|walking| walking.walk());
        let pass = Serve { facility_id: facility, resource_id: resource, minute, queue_id: None, walk: walk.as_ref(), placement: walking.as_ref().map(|walking| &*walking.geo) };
        match tick.cell.with(|world| service::serve(world, &manager.manager(), &pass, &pending))? {
            Ok(Served::Started(event_id, balance)) => {
                tick.resources_taken(balance)?;
                Ok(Some(event_id))
            }
            Ok(Served::Stopped(_)) => Ok(None),
            Err(failure) => Err(service::failure_of::<TestHost>(failure)),
        }
    });
    case.answered(&result);
    result
}

/// `ResourceServiceManager(world).complete(event)` of the pending event with this ID.
fn complete(case: &mut Case, event_id: &str) -> Result<(), Error> {
    let doc = case.doc();
    let event = doc["event_queue"].as_array().unwrap().iter().find(|event| event["event_id"] == event_id).unwrap().clone();
    let row = support::texts::row(&event);
    let result = case.tick(|tick| {
        let manager = service::manager_in(tick)?;
        match tick.cell.with(|world| service::complete(world, &manager.manager(), &row))? {
            Ok(balance) => tick.resources_taken(balance),
            Err(failure) => Err(service::failure_of::<TestHost>(failure)),
        }
    });
    case.answered(&result);
    result
}

/// `ResourceServiceManager(world).request(agent, facility, validate_state=False)`.
fn request(case: &mut Case, agent: &str, facility: &str) -> Result<Requested, Error> {
    let result = case.tick(|tick| {
        let manager = service::manager_in(tick)?;
        let holding = tick.holding()?;
        let needs = tick.value_section(&holding, SectionId::AccessibilityNeeds, |configs| &configs.accessibility_needs, AccessibilityNeeds::parse)?;
        let pending = tick.cell.with(|world| service::reserves_of(world))??;
        let asked = Request { agent_id: agent, facility_id: facility, minute: Minute::Absent, queue_id: None, needs: &needs };
        service::request(tick.cell, tick.host, &manager.manager(), &asked, &pending, false)
    });
    case.answered(&result);
    result
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
    for (action, digest) in [(Action::Wait, "2aabc4432833cddd2decbfa67e22dea7cea4a9d10ac834507226a4d2e19c8959"), (Action::Rest, "2f640708922958b147f98ce95dc100513b742872253bc6d4ffc7e1f3cac85285"), (Action::Sleep, "f1612734281eddc62b77ecc4b471fd46443dfc89037c5872d99435d3b20e3403")] {
        let mut case = Case::of(FILE, s(&given["world"]));
        assert_eq!(case.apply(action, s(&given["agent"]), None).unwrap(), applied(None, 12, true), "{action:?}");
        assert_eq!(case.hex(), digest);
    }
}

/// `test_a_social_action_schedules_python_s_event` (pytest digests `4a0541d31b600176`,
/// `2861f4df8b51b5a8`, `7ba195e90b4e41d3`).
#[test]
fn a_social_action_schedules_its_event() {
    let pair = &inputs("social_action")["pair"];
    for (action, digest) in [(Action::Socialize, "0f840912889d248c1daf81382232e0e6f4566dc915951dd4be7b2c77699f9605"), (Action::CareFamily, "e66ebeaed3dca3eb80bb179988defb4dd5aa2ec4895faf30021aae4d3772ddbd"), (Action::HelpOthers, "870a61152f47e033f1024982cd08cf4b872ff0a5a5e6304a1287a6c1c722788f")] {
        let mut case = Case::of(FILE, "minute_5");
        let pending = pending_count(&case.doc());
        assert_eq!(case.apply(action, s(&pair[0]), Some(s(&pair[1]))).unwrap(), applied(Some(s(&pair[1])), 10, true), "{action:?}");
        assert_eq!(pending_count(&case.doc()), pending + 1);
        assert_eq!(case.hex(), digest);
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
    assert_eq!(case.hex(), "48cf36d95b5b9d61557c1f42e85bb44c764df6afcd04783fe2b4289a5821dfb2");
    // Past the last minute there is no event, and the planned decision stands.
    case.edit(|doc| doc["model_configs"]["body_dynamics"]["social_interaction_minutes"] = doc["duration_minutes"].clone());
    assert_eq!(case.apply(Action::Socialize, s(&pairs[1][0]), Some(s(&pairs[1][1]))).unwrap(), applied(None, 12, false));
    assert_eq!(case.hex(), "266999a76d3940df6696597cac5dd34e2c42e7b10d8dc0f84b7c1e314a78807a");
}

/// `test_without_a_config_the_default_minutes_are_python_s` (pytest digest `50325313983b94ac`).
#[test]
fn without_a_config_the_default_minutes() {
    let pair = &inputs("social_action")["pair"];
    let mut case = Case::of(FILE, s(&inputs("default_minutes")["world"]));
    assert_eq!(case.apply(Action::Socialize, s(&pair[0]), Some(s(&pair[1]))).unwrap(), applied(Some(s(&pair[1])), 10, true));
    assert_eq!(case.hex(), "9a814903ab4bb08eb1111cc58e67f93e5498d580a868e636f7b47541c69ac17a");
}

/// `test_a_social_action_needs_someone_else_in_the_zone` (pytest digest `a8a42ffe67df1a3a` after
/// each step).
#[test]
fn a_social_action_needs_someone_else_in_the_zone() {
    let given = inputs("needs_someone_else");
    let mut case = Case::of(FILE, "minute_5");
    let mut worlds = Vec::new();
    for target in given["targets"].as_array().unwrap() {
        assert_eq!(case.apply(Action::HelpOthers, s(&given["agent"]), target.as_str()).unwrap(), applied(None, 12, false), "{target}");
        worlds.push(case.bytes());
    }
    // The pytest's digest was the same after each step.
    assert!(worlds.iter().all(|world| *world == worlds[0]));
    assert_eq!(case.hex(), "77061f57f81971c6be4cb2017bd3566d1dfe5d5e59ddc32c43aae064abbcbc37");
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
    for (action, name, digest) in [(Action::RequestToilet, "request_toilet", "f9b6362f7a1ad46255bd75ae14084e640249387eb1c73cdce014788d7249fc6f"), (Action::FetchWater, "fetch_water", "c4cd33c4a84a3d2afc918beb6f933afd2792e714ddd6ebecdd02db038965b8b0"), (Action::CollectMeal, "collect_meal", "ebaebd1848e4b3796b381e32038f7c4a8fa43d031874cc00a03069c41c2cfb3b")] {
        let mut case = Case::of(FILE, "minute_5");
        let mut worlds = Vec::new();
        for step in given[name].as_array().unwrap() {
            let (agent, zone, target) = (s(&step[0]), s(&step[1]), s(&step[2]));
            case.edit(|doc| {
                row_mut(doc, "agents", agent)["zone_id"] = json!(zone);
                doc["model_state"]["movement_v1"]["positions"][agent]["zone_id"] = json!(zone);
            });
            assert_eq!(case.apply(action, agent, None).unwrap(), applied(Some(target), 12, true), "{step}");
            worlds.push(case.bytes());
        }
        assert_eq!(worlds.len(), 8);
        assert_eq!(case.hex(), digest);
    }
}

/// `test_a_trip_past_the_end_of_the_run_is_refused_as_python_refuses` (pytest digests
/// `7e223ff83025985b` twice, `49966c9a404bdcb4`).
#[test]
fn a_trip_past_the_end_of_the_run_is_refused() {
    let given = inputs("trip_past_the_end");
    let mut case = Case::of(FILE, s(&given["world"]));
    let mut worlds = Vec::new();
    for step in given["steps"].as_array().unwrap() {
        let action = Action::parse(s(&step[0])).unwrap();
        assert_eq!(case.apply(action, s(&step[1]), None).unwrap(), applied(None, 12, false), "{step}");
        worlds.push(case.bytes());
    }
    assert!(worlds[0] == worlds[1]);
    assert_eq!(case.hex(), "3267bee81f34fa9ac6e083ce60f782c24344868c2b0b0da12b622db21d22d1af");
}

/// `test_no_known_facility_is_recorded_as_python_records` (pytest digests `de420a4402712c0d`,
/// `67380efd43798097` twice).
#[test]
fn no_known_facility_is_recorded() {
    let given = inputs("no_known_facility");
    let agent = s(&given["agent"]);
    let mut case = Case::of(FILE, s(&given["world"]));
    let mut worlds = Vec::new();
    for action in [Action::FetchWater, Action::CollectMeal, Action::RequestToilet] {
        assert_eq!(case.apply(action, agent, None).unwrap(), applied(None, 12, false), "{action:?}");
        worlds.push(case.bytes());
    }
    assert!(worlds[1] == worlds[2]);
    assert_eq!(case.hex(), "c6977b38bd0e346bf256cf3d88deb7957ba49cd3827a01fac46d65e77a1b87d8");
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
    assert_eq!(case.hex(), "bbfb03e74247f80c4be1b3729d43b155e0ad8c892783bf3848b2c450d40f4587");
}

/// `test_a_request_of_the_run_s_last_minute_is_closed_at_once` (pytest digests
/// `66c8a5870bf1b6f4`, `32f0d7926c7ef2c5`).
#[test]
fn a_request_of_the_run_s_last_minute_is_closed_at_once() {
    let given = inputs("request_of_the_last_minute");
    for (action, digest) in [(Action::SeekHelp, "65ff571811ac5f4b1363743a8c24a0065e2688dd538cbaccc3b3385ff662867c"), (Action::SeekInformation, "40ae17d6a5f10f6374f1d26001a7e597d25385072830280090b0bd4010dc3145")] {
        let mut case = Case::of(FILE, s(&given["world"]));
        let request = "support-5d7322b2c209034dac8ce21e";
        assert_eq!(case.apply(action, s(&given["agent"]), None).unwrap(), applied(Some(request), 12, false), "{action:?}");
        assert_eq!(row(&case.doc(), "support_requests", request)["resolved_minute"], json!(5));
        assert_eq!(case.hex(), digest);
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
    assert_eq!(case.hex(), "bd51a5cd154ef7fbded4e091589fe23a0a760a7a45060eaeeaf6f7af93ecbd55");
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
    case.edit(|doc| row_mut(doc, "zones", destination)["capacity"] = json!(0));
    assert!(!schedule(&mut case, agent, &route, "return").unwrap());
    let duration = case.doc()["duration_minutes"].clone();
    case.edit(|doc| {
        row_mut(doc, "zones", destination)["capacity"] = capacity;
        doc["duration_minutes"] = json!(5 + route.minutes);
    });
    assert!(!schedule(&mut case, agent, &route, "return").unwrap());
    case.edit(|doc| doc["duration_minutes"] = duration);
    assert!(schedule(&mut case, agent, &route, "return").unwrap());
    assert_eq!(row(&case.doc(), "agents", agent)["activity"], json!("moving"));
    assert_eq!(case.hex(), "640749a65940d7ead17f5f5f499a7c3913eb65b0b7216f96f42a4370b555b359");
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
    assert_eq!(case.hex(), "17e17f23640dcfd2de39337dc40ff7609a7042ddaec082b4aa371e5f69052749");
}

/// `test_each_wait_is_python_s_record` (pytest digests: `capacity` `bb6a050583414052`, `staff`
/// `aea91f1779cd9538`, `stock` `83e5c9064839b2b3`, `run_end` `62eee99e27894a3c`, `water_outage`
/// `1b96482b22525348`): the head waits, its stall is recorded once, and the same wait the next
/// minute adds nothing.
#[test]
fn each_wait_is_recorded_once() {
    let given = inputs("each_wait");
    for (reason, digest) in [("capacity", "4cf6b5308dc1569901a5219327a2ba25c39b62fb7d7cff52ae90375f322ccc02"), ("staff", "a2a629b11053f6b0621c3b2dd2d574664f043a109ad2446d2defe9961a5d0bf7"), ("stock", "2ed219a62070ec04df55a4b2c004798d13e839ba7e40cca5792533285d1a298b"), ("run_end", "6c9018cc26e4bd2602acc48fb7510f9e53af660d74ecc3f3f85338c015062e3f"), ("water_outage", "ae75efe1a692ed6bd1503a75c6908801d064ddfd9c4a37020d8a4d91adc9560a")] {
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
        assert_eq!(case.hex(), digest);
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
        ("at_water_station", "water_ml", "hydration_need", "hydration_relief", "17e17f23640dcfd2de39337dc40ff7609a7042ddaec082b4aa371e5f69052749", "70102a3d5cf9c3df1e5551cc231f44db8cd80e23ff13669a28c54dabb025ef83"),
        ("at_meal_distribution", "food_meal_units", "hunger", "hunger_relief", "513652304385c620a39031c3c6936e6f18dffaf2f09ccf17c756a9bb64acd657", "fdd94d2612c3af532866c861a61cfd4b2fa8e7d97c955cf4074b65f20202c512"),
    ] {
        let given = inputs(kind);
        let (agent, facility) = (s(&given["agent"]), s(&given["facility"]));
        let mut case = Case::of(FILE, s(&given["world"]));
        let event_id = serve(&mut case, facility, resource).unwrap().unwrap();
        assert_eq!(case.hex(), started);
        case.edit(|doc| row_mut(doc, "agents", agent)["needs"][need] = json!(0.812345));
        let doc = case.doc();
        let event = doc["event_queue"].as_array().unwrap().iter().find(|event| event["event_id"] == *event_id).unwrap().clone();
        complete(&mut case, &event_id).unwrap();
        assert_eq!(case.hex(), completed);
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
    assert_eq!(case.hex(), "814ce54aa76fce5c77e6818ada19e7488eb8072ac8511bb262650e6d23606958");
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
