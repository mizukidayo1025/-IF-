//! The branches of `advance_movement` that depend on the walk's order, on the generated Scenario's
//! world (Issue #225, lane M9): the tests of `tests/integration/test_movement_walk_order.py`, moved
//! here.
//!
//! The gate and the 7-day run never reach them (docs/rust-port/lanes/l3-movement.md), so these tests
//! are all that checks them: a queue trip that replans, and a person placed in a slot in the walk.
//! Each world is the pytest's just before its first call, read from
//! `tests/data/movement_walk_order.json` (`tests/support/core_fixtures_movement.py` writes it): the
//! generated Scenario (or the one with split queues, `with_split_queues`) with people sent to the
//! toilet's queue by its service (`ToiletServiceManager`), their trips edited, and the toilet moved
//! (or a wall put on a walk) as the pytest did. A test calls the core's walk through a host that
//! follows the world (`rules::movement::advance_through`, the kernel `advance_movement` calls;
//! `support/steps.rs`), and edits the world between two calls as the pytest did.
//!
//! The pytest chained each call's answer, the result and the whole world after it, into a digest of
//! Python's `repr` and compared it with the deleted Python rule's (`ANSWERS`, recorded at 2d8fb46).
//! Those digests are recorded again here from the core (decision 1 of #225): `Answers` chains each
//! call's result, the order of the movement state's maps (the pytest kept it) and the whole world
//! after it (`support/answers.rs`). Each was recorded from the core in #225 lane M9; on the same
//! kernel the pytest of the same name passed with the digest whose first 12 hex digits its doc
//! comment gives.

mod support;

use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};
use sheltersim_core::error::Error;
use sheltersim_core::rules::movement::advance_through;
use sheltersim_core::state::movement::MovementRow;
use sheltersim_core::world::WorldCell;

use support::cases;
use support::steps::{row, Case};

const FILE: &str = "movement_walk_order";
const MOVEMENT: &str = "movement_v1";
const OVERLAY: &str = "geometry_overlay_v1";
const TOILET: &str = "toilet_01";
const QUEUE_REPLANS: &str = "physical_queue_replan_count";
const QUEUE_BLOCKED: &str = "physical_queue_replan_blocked_count";
/// The seeds of the random walks.
const RANDOM_WALKS: usize = 30;

fn data() -> &'static Value {
    static DATA: OnceLock<Value> = OnceLock::new();
    DATA.get_or_init(|| support::load::data(FILE))
}

/// The world of a test, with the host that follows it, and what the test read before its call.
fn setup(name: &str) -> (Case, Value) {
    let test = data()["inputs"].get(name).unwrap_or_else(|| panic!("{FILE} has no test {name}")).clone();
    let entry = cases::resolve(&data()["worlds"], test["world"].as_str().unwrap());
    (Case::new(entry), test)
}

/// The order of the movement state's maps: positions, journeys and the edges' minutes.
fn orders(case: &Case) -> Vec<Vec<String>> {
    case.loaded
        .cell
        .with(|world| {
            let row = world.row::<MovementRow>(MOVEMENT).unwrap();
            vec![
                row.positions.keys().map(|id| id.to_string()).collect(),
                row.journeys.keys().map(|id| id.to_string()).collect(),
                row.edge_usage_minutes.keys().map(|id| id.to_string()).collect(),
            ]
        })
        .unwrap()
}

/// `advance_movement` at `minute`, its answer added to the case's.
fn walk(case: &mut Case, minute: i64) -> Result<Vec<Arc<str>>, Error> {
    let loaded = std::rc::Rc::clone(&case.loaded);
    let result = advance_through(&loaded.cell, &mut case.host, minute);
    let orders = orders(case);
    case.answered(&(&result, orders));
    result
}

/// The walk at the world's minute, which returns nobody to replan.
fn walked(case: &mut Case) {
    let minute = case.minute();
    assert_eq!(walk(case, minute), Ok(Vec::new()));
}

fn check(case: &Case, digest: &str, name: &str) {
    assert_eq!(case.hex(), digest, "{name}");
}

fn journeys(doc: &Value) -> &Value {
    &doc["model_state"][MOVEMENT]["journeys"]
}

fn walking(doc: &Value) -> Vec<&str> {
    journeys(doc).as_object().unwrap().keys().map(String::as_str).collect()
}

fn metric(doc: &Value, name: &str) -> Option<f64> {
    doc["metric_state"].get(name).map(|value| value.as_f64().unwrap())
}

fn number(event_id: &Value) -> i64 {
    event_id.as_str().unwrap().rsplit('-').next().unwrap().parse().unwrap()
}

/// The world's minute set, as the pytest set `world.minute_index` between two calls.
fn at_minute(case: &mut Case, minute: i64) {
    case.edit(|doc| doc["minute_index"] = json!(minute));
}

// ---- A queue trip that replans -------------------------------------------------------------------

/// Pytest digest `ef386ff9e302`.
#[test]
fn two_replans_take_their_event_numbers_in_the_order_of_the_walk() {
    let name = "two_replans_take_their_event_numbers_in_the_order_of_the_walk";
    let (mut case, test) = setup(name);
    walked(&mut case);
    check(&case, "c48b249d0ed892a3ec35c9ed487661422daa5edc29e56d24072e71b0477f7fd3", name);
    let doc = case.doc();
    let (first, second) = (number(&journeys(&doc)["A001"]["event_id"]), number(&journeys(&doc)["A002"]["event_id"]));
    assert_eq!(first + 1, second);
    assert_eq!(metric(&doc, QUEUE_REPLANS), Some(2.0));
    assert_eq!(metric(&doc, "movement_replan_count"), Some(2.0));
    assert_eq!(orders(&case)[1], ["A001", "A002"]);
    let suppressed: Vec<&Value> =
        doc["event_log"].as_array().unwrap().iter().filter(|record| record["status"] == "suppressed").map(|record| &record["event"]["event_id"]).collect();
    assert_eq!(suppressed, [&test["old"]["A001"], &test["old"]["A002"]]);
}

/// Pytest digest `fd6ba4178a71`.
#[test]
fn a_replan_for_a_wall_on_the_walk() {
    let name = "a_replan_for_a_wall_on_the_walk";
    let (mut case, _) = setup(name);
    let before = journeys(&case.doc())["A002"].clone();
    walked(&mut case);
    check(&case, "bb1f119e63ca770769942a8986d53c3c2741a25aa9fee6b5ce16a34118a77523", name);
    let doc = case.doc();
    let after = &journeys(&doc)["A002"];
    assert_ne!(after["event_id"], before["event_id"]);
    assert_ne!(after["route_points"], before["route_points"]);
}

/// Pytest digest `b7315fc57f75`.
#[test]
fn a_replan_for_a_facility_that_changed_zone() {
    let name = "a_replan_for_a_facility_that_changed_zone";
    let (mut case, _) = setup(name);
    walked(&mut case);
    check(&case, "31050304717c9ae706d55f3c730a260f02946ffd5875366245f9ca2e2b0f7b2d", name);
}

/// Pytest digest `964f0525338b`.
#[test]
fn a_replan_with_no_room_route_is_blocked_and_counted() {
    let name = "a_replan_with_no_room_route_is_blocked_and_counted";
    let (mut case, _) = setup(name);
    walked(&mut case);
    check(&case, "45f21a6fa25f83301d4f235dffb4150368780c4ced5c9ea19fb94fdeef119133", name);
    let doc = case.doc();
    assert!(walking(&doc).is_empty());
    assert_eq!(metric(&doc, QUEUE_BLOCKED), Some(2.0));
    assert_eq!(metric(&doc, "movement_replan_blocked_count"), Some(2.0));
}

/// Pytest digest `c63af15a6ee5`.
#[test]
fn a_replan_past_the_end_of_the_run_is_blocked_in_its_schedule() {
    let name = "a_replan_past_the_end_of_the_run_is_blocked_in_its_schedule";
    let (mut case, _) = setup(name);
    walked(&mut case);
    check(&case, "0e720034285ebd56a0bb0ed22e61e0dc861e02be81258a5a7b6d4afb4f73abd5", name);
    let doc = case.doc();
    assert_eq!(metric(&doc, "physical_queue_route_blocked_count"), Some(2.0));
    // The replan's own count goes up too: the Python rule counted it when the schedule gave up.
    assert_eq!(metric(&doc, QUEUE_REPLANS), Some(2.0));
}

/// Pytest digest `d7a46d640910`.
#[test]
fn a_replan_to_where_the_person_stands_takes_no_trip() {
    let name = "a_replan_to_where_the_person_stands_takes_no_trip";
    let (mut case, _) = setup(name);
    walked(&mut case);
    check(&case, "3bb5690b4839f66883878f91c5a8572db4f9d566afd66f0a7add22093e469bc9", name);
    let doc = case.doc();
    assert!(walking(&doc).is_empty());
    assert_eq!(metric(&doc, QUEUE_REPLANS), Some(2.0));
}

/// Pytest digest `ff6a37ca560d`.
#[test]
fn someone_on_their_way_on_a_queue_trip_is_queued_by_the_replan() {
    let name = "someone_on_their_way_on_a_queue_trip_is_queued_by_the_replan";
    let (mut case, _) = setup(name);
    walked(&mut case);
    check(&case, "687132709f3cf68b3269151f0f18fdb4abf53465682711fe2d4d9346482da408", name);
    let doc = case.doc();
    let agent = row(&doc, "agents", "A001");
    assert_eq!((&agent["activity"], &agent["target_id"]), (&json!("queued"), &json!(TOILET)));
}

/// Pytest digest `f1ea36b5ff32`.
#[test]
fn someone_on_their_way_whose_replan_is_blocked_stands_still() {
    let name = "someone_on_their_way_whose_replan_is_blocked_stands_still";
    let (mut case, _) = setup(name);
    walked(&mut case);
    check(&case, "db2dffb36172a99ad750dac9601c5850ed70ef9af99983b88be381f7cd65b9eb", name);
    let doc = case.doc();
    assert_eq!(row(&doc, "agents", "A001")["activity"], "moving");
    assert!(!walking(&doc).contains(&"A001"));
}

/// Pytest digest `e38534325228`.
#[test]
fn a_replan_when_the_facility_is_gone_only_ends_the_trip() {
    let name = "a_replan_when_the_facility_is_gone_only_ends_the_trip";
    let (mut case, _) = setup(name);
    walked(&mut case);
    check(&case, "1ede9081f6b32942b0560b69874ac0161a2b9e2f194909b02d350d168fd4d231", name);
    let doc = case.doc();
    assert!(walking(&doc).is_empty());
    assert_eq!(metric(&doc, "movement_replan_count"), None);
}

/// Pytest digest `ff6a37ca560d`.
#[test]
fn a_replan_of_the_facility_s_own_queue() {
    let name = "a_replan_of_the_facility_s_own_queue";
    let (mut case, _) = setup(name);
    let before = journeys(&case.doc())["A001"]["event_id"].clone();
    walked(&mut case);
    check(&case, "687132709f3cf68b3269151f0f18fdb4abf53465682711fe2d4d9346482da408", name);
    assert_ne!(journeys(&case.doc())["A001"]["event_id"], before);
}

/// Whoever is ahead in the queue, walked already or not when the follower replans. Pytest digests
/// `a7b6df0115fe` (A001 ahead) and `93004fcf3e62` (A002 ahead).
#[test]
fn a_slot_read_with_the_one_ahead_before_or_after_their_walk() {
    for (index, digest) in [(0, "b3b696eb15def86d8d40aee4619073d010edb06afc514659d0fdf55864bfe669"), (1, "cdd53c952ebbff51704ebc30efcdb4b71da74148fb4d2febb223beb8357d899c")] {
        let name = format!("a_slot_read_with_the_one_ahead_before_or_after_their_walk[people{index}]");
        let (mut case, _) = setup(&name);
        walked(&mut case);
        check(&case, digest, &name);
    }
}

/// The dependence with a coordinate in it: the follower takes the slot the head walked off. Pytest
/// digest `79d8fef9bb8a`.
#[test]
fn the_one_ahead_moving_changes_the_follower_s_destination() {
    let name = "the_one_ahead_moving_changes_the_follower_s_destination";
    let (mut case, test) = setup(name);
    let slot = &test["slot"];
    assert_eq!(journeys(&case.doc())["A002"]["route_points"].as_array().unwrap().last(), Some(slot));
    walked(&mut case);
    check(&case, "c2203464eac9e62f52e77fa73388199c2c784e2ded6a8a49bfbaacbc5e32ec09", name);
    let doc = case.doc();
    assert_eq!(walking(&doc), ["A001", "A002"]);
    assert_ne!(journeys(&doc)["A002"]["route_points"].as_array().unwrap().last(), Some(slot));
}

/// A001 replans and so stands elsewhere; A002, later in the walk, is placed in a slot. Pytest
/// digest `6eb1ed14a325`.
#[test]
fn a_replan_seen_by_the_slot_of_someone_later_in_the_walk() {
    let name = "a_replan_seen_by_the_slot_of_someone_later_in_the_walk";
    let (mut case, _) = setup(name);
    walked(&mut case);
    check(&case, "52b8d31826a157fb2db41b1f8a2906b4a89d3a63e66ef5b40ff8d3db590534a5", name);
    let doc = case.doc();
    assert!(!walking(&doc).contains(&"A002"));
    assert_eq!(doc["model_state"][MOVEMENT]["positions"]["A002"]["position_kind"], "physical");
}

// ---- A person placed in a slot during the walk ----------------------------------------------

/// Each is queued with a trip whose task went: the walk places them, one after another. Pytest
/// digests `660dd9e12fd0` (A001, A002, A003) and `85b36c90eab3` (A003, A002, A001).
#[test]
fn people_placed_in_slots_read_who_has_walked() {
    for (index, digest) in [(0, "d01705fa5bb17119e59d904498bfe6af141ef5a186608d3db7fcd8416e960fc8"), (1, "10dc3256d272330a0a68df42718866ed6a5ba55b1ee1173f706238dd778d3c8c")] {
        let name = format!("people_placed_in_slots_read_who_has_walked[queue_order{index}]");
        let (mut case, _) = setup(&name);
        walked(&mut case);
        check(&case, digest, &name);
        assert!(walking(&case.doc()).is_empty());
    }
}

/// The completion was applied: `finish_movement` of a queue trip, in the walk. Pytest digest
/// `c1e3a73b53bb`.
#[test]
fn a_queued_person_past_their_trip_keeps_their_arrival() {
    let name = "a_queued_person_past_their_trip_keeps_their_arrival";
    let (mut case, _) = setup(name);
    walked(&mut case);
    check(&case, "3abd8b311d715087d0a97b6aafcf51668f423dd683e412b4681ad3df68084de6", name);
    assert!(!walking(&case.doc()).contains(&"A001"));
}

// ---- Walks that replan twice in a tick -----------------------------------------------------------

/// Pytest digest `88d59727ec86`.
#[test]
fn one_person_replans_in_two_ticks_running() {
    let name = "one_person_replans_in_two_ticks_running";
    let (mut case, _) = setup(name);
    walked(&mut case);
    let first = journeys(&case.doc())["A001"].clone();
    at_minute(&mut case, 2);
    case.edit(|doc| doc["model_state"][OVERLAY]["facility_points"][TOILET]["minute"] = json!(7));
    walked(&mut case);
    let second = journeys(&case.doc())["A001"].clone();
    assert_ne!(first["event_id"], second["event_id"]);
    // And a minute already done does nothing, whatever it would replan.
    walked(&mut case);
    check(&case, "984ed71edcdcda4bcb28797037776db0e1fd4d2bf53e29441a54be148cd47d96", name);
}

/// One replans in the walk and takes an event; the other is returned to the controller. Pytest
/// digest `dbd027987b78`.
#[test]
fn a_queue_replan_and_a_walker_who_must_replan_in_one_walk() {
    let name = "a_queue_replan_and_a_walker_who_must_replan_in_one_walk";
    let (mut case, _) = setup(name);
    assert_eq!(walk(&mut case, 1), Ok(vec![Arc::from("A003")]));
    check(&case, "6ee40efa9585212cd9ca8cd975952fc362da1ace744468e8fab36b5ebf85cfac", name);
    assert_eq!(metric(&case.doc(), QUEUE_REPLANS), Some(2.0));
}

// ---- Refusals ------------------------------------------------------------------------------------

/// The deleted rule's ValueError, which the kernel raises before it writes anything.
#[test]
fn a_trip_with_no_queue_task_is_python_s_error_before_any_change() {
    let (mut case, _) = setup("a_trip_with_no_queue_task_is_python_s_error_before_any_change");
    let before = case.bytes();
    let loaded = std::rc::Rc::clone(&case.loaded);
    let refused = advance_through(&loaded.cell, &mut case.host, 1);
    assert_eq!(refused, Err(Error::Value("physical queue reservation has no matching active queue task".to_owned())));
    assert_eq!(case.bytes(), before);
}

// ---- At random -----------------------------------------------------------------------------------

/// A random walk: everyone in the facility's own queue, in an order drawn at random, each with a
/// trip of a kind drawn at random (some of them stale, or of a task that went), the toilet moved a
/// little and, half the time, a block put in its area; the walk then runs minutes 1, 2 and 3.
fn random_walk(seed: usize) -> Case {
    let (mut case, _) = setup(&format!("random_walks_are_python_s_bit_for_bit[{seed}]"));
    for minute in 1..=3 {
        at_minute(&mut case, minute);
        walk(&mut case, minute).unwrap();
    }
    case
}

/// The digests of the random walks, by seed (the pytest drew 40 and compared each with the deleted
/// Python rule's; the first 30 are kept here, to keep the fixture small). On the same kernel the
/// pytest passed for every seed.
const RANDOM_ANSWERS: [&str; RANDOM_WALKS] = [
    "04390f70681132a849f9b61ddbf91cf4cb2ef06b3dd88060c02521816f39a0de",
    "8aca516234af28f8c91abcb5d7edacf70cf59aebcde618ed56543027ba9ce5ae",
    "89c5eb15a2cf4b1aec22bb44fc5e4036d1c02b4d5f90881150aae4d545823777",
    "3f2dd762adac882933bc503ffb6c87ce486dd1ea08fd838f3950016752517d97",
    "d787b30fb78418798aa89df921d22567b6d42439bd8eeaa6eadbce39111663fc",
    "7d0b8da5e6e4329e8a7bdd4052955865ec5d00b8d769f2fc89fad762d74d689a",
    "69764441acaca6160abd7c69baefe7e2130638956de6567579a190d576069c2d",
    "f9940ff753b1fec4e8504486e215730330d114587677f9a77ab4f68557f8b93e",
    "2418a3460d72cf1181636288a1c72f6945cf4b8b7f3647db6ae1c25771bf354b",
    "7cf9150fda81b86870da8bb4aff3166d7edacfcbfa49eae83dc5c77245f71762",
    "74d8ba842e76a2d96bc4dac33146157ec06265f952e2e19a14bbd1250897edd4",
    "1edf2a760cb050dd47e5ba264c0aeead41a326e79899325b7a4745fad9af4262",
    "5aab298af502fa869c7ac6fd6b2bc2cc641d36c362257d7328b92ed27e9ee925",
    "5970a41bedd9557194e743b8e7b0d3bd28744dc462d6737ddbb0649cb6d624d9",
    "f0c7d6d4d165c6ec2e2b1dfed84dd600afca83d6c3a2158f492b8c3aff528c91",
    "d2dfbbc7e05e11d9d9a7c81a9396b043151c18ec086be6aa207c8a1cce5eb82a",
    "9f1dd8c14596129bd4ea4034a020e5bead14f49c239cd297d12aaa8968012bf5",
    "d036728c5ab93c69e1cdb0300c3180d8cd254b92c7c4b664b117b3f73353b7d3",
    "208ff8e80fabc2f4981810861e1254722e95634f86b4801d747d3ba8add444ac",
    "7010f0dc353b8ecedf3c8ebf264c72da892e800894702a8df6641c7096183998",
    "28273f2d4134e948f2e293df775df507efadf7c216aeecef781ebf529d1907d8",
    "24432880fcad9ee3233fddf14ad9263eca2f18d464cb7d2ba81c3e3c3a9871ef",
    "584f2a53d1a0301650a5c1552cc2a85dfb17ae2d1a72da6248b242de24adfe3b",
    "c8c904b3302cfd1a02b97aae60f8871b08d6d3c72fc5b3664e9e092d8044b04e",
    "cc08150dc26916e3c23d834f046fde475809cbc08528043b9fb7d5585daffffa",
    "9054ec7081ef6612ef28d22072fa3d6338eb8d1398897e8c8fcc5a351605a0df",
    "9e605bee2976de2beb31fb2586c165cd297687072ddbc1df7145cdd4542a9c3f",
    "bbca75b286ff19ce81886e482f4251472ec95d10b86caaa7ba9b65e1b8627ecd",
    "f063547931db7f49847293e8168a4dbe7b3a6b89bfbcff6dac695d2f3a9faf5b",
    "865bcbe3ada73a006037b6f99b5db8310d8c4a259d5b82fbfe5fe4a3c12e6970",
];

#[test]
fn random_walks_are_the_recorded_ones() {
    let found: Vec<String> = (0..RANDOM_WALKS).map(|seed| random_walk(seed).hex()).collect();
    assert_eq!(found, RANDOM_ANSWERS);
}

/// Or the test above compares little: replans of each ending, and people placed in slots.
#[test]
fn the_random_walks_reach_the_branches() {
    let mut replans = 0.0;
    let mut blocked = 0.0;
    for seed in 0..12 {
        let doc = random_walk(seed).doc();
        replans += metric(&doc, QUEUE_REPLANS).unwrap_or(0.0);
        blocked += metric(&doc, QUEUE_BLOCKED).unwrap_or(0.0);
    }
    assert!(replans > 0.0);
    assert!(blocked > 0.0);
}
