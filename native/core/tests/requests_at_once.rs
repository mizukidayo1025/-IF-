//! What `_apply_effect` does with no trip to make, and the checks of having arrived (Issue #225,
//! lane M6; moved from `tests/integration/test_requests_at_once.py`, with the request at once of
//! `test_resource_service_texts.py` its queue turns down).
//!
//! A person who asks a queue at once and a request closed at once, on a world with a floor plan and
//! on one without, through the core's step of the propose phase (`propose::apply_alone`); and
//! `at_queue_slot`, `at_service_point` and `near_point` of `rules::movement`. The worlds are the
//! generated Scenario at minute 0 of `tests/data/generated_texts.json` and its variants, and the
//! Scenario without a floor plan of `tests/data/staff_assignment.json`
//! (`tests/support/core_fixtures_service.py`); each was exported with the controller the pytest made
//! after its edits.
//!
//! The pytest compared the world with the `world_digest` of what the deleted Python rule left,
//! written with Python's `repr` of the events; the digests here are of each step's answer and the
//! world's canonical parts after it (`steps::Case::answered`, `support/answers.rs`), recorded from
//! the core in #225 lane M6 (decision 1): on the same kernel each pytest named in a test's doc
//! comment passed with the digests it had, and the PyO3 entry left the same canonical text.

mod support;

use serde_json::{json, Value};
use sheltersim_core::config::movement::Movement;
use sheltersim_core::config::SectionId;
use sheltersim_core::geometry::Pt;
use sheltersim_core::host::Host;
use sheltersim_core::rules::kit::Held;
use sheltersim_core::rules::movement::{self, Where};
use sheltersim_core::rules::proposals::Action;
use sheltersim_core::world::WorldCell;

use support::steps::{self, applied, row, s, Case};

const GENERATED: &str = "generated_texts";
const PLAIN: &str = "staff_assignment";

fn inputs(name: &str) -> Value {
    steps::inputs(GENERATED, name)
}

fn metric(doc: &Value, name: &str) -> f64 {
    doc["metric_state"].get(name).and_then(Value::as_f64).unwrap_or(0.0)
}

fn listed(doc: &Value, table: &str, id: &str, field: &str, agent: &str) -> bool {
    row(doc, table, id)[field].as_array().unwrap().contains(&json!(agent))
}

// ---- A queue at once ------------------------------------------------------------------------------

/// `test_a_toilet_at_an_independent_queue` (pytest digest `c5ae5dfd75fc5f19`).
#[test]
fn a_toilet_at_an_independent_queue() {
    let mut case = Case::of(GENERATED, s(&inputs("toilet_at_an_independent_queue")["world"]));
    assert_eq!(case.apply(Action::RequestToilet, "A001", None).unwrap(), applied(Some("accessible_toilet_01"), 7, true));
    let doc = case.doc();
    assert_eq!(row(&doc, "facilities", "accessible_toilet_01")["zone_id"], row(&doc, "facilities", "toilet_01")["zone_id"]);
    assert_eq!(row(&doc, "agents", "A001")["activity"], json!("queued"));
    assert!(listed(&doc, "facilities", "accessible_toilet_01", "queue_agent_ids", "A001"));
    assert_eq!(case.hex(), "e82ebd9608f89764802c704540cd1f285d32877a98f486eae3c4b4e8ce45a8fe");
}

/// `test_a_service_at_an_independent_queue` (pytest digest `2ef942f400f86948`).
#[test]
fn a_service_at_an_independent_queue() {
    let given = inputs("service_at_an_independent_queue");
    let facility = s(&given["facility"]);
    let mut case = Case::of(GENERATED, s(&given["world"]));
    assert_eq!(case.apply(Action::FetchWater, "A001", None).unwrap(), applied(Some(facility), 7, true));
    assert!(listed(&case.doc(), "facilities", facility, "resource_queue_agent_ids", "A001"));
    assert_eq!(case.hex(), "0acca3262ab0ba5249c09de37979ebbb34f8b72db1448eab6593126c8a64e5ca");
}

/// `test_someone_at_the_point_of_the_facility_asks_at_once` (pytest digests `438170f6746a69b5`,
/// `12cae6962fa17612`, `0aee98f71212d596`): a floor plan without service points or queues of its
/// own, and the person standing at the facility's point.
#[test]
fn someone_at_the_point_of_the_facility_asks_at_once() {
    let given = inputs("at_the_point_asks_at_once");
    for (action, word, digest) in [(Action::RequestToilet, "toilet", "d47a9e285a0f8ca07a6f1469c211772bbfc5933ad5a0efd12a54ed07417f80e6"), (Action::FetchWater, "water", "4e1ee0dedc334dfa7636b4b365aa47ed76d5c2bbeb8cef322c3eee60342b2aed"), (Action::CollectMeal, "meal", "c0cc2715e0e32d5be44abb9a32ac8d9af0c31425a217a24dd42ad394736b7138")] {
        let facility = s(&given[word]["facility"]);
        let mut case = Case::of(GENERATED, s(&given[word]["world"]));
        assert_eq!(case.apply(action, "A001", None).unwrap(), applied(Some(facility), 7, true), "{word}");
        assert_eq!(row(&case.doc(), "agents", "A001")["activity"], json!("queued"));
        assert_eq!(case.hex(), digest);
    }
}

/// `test_without_a_floor_plan_someone_in_the_zone_asks_at_once` (pytest digest
/// `510ba205709d5c99`): the Scenario has toilets only, so the water and the meal were skipped.
#[test]
fn without_a_floor_plan_someone_in_the_zone_asks_at_once() {
    let given = steps::inputs(PLAIN, "plain_at_toilet");
    let facility = s(&given["facility"]);
    let mut case = Case::of(PLAIN, s(&given["world"]));
    assert_eq!(case.apply(Action::RequestToilet, s(&given["agent"]), None).unwrap(), applied(Some(facility), 7, true));
    assert_eq!(case.hex(), "d934382a1c2c7c015ee6b3c4568df1918096b97d21f403b3263599c1d07891a8");
}

/// `test_a_person_the_service_turns_down_is_recorded_with_python_s_text` (pytest digest
/// `cca2a54dd1d43e3c`).
#[test]
fn a_person_the_service_turns_down_is_recorded_with_python_s_text() {
    let mut case = Case::of(GENERATED, s(&inputs("service_turns_down")["world"]));
    assert_eq!(case.apply(Action::FetchWater, "A001", None).unwrap(), applied(None, 7, false));
    let doc = case.doc();
    let record = doc["model_state"]["resource_service_v1"]["request_history"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(record["reason"], json!("request_rejected"));
    assert_eq!(record["detail"], json!("Agent 'A001' cannot request resource service while active"));
    assert_eq!(case.hex(), "b790e8745919c178b430652666b0d4e5aba4944ffe2cd84ad53b0e7635e5ceef");
}

/// `test_a_request_at_once_the_queue_turns_down_keeps_python_s_text` of
/// test_resource_service_texts.py (pytest digests `queue` `3a9569d6ad20ffc0`, `active`
/// `4a025222d590125b`, `exclusive` `8074303070235945`): the propose step records the ValueError's
/// text as the request's detail, and counts the rejection.
#[test]
fn a_request_at_once_the_queue_turns_down_keeps_python_s_text() {
    let given = inputs("queue_turns_down");
    for (why, text, digest) in [
        ("queue", "no accessible independent resource service queue is available", "6273ef3f8a937cd3282634ed0677fbe8c846712ce53e14ea393430846ab7dbe2"),
        ("active", "Agent 'A001' cannot request resource service while active", "cb34fca4b367a7f3c9ee5d2591c6aaeb1417f5dea0cdcabc6e52ba79c21bea49"),
        ("exclusive", "Agent 'A001' already has an exclusive task", "20903a3acfac4339784cd18591225719e3380aaade971008edd283a78aded7f3"),
    ] {
        let mut case = Case::of(GENERATED, s(&given[why]));
        assert_eq!(case.apply(Action::FetchWater, "A001", None).unwrap(), applied(None, 7, false), "{why}");
        let doc = case.doc();
        let record = doc["model_state"]["resource_service_v1"]["request_history"].as_array().unwrap().last().unwrap().clone();
        assert_eq!((&record["outcome"], &record["reason"], &record["detail"]), (&json!("rejected"), &json!("request_rejected"), &json!(text)), "{why}");
        assert_eq!(metric(&doc, "resource_service_request_rejected_count"), 1.0);
        assert_eq!(case.hex(), digest);
    }
}

/// `test_a_service_action_with_no_service_is_rejected` (pytest digest `01b3b5c39ad4a7a1`): the
/// controller's `resource_service` is None.
#[test]
fn a_service_action_with_no_service_is_rejected() {
    let mut case = Case::of(GENERATED, s(&inputs("no_service")["world"]));
    assert_eq!(case.apply(Action::FetchWater, "A001", None).unwrap(), applied(None, 7, false));
    assert_eq!(metric(&case.doc(), "resource_service_request_rejected_count"), 1.0);
    assert_eq!(case.hex(), "fc2d35e6f3f32be55728bda4cd028daf41a005fa7e2490835190d91d73b92c3b");
}

/// `test_a_capacity_wait_comes_before_a_request_at_once` (pytest digest `2cc71f44ef0c8a6b`):
/// choosing a toilet routes to each one the person knows; a full passage on the way to one records
/// its wait, then the person asks the queue of another at once. The wait comes first (sequence 1,
/// after the trip's 0).
#[test]
fn a_capacity_wait_comes_before_a_request_at_once() {
    let mut case = Case::of(PLAIN, s(&steps::inputs(PLAIN, "capacity_wait")["world"]));
    // Three queue at once, and the fourth walks to the toilet left empty, through the passage.
    for (agent, toilet) in [("A003", "toilet_1"), ("A004", "toilet_2"), ("A005", "toilet_3"), ("A002", "toilet_4")] {
        assert_eq!(case.apply(Action::RequestToilet, agent, None).unwrap(), applied(Some(toilet), 7, true), "{agent}");
    }
    assert_eq!(case.apply(Action::RequestToilet, "A001", None).unwrap(), applied(Some("toilet_1"), 7, true));
    let doc = case.doc();
    let waits: Vec<(Value, Value, Value)> = doc["event_log"]
        .as_array()
        .unwrap()
        .iter()
        .map(|record| &record["event"])
        .filter(|event| event["event_type"] == "zone_edge_capacity_wait")
        .map(|event| (event["event_id"].clone(), event["sequence"].clone(), event["payload"]["edge_id"].clone()))
        .collect();
    assert_eq!(waits, [(json!("zone-edge-capacity-wait-1"), json!(1), json!("toilet_corridor"))]);
    assert_eq!(doc["id_counters"], json!({"agent": 100, "event": 2}));
    assert_eq!(case.hex(), "d20abfaee9811d4af94caca6cc0dc724cdb544298fad64cde7cf3ec00b9204da");
}

// ---- A request closed at once ---------------------------------------------------------------------

/// `test_a_request_of_the_last_minute_is_closed_at_once` (pytest digests `bad73c778047a79d`,
/// `531955270a9a0d1f`): `seek_help` records two care receipts, asked and interrupted.
#[test]
fn a_request_of_the_last_minute_is_closed_at_once() {
    let name = s(&inputs("last_minute")["world"]).to_owned();
    for (action, receipts, digest) in [(Action::SeekHelp, 2, "b21b50cb27a2820e89da7b87983e8a346aeab96ea17bb826b6ac24789569044a"), (Action::SeekInformation, 0, "5ddccba0535486c9afe7eb8df00e50575baa0c73b6f90f5cb3501ab1dba12f42")] {
        let mut case = Case::of(GENERATED, &name);
        let before = case.doc();
        let events = before["id_counters"].get("event").and_then(Value::as_i64).unwrap_or(0);
        let request = "support-dc1da28153b6ded2fec8874d";
        assert_eq!(case.apply(action, "A001", None).unwrap(), applied(Some(request), 366, false), "{action:?}");
        let doc = case.doc();
        let opened = row(&doc, "support_requests", request);
        let person = row(&doc, "agents", "A001");
        assert_eq!(opened["status"], json!("unresolved"));
        assert_eq!(opened["resolved_minute"], json!(359));
        assert_eq!(person["support_cooldown_until"], opened["cooldown_until"]);
        assert_eq!(person["support_request_id"], Value::Null);
        assert_eq!(person["activity"], row(&before, "agents", "A001")["activity"]);
        assert_eq!(metric(&doc, "support_unresolved_count"), 1.0);
        assert!(!doc["event_queue"].as_array().unwrap().iter().any(|event| event["event_type"] == "support_request_timeout"));
        assert_eq!(doc["id_counters"].get("event").and_then(Value::as_i64).unwrap_or(0), events + receipts, "{action:?}");
        assert_eq!(case.hex(), digest);
    }
}

/// `test_a_person_in_a_cooldown_is_turned_down_in_the_last_minute_too` (pytest digest
/// `e3dfec0b66e237b2`).
#[test]
fn a_person_in_a_cooldown_is_turned_down_in_the_last_minute_too() {
    let mut case = Case::of(GENERATED, s(&inputs("last_minute")["cooldown"]));
    assert_eq!(case.apply(Action::SeekHelp, "A001", None).unwrap(), applied(None, 366, false));
    assert_eq!(metric(&case.doc(), "support_request_rejected_count"), 1.0);
    assert_eq!(case.hex(), "3b8d61d7557fa529cb3e5d761d67f8e23e74a75d95843d4e75acfb09d62554cf");
}

// ---- Having arrived -------------------------------------------------------------------------------

/// `agent_is_at_queue_slot` and `agent_is_at_facility_service_point` of a placed world, which
/// change nothing.
fn arrived(case: &mut Case, agent: &str, facility: &str, queue: Option<&str>, point: Option<&str>) -> (bool, bool) {
    let before = case.bytes();
    let answers = case.tick(|tick| {
        let holding = tick.holding().unwrap();
        let geo = tick.host.floor_plan().unwrap();
        let section = tick.value_now(&holding, SectionId::Movement, |configs| &configs.movement, Movement::parse).unwrap();
        let moving = holding.held("movement_v1", "the world holds another movement state");
        let queues = if queue.is_some() { holding.held("facility_queues_v1", "the world holds another queue state") } else { Held::Absent };
        let slot = Where { geo: &geo, movement: moving.clone(), queues };
        let at = Where { geo: &geo, movement: moving, queues: Held::Absent };
        tick.cell
            .with(|world| {
                (
                    movement::at_queue_slot(world, &slot, agent, facility, queue).unwrap(),
                    movement::at_service_point(world, &at, &section.model, agent, facility, point).unwrap(),
                )
            })
            .unwrap()
    });
    assert!(case.bytes() == before);
    answers
}

/// `test_having_arrived_at_a_slot_or_the_service_point`: three people asked the toilet's queue;
/// each is in neither place where they stood, and at the service point at the facility's point.
#[test]
fn having_arrived_at_a_slot_or_the_service_point() {
    let given = inputs("arrived_at_a_slot");
    let mut case = Case::of(GENERATED, s(&given["world"]));
    let mut answers = Vec::new();
    for agent in given["people"].as_array().unwrap() {
        let agent = s(agent);
        answers.push(arrived(&mut case, agent, "toilet_01", None, None));
        case.edit(|doc| doc["model_state"]["movement_v1"]["positions"][agent]["point"] = given["point"].clone());
        answers.push(arrived(&mut case, agent, "toilet_01", None, None));
    }
    assert_eq!(answers, [(false, false), (false, true)].repeat(3));
    let slot = case.tick(|tick| {
        let geo = tick.host.floor_plan().unwrap();
        let holding = tick.holding().unwrap();
        let at = Where { geo: &geo, movement: holding.held("movement_v1", "the world holds another movement state"), queues: Held::Absent };
        tick.cell.with(|world| movement::at_queue_slot(world, &at, "A001", "no_such_facility", None).unwrap()).unwrap()
    });
    assert!(!slot);
}

/// The core's half of `test_having_arrived_at_the_end_of_a_trip`: near the trip's last point,
/// within its tolerance (the pytest keeps the journeys the PyO3 entry reads as none).
#[test]
fn having_arrived_at_the_end_of_a_trip() {
    let mut case = Case::of(GENERATED, "generated");
    let target = Pt { x: 1.5, y: 2.5 };
    let mut answers = Vec::new();
    for point in [json!({"x": 1.5, "y": 2.5}), json!({"x": 1.5, "y": 2.5502}), json!({"x": 1.5, "y": 2.5499})] {
        case.edit(|doc| doc["model_state"]["movement_v1"]["positions"]["A001"]["point"] = point);
        answers.push(case.tick(|tick| {
            let holding = tick.holding().unwrap();
            let moving = holding.held("movement_v1", "the world holds another movement state");
            tick.cell.with(|world| movement::near_point(world, &moving, "A001", target).unwrap()).unwrap()
        }));
    }
    assert_eq!(answers, [true, false, true]);
}

/// `test_having_arrived_with_an_independent_queue`: standing where they were placed, neither in
/// the slot nor at the point.
#[test]
fn having_arrived_with_an_independent_queue() {
    let given = inputs("arrived_with_an_independent_queue");
    let mut case = Case::of(GENERATED, s(&given["world"]));
    let (queue, point) = (s(&given["queue"]), s(&given["point"]));
    assert_eq!(arrived(&mut case, "A001", "toilet_01", Some(queue), Some(point)), (false, false));
    assert!(!arrived(&mut case, "A001", "toilet_01", Some(queue), Some("no_such_point")).1);
}
