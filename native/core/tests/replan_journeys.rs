//! `_replan_journeys` on the generated Scenario's world (Issue #225, lane M9): the tests of
//! `tests/integration/test_replan_journeys.py` that check the core's step
//! (`rules::tick::update::replan_journeys`), moved here.
//!
//! A replan is rare: one call in the gate and none in the 7-day run, so these tests are what checks
//! most of its branches. Each world is the pytest's just before the call, read from
//! `tests/data/replan_journeys.json` (`tests/support/core_fixtures_movement.py` writes it): the
//! Scenario at minute 1, a walk scheduled through the controller (to the toilet, within its room, to
//! a bed of a temporary room, or to the staff for support), then an edge closed or the walk otherwise
//! spoilt as the pytest did. A test runs the step on it with the Run read of the controller the
//! pytest made, through a host that follows the world (`support/steps.rs`), and checks what the
//! pytest checked.
//!
//! The pytest compared the whole world after the step, its counters, metrics, pending events, log
//! and journeys with what the deleted Python step made, by a digest of their Python `repr`. Those
//! digests are recorded again here from the core (decision 1 of #225): `Answers` chains the step's
//! result, the order of the movement state's maps (the pytest kept the journeys' order) and the
//! whole world after it (`support/answers.rs`). Each was recorded from the core in #225 lane M9; on
//! the same kernel the pytest of the same name passed with the digest whose first 12 hex digits its
//! doc comment gives.

mod support;

use std::sync::{Arc, OnceLock};

use serde_json::Value;
use sheltersim_core::error::Error;
use sheltersim_core::rules::tick::update;
use sheltersim_core::state::movement::MovementRow;
use sheltersim_core::world::WorldCell;

use support::cases;
use support::steps::{row, Case};

const FILE: &str = "replan_journeys";
const MOVEMENT: &str = "movement_v1";
const ALTERNATE: &str = "route_sleeping_toilet_alt";
const REPLANS: &str = "movement_replan_count";
const BLOCKED: &str = "movement_replan_blocked_count";
/// The answers of `a_walk_takes_the_other_route`, which two other tests leave too.
const OTHER_ROUTE: &str = "44a03e083d23f93b0dfe357023438df7e3c512610787ca93982ccf798975268c";

fn data() -> &'static Value {
    static DATA: OnceLock<Value> = OnceLock::new();
    DATA.get_or_init(|| support::load::data(FILE))
}

/// The world of a test, with the host that follows it, and what the test gives the step besides.
fn setup(name: &str) -> (Case, Value) {
    let test = data()["inputs"].get(name).unwrap_or_else(|| panic!("{FILE} has no test {name}")).clone();
    let entry = cases::resolve(&data()["worlds"], test["world"].as_str().unwrap());
    (Case::new(entry), test)
}

/// The order of the movement state's maps: positions, journeys and the edges' minutes.
fn orders(case: &Case) -> String {
    case.loaded
        .cell
        .with(|world| {
            let row = world.row::<MovementRow>(MOVEMENT).unwrap();
            format!(
                "{:?} {:?} {:?}",
                row.positions.keys().collect::<Vec<_>>(),
                row.journeys.keys().collect::<Vec<_>>(),
                row.edge_usage_minutes.keys().collect::<Vec<_>>()
            )
        })
        .unwrap()
}

/// The step on the test's world, with the people it gives, its answer added to the case's.
fn replanned(name: &str) -> (Case, Value, Result<(), Error>) {
    let (mut case, test) = setup(name);
    let people: Vec<Arc<str>> = test["people"].as_array().unwrap().iter().map(|id| Arc::from(id.as_str().unwrap())).collect();
    let result = case.tick(|tick| update::replan_journeys(tick, &people));
    let orders = orders(&case);
    case.answered(&(&result, orders));
    (case, test, result)
}

/// The step, which must apply, and the digest of its answers.
fn applied(name: &str, digest: &str) -> (Case, Value, Value) {
    let (case, test, result) = replanned(name);
    assert_eq!(result, Ok(()), "{name}");
    assert_eq!(case.hex(), digest, "{name}");
    let doc = case.doc();
    (case, test, doc)
}

fn journeys(doc: &Value) -> &Value {
    &doc["model_state"][MOVEMENT]["journeys"]
}

fn journey<'a>(doc: &'a Value, agent: &str) -> &'a Value {
    &journeys(doc)[agent]
}

fn walking(doc: &Value, agent: &str) -> bool {
    journeys(doc).get(agent).is_some()
}

fn position<'a>(doc: &'a Value, agent: &str) -> &'a Value {
    &doc["model_state"][MOVEMENT]["positions"][agent]
}

fn metric(doc: &Value, name: &str) -> Option<f64> {
    doc["metric_state"].get(name).map(|value| value.as_f64().unwrap())
}

fn activity<'a>(doc: &'a Value, agent: &str) -> &'a str {
    row(doc, "agents", agent)["activity"].as_str().unwrap()
}

fn request<'a>(doc: &'a Value, test: &Value) -> &'a Value {
    row(doc, "support_requests", test["request"].as_str().unwrap())
}

fn status_of(doc: &Value, event_id: &Value) -> Option<String> {
    let records = doc["event_log"].as_array().unwrap();
    records.iter().find(|record| record["event"]["event_id"] == *event_id).map(|record| record["status"].as_str().unwrap().to_owned())
}

fn number(event_id: &Value) -> i64 {
    event_id.as_str().unwrap().rsplit('-').next().unwrap().parse().unwrap()
}

// ---- Walks between zones -------------------------------------------------------------------------

/// Pytest digest `1cdf31043388`.
#[test]
fn a_walk_takes_the_other_route() {
    let (_, test, doc) = applied("a_walk_takes_the_other_route", OTHER_ROUTE);
    let old = &test["old"];
    assert_eq!(journey(&doc, "A001")["route_edge_ids"], serde_json::json!([ALTERNATE]));
    assert_ne!(journey(&doc, "A001")["event_id"], old["event_id"]);
    assert_eq!(metric(&doc, REPLANS), Some(1.0));
    assert_eq!(status_of(&doc, &old["event_id"]).as_deref(), Some("suppressed"));
}

/// Pytest digest `6689c48e638a`.
#[test]
fn a_walk_with_no_route_stops_and_is_counted() {
    let (_, _, doc) = applied("a_walk_with_no_route_stops_and_is_counted", "98e83fd5434cebd96b841a55b81bffbfd8891e446894f22fd720f4c15d4c2fdb");
    assert!(!walking(&doc, "A001"));
    assert_eq!(metric(&doc, BLOCKED), Some(1.0));
    assert_eq!(activity(&doc, "A001"), "idle");
}

/// Pytest digest `62922aa33b07`.
#[test]
fn people_replan_in_the_order_given_and_take_their_event_numbers() {
    let (_, _, doc) = applied("people_replan_in_the_order_given_and_take_their_event_numbers", "cd2857b56d11aa5b7157e213982b07496a793dff469387d1a0ed8392c90bc437");
    let numbers: Vec<i64> = ["A003", "A001", "A002"].iter().map(|agent| number(&journey(&doc, agent)["event_id"])).collect();
    assert_eq!(numbers, [numbers[0], numbers[0] + 1, numbers[0] + 2]);
}

/// Pytest digest `1cdf31043388`: the world `a_walk_takes_the_other_route` leaves.
#[test]
fn someone_with_no_journey_or_gone_is_passed_over() {
    let (_, _, doc) = applied("someone_with_no_journey_or_gone_is_passed_over", OTHER_ROUTE);
    assert_eq!(metric(&doc, REPLANS), Some(1.0));
}

/// Pytest digest `8204ab4a3512`.
#[test]
fn a_walk_stopped_in_another_zone_is_where_its_position_says() {
    let (_, _, doc) = applied("a_walk_stopped_in_another_zone_is_where_its_position_says", "ef7b22ec3e81412f1a413eeb9ec2c643d1fd4dfd6b51ddcf498d1ca9491f39b3");
    assert_eq!(row(&doc, "agents", "A001")["zone_id"], "corridor");
}

/// Pytest digest `9e655475230e`.
#[test]
fn the_trip_past_the_end_of_the_run_is_not_made() {
    let (_, _, doc) = applied("the_trip_past_the_end_of_the_run_is_not_made", "ca9bc869ee79e0ec4e2fbc827130972475de3c79ae048160ea6058e15e13e736");
    assert!(!walking(&doc, "A001"));
}

/// Pytest digest `2541b8d93f8f`.
#[test]
fn a_walk_whose_event_is_gone_stops_without_a_record() {
    let (_, test, doc) = applied("a_walk_whose_event_is_gone_stops_without_a_record", "5a204e90c4d53e0887e5eb9a80acdd71ed3005908816b63e79404c8eb367d640");
    let old = &test["old"];
    assert_eq!(status_of(&doc, &old["event_id"]), None);
    assert!(doc["active_tasks"].as_array().unwrap().iter().all(|task| task["id"] != old["task_id"]));
    assert_eq!(journey(&doc, "A001")["route_edge_ids"], serde_json::json!([ALTERNATE]));
}

/// Pytest digest `eb8eb130a300`.
#[test]
fn a_walk_to_its_own_zone_has_no_route_and_is_counted() {
    let (_, _, doc) = applied("a_walk_to_its_own_zone_has_no_route_and_is_counted", "a539a5f65b5179a127c9d745e7455ebaa8728c34780dacb0dd2e807973b5995d");
    assert!(!walking(&doc, "A001"));
    assert_eq!(metric(&doc, BLOCKED), Some(1.0));
    assert_eq!(metric(&doc, REPLANS), Some(1.0));
}

/// Pytest digest `1cdf31043388`: the world `a_walk_takes_the_other_route` leaves. The new route's
/// edge has a capacity (`max_agents_in_transit`), which the Run read of the controller.
#[test]
fn edge_capacities_are_counted_on_the_new_route() {
    let (_, _, doc) = applied("edge_capacities_are_counted_on_the_new_route", OTHER_ROUTE);
    assert!(walking(&doc, "A001"));
}

// ---- Walks within a room ---------------------------------------------------------------------------

/// Pytest digest `5f4fe364be48`.
#[test]
fn a_room_walk_has_a_new_path() {
    let (_, test, doc) = applied("a_room_walk_has_a_new_path", "729dc3cb683902d8c508e7bc41e4ce9e8a7d4053a8648163f42f1070aa2eaf7a");
    assert_ne!(journey(&doc, "A001")["route_points"][0], test["old"]["route_points"][0]);
}

/// Pytest digest `5eb2ecd919c1`.
#[test]
fn a_room_walk_that_is_already_there_is_placed() {
    let (_, test, doc) = applied("a_room_walk_that_is_already_there_is_placed", "59f85f06a965ddec0573c301de9535be66d3b60a2ba29b3d13aa3c76c76a1c5a");
    assert!(!walking(&doc, "A001"));
    let points = test["old"]["route_points"].as_array().unwrap();
    assert_eq!(position(&doc, "A001")["point"], *points.last().unwrap());
}

/// Pytest digest `16f3ec3b3f8a`.
#[test]
fn a_room_walk_with_no_path_is_counted() {
    let (_, _, doc) = applied("a_room_walk_with_no_path_is_counted", "e872483b92f3befc7b4decf5d78ec59930df5d07cdf6f86cadb42254d03a5a80");
    assert_eq!(metric(&doc, BLOCKED), Some(1.0));
}

/// Pytest digest `91b6e7d95488`.
#[test]
fn without_a_floor_plan_a_room_walk_is_counted() {
    let (_, _, doc) = applied("without_a_floor_plan_a_room_walk_is_counted", "d8e6093045f7dc66c5c5d0d6e67bb586ba1a43ea4c67f3bcff595a316f430cb6");
    assert_eq!(metric(&doc, BLOCKED), Some(1.0));
}

/// Pytest digest `321b3db4a63a`.
#[test]
fn a_room_walk_past_the_end_of_the_run_is_not_made_nor_blocked() {
    let (_, _, doc) = applied("a_room_walk_past_the_end_of_the_run_is_not_made_nor_blocked", "61a41f0449ac55203ab555e896f0d7d227170a13d17971391f5f2ba675345677");
    assert!(!walking(&doc, "A001"));
    assert_eq!(metric(&doc, BLOCKED).unwrap_or(0.0), 0.0);
    assert_eq!(metric(&doc, REPLANS), Some(1.0));
}

// ---- The walk to a bed (sleep_bed) -----------------------------------------------------------------

/// Pytest digest `1a628ec8624f`.
#[test]
fn a_walk_to_a_bed_has_a_new_path() {
    let (_, test, doc) = applied("a_walk_to_a_bed_has_a_new_path", "b2f27cf3d2bd89e2276a47dd2b750372a086d89ec0afbaf6d353e7deb5b98760");
    assert_eq!(journey(&doc, "A001")["purpose"], "sleep_bed");
    assert_ne!(journey(&doc, "A001")["event_id"], test["old"]["event_id"]);
}

/// Pytest digest `039a4c631392`.
#[test]
fn a_walk_to_a_bed_that_arrived_sleeps_there() {
    let (_, _, doc) = applied("a_walk_to_a_bed_that_arrived_sleeps_there", "d2b8980094a69d9d76f306d36067c4a8186758b782d8cc494c423235998b7f7b");
    assert_eq!(activity(&doc, "A001"), "sleeping");
    assert_eq!(position(&doc, "A001")["progress"], 1.0);
}

/// Pytest digest `57ef20709add`.
#[test]
fn a_walk_to_a_bed_past_the_end_of_the_run_gives_it_up() {
    let (_, _, doc) = applied("a_walk_to_a_bed_past_the_end_of_the_run_gives_it_up", "4b5c112c83215113e1132785bcc048fbb6f0fd34264c75d35aac9aead32e7fc1");
    assert_eq!(activity(&doc, "A001"), "sleeping");
    assert!(doc["model_state"]["geometry_overlay_v1"]["bed_reservations"].get("A001").is_none());
    assert_eq!(metric(&doc, BLOCKED), Some(1.0));
}

/// Pytest digest `7ccab4a67ee1`.
#[test]
fn a_walk_to_a_room_that_went_gives_it_up() {
    let (_, _, doc) = applied("a_walk_to_a_room_that_went_gives_it_up", "e4080af382595597e437b62418ccd8a1ce1ec0a988319a1965c5a57012b9f6fb");
    assert_eq!(metric(&doc, BLOCKED), Some(1.0));
    assert_eq!(activity(&doc, "A001"), "sleeping");
}

// ---- The walk to the staff for support (staff_support_travel) --------------------------------------

/// Pytest digest `9a3489ab32e3`.
#[test]
fn support_with_another_route_is_rescheduled() {
    let (_, test, doc) = applied("support_with_another_route_is_rescheduled", "34d4c0a095559bd82e2e4c73a65cbb51f59383e4b3f6b9ae9245c6bb0330465d");
    assert_eq!(journey(&doc, "A001")["route_edge_ids"], serde_json::json!(["route_sleeping_staff_alt"]));
    assert_eq!(request(&doc, &test)["status"], "assigned");
    assert_eq!(metric(&doc, REPLANS), Some(1.0));
    let pending = doc["event_queue"].as_array().unwrap();
    assert!(pending.iter().any(|event| event["provenance"].get("rescheduled_after_route_change").is_some_and(|flag| flag != false)));
}

/// Pytest digest `b4b0409a6739`.
#[test]
fn support_where_the_staff_already_are_starts_there() {
    let (_, test, doc) = applied("support_where_the_staff_already_are_starts_there", "1faa2715af2d579400c18f489c14abc38175b4c969f66587b17e1bea6bd37538");
    assert_eq!(activity(&doc, "A001"), "seeking_help");
    assert_eq!(request(&doc, &test)["status"], "assigned");
}

/// Pytest digest `1d90584fe736`.
#[test]
fn support_with_no_route_goes_back_to_the_queue() {
    let (_, test, doc) = applied("support_with_no_route_goes_back_to_the_queue", "688f98dc9c2c983b9f87e4893fcc6b69dc35e0ce900dd835830fdb84f4dff6a1");
    assert_eq!(request(&doc, &test)["status"], "pending");
    assert_eq!(metric(&doc, BLOCKED), Some(1.0));
    assert_eq!(row(&doc, "agents", "A001")["support_request_id"], test["request"]);
}

/// Pytest digest `ab4c53aa38c8`.
#[test]
fn support_past_its_timeout_is_left_unresolved() {
    let (_, test, doc) = applied("support_past_its_timeout_is_left_unresolved", "d4e1937f78e5e3c26d28f5931a8ea229e4c3fd2728b6a7ed4461909ad12256fd");
    assert_eq!(request(&doc, &test)["status"], "unresolved");
    assert_eq!(activity(&doc, "A001"), "idle");
    assert_eq!(metric(&doc, "support_unresolved_count"), Some(1.0));
}

/// Pytest digest `f09cd2fb0aaa`.
#[test]
fn support_past_the_end_of_the_run_goes_back_to_the_queue() {
    let (_, test, doc) = applied("support_past_the_end_of_the_run_goes_back_to_the_queue", "d22fe0df9bcd55cd8bb0bb51ea7f8eee86db1200995d747c773af74e56e1c2b7");
    assert_eq!(request(&doc, &test)["status"], "pending");
}

/// Pytest digest `2ddf59bf7eb7`.
#[test]
fn support_whose_walk_a_full_zone_refuses_goes_back_to_the_queue() {
    let (_, test, doc) = applied("support_whose_walk_a_full_zone_refuses_goes_back_to_the_queue", "3e94ca626592edc849c8706f40e00766632716bcb5e0609f14f1806bcc8d723b");
    assert_eq!(request(&doc, &test)["status"], "pending");
    assert_eq!(metric(&doc, BLOCKED), Some(1.0));
    assert!(!walking(&doc, "A001"));
}

/// The Python step's error, after the stop it has written, and the world as that step left it.
fn says_inconsistent(name: &str, text: &str, digest: &str) {
    let (case, _, result) = replanned(name);
    assert_eq!(result, Err(Error::Value(text.to_owned())), "{name}");
    assert_eq!(case.hex(), digest, "{name}");
}

/// Pytest digest `11ea2f11622f`.
#[test]
fn support_that_lost_its_task_is_python_s_error() {
    says_inconsistent("support_that_lost_its_task_is_python_s_error", "replanned resident trip lost its assigned support task", "eab29bad53a194e57a128a9ee82f79ebb9ebe48d5fe5d48438c8b0a2e6ea9a70");
}

/// Pytest digest `34d27c8b92f9`.
#[test]
fn support_with_a_moved_completion_is_python_s_error() {
    says_inconsistent("support_with_a_moved_completion_is_python_s_error", "replanned resident trip has inconsistent support completion", "c25fe5ce462b6759028ef7aa42aa560d5b27c54a291f24cb80df37b9c2dad3a4");
}
