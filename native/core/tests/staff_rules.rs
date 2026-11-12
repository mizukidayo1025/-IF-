//! The staff's rules of a tick and the shared operations' receipts on the worlds the pytest files
//! built (Issue #225, lane M4; moved from `tests/integration/test_staff_rules.py` and three tests of
//! `test_handler_rest_rules.py`).
//!
//! The worlds are `tests/support/core_fixtures_staff.py`'s (`staff_rules.json`): the small replay
//! with three staff members, edited as each pytest edited it. Each test calls the core's rule the
//! PyO3 entry of the pytest called (`rules::staff`, `rules::common_operations`), with what that
//! entry reads besides the world read as the core's tick reads it (`kit::holding`, the sections of
//! the exported configs, the floor plan). The rules have no Python body left (lane L10,
//! `kernel_only`): they are checked against the deleted rule's formula as the pytest wrote it (the
//! fixture's values), or against a digest of what that rule gave, which the test writes again
//! byte for byte (`support/dumps.rs`, `support/canonical.rs`).

mod support;

use std::sync::Arc;

use serde_json::Value;
use sheltersim_core::cell::Shard;
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::config::movement::Movement;
use sheltersim_core::config::staff::{known_staff_operations, support_task_minutes};
use sheltersim_core::config::SectionId;
use sheltersim_core::error::Error;
use sheltersim_core::events::writer::{WriteError, WriteResult};
use sheltersim_core::events::EventStatus;
use sheltersim_core::host::Host;
use sheltersim_core::rules::common_operations::{json_value, record_through, Logged, Receipt};
use sheltersim_core::rules::kit;
use sheltersim_core::rules::staff::{self, Assigned, Full, Operations, Service, Step, WaitRoutes};
use sheltersim_core::rules::staff_operation_events as ops;
use sheltersim_core::rules::tick::arbitrate::Staff;
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, World, WorldCell};

use support::canonical::{canonical_json, parts};
use support::dumps::Dumps;
use support::{cases, load, TestHost};

const FILE: &str = "staff_rules";
const KEY: &str = "staff_operations_v1";
const SHARED: &str = "shared_operations_v1";
const OTHER_DOCUMENT: &str = "the world holds another document than the row's";

fn data() -> Value {
    load::data(FILE)
}

/// The first 16 hex digits of the sha256 of a text, as the pytest files pinned them.
fn digest(text: &str) -> String {
    sha256_hex(text.as_bytes())[..16].to_owned()
}

/// A world of the file, loaded, with its host.
struct Rig {
    entry: Value,
    cell: Shard<World>,
    host: TestHost,
}

fn rig_of(entry: Value) -> Rig {
    let cell = Shard::new(load::world(&entry["world"], &entry["slots"]));
    let host = load::host(&entry);
    Rig { entry, cell, host }
}

fn rig(data: &Value, name: &str) -> Rig {
    rig_of(cases::resolve(&data["worlds"], name))
}

impl Rig {
    fn staff(&mut self) -> Staff {
        Staff::of(&kit::holding(&self.cell, &mut self.host).unwrap()).unwrap()
    }

    fn parts(&self) -> Vec<(&'static str, Vec<u8>)> {
        self.cell.with(|world| parts(world)).unwrap()
    }

    /// The world's part of the canonical document, as a value.
    fn part(&self, name: &str) -> V {
        let parts = self.parts();
        let (_, bytes) = parts.iter().find(|(key, _)| *key == name).unwrap();
        json_value(bytes).unwrap()
    }

    fn namespace(&self, name: &str) -> V {
        match self.part("model_state") {
            V::Map(mut map) => map.shift_remove(name).unwrap_or_else(|| panic!("no {name}")),
            _ => unreachable!(),
        }
    }

    fn metric(&self, name: &str) -> V {
        match self.part("metric_state") {
            V::Map(map) => map.get(name).cloned().unwrap_or(V::Null),
            _ => unreachable!(),
        }
    }

    /// `dict(world.metric_state)`, in the row's order (which the Python dict follows).
    fn metric_state(&self) -> V {
        self.cell.with(|world| V::Map(world.metric_state().unwrap().iter().map(|(key, value)| (key.clone(), V::Float(*value))).collect())).unwrap()
    }

    fn set_minute(&self, minute: i64) {
        self.cell.with(|world| world.scalars.get_mut().minute_index = Fit::Ok(minute)).unwrap();
    }

    /// `update_staff_operations(world, minute)`.
    fn update(&mut self, minute: i64) -> Result<(), Error> {
        let staff = self.staff();
        self.cell.with(|world| staff::update_operations(world, &staff.inputs(None), minute)).unwrap()
    }

    /// `assign_staff_operations(world, minute, router)` up to the router: what it started without
    /// routes, or `NeedsRoutes` where a free worker has a task to take (the PyO3 entry then reads
    /// the router).
    fn operations(&mut self, minute: i64) -> WriteResult<Operations> {
        let holding = kit::holding(&self.cell, &mut self.host).unwrap();
        let known = kit::kept_section_value(&self.cell, &mut self.host, &holding, SectionId::StaffOperationsKnown, |configs| &configs.staff_operations_known, known_staff_operations).unwrap();
        let staff = Staff::of(&holding).unwrap();
        self.cell.with(|world| staff::assign_operations(world, &staff.inputs(None), &known, minute, None)).unwrap()
    }

    /// `schedule_staff_operation_relief(world, minute)`.
    fn relief(&mut self, minute: i64) -> WriteResult<Vec<Arc<str>>> {
        let staff = self.staff();
        self.cell.with(|world| staff::relief(world, &staff.inputs(None), minute)).unwrap()
    }

    /// `assign_none` of the PyO3 crate without the validation: whether every request stays pending
    /// (and was written so).
    fn plain(&mut self, minute: i64) -> bool {
        let staff = self.staff();
        self.cell.with(|world| staff::assign_plain(world, &staff.inputs(None), minute)).unwrap().unwrap()
    }

    /// `StaffService(world, ...).assign_pending(minute, validate_state=False)` of a service with no
    /// route: the plain case first, and where a request may be assigned, what the tasks read.
    fn assign(&mut self, service: &Service, minute: i64) -> Vec<Arc<str>> {
        let holding = kit::holding(&self.cell, &mut self.host).unwrap();
        let staff = Staff::of(&holding).unwrap();
        let mut taken = Vec::new();
        let first = self.cell.with(|world| staff::assign_pending(world, &staff.inputs(None), service, minute, None, &mut taken)).unwrap();
        match first.unwrap() {
            Assigned::Done(assigned) => assigned,
            Assigned::NeedsAll => {
                let geo = self.host.floor_plan().ok();
                let held = |name: &str, other: &'static str| holding.held(name, other);
                let (operations, movement) = (held(KEY, OTHER_DOCUMENT), held("movement_v1", "the world holds another movement state"));
                let body = held("body_state_v1", "the world holds another body state");
                let program = held("effect_program_v1", "the world holds another effect_program_v1");
                let tasks = ops::Inputs {
                    staff: &operations,
                    movement: &movement,
                    body: &body,
                    program: &program,
                    shared_absent: !holding.keys.contains_key(SHARED),
                    geo: geo.as_ref(),
                    formula: None,
                };
                let inputs = staff.inputs(Some(&tasks));
                let full = Full { travel: None };
                let result = self.cell.with(|world| staff::assign_pending(world, &inputs, service, minute, Some(&full), &mut taken)).unwrap();
                match result.unwrap() {
                    Assigned::Done(assigned) => assigned,
                    Assigned::NeedsAll => panic!("the assignment asked for its inputs again"),
                }
            }
        }
    }

    /// `_support_wait_snapshot(world, active, sample)` of the open requests (pending or assigned,
    /// by ID), as the PyO3 entry makes its value: `[details, no_qualified]`. The routes' inputs are
    /// read where the core asks for them, as `with_wait_inputs` reads them.
    fn snapshot(&mut self, sample: i64) -> V {
        let ids: Vec<Arc<str>> = match self.part("support_requests") {
            V::List(rows) => rows
                .iter()
                .filter_map(|row| {
                    let V::Map(row) = row else { unreachable!() };
                    let open = matches!(row.get("status"), Some(V::Str(status)) if &**status == "pending" || &**status == "assigned");
                    match row.get("id") {
                        Some(V::Str(id)) if open => Some(id.clone()),
                        _ => None,
                    }
                })
                .collect(),
            _ => unreachable!(),
        };
        let holding = kit::holding(&self.cell, &mut self.host).unwrap();
        let staff = Staff::of(&holding).unwrap();
        let inputs = staff.inputs(None);
        let mut waits = self.cell.with(|world| staff::open_waits(world, &inputs, None, &ids, sample, false)).unwrap().unwrap();
        if waits.is_none() {
            let loads_staff = holding.held(KEY, OTHER_DOCUMENT);
            let geo = self.host.floor_plan().ok();
            let movement = kit::kept_section_value(&self.cell, &mut self.host, &holding, SectionId::Movement, |configs| &configs.movement, Movement::parse).unwrap();
            let support_minutes = kit::kept_section_value(&self.cell, &mut self.host, &holding, SectionId::SupportTaskMinutes, |configs| &configs.support_task_minutes, support_task_minutes).unwrap();
            let routes = WaitRoutes { loads_staff: &loads_staff, geo: geo.as_ref(), movement: &movement, support_minutes: &support_minutes };
            waits = self.cell.with(|world| staff::open_waits(world, &inputs, Some(&routes), &ids, sample, false)).unwrap().unwrap();
        }
        let waits = waits.expect("the waits with their routes");
        let mut order: Vec<usize> = (0..waits.requests.len()).collect();
        order.sort_by(|first, second| waits.requests[*first].agent.as_bytes().cmp(waits.requests[*second].agent.as_bytes()));
        let mut details = indexmap::IndexMap::new();
        for index in order {
            let row = &waits.requests[index];
            let reasons = waits.reasons_of(row).into_iter().map(|reason| V::Str(Arc::from(reason))).collect();
            let detail = [("request_id", V::Str(ids[row.index].clone())), ("waited_minutes", V::Int(row.waited)), ("reason_codes", V::List(reasons))];
            details.insert(row.agent.clone(), V::Map(detail.into_iter().map(|(key, value)| (Arc::from(key), value)).collect()));
        }
        let mut no_qualified: Vec<&str> = waits.requests.iter().filter(|row| waits.has_no_qualified_staff(row)).map(|row| &*row.agent).collect();
        no_qualified.sort_unstable();
        V::List(vec![V::Map(details), V::List(no_qualified.into_iter().map(|agent| V::Str(Arc::from(agent))).collect())])
    }

    /// `record_shared_operation(world, receipt)` at the receipt's minute, with the receipt's record
    /// in the EventLog (`find_record` of the host).
    fn record(&mut self, logged: &Value) -> Result<(), Error> {
        let record = &logged["record"];
        let event = load::event(&record["event"]);
        let status = EventStatus::parse(record["status"].as_str().unwrap()).unwrap();
        let found = Logged { status, realized_minute: record["realized_minute"].as_i64().unwrap(), event };
        self.host.records.insert(found.event.event_id.clone(), found);
        let receipt = Receipt::of(&load::v(&logged["receipt"])).unwrap();
        record_through(&self.cell, &mut self.host, &receipt)
    }
}

fn is_unsupported<T: std::fmt::Debug>(result: &WriteResult<T>) -> bool {
    matches!(result, Err(WriteError::Error(Error::Unsupported(_))))
}

/// The zones of `staff_operations_v1`, as the core's test writes the deleted rule's (sorted keys).
fn zones_text(rig: &Rig) -> String {
    let V::Map(state) = rig.namespace(KEY) else { unreachable!() };
    Dumps::default().sorted().text(&state["zones"])
}

/// `StaffService(world)`: what `assign_pending` takes of a service of no route, with its
/// `fairness_after_minutes`.
fn service(data: &Value, fairness_after: Option<i64>) -> Service {
    let given = &data["service"];
    Service {
        policy: Arc::from(given["policy"].as_str().unwrap()),
        task_minutes: given["task_minutes"].as_i64().unwrap(),
        fairness_after: fairness_after.unwrap_or_else(|| given["fairness_after"].as_i64().unwrap()),
        resolver: Step::None,
        scheduler: Step::None,
    }
}

// ---- update_staff_operations ---------------------------------------------------------------------

/// Each zone's sanitation falls by `load * rate * elapsed`, down to 0.0, on 40 rounds of random
/// rates (Python's `random.Random(7)`), each updated at 4 random minutes.
#[test]
fn the_sanitation_is_the_python_rules_on_random_rates() {
    let data = data();
    let rounds = data["tests"]["sanitation_random_rates"].as_array().unwrap();
    assert_eq!(rounds.len(), 40);
    for round in rounds {
        let mut rig = rig(&data, round["world"].as_str().unwrap());
        for step in round["steps"].as_array().unwrap() {
            rig.update(step[0].as_i64().unwrap()).unwrap();
            assert_eq!(zones_text(&rig), step[1].as_str().unwrap(), "{}", round["world"]);
        }
    }
}

#[test]
fn a_rate_of_one_percent_more_shows() {
    let data = data();
    let (mut world, mut other) = (rig(&data, "plain"), rig(&data, "rate_one_percent_more"));
    world.update(100).unwrap();
    other.update(100).unwrap();
    assert_ne!(zones_text(&world), zones_text(&other));
}

/// `canonical_staff_operations_config` takes an int rate in its range (0, or 1 for the global
/// one); the Python rule's product was then an int, taken off the sanitation as its float.
#[test]
fn an_int_rate_the_config_takes_is_the_python_rule_s() {
    let data = data();
    let cases = data["tests"]["int_rate"].as_array().unwrap();
    assert_eq!(cases.len(), 3);
    for case in cases {
        let mut rig = rig(&data, case["world"].as_str().unwrap());
        rig.update(case["minute"].as_i64().unwrap()).unwrap();
        assert_eq!(zones_text(&rig), case["zones"].as_str().unwrap());
    }
}

/// `config["effects_by_zone"]`: the Python rule raised KeyError for a config no rule writes; the
/// core leaves it to Python, with nothing changed.
#[test]
fn a_config_without_its_table_changes_nothing_and_is_refused() {
    let data = data();
    let mut rig = rig(&data, "no_zone_table");
    let before = rig.parts();
    assert!(matches!(rig.update(50), Err(Error::Unsupported(_))));
    assert!(rig.parts() == before);
}

// ---- assign_staff_operations, schedule_staff_operation_relief ------------------------------------

fn check_operations(group: &str, count: usize) {
    let data = data();
    let cases = data["tests"]["operations"][group].as_array().unwrap();
    assert_eq!(cases.len(), count);
    for case in cases {
        let name = case["world"].as_str().unwrap();
        let mut rig = rig(&data, name);
        let result = rig.operations(1);
        match case["expected"].as_str().unwrap() {
            "routes" => assert!(matches!(result, Ok(Operations::NeedsRoutes)), "{name}: {result:?}"),
            "none" => assert!(matches!(&result, Ok(Operations::Done(started)) if started.is_empty()), "{name}: {result:?}"),
            "refused" => assert!(is_unsupported(&result), "{name}: {result:?}"),
            other => panic!("{other}"),
        }
    }
}

/// A patrol is due where the zone's stress is at least its threshold (0.6, or a zone's own, an
/// int too): then the router is read.
#[test]
fn a_free_worker_with_a_task_reads_the_router_and_without_one_doesnt() {
    check_operations("patrol", 4);
}

/// A cleaning is due below a sanitation of 0.85, unless a worker is on the zone already.
#[test]
fn the_cleaning_threshold_is_the_rules() {
    check_operations("cleaning", 3);
}

/// An outreach is due at a mean uncertainty of 0.45.
#[test]
fn the_outreach_threshold_is_the_rules() {
    check_operations("outreach", 2);
}

/// A repair is due where a facility lacks a working unit; another formula is the Python rule's.
#[test]
fn a_repair_reads_the_router_and_another_formula_goes_to_the_python_rule() {
    check_operations("repair", 3);
}

#[test]
fn a_tick_with_nothing_to_start_changes_nothing() {
    let data = data();
    let names = data["tests"]["nothing_to_start"].as_array().unwrap();
    assert_eq!(names.len(), 5);
    for name in names {
        let mut rig = rig(&data, name.as_str().unwrap());
        let before = rig.parts();
        assert_eq!(rig.relief(3).unwrap(), Vec::<Arc<str>>::new());
        assert!(matches!(rig.operations(3), Ok(Operations::Done(started)) if started.is_empty()));
        assert!(rig.parts() == before, "{name}");
    }
}

#[test]
fn a_task_with_no_id_sends_the_relief_to_the_python_rule() {
    let data = data();
    let mut rig = rig(&data, "working_without_task");
    assert!(is_unsupported(&rig.relief(3)));
    assert_eq!(rig.relief(199).unwrap(), Vec::<Arc<str>>::new());
}

// ---- StaffService.assign_pending -----------------------------------------------------------------

/// `[(id, last_assignment_attempt_minute, last_assignment_wait_reason_code), ...]` of the requests,
/// in the order of `world.support_requests` (made in ID order).
fn waiting_fields(rig: &Rig) -> V {
    let V::List(rows) = rig.part("support_requests") else { unreachable!() };
    let fields = ["id", "last_assignment_attempt_minute", "last_assignment_wait_reason_code"];
    V::List(rows.iter().map(|row| V::List(fields.iter().map(|field| match row { V::Map(row) => row.get(*field).cloned().unwrap_or(V::Null), _ => unreachable!() }).collect())).collect())
}

/// 30 rounds of random skills and care kinds (Python's `random.Random(11)`) where every request
/// stays pending: the waiting fields and the metrics, and the metrics after a second call at the
/// same minute, which adds nothing. The digest of their texts is the one the pytest took from the
/// Python rule before it went (lane L10).
#[test]
fn requests_that_stay_pending_are_the_python_rules() {
    let data = data();
    let service = service(&data, None);
    let rounds = data["tests"]["pending_rounds"].as_array().unwrap();
    assert_eq!(rounds.len(), 30);
    let mut texts = Vec::new();
    for name in rounds {
        let mut rig = rig(&data, name.as_str().unwrap());
        assert_eq!(rig.assign(&service, 0), Vec::<Arc<str>>::new());
        let first = Dumps::default().text(&V::List(vec![waiting_fields(&rig), rig.metric_state()]));
        assert_eq!(rig.assign(&service, 0), Vec::<Arc<str>>::new());
        texts.push(first + &Dumps::default().text(&rig.metric_state()));
    }
    assert_eq!(digest(&texts.join("\n")), "9762d43e25ebc4eb");
}

#[test]
fn the_wait_counts_staff_off_shift_away_and_busy_as_the_python_rule_does() {
    let data = data();
    let mut rig = rig(&data, "off_shift");
    assert_eq!(rig.assign(&service(&data, None), 0), Vec::<Arc<str>>::new());
    assert_eq!(rig.metric("support_assignment_metrics_minute"), V::Float(0.0));
    assert_eq!(rig.metric("support_wait_staff_off_shift_request_minutes"), V::Float(1.0));
    assert_eq!(rig.metric("support_wait_staff_busy_request_minutes"), V::Float(1.0));
    assert_eq!(rig.metric("support_no_qualified_worker_request_minutes"), V::Float(0.0));
}

/// A free worker may take the request: the plain case changes nothing, and the assignment (no
/// route is known) takes it where it stands.
#[test]
fn a_free_worker_goes_past_the_plain_kernel() {
    let data = data();
    let mut rig = rig(&data, "free_worker");
    let before = rig.parts();
    assert!(!rig.plain(0));
    assert!(rig.parts() == before);
    assert_eq!(rig.assign(&service(&data, None), 0), vec![Arc::<str>::from("support-1")]);
}

/// The plain case decides nothing at another minute than the world's, or with an int priority of
/// the LLM (a bool one is no priority).
#[test]
fn what_the_plain_kernel_doesnt_decide_goes_to_assign_work() {
    let data = data();
    let mut rig = rig(&data, "all_busy");
    let before = rig.parts();
    assert!(!rig.plain(1));
    assert!(rig.parts() == before);
    let mut rig = self::rig(&data, "priority_int");
    let before = rig.parts();
    assert!(!rig.plain(0));
    assert!(rig.parts() == before);
    assert!(self::rig(&data, "priority_bool").plain(0));
    assert!(self::rig(&data, "all_busy").plain(0));
}

// ---- What the metrics read -----------------------------------------------------------------------

/// A head of each care kind, with every worker of one skill busy: nobody is qualified unless the
/// skill is among the kind's (the table's, or "support" for a kind it does not name; emotional
/// support without a head).
#[test]
fn each_kind_asks_for_the_skills_of_the_table() {
    let data = data();
    let kinds = &data["tests"]["kinds"];
    let skills = kinds["skills"].as_array().unwrap();
    let heads = kinds["heads"].as_object().unwrap();
    assert_eq!(heads.len(), 10);
    for (kind, head) in heads {
        for (index, name) in skills.iter().enumerate() {
            let mut entry = cases::resolve(&data["worlds"], name.as_str().unwrap());
            if kind != "emotional_support" {
                entry["world"]["model_state"][SHARED]["support_requests"][head["request_id"].as_str().unwrap()] = head.clone();
            }
            let mut rig = rig_of(entry);
            let V::List(answer) = rig.snapshot(0) else { unreachable!() };
            let qualifies = kinds["qualifies"][kind][index].as_bool().unwrap();
            assert_eq!(answer[1] == V::List(Vec::new()), qualifies, "{kind} {name}");
        }
    }
}

/// 120 rounds of random staff (Python's `random.Random(23)`): busy, on another operation, resting,
/// off shift; requests pending, in progress or on the way. The digest of the snapshots' texts is
/// the one the pytest took from the Python rule before it went (lane L10).
#[test]
fn the_reasons_are_the_python_rules_on_random_staff() {
    let data = data();
    let rounds = data["tests"]["reasons_rounds"].as_array().unwrap();
    assert_eq!(rounds.len(), 120);
    let mut answers = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for name in rounds {
        let mut rig = rig(&data, name.as_str().unwrap());
        let minute = rig.entry["world"]["minute_index"].as_i64().unwrap();
        let answer = rig.snapshot(minute);
        let V::List(parts) = &answer else { unreachable!() };
        let V::Map(details) = &parts[0] else { unreachable!() };
        for detail in details.values() {
            let V::Map(detail) = detail else { unreachable!() };
            let V::List(reasons) = &detail["reason_codes"] else { unreachable!() };
            seen.extend(reasons.iter().map(|reason| format!("{reason:?}")));
        }
        answers.push(Dumps::default().text(&answer));
    }
    assert_eq!(digest(&answers.join("\n")), "a4fb2a9f6acf083b");
    for reason in ["staff_busy", "staff_on_other_operation", "staff_resting", "resident_travel", "support_in_progress"] {
        assert!(seen.contains(&format!("{:?}", V::Str(Arc::from(reason)))), "{reason}");
    }
}

/// A free worker's route is read for the wait; the digest the pytest took from the Python rule.
#[test]
fn a_free_worker_s_route_is_read_as_the_python_rule_reads_it() {
    let data = data();
    let mut rig = rig(&data, "free_worker");
    let answer = rig.snapshot(0);
    let V::List(parts) = &answer else { unreachable!() };
    assert_ne!(parts[0], V::Map(Default::default()));
    assert_eq!(digest(&Dumps::default().text(&answer)), "af5c0d0aa04d98b8");
}

// ---- The assignment of a request (test_handler_rest_rules.py) ------------------------------------

/// `_digest(_bytes(world))` of test_handler_rest_rules.py: the world's canonical text as a JSON
/// string (`json.dumps(text, sort_keys=True, ensure_ascii=False)`).
fn world_digest(rig: &Rig) -> String {
    let text = rig.cell.with(|world| canonical_json(world, &rig.entry["world"]["rng_state"])).unwrap();
    digest(&Dumps::default().unicode().sorted().text(&V::Str(Arc::from(String::from_utf8(text).unwrap()))))
}

/// The world after the assignment is the one the Python rule left: the digest the pytest took
/// before it went (lane L10).
#[test]
fn a_free_worker_takes_a_request_as_in_python() {
    let data = data();
    let mut rig = rig(&data, "staffed_request");
    let assigned = rig.assign(&service(&data, None), 10);
    assert_eq!(assigned, vec![Arc::<str>::from("support-t1")]);
    let V::List(rows) = rig.part("support_requests") else { unreachable!() };
    assert!(matches!(&rows[0], V::Map(row) if row["status"] == V::Str(Arc::from("assigned"))));
    assert_eq!(world_digest(&rig), "b2e57bbfee3b75ed");
}

#[test]
fn an_old_request_is_promoted_as_in_python() {
    let data = data();
    let mut rig = rig(&data, "staffed_old_request");
    assert_eq!(rig.assign(&service(&data, Some(3)), 10), vec![Arc::<str>::from("support-t1")]);
    assert!(matches!(rig.metric("support_fairness_promoted_assignment_count"), V::Float(count) if count >= 1.0));
    assert_eq!(world_digest(&rig), "d00be4ea83ec3799");
}

#[test]
fn nobody_free_leaves_the_plain_kernel_running() {
    let data = data();
    assert!(rig(&data, "staffed_nobody_free").plain(10));
}

// ---- record_shared_operation ---------------------------------------------------------------------

/// `json.dumps(world.model_state[SHARED])` of the tree the pytest kept: the row's document, the
/// namespace's keys in the order Python made the tree (the fixture's `shared_keys`), and what the
/// kernel wrote into it in the row's order, sorted (`_same_receipt` of the pytest).
fn shared_text(rig: &Rig, data: &Value) -> String {
    let V::Map(mut state) = rig.namespace(SHARED) else { unreachable!() };
    let keys = data["tests"]["shared_keys"].as_array().unwrap();
    assert_eq!(keys.len(), state.len());
    let ordered = keys.iter().map(|key| {
        let key = key.as_str().unwrap();
        (Arc::from(key), state.shift_remove(key).unwrap())
    });
    Dumps::default().text(&V::Map(ordered.collect()))
}

fn text_of(error: Error) -> String {
    match error {
        Error::Value(text) => text,
        other => panic!("not a ValueError: {other:?}"),
    }
}

/// Four receipts of one request, one minute apart: each head kept in the row until the request
/// completes, and the history of the four. The stress after it is a float of the run, pinned by
/// the digest of the four states (with sorted keys).
///
/// The digest was recorded from the core in #225 lane M4: the pytest's was of the Python tree,
/// whose heads have the contract's key order, which the row does not keep. On the same kernel the
/// pytest `test_staff_rules.py::test_a_request_s_receipts_are_the_python_rules_and_kept_in_the_row`
/// with digest `f685064e884a` passed, and its four trees written with sorted keys give this digest.
#[test]
fn a_request_s_receipts_are_the_python_rules_and_kept_in_the_row() {
    let data = data();
    let mut rig = rig(&data, "plain");
    let mut states = Vec::new();
    for (step, status) in data["tests"]["request_receipts"].as_array().unwrap().iter().zip(["requested", "assigned", "started", "completed"]) {
        rig.set_minute(step["minute"].as_i64().unwrap());
        rig.record(step).unwrap();
        states.push(Dumps::default().sorted().text(&rig.namespace(SHARED)));
        let V::Map(state) = rig.namespace(SHARED) else { unreachable!() };
        let V::Map(heads) = &state["support_requests"] else { unreachable!() };
        let head = heads.get("request-1").map(|head| match head {
            V::Map(head) => head["status"].clone(),
            _ => unreachable!(),
        });
        assert_eq!(head, (status != "completed").then(|| V::Str(Arc::from(status))));
    }
    let V::Map(state) = rig.namespace(SHARED) else { unreachable!() };
    assert_eq!(state["support_requests"], V::Map(Default::default()));
    assert!(matches!(&state["history"], V::List(items) if items.len() == 4));
    assert_eq!(digest(&states.join("\n")), "4dff2f9f77abe03a");
}

#[test]
fn a_receipt_the_rule_refuses_changes_nothing_and_has_its_text() {
    let data = data();
    let first = &data["tests"]["refused"]["first"];
    let mut rig = rig(&data, "plain");
    rig.set_minute(9);
    let before = shared_text(&rig, &data);
    assert_eq!(text_of(rig.record(first).unwrap_err()), "shared operation receipt must use the current simulation boundary minute");
    assert_eq!(shared_text(&rig, &data), before);
    let mut rig = self::rig(&data, "plain");
    rig.set_minute(3);
    rig.record(first).unwrap();
    let before = shared_text(&rig, &data);
    assert_eq!(text_of(rig.record(first).unwrap_err()), "shared operation event ID has already been recorded");
    assert_eq!(shared_text(&rig, &data), before);
}

#[test]
fn an_order_or_identity_the_rule_refuses_is_its_own() {
    let data = data();
    let refused = &data["tests"]["refused"];
    let mut rig = rig(&data, "plain");
    rig.set_minute(3);
    rig.record(&refused["first"]).unwrap();
    for (name, part) in [("completed", "out of order"), ("other_request", "out of order"), ("other_agent", "identity changed")] {
        let text = text_of(rig.record(&refused[name]).unwrap_err());
        assert!(text.contains(part), "{name}: {text}");
    }
}

#[test]
fn a_state_change_that_is_not_the_world_s_is_the_rules_to_refuse() {
    let data = data();
    let given = &data["tests"]["state_change"];
    let mut rig = rig(&data, "plain");
    rig.set_minute(3);
    for logged in given["before"].as_array().unwrap() {
        rig.record(logged).unwrap();
    }
    let text = text_of(rig.record(&given["completed"]).unwrap_err());
    assert!(text.contains("does not match the current saved"), "{text}");
}

/// The digest of the document the rule left, recorded from it (and again with the row's key
/// order, in #152 P8b-1); a second receipt of the event is turned down.
#[test]
fn a_facility_s_recovery_receipt_is_the_rule_s() {
    let data = data();
    let recovery = &data["tests"]["recovery"];
    let mut rig = rig(&data, "plain");
    rig.set_minute(3);
    rig.record(recovery).unwrap();
    assert_eq!(digest(&shared_text(&rig, &data)), "60ddeee8a3409092");
    let before = shared_text(&rig, &data);
    assert_eq!(text_of(rig.record(recovery).unwrap_err()), "shared operation event ID has already been recorded");
    assert_eq!(shared_text(&rig, &data), before);
}

#[test]
fn a_recovery_receipt_of_another_state_is_left_to_the_rule_s_text() {
    let data = data();
    let mut rig = rig(&data, "recovery_degraded");
    let before = shared_text(&rig, &data);
    let text = text_of(rig.record(&data["tests"]["recovery"]).unwrap_err());
    assert_eq!(text, "facility recovery receipt does not match the applied facility state");
    assert_eq!(shared_text(&rig, &data), before);
}
