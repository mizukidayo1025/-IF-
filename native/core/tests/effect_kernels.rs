//! The handlers and the guard of an EffectProgram (`rules::effects`) on the programs of a world
//! (Issue #225, lane M7; moved from `tests/integration/test_effect_kernels.py`).
//!
//! The worlds are the generated Scenario the Project screen makes, with each test's program
//! scheduled by `schedule_effect_program` in Python (`generated_texts.json`'s `kernel_*`,
//! `tests/support/core_fixtures_kernels.py`), and the native host's contracts of their parameters
//! as recorded there (`support/contracts.rs`). Each handler runs the kernel its PyO3 entry calls
//! (`effects::facility_event` and the others), with the host of a Run's world
//! (`load::following_host`).
//!
//! An outcome is the class and text the deleted Python rule raised (written down from it: lane L10,
//! `kernel_only`), with the world's canonical bytes unchanged. A zone condition's start leaves the
//! world the pytest file expected (`kernel_zone_<op>_started`, built in Python), and its expiry the
//! world before it. A success the pytest file compared with a digest of the Python rule's whole
//! world and receipts (`json.dumps(world.to_dict())`, a dict's order and Python's objects) is
//! compared with a digest recorded from the core in #225 lane M7: the sha256 of the world's
//! canonical JSON (`canonical_json()`), a newline, and the receipts' records as the log writes them,
//! as a JSON list. On the same kernel the pytest digests passed (named at each value), and the PyO3
//! entries gave these digests on the worlds the pytest file built (checked when they were recorded).
//! What the pytest file checked by name besides the digest is checked here by name too.

mod support;

use std::rc::Rc;

use serde_json::{json, Value};
use sheltersim_core::error::Error;
use sheltersim_core::events::EventStatus;
use sheltersim_core::rules::care_receipts::Made;
use sheltersim_core::rules::effects::{self, GuardIn, Structure};
use sheltersim_core::rules::kit;
use sheltersim_core::world::{Fit, World, WorldCell};

use support::canonical::parts;
use support::load::{self, Loaded};
use support::texts::{self, big, changed, sha};
use support::{cases, contracts, TestHost};

const FILE: &str = "generated_texts";
const KEY: &str = "effect_program_v1";
const START: i64 = 5;
const END: i64 = 30;
const FAILED: &str = "EventApplicationError";
const SUPPRESSED: &str = "EventSuppressed";

/// A world of the fixture, loaded with the host of a Run's world and the recorded contracts.
struct Case {
    entry: Value,
    loaded: Rc<Loaded>,
    host: TestHost,
}

fn case(name: &str) -> Case {
    let entry = cases::entry(FILE, name);
    let loaded = Rc::new(load::loaded(&entry));
    let mut host = load::following_host(&entry, &loaded);
    // The contracts are the variant's own key, which `cases::entry` (the world made again) drops.
    if let Some(asked) = load::data(FILE)["worlds"][name].get("contracts") {
        host.contracts = Some(contracts::recorded(asked));
    }
    Case { entry, loaded, host }
}

/// The handlers, by the class of the pytest file.
#[derive(Clone, Copy, Debug)]
enum Handler {
    Facility { expire: bool },
    Environment { expire: bool },
    TemporarySpace { expire: bool },
    Structure,
}

use Handler::*;

impl Case {
    fn edit<T>(&self, edit: impl FnOnce(&mut World) -> T) -> T {
        self.loaded.cell.with(edit).unwrap()
    }

    fn bytes(&self) -> String {
        texts::canonical_of(&self.loaded.cell, &self.entry)
    }

    /// The world's canonical document, as JSON.
    fn document(&self) -> Value {
        serde_json::from_str(&self.bytes()).unwrap()
    }

    fn effect_state(&self) -> Value {
        self.document()["model_state"][KEY].clone()
    }

    /// A facility's row of the document.
    fn facility(&self, id: &str) -> Value {
        self.document()["facilities"].as_array().unwrap().iter().find(|row| row["id"] == id).cloned().unwrap_or_else(|| panic!("no facility {id}"))
    }

    /// The IDs of a table of the fixture's world, sorted.
    fn sorted_ids(&self, table: &str) -> Vec<String> {
        let mut ids: Vec<String> = self.entry["world"][table].as_array().unwrap().iter().map(|row| row["id"].as_str().unwrap().to_owned()).collect();
        ids.sort();
        ids
    }

    fn table_len(&self, table: &str) -> usize {
        self.document()[table].as_array().unwrap().len()
    }

    /// `handler.apply(world, event)`: the receipts of a structural change.
    fn apply(&mut self, handler: Handler, event: &Value) -> Result<Vec<Made>, Error> {
        let row = texts::row(event);
        let (cell, host) = (&self.loaded.cell, &mut self.host);
        let one = |made: Option<Made>| made.into_iter().collect();
        match handler {
            Facility { expire } => effects::facility_event(cell, host, &row, expire).map(|()| Vec::new()),
            Environment { expire } => effects::environment_event(cell, host, &row, expire).map(|()| Vec::new()),
            TemporarySpace { expire } => effects::temporary_space_event(cell, host, &row, expire).map(|()| Vec::new()),
            Structure => match effects::structure_of(&row)? {
                Structure::TaskOrder => effects::task_order_event(cell, host, &row).map(|()| Vec::new()),
                Structure::Handoff => effects::handoff_event(cell, host, &row).map(one),
                Structure::Repair => effects::repair_event(cell, host, &row).map(one),
                Structure::Role => effects::role_event(cell, host, &row).map(|()| Vec::new()),
                Structure::Add => effects::add_event(cell, host, &row).map(|_| Vec::new()),
            },
        }
    }

    /// `_python_s_outcome`: the handler raises `class` with `expected`, and the world did not change.
    fn says(&mut self, handler: Handler, event: &Value, class: &str, expected: &str) {
        let before = self.bytes();
        let said = texts::outcome(self.apply(handler, event));
        assert_eq!((said.0, said.1.as_str()), (class, expected), "{handler:?} {event}");
        assert!(self.bytes() == before, "the world changed: {handler:?} {event}");
    }

    /// `_refused`: a value outside the contract, Unsupported with nothing changed.
    fn refused(&mut self, handler: Handler, event: &Value) {
        let before = self.bytes();
        match self.apply(handler, event) {
            Err(Error::Unsupported(_)) => {}
            other => panic!("not refused: {:?} {event}", other.map(|made| made.len())),
        }
        assert!(self.bytes() == before, "the world changed: {handler:?} {event}");
    }

    /// The handler applies, and the world and its receipts are the digest's (`_as_recorded`).
    fn recorded(&mut self, handler: Handler, event: &Value, digest: &str) {
        let made = self.apply(handler, event).unwrap_or_else(|error| panic!("{handler:?}: {error:?} {event}"));
        let records: Vec<String> = made
            .iter()
            .map(|made| String::from_utf8(made.event.record_json(made.minute, EventStatus::Applied, Some(&made.detail)).unwrap().into_vec()).unwrap())
            .collect();
        let text = format!("{}\n[{}]", self.bytes(), records.join(","));
        assert_eq!(sha(&text), digest, "{handler:?} {event}");
    }

    fn pending(&self, event_type: &str, op: Option<&str>) -> Value {
        texts::pending(&self.entry, event_type, op)
    }

    /// The pending events of a type, in the queue's order.
    fn all_pending(&self, event_type: &str) -> Vec<Value> {
        self.entry["world"]["event_queue"].as_array().unwrap().iter().filter(|event| event["event_type"] == event_type).cloned().collect()
    }

    fn has_active_modifier(&mut self, facility: &str) -> bool {
        let holding = kit::holding(&self.loaded.cell, &mut self.host).unwrap();
        let program = holding.held(KEY, "another state");
        self.edit(|world| effects::has_active_modifier(world, &program, facility)).unwrap()
    }

    /// `should_apply_effect_guard` of an event.
    fn guard(&mut self, event: &Value) -> Result<bool, Error> {
        let holding = kit::holding(&self.loaded.cell, &mut self.host).unwrap();
        let (program, ledger) = (holding.held(KEY, "another state"), holding.held("resource_ledger_v1", "another ledger"));
        effects::guard(&self.loaded.cell, &mut self.host, &GuardIn { program: &program, ledger: &ledger }, &texts::row(event))
    }
}

/// The world's canonical parts but `rng_state`.
fn parts_of(case: &Case) -> Vec<(&'static str, Vec<u8>)> {
    case.loaded.cell.with(|world| parts(world)).unwrap()
}

fn payload(event: &Value, changes: Value) -> Value {
    changed(event, changes, json!({}))
}

fn toilet(case: &Case) -> String {
    let document = case.document();
    let mut toilets: Vec<String> = document["facilities"].as_array().unwrap().iter().filter(|row| row["kind"] == "toilet").map(|row| row["id"].as_str().unwrap().to_owned()).collect();
    toilets.sort();
    toilets.remove(0)
}

// ---- The handlers ----------------------------------------------------------------------------

#[test]
fn a_facility_modifier_starts_stacks_and_expires() {
    let mut case = case("kernel_facility_stack");
    let facility = toilet(&case);
    let starts = case.all_pending("effect_program_apply");
    let expiries = case.all_pending("effect_program_expire");
    let (apply, expire) = (Facility { expire: false }, Facility { expire: true });
    let baseline = case.facility(&facility)["working_units"].clone();
    // Expiring what did not start is suppressed, with the Python rule's text.
    case.says(expire, &expiries[0], SUPPRESSED, "facility availability modifier is not active");
    // Recorded from the core in #225 lane M7; on the same kernel the pytest digest 7eafd5f13bd8 passed.
    case.recorded(apply, &starts[0], "29d23a793d04d4f0550830827501f1be57226d21e46c4225e5df9cdf3f342a3e");
    // The first modifier captures the baseline; the facility works the lowest of the stack.
    assert_eq!(
        case.effect_state()["active_facility_modifiers"][&facility],
        json!({
            "baseline_working_units": baseline,
            "modifiers": [{
                "modifier_id": starts[0]["payload"]["modifier_id"],
                "program_id": "program-1",
                "effect_id": "first",
                "working_units": 0,
                "start_minute": START,
                "sequence": starts[0]["sequence"],
            }],
        })
    );
    assert_eq!(case.facility(&facility)["working_units"], 0);
    // Recorded from the core in #225 lane M7; on the same kernel the pytest digest f28c32edc575 passed.
    case.recorded(apply, &starts[1], "6aa85deed8a5a583d6827a8c2efc1038e8a7b643c0bd83a48ba6eb1560f1ae75");
    // A modifier already active is suppressed, with the Python rule's text.
    case.says(apply, &starts[1], SUPPRESSED, "facility availability modifier is already active");
    assert_eq!(case.effect_state()["active_facility_modifiers"][&facility]["modifiers"].as_array().unwrap().len(), 2);
    assert!(case.has_active_modifier(&facility));
    // Recorded from the core in #225 lane M7; on the same kernel the pytest digest fc8c8d9822a1 passed.
    case.recorded(expire, &expiries[0], "3c9d56373f8ed92e14eae4a721b7e649245d134401f5078d6ec9ff346020f652");
    assert_eq!(case.effect_state()["active_facility_modifiers"][&facility]["modifiers"].as_array().unwrap().len(), 1);
    assert_eq!(case.facility(&facility)["working_units"], 0);
    // Recorded from the core in #225 lane M7; on the same kernel the pytest digest 733a34c00d68 passed.
    case.recorded(expire, &expiries[1], "ef4678bda5230509636c3b852db1f8b3b37eff794764a77ffbac0eed2b706e27");
    // The last expiry takes the stack away and gives back the baseline.
    assert_eq!(case.facility(&facility)["working_units"], baseline);
    assert!(case.effect_state()["active_facility_modifiers"].get(&facility).is_none());
    assert!(!case.has_active_modifier(&facility));
}

#[test]
fn a_facility_event_that_fails_says_python_s_text_or_is_refused() {
    // A value the rule turns down is its text; an ID that is not a str is outside the contract.
    let units = "working_units must be an integer";
    let capacity = "facility availability modifier violates capacity or occupancy";
    let scheduled = "effect program event does not reference a scheduled effect";
    let cases: [(&str, Value, Option<&str>); 11] = [
        ("working_units", json!(true), Some(units)),
        ("working_units", json!(1.0), Some(units)),
        ("working_units", json!("0"), Some(units)),
        ("working_units", json!(10_000), Some(capacity)),
        ("working_units", json!(-1), Some(capacity)),
        ("facility_id", json!("no-such-facility"), Some("effect program references an unknown facility")),
        ("facility_id", json!(1), None),
        ("program_id", json!("no-such-program"), Some(scheduled)),
        ("effect_id", json!("no-such-effect"), Some(scheduled)),
        ("modifier_id", json!(1), None),
        ("unknown_field", json!(1), Some("effect program apply payload fields do not match")),
    ];
    for (change, value, text) in cases {
        let mut case = case("kernel_facility");
        let mut changes = serde_json::Map::new();
        changes.insert(change.to_owned(), value);
        let event = payload(&case.pending("effect_program_apply", None), Value::Object(changes));
        match text {
            None => case.refused(Facility { expire: false }, &event),
            Some(text) => case.says(Facility { expire: false }, &event, FAILED, text),
        }
    }
}

/// The zone conditions of the pytest file.
const ZONE_OPERATORS: [&str; 6] = ["modify_demand_rate", "set_ambient_noise", "set_lighting", "set_privacy", "set_temperature", "set_ventilation"];

#[test]
fn a_zone_condition_starts_and_expires() {
    for op in ZONE_OPERATORS {
        let mut case = case(&format!("kernel_zone_{op}"));
        let start = case.pending("effect_program_environment", None);
        let expiry = case.pending("effect_program_environment_expire", None);
        let (apply, expire) = (Environment { expire: false }, Environment { expire: true });
        let before = parts_of(&case);
        case.says(expire, &expiry, SUPPRESSED, "zone environment modifier is not active");
        case.apply(apply, &start).unwrap_or_else(|error| panic!("{op}: {error:?}"));
        // The start writes its modifier's record and nothing else: the world the pytest file made.
        let expected = load::loaded(&cases::entry(FILE, &format!("kernel_zone_{op}_started"))).cell.with(|world| parts(world)).unwrap();
        assert!(parts_of(&case) == expected, "{op}");
        case.says(apply, &start, SUPPRESSED, "zone environment modifier is already active");
        // The expiry takes it away again.
        case.apply(expire, &expiry).unwrap_or_else(|error| panic!("{op}: {error:?}"));
        assert!(parts_of(&case) == before, "{op}");
    }
}

#[test]
fn a_temporary_space_starts_and_expires() {
    let mut case = case("kernel_temporary_space");
    let zone = case.sorted_ids("zones").remove(0);
    let start = case.pending("effect_program_temporary_space", None);
    let expiry = case.pending("effect_program_temporary_space_expire", None);
    let (apply, expire) = (TemporarySpace { expire: false }, TemporarySpace { expire: true });
    let not_started = "temporary-space effect did not start or does not match its expiry";
    case.says(expire, &expiry, SUPPRESSED, not_started);
    // Recorded from the core in #225 lane M7; on the same kernel the pytest digest 00662fd40043 passed.
    case.recorded(apply, &start, "cbf4dc30d10ecc5ff2a026f1b6a9f9062666c3e70c537714cacf30f96aa9cd66");
    let modifier = start["payload"]["modifier_id"].as_str().unwrap().to_owned();
    let space = case.document()["model_state"]["temporary_spaces_v1"]["spaces"][&modifier].clone();
    let fields = ["zone_id", "people_capacity", "sleeping_capacity", "start_minute", "end_minute", "active", "start_event_id", "end_event_id", "geometry_status"];
    let read: Value = fields.iter().map(|name| (name.to_string(), space[*name].clone())).collect::<serde_json::Map<_, _>>().into();
    assert_eq!(
        read,
        json!({
            "zone_id": zone,
            "people_capacity": 5,
            "sleeping_capacity": 2,
            "start_minute": START,
            "end_minute": END,
            "active": true,
            "start_event_id": start["event_id"],
            "end_event_id": null,
            "geometry_status": "zone_only_capacity",
        })
    );
    let expiry = changed(&expiry, json!({}), json!({"causation_id": start["event_id"]}));
    // Recorded from the core in #225 lane M7; on the same kernel the pytest digest a113537dbdfe passed.
    case.recorded(expire, &expiry, "32a73349120a8bfde4de21610c520f00a811b43ce864aa109f2960ed03dd9c5d");
    let spaces = case.document()["model_state"]["temporary_spaces_v1"]["spaces"].clone();
    assert_eq!(spaces.as_object().unwrap().len(), 1);
    assert_eq!(spaces[&modifier]["active"], false);
}

#[test]
fn a_new_member_or_toilet_is_the_python_rule_s() {
    // Recorded from the core in #225 lane M7; on the same kernel the pytest digests b36fac765c95
    // (add_staff) and 47fc03365bbc (add_toilet) passed.
    for (op, digest) in [("add_staff", "be51a7abe81e6a3f0237a3cbd4518eeae5148da8172bd81511c1e7d908fab1ba"), ("add_toilet", "143fabffe1e16bab0a08373a3edf045ba822e4df65d378d98729b6ddbbebf2ef")] {
        let mut case = case(&format!("kernel_{op}"));
        let zone = case.sorted_ids("zones").remove(0);
        let event = case.pending("effect_program_structure", Some(op));
        let (staff, facilities) = (case.sorted_ids("staff"), case.sorted_ids("facilities"));
        case.recorded(Structure, &event, digest);
        let suffix = event["payload"]["modifier_id"].as_str().unwrap().strip_prefix("modifier-").unwrap().to_owned();
        let document = case.document();
        if op == "add_staff" {
            let added: Vec<String> = (1..=2).map(|index| format!("effect-staff-{suffix}-{index}")).collect();
            let now: Vec<String> = document["staff"].as_array().unwrap().iter().map(|row| row["id"].as_str().unwrap().to_owned()).filter(|id| !staff.contains(id)).collect();
            assert_eq!(now, added);
            for row in document["staff"].as_array().unwrap().iter().filter(|row| added.iter().any(|id| row["id"] == **id)) {
                assert_eq!((&row["zone_id"], &row["skills"]), (&json!(zone), &json!(["support"])));
            }
        } else {
            let added: Vec<String> = (1..=2).map(|index| format!("effect-toilet-{suffix}-{index}")).collect();
            let now: Vec<String> = document["facilities"].as_array().unwrap().iter().map(|row| row["id"].as_str().unwrap().to_owned()).filter(|id| !facilities.contains(id)).collect();
            assert_eq!(now, added);
            for id in &added {
                let row = case.facility(id);
                assert_eq!((&row["kind"], &row["zone_id"], &row["capacity"], &row["working_units"]), (&json!("toilet"), &json!(zone), &json!(1), &json!(1)));
            }
        }
    }
}

#[test]
fn past_the_bound_of_members_is_the_rule_s_text() {
    let mut case = case("kernel_add_staff");
    let event = payload(&case.pending("effect_program_structure", Some("add_staff")), json!({"count": 1000}));
    case.says(Structure, &event, FAILED, "adding staff would exceed the 100-staff runtime bound");
}

#[test]
fn the_bound_of_an_addition_is_the_python_rule_s() {
    // Recorded from the core in #225 lane M7; on the same kernel the pytest digests 73dfcd9ed2b1
    // (add_staff) and 3f29f03a3ff7 (add_toilet) passed.
    let bounds = [
        ("add_staff", "staff", 100, "adding staff would exceed the 100-staff runtime bound", "2f4b05c3a25c6460490f24fa5c1170e764eb5d744e0cacf33c2090c05e0f4f32"),
        ("add_toilet", "facilities", 200, "adding toilets would exceed the 200-facility bound", "a4fc227ec5caa77f7aee3ed0c7d4cf0b1f6be42d5f5e980ff9d1dc79d5bd79b1"),
    ];
    for (op, table, bound, text, digest) in bounds {
        let name = format!("kernel_bound_{op}");
        let mut case = case(&name);
        let event = case.pending("effect_program_structure", Some(op));
        let room = bound - case.table_len(table);
        case.says(Structure, &payload(&event, json!({"count": room + 1})), FAILED, text);
        case.recorded(Structure, &payload(&event, json!({"count": room})), digest);
        assert_eq!(case.table_len(table), bound);
    }
}

#[test]
fn a_role_change_is_the_same() {
    // The contract's own text of skills it turns down stays in the pytest file (pydantic's).
    let mut case = case("kernel_role");
    let staff_id = case.sorted_ids("staff").remove(0);
    let event = case.pending("effect_program_structure", Some("set_staff_role"));
    // Recorded from the core in #225 lane M7; on the same kernel the pytest digest 605a5afbb934 passed.
    case.recorded(Structure, &event, "5ca1db24f8145d46587fbe3660b822ad95ee5932b9586b22bd96bf42bcc038d0");
    let document = case.document();
    let row = document["staff"].as_array().unwrap().iter().find(|row| row["id"] == *staff_id).unwrap();
    assert_eq!(row["skills"], json!(["alpha", "zeta"]));
}

#[test]
fn a_repair_is_the_same_and_checks_what_it_expects() {
    let mut case = case("kernel_repair");
    let facility = toilet(&case);
    let capacity = case.facility(&facility)["capacity"].as_i64().unwrap();
    let event = case.pending("effect_program_structure", Some("facility_repair"));
    // Recorded from the core in #225 lane M7; on the same kernel the pytest digest 0d3e34f6f0c3 passed.
    case.recorded(Structure, &event, "07c19f628c03475eb064e7abea622910c08278338eef93d13ab5723030c13c8e");
    assert_eq!(case.facility(&facility)["working_units"], capacity);
    let mut stale = self::case("kernel_repair");
    stale.edit(|world| world.rows_mut().unwrap().facilities.get_mut(&facility).unwrap().working_units = capacity);
    stale.says(Structure, &event, FAILED, "facility repair preconditions no longer match the live facility state");
}

#[test]
fn an_order_is_the_same_and_only_for_its_minute() {
    let mut case = case("kernel_order");
    let event = case.pending("effect_program_structure", Some("staff_task_order"));
    let staff_id = event["payload"]["staff_id"].as_str().unwrap().to_owned();
    // Recorded from the core in #225 lane M7; on the same kernel the pytest digest 89422db60646 passed.
    case.recorded(Structure, &event, "01ba3afaab1ce691c260674cb4a0702618202b5e0239897f84b5218dd265d597");
    let document = case.document();
    let row = document["staff"].as_array().unwrap().iter().find(|row| row["id"] == *staff_id).unwrap();
    assert!(!row["active_task_id"].is_null());
    let mut stale = self::case("kernel_order");
    let minute = stale.entry["world"]["minute_index"].as_i64().unwrap();
    let late = changed(&event, json!({}), json!({"due_minute": minute + 1}));
    stale.says(Structure, &late, FAILED, "staff task order does not reference a current approved program");
}

#[test]
fn a_handoff_without_its_program_is_the_rule_s_text() {
    // The whole of a handoff is compared in the gate's scenario, where a task is under way.
    let mut case = case("generated");
    let staff = case.sorted_ids("staff");
    let event = json!({
        "event_id": "handoff",
        "kind": "intervention",
        "due_minute": 1,
        "priority": 0,
        "sequence": 999_999,
        "event_type": "effect_program_structure",
        "payload": {
            "program_id": "program-1",
            "effect_id": "effect-1",
            "modifier_id": "modifier-1",
            "op": "staff_handoff",
            "staff_id": staff[0],
            "parameters": {"staff_id": staff[0], "replacement_staff_id": staff[1], "task_id": "no-task"},
        },
        "source_id": null,
        "causation_id": null,
        "provenance": {KEY: {"program_id": "program-1", "effect_id": "effect-1"}},
    });
    case.says(Structure, &event, FAILED, "staff handoff does not reference an approved program");
}

#[test]
fn a_structure_event_with_other_fields_is_the_rule_s_text() {
    let mut case = case("kernel_add_staff");
    let event = payload(&case.pending("effect_program_structure", Some("add_staff")), json!({"other": 1}));
    case.says(Structure, &event, FAILED, "structural primitive payload fields do not match");
}

// ---- The guard --------------------------------------------------------------------------------

/// `_guarded`: an event whose marker has the guard, of no program.
fn guarded(guard: Value, payload: Value) -> Value {
    json!({
        "event_id": "guarded-event",
        "kind": "intervention",
        "due_minute": 1,
        "priority": 0,
        "sequence": 999_999,
        "event_type": "effect_program_apply",
        "payload": payload,
        "source_id": null,
        "causation_id": null,
        "provenance": {KEY: {"guard": guard, "program_id": "no-such-program"}},
    })
}

#[test]
fn a_large_int_and_a_float_or_a_wider_int_compare_as_python_s() {
    // Python compares an int with a float, or with an int past 64 bits, exactly. The answers are
    // those of the Python rule (`_guard_answer` of the pytest file restated it).
    let two_53 = 1_i64 << 53;
    let two_62 = 1_i64 << 62;
    let two_70 = || big("1180591620717411303424");
    let minus_two_70 = || big("-1180591620717411303424");
    let cases: Vec<(i64, &str, Value, bool)> = vec![
        // A cast to f64 would round 2**53 + 1 to 2**53.
        (two_53 + 1, "eq", json!(9007199254740992.0), false),
        (two_53 + 1, "gt", json!(9007199254740992.0), true),
        (two_53, "eq", json!(9007199254740992.0), true),
        (two_53 + 1, "lt", json!(9007199254740994.0), true),
        (5, "lt", json!(5.5), true),
        (5, "gte", json!(5.5), false),
        (-5, "gt", json!(-5.5), true),
        (two_62, "lt", json!(9223372036854775808.0), true),
        (two_62, "lt", json!(1e300), true),
        (two_62, "gt", json!(-1e300), true),
        (two_62, "lt", two_70(), true),
        (two_62, "gt", minus_two_70(), true),
        (two_62, "eq", two_70(), false),
        (two_62, "ne", two_70(), true),
    ];
    let mut case = case("generated");
    for (minute, operator, value, answer) in cases {
        case.edit(|world| world.scalars.get_mut().minute_index = Fit::Ok(minute));
        let event = guarded(json!({"operator": operator, "field": "simulation.minute", "value": value}), json!({}));
        assert_eq!(case.guard(&event).unwrap(), answer, "{minute} {operator} {value}");
    }
}

#[test]
fn an_event_without_a_marker_applies() {
    let mut case = case("generated");
    let mut event = guarded(json!(null), json!({}));
    event["provenance"] = json!({});
    assert!(case.guard(&event).unwrap());
}
