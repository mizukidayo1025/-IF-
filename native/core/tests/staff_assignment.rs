//! The staff's assignment: a task, a relief, a route's wait, and the support wait's reasons (Issue
//! #225, lane M6; moved from `tests/integration/test_staff_assignment.py`).
//!
//! The worlds are `tests/data/staff_assignment.json` (`tests/support/core_fixtures_service.py`):
//! `scenario_100_agents.json` with the staff and configs each pytest gave and the simulator's
//! extensions, the worlds of a controller whose Run has route capacities, and the draws of the
//! pytest's `random.Random(2615)`. Each case calls the core's step of the PyO3 entry the pytest
//! called: `staff::assign_operations` with the router the pytest gave (`_graph_of`, read as the
//! PyO3 entry reads a `SpaceRouter`), `staff::relief`, `staff::open_waits` (`_support_wait_snapshot`),
//! the controller's `_route` and `StaffService.assign_pending` with the controller's own route and
//! trip, each as the tick's arbitrate phase calls it.
//!
//! The pytest compared what a step started with a digest of what the deleted Python rule started
//! (`PYTHON_DIGESTS`: its answer, the world, its records and its events, with `json.dumps`); here
//! the digest is of the answers and the worlds after them (`support/answers.rs`), recorded from the core
//! in #225 lane M6 (decision 1): on the same kernel each pytest named in a test's doc comment passed
//! with the digests it had.

mod support;

use std::sync::Arc;

use serde_json::{json, Value};
use sheltersim_core::config::movement::Movement;
use sheltersim_core::config::staff::{known_staff_operations, support_task_minutes};
use sheltersim_core::config::SectionId;
use sheltersim_core::error::Error;
use sheltersim_core::events::writer::{WriteError, WriteResult};
use sheltersim_core::host::Host;
use sheltersim_core::routes::{Edge, Router};
use sheltersim_core::rules::staff::{self, Assigned, Full, Operations, Routed, Step, Travel, WaitRoutes};
use sheltersim_core::rules::tick::arbitrate::Staff;
use sheltersim_core::rules::tick::propose;
use sheltersim_core::rules::travel::{self, Routing, Walking};
use sheltersim_core::run::services::staff_service;
use sheltersim_core::world::WorldCell;

use support::answers::Answers;
use support::steps::{self, row, s, Case};

const FILE: &str = "staff_assignment";
const OTHER: &str = "the world holds another document than the row's";

fn inputs(name: &str) -> Value {
    steps::inputs(FILE, name)
}

fn written<T>(result: WriteResult<T>) -> Result<T, Error> {
    match result {
        Ok(value) => Ok(value),
        Err(WriteError::Error(error)) => Err(error),
        Err(other) => panic!("{other:?}"),
    }
}

/// The router of a `SpaceRouter` a Python caller gave, as the PyO3 entry reads its graph
/// (`read_graph`): the open edges, but those of no room.
fn router_of(graph: &Value) -> Router {
    let zones = graph["zones"].as_array().unwrap().iter().map(|zone| Arc::from(s(zone)));
    let edges = graph["edges"].as_array().unwrap().iter().filter_map(|edge| {
        let capacity = edge["capacity"].as_i64();
        if edge["closed"].as_bool().unwrap() || capacity.is_some_and(|capacity| capacity <= 0) {
            return None;
        }
        Some(Edge { id: Arc::from(s(&edge["id"])), from: Arc::from(s(&edge["from"])), to: Arc::from(s(&edge["to"])), minutes: edge["minutes"].as_i64().unwrap(), capacity })
    });
    Router::new(zones.collect::<Vec<_>>(), edges.collect::<Vec<_>>())
}

/// `assign_staff_operations(world, minute, router)`: the plain call first, and where a free worker
/// has a task to take, again with the routes, the floor plan and the formula.
fn assign_operations(case: &mut Case, minute: i64, graph: &Value) -> Vec<Arc<str>> {
    let router = router_of(graph);
    let result = case.tick(|tick| -> Result<Vec<Arc<str>>, Error> {
        let holding = tick.holding()?;
        if tick.is_absent(&holding, "staff_operations_v1")? {
            return Ok(Vec::new());
        }
        let known = tick.value_section(&holding, SectionId::StaffOperationsKnown, |configs| &configs.staff_operations_known, known_staff_operations)?;
        let staff = Staff::of(&holding)?;
        let first = tick.cell.with(|world| staff::assign_operations(world, &staff.inputs(None), &known, minute, None))?;
        match written(first)? {
            Operations::Done(started) => Ok(started),
            Operations::NeedsRoutes => {
                tick.host.read_queue_rows()?;
                let tasks = tick.gathered(&holding, true, true)?;
                let movement = holding.held("movement_v1", "the world holds another movement state");
                let loads = holding.held("staff_operations_v1", OTHER);
                let routed = Routed { router: &router, movement: &movement, loads_staff: &loads };
                let task_inputs = tasks.inputs();
                let inputs = staff.inputs(Some(&task_inputs));
                let result = tick.cell.with(|world| staff::assign_operations(world, &inputs, &known, minute, Some(&routed)))?;
                match tasks.finish(written(result))? {
                    Operations::Done(started) => Ok(started),
                    Operations::NeedsRoutes => panic!("the tasks asked for their routes again"),
                }
            }
        }
    })
    .unwrap();
    case.answered(&result);
    result
}

/// `schedule_staff_operation_relief(world, minute)`.
fn relief(case: &mut Case, minute: i64) -> Vec<Arc<str>> {
    let result = case.tick(|tick| -> Result<Vec<Arc<str>>, Error> {
        let holding = tick.holding()?;
        if tick.is_absent(&holding, "staff_operations_v1")? {
            return Ok(Vec::new());
        }
        tick.host.read_queue_rows()?;
        let staff = Staff::of(&holding)?;
        written(tick.cell.with(|world| staff::relief(world, &staff.inputs(None), minute))?)
    })
    .unwrap();
    case.answered(&result);
    result
}

/// `_support_wait_snapshot(world, active, minute)[0]` of the pending and assigned requests in ID
/// order: by person, `(request, waited, reasons)`. The first call reads no routes; where it asks
/// for them, the second reads the edges' loads, the floor plan and the sections.
fn waits(case: &mut Case, sample: i64) -> Vec<(String, String, i64, Vec<String>)> {
    let doc = case.doc();
    let mut ids: Vec<Arc<str>> = doc["support_requests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|request| request["status"] == "pending" || request["status"] == "assigned")
        .map(|request| Arc::from(s(&request["id"])))
        .collect();
    ids.sort();
    case.tick(|tick| -> Result<_, Error> {
        let holding = tick.holding()?;
        let staff = Staff::of(&holding)?;
        let inputs = staff.inputs(None);
        let found = match tick.cell.with(|world| staff::open_waits(world, &inputs, None, &ids, sample, false))?? {
            Some(found) => found,
            None => {
                tick.host.read_queue_rows()?;
                let loads = holding.held("staff_operations_v1", OTHER);
                let plan = tick.plan();
                let movement = tick.value_section(&holding, SectionId::Movement, |configs| &configs.movement, Movement::parse)?;
                let support = tick.value_section(&holding, SectionId::SupportTaskMinutes, |configs| &configs.support_task_minutes, support_task_minutes)?;
                let routes = WaitRoutes { loads_staff: &loads, geo: plan.get(), movement: &movement, support_minutes: &support };
                tick.cell.with(|world| staff::open_waits(world, &inputs, Some(&routes), &ids, sample, false))??.expect("the waits asked for their routes again")
            }
        };
        let mut out: Vec<(String, String, i64, Vec<String>)> = found
            .requests
            .iter()
            .map(|request| (request.agent.to_string(), ids[request.index].to_string(), request.waited, found.reasons_of(request).into_iter().map(str::to_owned).collect()))
            .collect();
        // `sorted(active_support, key=agent_id)`, stable.
        out.sort_by(|first, second| first.0.as_bytes().cmp(second.0.as_bytes()));
        Ok(out)
    })
    .unwrap()
}

/// The reasons of each request, by person (`_reasons`).
fn reasons(case: &mut Case) -> Vec<Vec<String>> {
    waits(case, 0).into_iter().map(|(_, _, _, reasons)| reasons).collect()
}

fn staff_state<'a>(doc: &'a Value, id: &str) -> &'a Value {
    &doc["model_state"]["staff_operations_v1"]["staff"][id]
}

/// A call of `assign_operations` by its label: the case, its answer and its digest.
fn assigned(label: &str) -> (Case, Vec<Arc<str>>, String) {
    let given = &inputs("assign_operations")[label];
    let mut case = Case::of(FILE, s(&given["world"]));
    let answer = assign_operations(&mut case, given["minute"].as_i64().unwrap(), &given["router"]);
    let digest = case.hex();
    (case, answer, digest)
}

// ---- assign_staff_operations ----------------------------------------------------------------------

/// `test_a_worker_takes_the_task_the_python_rule_ranks_first` (pytest digests `ranks.None`
/// `7da4f6ec183639af`, `ranks.staff-urgency-first-v1` `0a361a1c16e7d532`,
/// `ranks.staff-travel-first-v1` `9b591b8fac3c7782`).
#[test]
fn a_worker_takes_the_task_ranked_first() {
    for (label, digest) in [("ranks.None", "17287dde79cf795a8ce46b51c3efff0075546b730a57d7927d123764328f93c0"), ("ranks.staff-urgency-first-v1", "3938dd5c25049781b552f19a1cc4f72afe1d7db59d771a46a1bd2cbb1d50cd46"), ("ranks.staff-travel-first-v1", "8e51ed1c6f24583fe5851b9c9f2ba7358d33fb506a1265cb6a58eb885b12f565")] {
        let (case, answer, got) = assigned(label);
        assert_eq!(answer.len(), 2, "{label}");
        let doc = case.doc();
        let mut kinds: Vec<&Value> = doc["model_state"]["staff_operations_v1"]["staff"].as_object().unwrap().values().map(|record| &record["task_kind"]).collect();
        kinds.sort_by_key(|kind| kind.to_string());
        kinds.dedup();
        assert!(kinds.len() >= 2, "{label}");
        assert_eq!(got, digest);
    }
}

/// `test_a_zone_s_threshold_names_the_reason_as_python_formats_it` (pytest digest
/// `3e1aecc8255f15d4`).
#[test]
fn a_zone_s_threshold_names_the_reason() {
    let (case, answer, digest) = assigned("zone_threshold");
    assert!(!answer.is_empty());
    assert_eq!(staff_state(&case.doc(), "S01")["reason"], json!("sanitation_index_below_1"));
    assert_eq!(digest, "9636da47cd9eb4c33d7a73c2420dac8b55d5500660fb793975ff149e5899589f");
}

/// `test_the_rotation_reads_the_history_and_the_fatigue` (pytest digests `rotation.0.0005`
/// `fe1a9f6b18252347`, `rotation.0.0015` `bf315e3139710a99`, `rotation.0.0025`
/// `12ac201e10b4c1df`, `rotation.0.001` `65082333055b238e`): 1000 * 0.0005 is half way, `round()`
/// goes to even, and the ties go by ID.
#[test]
fn the_rotation_reads_the_history_and_the_fatigue() {
    for (label, digest) in [("rotation.0.0005", "722db56ee1ad8a9779632be43ba07feb5181fcfc4003973e98bfd29b8b61c635"), ("rotation.0.0015", "18cbe24980e156c0b5bafc2abf6028efa3ad4d97e31d197892c748ea880aa879"), ("rotation.0.0025", "a39e73c8cf2bfa85088961a6ce3870bde6f6c03e7fa472c0c23d41c7ca36aabb"), ("rotation.0.001", "73abc72e023cc5b6fe44886cf900c2cfa0d2f3f7ecc033f9753e3176c45ca141")] {
        let (_, answer, got) = assigned(label);
        assert_eq!(answer.len(), 1, "{label}");
        assert_eq!(got, digest);
    }
}

/// `test_a_patrol_s_urgency_is_half_the_mean_stress_against_another_task` (pytest digests
/// `patrol_urgency.0.598` `2462696bf8fe417f`, `patrol_urgency.0.602` `c235dab606e85b15`): either
/// side of the rank, a 1% change of the factor swaps them.
#[test]
fn a_patrol_s_urgency_is_half_the_mean_stress() {
    for (label, kind, digest) in [("patrol_urgency.0.598", "cleaning", "cc8a4f033c9a68561df7b82d7479e12c1df2bb8184edc7ff557da4ffcc96bb2d"), ("patrol_urgency.0.602", "patrol", "184a999d3f07c439e2b7b0db1b2e56989d8a8e64a0de4d062735e101149f8faa")] {
        let (case, answer, got) = assigned(label);
        assert!(!answer.is_empty());
        assert_eq!(staff_state(&case.doc(), "S01")["task_kind"], json!(kind), "{label}");
        assert_eq!(got, digest);
    }
}

/// `test_the_rotation_counts_recent_work_from_four_hours_back` (pytest digest
/// `d3535c084e011d31`).
#[test]
fn the_rotation_counts_recent_work_from_four_hours_back() {
    let (case, answer, digest) = assigned("recent_work");
    assert!(!answer.is_empty());
    let doc = case.doc();
    assert_ne!(staff_state(&doc, "S1")["phase"], json!("idle"));
    assert_eq!(staff_state(&doc, "S0")["phase"], json!("idle"));
    assert_eq!(digest, "2fb541847b45441b4e787786c010a4ea029b6fcba8be360d5640cd5564328af1");
}

/// `test_a_fatigue_rounds_at_a_thousandth_in_the_rotation` (pytest digest `f4f886f4b8462b42`):
/// both round to 50, so the ID decides; 1010 * fatigue would give 51 and 50.
#[test]
fn a_fatigue_rounds_at_a_thousandth_in_the_rotation() {
    let (case, answer, digest) = assigned("fatigue_rounds");
    assert!(!answer.is_empty());
    assert_ne!(staff_state(&case.doc(), "S0")["phase"], json!("idle"));
    assert_eq!(digest, "717abfe773f6eee6e1a81540282e513c3fe049da6716e1c6057ee5edc7605ae8");
}

/// `test_the_travel_first_formula_takes_the_nearer_task` (pytest digests `travel_first`
/// `b294e15f8056d14c`, `urgency_first` `8b18793a5e14593f`).
#[test]
fn the_travel_first_formula_takes_the_nearer_task() {
    let (near, answer, digest) = assigned("travel_first");
    assert!(!answer.is_empty());
    assert_eq!(staff_state(&near.doc(), "S01")["target_zone_id"], json!("toilet"));
    assert_eq!(digest, "d5bdcd9d7486c14443d8b31b332ed720fd78218dce1438dfb79027793e171ed2");
    let (urgent, answer, digest) = assigned("urgency_first");
    assert!(!answer.is_empty());
    assert_ne!(staff_state(&urgent.doc(), "S01")["target_zone_id"], json!("toilet"));
    assert_eq!(digest, "5c6e477b93447cbc78c878b0fa1b5f1435961f4b8ba7f12bad320c5a4056dea3");
}

/// `test_equal_ranks_go_by_the_kind_as_python_compares_them` (pytest digest `f3c1783863594513`):
/// `1.0 - 0.6 == 0.8 * 0.5`.
#[test]
fn equal_ranks_go_by_the_kind() {
    let (case, answer, digest) = assigned("equal_ranks");
    assert!(!answer.is_empty());
    assert_eq!(staff_state(&case.doc(), "S01")["task_kind"], json!("patrol"));
    assert_eq!(digest, "2a25ac024040d0c584f94cce9610d1855155207a0bbb91ded2e862cc8be6e621");
}

/// `test_a_small_threshold_names_the_reason_with_an_exponent` (pytest digest `bea27665c28fd976`).
#[test]
fn a_small_threshold_names_the_reason_with_an_exponent() {
    let (case, answer, digest) = assigned("small_threshold");
    assert!(!answer.is_empty());
    assert_eq!(staff_state(&case.doc(), "S01")["reason"], json!("sanitation_index_below_1e-05"));
    assert_eq!(digest, "36c2da48e0b0981820208b6d02abe226e70e71f2930eb32904f420b01857be7d");
}

/// `test_a_full_passage_keeps_a_worker_from_a_task` (pytest digest `efff7d4ce6533e45`).
#[test]
fn a_full_passage_keeps_a_worker_from_a_task() {
    let (_, answer, digest) = assigned("full_passage");
    assert_eq!(answer.len(), 1);
    assert_eq!(digest, "3992a88466a82238c39c37052878680b7dde8f586f58aadd3ab7ea268771d975");
}

/// The passages' loads at a minute (`shared_passage_edge_loads`).
fn passage_loads(case: &mut Case, minute: i64) -> Vec<(String, i64)> {
    let mut loads: Vec<(String, i64)> = case.tick(|tick| {
        let holding = tick.holding().unwrap();
        let movement = holding.held("movement_v1", "the world holds another movement state");
        let loads = holding.held("staff_operations_v1", OTHER);
        tick.cell.with(|world| travel::passage_edge_loads(world, &movement, &loads, minute).unwrap()).unwrap()
    })
    .into_iter()
    .map(|(edge, load)| (edge.to_string(), load))
    .collect();
    loads.sort();
    loads
}

/// `test_the_passage_loads_are_the_python_rule_s` (pytest digests `passage_loads.0`
/// `a93d4ab3da46b5fe`, `passage_loads.1` `0f0fb0bf34700171`): the zones stay reserved, so a later
/// minute starts nothing new on them and reads the loads.
#[test]
fn the_passage_loads_are_read_a_minute_later() {
    let given = &inputs("assign_operations")["passage_loads"];
    let mut case = Case::of(FILE, s(&given["world"]));
    assign_operations(&mut case, 0, &given["router"]);
    assert!(!passage_loads(&mut case, 1).is_empty());
    case.edit(|doc| doc["minute_index"] = json!(1));
    assign_operations(&mut case, 1, &given["router"]);
    assert_eq!(case.hex(), "dfc4b75537748b640124c6b10a35ef64b050ba8514e94aa9d0553adc5d939ee9");
}

/// `test_a_trip_under_way_loads_the_edge_its_share_of_the_time_has_reached` (pytest digest
/// `84e9301a84f3442f`): at 496 of 1000 minutes the walker is on the first edge; a 1% scale would
/// say the second.
#[test]
fn a_trip_under_way_loads_the_edge_its_share_of_the_time_has_reached() {
    let given = &inputs("assign_operations")["trip_under_way"];
    let mut case = Case::of(FILE, s(&given["world"]));
    assert_eq!(passage_loads(&mut case, 496), [("sleeping_corridor".to_owned(), 1)]);
    let answer = assign_operations(&mut case, 496, &given["router"]);
    assert!(!answer.is_empty());
    assert_eq!(case.hex(), "5c5c5842f4cef109cc29e26ee59d99947cb14647743c746bdd1a2a7fb0007269");
}

// ---- schedule_staff_operation_relief --------------------------------------------------------------

fn replacement(case: &Case) -> Value {
    case.doc()["event_queue"].as_array().unwrap().iter().find(|event| event["event_type"] == "staff_operation_relief").unwrap()["payload"]["replacement_staff_id"].clone()
}

/// `test_a_relief_goes_to_the_replacement_the_python_rule_picks` (pytest digest
/// `708bac4aa204b371`): the one scheduled is not asked for again.
#[test]
fn a_relief_goes_to_a_replacement_at_the_task() {
    let mut case = Case::of(FILE, s(&inputs("relief")["world"]));
    let answer = relief(&mut case, 1);
    assert_eq!(answer.len(), 1);
    assert_eq!(case.hex(), "9a13902f0ef38c6125e8c7982fd36227f2f3f882144b6ed79a56fd5062408bb1");
    assert_eq!(relief(&mut case, 1), Vec::<Arc<str>>::new());
    assert!([json!("S02"), json!("S03")].contains(&replacement(&case)));
}

/// `test_a_relief_goes_to_the_replacement_with_the_least_work` (pytest digest
/// `e4690399fa7ab401`).
#[test]
fn a_relief_goes_to_the_replacement_with_the_least_work() {
    let mut case = Case::of(FILE, s(&inputs("relief_least_work")["world"]));
    relief(&mut case, 1);
    assert_eq!(case.hex(), "5723acf388e28ae26385bfb44579fd373e82be70e383d856abd17a2c11977c0e");
    assert_eq!(replacement(&case), json!("S03"));
}

/// `test_no_relief_for_a_worker_still_on_shift_or_too_near_the_end`: still on shift at minute 0;
/// at minute 1 the task is due at 2, too near the end.
#[test]
fn no_relief_for_a_worker_still_on_shift_or_too_near_the_end() {
    let mut on_shift = Case::of(FILE, s(&inputs("on_a_task")["world"]));
    let before = on_shift.bytes();
    assert_eq!(relief(&mut on_shift, 0), Vec::<Arc<str>>::new());
    assert!(on_shift.bytes() == before);
    let mut near = Case::of(FILE, s(&inputs("no_relief_near_the_end")["world"]));
    assert_eq!(relief(&mut near, 1), Vec::<Arc<str>>::new());
    assert_eq!(near.doc()["event_queue"], on_shift.doc()["event_queue"]);
}

// ---- What the metrics read, with routes -----------------------------------------------------------

/// `test_the_reasons_a_free_worker_s_route_gives_are_the_python_rule_s`: not attempted at the
/// sampled minute, attempted with no reason recorded, then the reason recorded.
#[test]
fn the_reasons_a_free_worker_s_route_gives() {
    let steps = inputs("waits")["free_worker"].clone();
    let wanted = ["assignment_not_attempted_at_sampled_minute", "assignment_failure_reason_not_recorded", "assignment_failure_reason_not_recorded", "route_unavailable_at_assignment"];
    for (name, reason) in steps.as_array().unwrap().iter().zip(wanted) {
        assert_eq!(reasons(&mut Case::of(FILE, s(name))), [[reason]], "{name}");
    }
}

/// `test_a_route_past_the_end_of_the_run_is_its_reason`.
#[test]
fn a_route_past_the_end_of_the_run_is_its_reason() {
    assert_eq!(reasons(&mut Case::of(FILE, s(&inputs("waits")["past_the_end"]))), [["insufficient_run_time"]]);
}

/// `test_a_route_that_ends_exactly_at_the_run_s_end_is_too_late`: its minutes and ten of service.
#[test]
fn a_route_that_ends_exactly_at_the_run_s_end_is_too_late() {
    for item in inputs("waits")["ends_exactly"].as_array().unwrap() {
        let got = reasons(&mut Case::of(FILE, s(&item[0])));
        assert_eq!(got[0].iter().any(|reason| reason == "insufficient_run_time"), item[1].as_bool().unwrap(), "{item}");
    }
}

/// `test_a_full_destination_counts_who_is_on_the_way`: one there and one walking there fill a
/// toilet of two.
#[test]
fn a_full_destination_counts_who_is_on_the_way() {
    let worlds = inputs("waits")["full_destination"].clone();
    assert_eq!(reasons(&mut Case::of(FILE, s(&worlds[0]))), [["staff_destination_full"]]);
    assert_ne!(reasons(&mut Case::of(FILE, s(&worlds[1]))), [["staff_destination_full"]]);
}

/// `test_a_full_destination_and_a_full_passage_are_their_reasons`.
#[test]
fn a_full_destination_and_a_full_passage_are_their_reasons() {
    let worlds = inputs("waits")["full_passage"].clone();
    let wanted = ["staff_destination_full", "route_capacity_full", "no_route_to_qualified_staff"];
    for (name, reason) in worlds.as_array().unwrap().iter().zip(wanted) {
        assert_eq!(reasons(&mut Case::of(FILE, s(name))), [[reason]], "{name}");
    }
}

/// `test_many_worlds_read_the_same_reasons_with_free_and_busy_workers` (pytest digest
/// `886f522f9c88ebae` of the snapshots' `json.dumps`): 120 worlds of the pytest's draws, each its
/// base with the people's zones, the busy staff, the requests, the metric and a zone's capacity
/// set as the pytest set them (`_drawn_world` of the builder, which checks they make its world).
#[test]
fn many_worlds_read_the_same_reasons_with_free_and_busy_workers() {
    let drawn = inputs("many_worlds")["worlds"].clone();
    let mut answers = Answers::default();
    let mut seen: Vec<String> = Vec::new();
    for world in drawn.as_array().unwrap() {
        let mut case = Case::of(FILE, s(&world["base"]));
        case.edit(|doc| {
            for (agent, zone) in doc["agents"].as_array_mut().unwrap().iter_mut().zip(s(&world["zones"]).chars()) {
                agent["zone_id"] = json!(if zone == 't' { "toilet" } else { "sleeping" });
            }
            for member in doc["staff"].as_array_mut().unwrap() {
                if world["busy"].as_array().unwrap().contains(&member["id"]) {
                    member["active_task_id"] = json!("busy");
                }
            }
            doc["support_requests"] = world["requests"].clone();
            if world["metric"].as_bool().unwrap() {
                doc["metric_state"]["support_assignment_metrics_minute"] = json!(0.0);
            }
            if let Some(capacity) = world["capacity"].as_array() {
                steps::row_mut(doc, "zones", s(&capacity[0]))["capacity"] = capacity[1].clone();
            }
        });
        let found = waits(&mut case, 0);
        case.loaded.cell.with(|world| answers.add(&format!("{found:?}"), world)).unwrap();
        seen.extend(found.into_iter().flat_map(|(_, _, _, reasons)| reasons));
    }
    assert_eq!(drawn.as_array().unwrap().len(), 120);
    assert_eq!(answers.hex(), "325e8aeb57bd3c831c3161b661c4a29987f78545901cd7d811c957bc0406a0a4");
    for reason in ["staff_busy", "assignment_not_attempted_at_sampled_minute", "assignment_failure_reason_not_recorded", "insufficient_run_time", "staff_destination_full"] {
        assert!(seen.iter().any(|seen| seen == reason), "{reason}");
    }
}

// ---- _route with edge capacities ------------------------------------------------------------------

fn capacity_waits(case: &Case) -> usize {
    case.doc()["event_log"].as_array().unwrap().iter().filter(|record| record["event"]["event_type"] == "zone_edge_capacity_wait").count()
}

/// `test_a_route_with_a_full_edge_records_its_wait_as_the_python_rule_does` (pytest digest
/// `f4d92ff6d72e10df`): no route past the full edges, and its wait recorded once; a person already
/// there needs none.
#[test]
fn a_route_with_a_full_edge_records_its_wait() {
    let mut case = Case::of(FILE, s(&inputs("route_full_edge")["world"]));
    let person = s(&case.doc()["agents"][0]["profile"]["id"]).to_owned();
    assert_eq!(case.route("sleeping", "toilet", &person), None);
    let route = case.route("sleeping", "sleeping", &person).unwrap();
    assert_eq!((route.zone_ids.len(), route.edge_ids.len(), route.minutes), (1, 0, 0));
    assert_eq!(capacity_waits(&case), 1);
    assert_eq!(case.hex(), "50e3430e26f5e73d5ca6e54925bdd94b56fb10471b46fcef9f224d691de02e4e");
}

/// `test_a_route_with_room_on_its_edges_counts_the_trips_under_way` (pytest digest
/// `99b6008414a8e20a`).
#[test]
fn a_route_with_room_on_its_edges_counts_the_trips_under_way() {
    let mut case = Case::of(FILE, s(&inputs("route_room")["world"]));
    let person = s(&case.doc()["agents"][0]["profile"]["id"]).to_owned();
    let route = case.route("sleeping", "toilet", &person).unwrap();
    let ids = |items: &[Arc<str>]| items.iter().map(|id| id.to_string()).collect::<Vec<_>>();
    assert_eq!(ids(&route.zone_ids), ["sleeping", "corridor", "toilet"]);
    assert_eq!(ids(&route.edge_ids), ["sleeping_corridor", "corridor_toilet"]);
    assert_eq!(route.minutes, 2);
    assert_eq!(case.hex(), "82a691f41b01ab09236b1d7b4c4a93f67a500d97f2e720fc3f4ce6048026efbc");
}

/// `StaffService.assign_pending(minute, validate_state=False)` with the service as the Run read
/// it: the plain case first, and where a request may be assigned, the controller's own route and
/// trip.
fn assign_pending(case: &mut Case, minute: i64) -> Vec<Arc<str>> {
    let result = case.tick(|tick| -> Result<Vec<Arc<str>>, Error> {
        let policies = tick.policies.clone();
        let service = staff_service(policies.services.staff.object().unwrap()).unwrap();
        let holding = tick.holding()?;
        let staff = Staff::of(&holding)?;
        let mut taken = Vec::new();
        let first = tick.cell.with(|world| staff::assign_pending(world, &staff.inputs(None), &service, minute, None, &mut taken))?;
        match written(first)? {
            Assigned::Done(assigned) => Ok(assigned),
            Assigned::NeedsAll => {
                tick.host.read_queue_rows()?;
                let tasks = tick.gathered(&holding, true, false)?;
                let graph = match service.resolver {
                    Step::Own => Some(propose::graph(tick)?),
                    _ => None,
                };
                let plan = tick.plan();
                let section = tick.value_section(&holding, SectionId::Movement, |configs| &configs.movement, Movement::parse)?;
                let movement = holding.held("movement_v1", "the world holds another movement state");
                let loads = holding.held("staff_operations_v1", OTHER);
                let travel = graph.as_ref().map(|graph| Travel {
                    routing: Routing { graph, geo: plan.get(), movement: &section, staff: &loads },
                    walking: Walking { movement: &movement, geo: plan.get() },
                });
                let full = Full { travel };
                let task_inputs = tasks.inputs();
                let inputs = staff.inputs(Some(&task_inputs));
                let result = tick.cell.with(|world| staff::assign_pending(world, &inputs, &service, minute, Some(&full), &mut taken))?;
                tick.host.slots_changed(&taken)?;
                match tasks.finish(written(result))? {
                    Assigned::Done(assigned) => Ok(assigned),
                    Assigned::NeedsAll => panic!("the assignment asked for its inputs again"),
                }
            }
        }
    })
    .unwrap();
    case.answered(&result);
    result
}

/// `test_the_controller_s_own_route_and_trip_decide_an_assignment` (pytest digests `full`
/// `d7277550d063eaba`, `late` `5cb01dc39695c9aa`, `trip` `60c1b30ab87e9513`): a full edge leaves no
/// route (and records its wait), a task past the Run's end is not started, and a route with room
/// schedules the person's trip before the task's event.
#[test]
fn the_controller_s_own_route_and_trip_decide_an_assignment() {
    let given = inputs("own_route_and_trip");
    for (label, reason, digest) in [("full", Some("route_unavailable_at_assignment"), "bc48de1c93c48e8539cd28ffdd08af84d2de05cc9d2206bc52b389c1216dd936"), ("late", Some("insufficient_run_time"), "3a9c62a05211176bd0c6aa43725587165bcc620627991e8a06bc153897b27d1a"), ("trip", None, "e66cf39b52e14cfb77d7994eaf9b45fe0aa7afa85b07e78a49ec70de3b56418b")] {
        let mut case = Case::of(FILE, s(&given[label]["world"]));
        let assigned = assign_pending(&mut case, 0);
        let wanted: Vec<Arc<str>> = if reason.is_some() { Vec::new() } else { vec![Arc::from("support-1")] };
        assert_eq!(assigned, wanted, "{label}");
        assert_eq!(row(&case.doc(), "support_requests", "support-1")["last_assignment_wait_reason_code"], json!(reason), "{label}");
        assert_eq!(case.hex(), digest);
    }
}
