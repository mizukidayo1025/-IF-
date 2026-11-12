//! The rules of `domain/movement.py` on the generated Scenario's world (Issue #225, lane M9): the
//! tests of `tests/integration/test_movement_rules.py` that check the core's rules and that
//! `movement.rs` (hand-built worlds) does not check already, moved here.
//!
//! Each world is the pytest's just before its call, read from `tests/data/movement_rules.json`
//! (`tests/support/core_fixtures_movement.py` writes it): the generated Scenario after 5 ticks
//! through `TickEngine`, with someone put on a two-edge walk (`walker`) or the people and configs
//! edited as the pytest did. A test calls the core's rule that the kernel of the same name calls
//! (`advance_through`, `start`, `finish_through`, `movement_minutes`, `approach_route` of
//! `rules::movement`) through a host that follows the world (`support/steps.rs`), and edits the
//! world between two calls as the pytest did. The values the pytest computed in Python (the journey
//! and the first position `start_movement` writes, by the deleted rule's arithmetic; each queued
//! person's place; the deleted rule's minutes of a route) are in the fixture.
//!
//! For `advance_movement` and the walk to a footprint, the pytest chained each call's answer into a
//! digest of Python's `repr` and compared it with the deleted Python rule's (`ANSWERS`, recorded at
//! 2d8fb46, and `FOOTPRINT_WALKS`). Those digests are recorded again here from the core (decision 1
//! of #225): `Answers` chains each call's result and the whole world after it
//! (`support/answers.rs`); the walks are their `Debug` texts. Each was recorded from the core in
//! #225 lane M9; on the same kernel the pytest of the same name passed with the digest whose first
//! 12 hex digits its doc comment gives.

mod support;

use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use serde_json::{json, Value};
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::config::movement::Movement;
use sheltersim_core::config::SectionId;
use sheltersim_core::error::Error;
use sheltersim_core::geometry::{Geo, Pt};
use sheltersim_core::host::Host;
use sheltersim_core::rules::kit::{self, Held};
use sheltersim_core::rules::movement::{self as movement, Start, Where, OTHER_MOVEMENT};
use sheltersim_core::state::movement::{Journey, MovementRow, Point};
use sheltersim_core::world::WorldCell;

use support::cases;
use support::steps::Case;

const FILE: &str = "movement_rules";
const MOVEMENT: &str = "movement_v1";
const OVERLAY: &str = "geometry_overlay_v1";

fn data() -> &'static Value {
    static DATA: OnceLock<Value> = OnceLock::new();
    DATA.get_or_init(|| support::load::data(FILE))
}

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

/// The world of a test, with the host that follows it, and what the test read in Python.
fn setup(name: &str) -> (Case, Value) {
    let test = data()["inputs"].get(name).unwrap_or_else(|| panic!("{FILE} has no test {name}")).clone();
    let entry = cases::resolve(&data()["worlds"], test["world"].as_str().unwrap());
    (Case::new(entry), test)
}

/// `advance_movement` at `minute`, its answer added to the case's.
fn walk(case: &mut Case, minute: i64) -> Result<Vec<Arc<str>>, Error> {
    let loaded = Rc::clone(&case.loaded);
    let result = movement::advance_through(&loaded.cell, &mut case.host, minute);
    case.answered(&result);
    result
}

/// The movement state's row, changed by `f`, as the pytest changed the Python state.
fn change(case: &Case, f: impl FnOnce(&mut MovementRow)) {
    case.loaded.cell.with(|world| f(&mut world.row_mut::<MovementRow>(MOVEMENT).unwrap())).unwrap();
}

fn last_updated(case: &Case, minute: i64) {
    change(case, |row| row.last_updated_minute = minute);
}

fn state(doc: &Value) -> &Value {
    &doc["model_state"][MOVEMENT]
}

fn int(value: &Value) -> i64 {
    value.as_i64().unwrap()
}

fn s(value: &Value) -> &str {
    value.as_str().unwrap()
}

fn pt(value: &Value) -> Pt {
    Pt { x: value["x"].as_f64().unwrap(), y: value["y"].as_f64().unwrap() }
}

/// The floor plan of the world now, as the host reads it.
fn plan(case: &mut Case) -> Arc<Geo> {
    case.host.floor_plan().unwrap()
}

/// The movement section of the configs, as the kernels read it (`movement_of`).
fn section(case: &mut Case) -> Arc<Movement> {
    case.tick(|tick| {
        let holding = tick.holding().unwrap();
        tick.value_section(&holding, SectionId::Movement, |configs| &configs.movement, Movement::parse).unwrap().unwrap()
    })
}

// ---- advance_movement --------------------------------------------------------------------------------

/// Pytest digest `37fd3fbb3bf7`.
#[test]
fn a_walk_along_two_edges_both_ways() {
    let (mut case, test) = setup("a_walk_along_two_edges_both_ways");
    let agent = s(&test["agent"]);
    let journey = state(&case.doc())["journeys"][agent].clone();
    let (started, due) = (int(&journey["started_minute"]), int(&journey["due_minute"]));
    for minute in started..=due {
        last_updated(&case, minute - 1);
        assert_eq!(walk(&mut case, minute), Ok(Vec::new()));
    }
    assert!(!state(&case.doc())["edge_usage_minutes"].as_object().unwrap().is_empty());
    // The other way along the same edges: the polyline runs backwards.
    change(&case, |row| row.journeys.get_mut(agent).unwrap().route_zone_ids.reverse());
    last_updated(&case, started);
    walk(&mut case, started + 1).unwrap();
    assert_eq!(case.hex(), "c2c3e6d9dd47041246b9f1ed4d36b1e921bcf0c36fd4887cb0c0cd7e01c44538");
}

/// Pytest digest `bcfb106dc3be`.
#[test]
fn someone_with_no_position_gets_one_first() {
    let (mut case, test) = setup("someone_with_no_position_gets_one_first");
    let minute = case.minute();
    walk(&mut case, minute).unwrap();
    assert_eq!(case.hex(), "c984db8c74eed8558b990d76391bd65080da1315629438c38d9a7498e7df32b9");
    let doc = case.doc();
    assert!(state(&doc)["positions"].get(s(&test["agent"])).is_some());
    assert!(int(&state(&doc)["last_updated_minute"]) >= minute);
}

/// In service at a facility (and at none), asleep at a reserved bed (one past the room's beds, in a
/// room that is not there, of a bool index), and queued for nothing. Pytest digest `ced987274e0d`.
#[test]
fn in_service_and_sleeping_stand_at_their_places() {
    let (mut case, test) = setup("in_service_and_sleeping_stand_at_their_places");
    let minute = case.minute();
    walk(&mut case, minute).unwrap();
    assert_eq!(case.hex(), "f22cddc09ac7a0edf762679d2dae8db4da49143c63f6b7faa21cbd5e452a4983");
    assert_eq!(state(&case.doc())["positions"][s(&test["sleeper"])]["point"], json!({"x": 3.25, "y": 4.125}));
    // A bed that isn't two floats is refused, with nothing changed: a room's bed points are
    // Point2D's floats, so only a hand-made state has one.
    case.edit(|doc| {
        doc["model_state"][OVERLAY]["temporary_rooms"]["room"]["bed_points_m"][1] = json!({"x": 3, "y": 4});
        doc["model_state"][MOVEMENT]["last_updated_minute"] = json!(minute - 1);
    });
    let before = case.bytes();
    let loaded = Rc::clone(&case.loaded);
    assert!(matches!(movement::advance_through(&loaded.cell, &mut case.host, minute), Err(Error::Unsupported(_))));
    assert_eq!(case.bytes(), before);
}

/// The Scenario's world at minute 0 without its floor plan, a zone of which has no display point.
/// Pytest digest `34d3e98ffbbe`.
#[test]
fn without_a_floor_plan_people_have_display_points() {
    let (mut case, _) = setup("without_a_floor_plan_people_have_display_points");
    let minute = case.minute();
    walk(&mut case, minute).unwrap();
    assert_eq!(case.hex(), "9c4a77c2687d7005ca2959203310f98f454f27ca05a9f892942ff9bd88415941");
    let doc = case.doc();
    for record in state(&doc)["positions"].as_object().unwrap().values() {
        assert!(["display_only", "unavailable"].contains(&s(&record["position_kind"])), "{record}");
    }
}

/// Pytest digest `81736be1ce83`.
#[test]
fn a_room_walk_follows_its_points_and_stops_at_what_blocks_them() {
    let name = "a_room_walk_follows_its_points_and_stops_at_what_blocks_them";
    let (mut case, test) = setup(name);
    let agent = s(&test["agent"]);
    let journey = state(&case.doc())["journeys"][agent].clone();
    let (started, due) = (int(&journey["started_minute"]), int(&journey["due_minute"]));
    let mut walked = 0;
    for minute in started..=due {
        last_updated(&case, minute - 1);
        walked += usize::from(walk(&mut case, minute) == Ok(Vec::new()));
    }
    assert!(walked > 0);
    // An obstacle by the walk's second point: the configs' spatial constraints of the export that
    // has it, as the pytest set them.
    let (blocked, _) = setup(&format!("{name}/blocked"));
    case.entry["sections"] = blocked.entry["sections"].clone();
    case.edit(|doc| {
        doc["model_configs"] = blocked.entry["world"]["model_configs"].clone();
        doc["model_state"][MOVEMENT]["positions"][agent]["progress"] = json!(0.0);
        doc["model_state"][MOVEMENT]["last_updated_minute"] = json!(started - 1);
    });
    assert_eq!(walk(&mut case, started), Ok(vec![text(agent)]));
    assert_eq!(case.hex(), "9bb4fbfd793ef7da3537d2af2127cd421f7caa0bcf95a00ed91be1b40c03f807");
}

// ---- finish_movement ---------------------------------------------------------------------------------

/// `finish_movement` of a person (the kernel's: nothing without the namespace).
fn finish(case: &mut Case, agent: &str) {
    let loaded = Rc::clone(&case.loaded);
    let holding = kit::holding(&loaded.cell, &mut case.host).unwrap();
    if movement::finish_held(&holding).unwrap() {
        movement::finish_through(&loaded.cell, &mut case.host, agent).unwrap();
    }
}

/// One person in one call: nobody else moves during it, so the kernel decides it. A queued person
/// at the end of a queue trip keeps the point they walked to; one on another trip stands in the
/// slot behind those placed already, as the queue's slots placed them in Python.
#[test]
fn finishing_one_trip_may_read_the_slots() {
    let (mut case, test) = setup("finishing_one_trip_may_read_the_slots");
    for item in test["expected"].as_array().unwrap() {
        let agent = s(&item[0]);
        finish(&mut case, agent);
        let doc = case.doc();
        assert!(state(&doc)["journeys"].get(agent).is_none());
        assert_eq!(state(&doc)["positions"][agent], item[1], "{agent}");
    }
}

#[test]
fn finish_movement_of_each_kind() {
    let (mut case, test) = setup("finish_movement_of_each_kind");
    let agent = s(&test["agent"]);
    let before = state(&case.doc())["positions"][agent].clone();
    finish(&mut case, agent);
    // Still reserved in the queue: the person keeps their point, off the edge.
    let doc = case.doc();
    assert!(state(&doc)["journeys"].get(agent).is_none());
    let kept = json!({"zone_id": before["zone_id"], "point": before["point"], "edge_id": null, "progress": 0.0, "position_kind": "physical"});
    assert_eq!(state(&doc)["positions"][agent], kept);
    // No journey now: the person stands where a person stands, the centre of their zone.
    case.edit(|doc| support::steps::row_mut(doc, "agents", agent)["activity"] = json!("idle"));
    finish(&mut case, agent);
    let doc = case.doc();
    let zone = &support::steps::row(&doc, "agents", agent)["zone_id"];
    let centre = json!({"zone_id": zone, "point": test["center"], "edge_id": null, "progress": 0.0, "position_kind": "physical"});
    assert_eq!(state(&doc)["positions"][agent], centre);
    // Nobody by that name: nothing to place.
    let before = case.bytes();
    finish(&mut case, "nobody");
    assert_eq!(case.bytes(), before);
    // Without the namespace, nothing at all.
    case.edit(|doc| {
        doc["model_state"].as_object_mut().unwrap().remove(MOVEMENT);
    });
    finish(&mut case, agent);
    assert!(case.doc()["model_state"].get(MOVEMENT).is_none());
}

// ---- start_movement ----------------------------------------------------------------------------------

fn strs(value: &Value) -> Vec<Arc<str>> {
    value.as_array().unwrap().iter().map(|item| text(s(item))).collect()
}

fn opt(value: &Value, key: &str) -> Option<Arc<str>> {
    value.get(key).and_then(Value::as_str).map(text)
}

/// The journey and the first position `start_movement` writes for each kind of journey (along an
/// edge, on room points, on neither, with support, with a queue), starting at, one minute after and
/// one minute before the last update of the walk: what the pytest computed in Python by the deleted
/// rule's arithmetic. The rest of the state stays as it was.
#[test]
fn start_movement_writes_the_journey_and_the_first_position() {
    let name = "start_movement_writes_the_journey_and_the_first_position";
    let starts = setup(name).1["starts"].as_array().unwrap().clone();
    assert_eq!(starts.len(), 15);
    for item in &starts {
        let (mut case, _) = setup(name);
        let before = state(&case.doc()).clone();
        let wanted = &item["journey"];
        let (started, due) = (int(&wanted["started_minute"]), int(&wanted["due_minute"]));
        let points: Vec<Pt> = wanted["route_points"].as_array().unwrap().iter().map(pt).collect();
        let edge_ids = strs(&wanted["route_edge_ids"]);
        let journey = Journey {
            event_id: text(s(&wanted["event_id"])),
            task_id: text(s(&wanted["task_id"])),
            purpose: text(s(&wanted["purpose"])),
            facility_id: opt(wanted, "facility_id"),
            destination_zone_id: text(s(&wanted["destination_zone_id"])),
            route_zone_ids: strs(&wanted["route_zone_ids"]),
            route_edge_ids: edge_ids.clone(),
            route_points: points.iter().map(|point| Point { x: point.x, y: point.y }).collect(),
            started_minute: started,
            due_minute: due,
            queue_id: opt(wanted, "queue_id"),
            support_request_id: opt(wanted, "support_request_id"),
            support_staff_id: opt(wanted, "support_staff_id"),
        };
        let (agent, origin) = (text(s(&item["agent"])), text(s(&item["origin"])));
        let geo = plan(&mut case);
        let start = Start { agent_id: &agent, journey, origin: &origin, points: &points, edge_ids: &edge_ids, started, duration: due - started };
        case.loaded.cell.with(|world| movement::start(world, &geo, start)).unwrap().unwrap();
        let mut expected = before;
        expected["journeys"][&*agent] = wanted.clone();
        expected["positions"][&*agent] = item["position"].clone();
        assert_eq!(*state(&case.doc()), expected, "{} {}", item["kind"], item["offset"]);
    }
}

// ---- route_with_movement_time ------------------------------------------------------------------------

/// The minutes of each route (each edge of the world, a long one, and one of no edge) for people of
/// each mobility, with no movement model, one of floats and one of whole numbers, in uncrowded and
/// crowded rooms: the deleted rule's, computed in Python (the longer of the declared and the
/// surveyed walk, at the person's speed, slowed by the most crowded zone of the route).
#[test]
fn a_route_s_minutes_follow_speed_and_crowding() {
    let mut checked = 0;
    for model in 0..3 {
        for area in 0..3 {
            let name = format!("a_route_s_minutes_follow_speed_and_crowding[model{model}-area{area}]");
            let (mut case, test) = setup(&name);
            let section = section(&mut case);
            assert!(section.times_routes());
            let model = section.model.as_ref().unwrap();
            let geo = plan(&mut case);
            for item in test["minutes"].as_array().unwrap() {
                let zones: Vec<&str> = item[1].as_array().unwrap().iter().map(s).collect();
                let edges: Vec<&str> = item[2].as_array().unwrap().iter().map(s).collect();
                let declared = int(&item[3]);
                let minutes = case.loaded.cell.with(|world| movement::movement_minutes(world, &geo, model, s(&item[0]), &zones, &edges, declared)).unwrap();
                // A route of no edge keeps its minutes.
                let minutes = if edges.is_empty() { declared } else { minutes.unwrap() };
                assert_eq!(minutes, int(&item[4]), "{name} {item}");
                checked += 1;
            }
        }
    }
    assert!(checked > 0);
}

// ---- The walk to a facility's footprint --------------------------------------------------------------

/// The points around a footprint, for each mobility: its clearances are in Rust too. For each
/// facility of the floor plan, by ID, a square footprint around its point in the spatial
/// constraints, and three people of each mobility at the centre of its zone. The pytest took the
/// Scenario's world at minute 2; here it is the world at minute 5, which the other tests of this file
/// share. Pytest digest `f6bffc4d4625`.
#[test]
fn the_walk_to_a_facility_with_a_footprint_is_the_deleted_rule_s() {
    let prefix = "the_walk_to_a_facility_with_a_footprint_is_the_deleted_rule_s/";
    let names: Vec<&String> = data()["inputs"].as_object().unwrap().keys().filter(|name| name.starts_with(prefix)).collect();
    assert!(!names.is_empty());
    let mut walks = Vec::new();
    let mut found = 0;
    for name in names {
        let (mut case, test) = setup(name);
        let facility = s(&test["facility"]);
        let section = section(&mut case);
        let geo = plan(&mut case);
        let loaded = Rc::clone(&case.loaded);
        let holding = kit::holding(&loaded.cell, &mut case.host).unwrap();
        let at = Where { geo: &geo, movement: holding.held(MOVEMENT, OTHER_MOVEMENT), queues: Held::Absent };
        let footprint = &case.doc()["model_configs"]["spatial_constraints"]["equipment_footprints"][0]["polygon"];
        let corners: Vec<Pt> = footprint.as_array().unwrap().iter().map(pt).collect();
        let (low, high) = (corners[0], corners[2]);
        for agent in test["agents"].as_array().unwrap() {
            let route = loaded.cell.with(|world| movement::approach_route(world, &at, &section.model, s(agent), facility, None)).unwrap().unwrap();
            walks.push(format!("{route:?}"));
            if let Some((_, path)) = &route {
                found += usize::from(path.len() >= 2);
                // A walk ends beside the footprint, never inside it.
                let end = path.last().unwrap();
                assert!(!(low.x < end.x && end.x < high.x && low.y < end.y && end.y < high.y), "{facility} {end:?}");
            }
        }
    }
    assert!(found > 0);
    let mut joined = String::new();
    for walk in &walks {
        joined.push_str(walk);
        joined.push('\n');
    }
    assert_eq!(sha256_hex(joined.as_bytes()), "cea0995fbf377fecc0b61148caf063bb97b90dc1e715c7f0c4f7b060d379bded");
}
