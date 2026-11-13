//! The handlers' kernels and the steps around them, on worlds the Python side built (Issue #225, lane
//! M4; moved from `tests/integration/test_handler_rules.py` and `test_handler_rest_rules.py`).
//!
//! The toilet's queue and service (`rules::toilet`), the care receipts (`rules::care_receipts`), a
//! seek (`_apply_effect`, the propose phase's `apply_alone`), the end of a request or of a staff
//! member's support (`rules::support`), the walk home (`travel::schedule_return`), the arrival at a
//! toilet (`travel_completion::arrive`), the handlers of the operations and of the social events,
//! and the draws of a claim (`information_events`). Each test is the pytest test of the same name,
//! on the world it built (`tests/support/core_fixtures_handlers.py`: the generated Scenario of
//! `generated_scenario` after 5 minutes, edited in Python): the same inputs, and what it expected.
//! Where the pytest file compared the kernel's world with a copy it edited as the deleted Python
//! rule did, that copy is the fixture's `after` world and the core must leave the same bytes
//! (`canonical::parts`); a handler's case goes through `support/cases.rs`.

mod support;

use std::sync::{Arc, OnceLock};

use serde_json::Value;
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::config::information::InformationSpread;
use sheltersim_core::config::movement::Movement;
use sheltersim_core::config::population::AccessibilityNeeds;
use sheltersim_core::config::SectionId;
use sheltersim_core::error::Error;
use sheltersim_core::events::{EventRow, EventStatus};
use sheltersim_core::host::Host;
use sheltersim_core::json::Encoder;
use sheltersim_core::rules::care_receipts::{self, Transition};
use sheltersim_core::rules::common_operations::json_value;
use sheltersim_core::rules::dispatch::{self, Record};
use sheltersim_core::rules::proposals::{Action, Applied, Proposal};
use sheltersim_core::rules::tick::Tick;
use sheltersim_core::rules::{information_events, kit, support as care, tick, toilet, travel, travel_completion};
use sheltersim_core::run::held::Held as Read;
use sheltersim_core::run::Run;
use sheltersim_core::value::V;
use sheltersim_core::world::{World, WorldCell, METRIC_STATE};

use support::cases::{self, check_group};
use support::dumps::Dumps;
use support::load::{self, Loaded};
use support::{canonical, TestHost};

const FILE: &str = "handler_rules";

fn data() -> &'static Value {
    static DATA: OnceLock<Value> = OnceLock::new();
    DATA.get_or_init(|| load::data(FILE))
}

/// The calls of a group, which has `count` of them.
fn calls(group: &str, count: usize) -> &'static [Value] {
    let calls = data()["calls"].get(group).and_then(Value::as_array).unwrap_or_else(|| panic!("no calls {group}"));
    assert_eq!(calls.len(), count, "{group}");
    calls
}

fn entry(name: &Value) -> Value {
    cases::resolve(&data()["worlds"], name.as_str().unwrap())
}

fn s(value: &Value) -> &str {
    value.as_str().unwrap_or_else(|| panic!("not a str: {value}"))
}

type TheTick<'a> = Tick<'a, sheltersim_core::cell::Shard<World>, TestHost, std::cell::RefCell<Run>>;

/// A world of the fixture loaded, with its host (whose `find_record` finds its records).
struct Loading {
    entry: Value,
    loaded: Loaded,
    host: TestHost,
}

impl Loading {
    fn of(name: &Value) -> Self {
        let entry = entry(name);
        let loaded = load::loaded(&entry);
        let host = cases::host(&entry);
        Self { entry, loaded, host }
    }

    /// `f` on a tick of the world at its minute.
    fn tick<T>(&mut self, f: impl FnOnce(&mut TheTick<'_>) -> T) -> T {
        let minute = self.loaded.cell.with(|world| world.minute_index().unwrap()).unwrap();
        let mut tick = Tick {
            cell: &self.loaded.cell,
            host: &mut self.host,
            run: &self.loaded.run,
            policies: self.loaded.policies.clone(),
            minute,
            seed: self.loaded.seed.clone(),
            neighbors: self.loaded.neighbors.clone(),
        };
        f(&mut tick)
    }

    fn parts(&self) -> Vec<(&'static str, Vec<u8>)> {
        self.loaded.cell.with(|world| canonical::parts(world)).unwrap()
    }

    /// `WorldState.canonical_json()` of the world now.
    fn canonical(&self) -> String {
        let bytes = self.loaded.cell.with(|world| canonical::canonical_json(world, &self.entry["world"]["rng_state"])).unwrap();
        String::from_utf8(bytes).unwrap()
    }

    /// The canonical document of the world now.
    fn document(&self) -> Value {
        serde_json::from_str(&self.canonical()).unwrap()
    }

    /// The world is the fixture's world `name`, byte for byte.
    fn is(&self, name: &Value) {
        let wanted = Loading::of(name).parts();
        let now = self.parts();
        let differing: Vec<_> = wanted.iter().zip(&now).filter(|(one, other)| one != other).map(|(one, _)| one.0).collect();
        assert!(differing.is_empty(), "the world differs from {name} in {differing:?}");
    }
}

/// The world's parts did not change.
fn unchanged(before: &[(&'static str, Vec<u8>)], loading: &Loading) {
    let now = loading.parts();
    let differing: Vec<_> = before.iter().zip(&now).filter(|(one, other)| one != other).map(|(one, _)| one.0).collect();
    assert!(differing.is_empty(), "the world changed in {differing:?}");
}

/// The error the fixture names: `[variant, text]` of `core::error::Error`.
fn error_of(error: &Value) -> Error {
    let text = s(&error[1]).to_owned();
    match s(&error[0]) {
        "Value" => Error::Value(text),
        "Invariant" => Error::Invariant(text),
        "Unsupported" => Error::Unsupported(text.into()),
        "Failed" => Error::failed(text),
        other => panic!("not an error: {other}"),
    }
}

// ---- Facts of a document ------------------------------------------------------------------------

/// The value at `path` of a canonical document: a key of a map, a row of a list by its ID (an
/// agent's is its profile's), or an index of a list (from its end when negative).
fn at<'a>(document: &'a Value, path: &[Value]) -> Option<&'a Value> {
    let mut node = document;
    for key in path {
        node = match (node, key) {
            (Value::Object(map), Value::String(key)) => map.get(key)?,
            (Value::Array(items), Value::String(id)) => items.iter().find(|row| {
                let own = row.get("id").or_else(|| row.get("profile").and_then(|profile| profile.get("id")));
                own.and_then(Value::as_str) == Some(id.as_str())
            })?,
            (Value::Array(items), index) => {
                let index = index.as_i64().unwrap();
                let index = if index < 0 { items.len() as i64 + index } else { index };
                items.get(usize::try_from(index).ok()?)?
            }
            _ => return None,
        };
    }
    Some(node)
}

/// Each fact of the list holds: `[path, value]`, or `[path]` for what is not there.
fn check_facts(document: &Value, facts: &Value) {
    for fact in facts.as_array().unwrap() {
        let path = fact[0].as_array().unwrap();
        match fact.get(1) {
            Some(wanted) => assert_eq!(at(document, path), Some(wanted), "{path:?}"),
            None => assert_eq!(at(document, path), None, "{path:?} is there"),
        }
    }
}

// ---- Handlers -----------------------------------------------------------------------------------

fn json_of(row: &EventRow) -> V {
    let mut encoder = Encoder::with_capacity(512);
    row.write_json(&mut encoder).unwrap();
    json_value(&encoder.out).unwrap()
}

/// `EventLogRecord.to_dict()` of a record.
fn record_of(event: &EventRow, realized_minute: i64, status: EventStatus, detail: Option<&str>) -> V {
    let mut record = indexmap::IndexMap::new();
    record.insert(Arc::from("event"), json_of(event));
    record.insert(Arc::from("realized_minute"), V::Int(realized_minute));
    record.insert(Arc::from("status"), V::Str(Arc::from(status.as_str())));
    record.insert(Arc::from("detail"), detail.map_or(V::Null, |detail| V::Str(Arc::from(detail))));
    V::Map(record)
}

/// What the handler of a call returned where it applied, as the PyO3 entry returns it: its records,
/// or the record of its care receipt.
fn handled(loading: &mut Loading, call: &Value) -> Vec<V> {
    let native = cases::native(&loading.loaded, &call["handler"]);
    let event = load::event(&call["event"]);
    let made = loading.tick(|tick| dispatch::apply_alone_made(tick, &native, &event)).unwrap();
    let (records, receipt) = made.unwrap_or_else(|outcome| panic!("the handler raised {outcome:?}"));
    let mut out: Vec<V> = records.iter().map(|Record { event, realized_minute, status, detail }| record_of(event, *realized_minute, *status, detail.as_deref())).collect();
    if let Some(receipt) = receipt {
        assert!(out.is_empty());
        out.push(record_of(receipt.row(), receipt.minute(), EventStatus::Applied, Some(receipt.detail())));
    }
    out
}

/// test_handler_rest_rules.py's `_digest` of what a handler returned and the world it left:
/// `json.dumps({"records": [...], "world": canonical_text}, sort_keys=True, ensure_ascii=False)`.
fn records_digest(records: Vec<V>, loading: &Loading) -> String {
    let mut value = indexmap::IndexMap::new();
    value.insert(Arc::from("records"), V::List(records));
    value.insert(Arc::from("world"), V::Str(Arc::from(loading.canonical())));
    digest(&V::Map(value))
}

fn digest(value: &V) -> String {
    sha256_hex(Dumps::default().unicode().sorted().text(value).as_bytes())[..16].to_owned()
}

// ==== integration/test_handler_rules.py ===========================================================

#[test]
fn a_loaded_world_writes_python_s_canonical_json() {
    let loading = Loading::of(&Value::from("generated"));
    let wanted = data()["canonical_sha256"]["generated"].as_str().unwrap();
    assert_eq!(sha256_hex(loading.canonical().as_bytes()), wanted);
}

// ---- ToiletServiceManager.request ----------------------------------------------------------------

/// `ToiletServiceManager.request` without the world's validation, with what the PyO3 entry reads.
fn toilet_request(loading: &mut Loading, agent_id: &str, facility_id: &str) -> Result<toilet::Receipt, Error> {
    loading.tick(|tick| {
        let holding = tick.holding()?;
        let queues = holding.held("facility_queues_v1", "the world holds another queue state");
        let geo = tick.host.floor_plan()?;
        let needs = kit::kept_section_value(tick.cell, tick.host, &holding, SectionId::AccessibilityNeeds, |configs| &configs.accessibility_needs, AccessibilityNeeds::parse)?;
        let request = toilet::Request { agent_id, facility_id, queue_id: None, queues: &queues, geo: &geo, needs: &needs };
        toilet::request(tick.cell, tick.host, &request, false)
    })
}

#[test]
fn a_toilet_request_puts_the_person_last_in_the_queue() {
    let call = &calls("a_toilet_request_puts_the_person_last_in_the_queue", 1)[0];
    let mut loading = Loading::of(&call["world"]);
    let receipt = toilet_request(&mut loading, s(&call["agent_id"]), s(&call["facility_id"])).unwrap();
    // The deleted rule: a queue task of the request's minute, the person last in the facility's
    // list, queued with the facility as target; the receipt has the place.
    let wanted = &call["receipt"];
    assert_eq!(&*receipt.agent_id, s(&wanted[0]));
    assert_eq!(&*receipt.facility_id, s(&wanted[1]));
    assert_eq!(receipt.requested_at, wanted[2].as_i64().unwrap());
    assert_eq!(&*receipt.queue_task_id, s(&wanted[3]));
    assert_eq!(receipt.queue_position, wanted[4].as_i64().unwrap());
    loading.is(&call["after"]);
}

/// The request refuses with the Python rule's error, class and text, and changes nothing.
fn toilet_refusals(group: &str, count: usize) {
    for call in calls(group, count) {
        let mut loading = Loading::of(&call["world"]);
        let before = loading.parts();
        let refused = toilet_request(&mut loading, s(&call["agent_id"]), s(&call["facility_id"])).unwrap_err();
        assert_eq!(refused, error_of(&call["error"]));
        unchanged(&before, &loading);
    }
}

/// What the request refuses, in the rule's order and with its texts: a walk's arrival saves the
/// text as its event's detail (`_after_arrival`).
#[test]
fn what_a_toilet_request_refuses_is_python_s_text() {
    toilet_refusals("what_a_toilet_request_refuses_is_python_s_text", 4);
}

#[test]
fn a_queue_task_id_in_use_is_an_invariant_error() {
    toilet_refusals("a_queue_task_id_in_use_is_an_invariant_error", 1);
}

#[test]
fn the_facilities_to_serve_leave_out_only_an_empty_queue() {
    for call in calls("the_facilities_to_serve_leave_out_only_an_empty_queue", 2) {
        let mut loading = Loading::of(&call["world"]);
        let due = loading.tick(|tick| {
            let holding = tick.holding().unwrap();
            let queues = holding.held("facility_queues_v1", "the world holds another queue state");
            tick.cell.with(|world| toilet::facilities_to_serve(world, &queues)).unwrap().unwrap()
        });
        let wanted: Vec<&str> = call["due"].as_array().unwrap().iter().map(s).collect();
        assert_eq!(due.iter().map(|id| &**id).collect::<Vec<_>>(), wanted);
    }
}

/// A pass of `arbitrate` at the facility's shared queue, with the manager the Run read and the
/// world's CounterRNG: whether it has room, and what started.
fn start_head(loading: &mut Loading, facility_id: &str) -> (bool, toilet::Started) {
    let seed = loading.entry["world"]["seed"].to_string();
    loading.tick(|tick| {
        let policies = tick.policies.clone();
        let manager = policies.services.toilet.object().unwrap();
        let policy = match manager.get("policy_version").unwrap() {
            Read::Value(V::Str(text)) => text.clone(),
            other => panic!("not a policy: {other:?}"),
        };
        let holding = tick.holding().unwrap();
        let queues = holding.held("facility_queues_v1", "the world holds another queue state");
        let placement = tick.placement().unwrap();
        let room = tick.cell.with(|world| toilet::has_room(world, &queues, placement.as_deref(), facility_id)).unwrap().unwrap();
        let geo = tick.host.floor_plan().unwrap();
        let rng = toilet::Rng::Seed(seed);
        let start = toilet::Start {
            facility_id,
            minute: tick.minute,
            queue_id: None,
            lowest: manager.int("min_service_minutes").unwrap(),
            highest: manager.int("max_service_minutes").unwrap(),
            policy,
            rng: &rng,
            queues: &queues,
            geo: &geo,
            walk: None,
            metric_state_held: holding.field(METRIC_STATE),
        };
        (room, toilet::start(tick.cell, tick.host, &start).unwrap())
    })
}

/// The deleted rule: a uniform duration of whole minutes drawn at the head's key, a service task and
/// its completion of the next sequence, the person in service, and the wait counted.
#[test]
fn the_head_starts_its_service() {
    let call = &calls("the_head_starts_its_service", 1)[0];
    let mut loading = Loading::of(&call["world"]);
    let (room, started) = start_head(&mut loading, s(&call["facility_id"]));
    assert!(room);
    assert_eq!(started, toilet::Started::Event(Arc::from(s(&call["event_id"]))));
    loading.is(&call["after"]);
}

#[test]
fn a_head_that_cannot_finish_stays_in_line() {
    let call = &calls("a_head_that_cannot_finish_stays_in_line", 1)[0];
    let mut loading = Loading::of(&call["world"]);
    let before = loading.parts();
    let (_, started) = start_head(&mut loading, s(&call["facility_id"]));
    assert_eq!(started, toilet::Started::Stopped);
    unchanged(&before, &loading);
}

// ---- ToiletServiceCompletionHandler --------------------------------------------------------------

/// The Python rule (`complete_toilet_service` and the handler, gone in lane L10): the slot, the
/// task, the person and the count; 0.2 off the hygiene need (a change of 1% of it shows here); a
/// person not in a queue has no queue record to finish.
#[test]
fn a_toilet_completion_is_python_s() {
    check_group(FILE, "a_toilet_completion_is_python_s", 4);
}

/// A Scenario's facility kind is any str: `"toilet" in kind.casefold()`, Python's fold.
#[test]
fn a_facility_kind_beyond_ascii_is_folded_as_python_folds_it() {
    check_group(FILE, "a_facility_kind_beyond_ascii_is_folded_as_python_folds_it", 2);
}

/// `complete_toilet_service`'s ValueError, as the handler wraps it.
#[test]
fn a_toilet_completion_that_does_not_fit_is_python_s() {
    check_group(FILE, "a_toilet_completion_that_does_not_fit_is_python_s", 1);
}

// ---- care receipts -------------------------------------------------------------------------------

/// `append_applied_care_transition` of each transition in turn, as its PyO3 entry calls the core;
/// the first error stops.
fn append_transitions(loading: &mut Loading, transitions: &Value) -> Result<(), Error> {
    for transition in transitions.as_array().unwrap() {
        loading.tick(|tick| {
            let holding = kit::holding(tick.cell, tick.host)?;
            if care_receipts::nothing_to_append(&holding)? {
                return Ok(());
            }
            let source_ids = transition["source_ids"].as_array().map(|ids| ids.iter().map(|id| Arc::from(s(id))).collect()).unwrap_or_default();
            let (status, _) = care_receipts::status_of(s(&transition["status"]))?;
            care_receipts::check_tree(&holding)?;
            let text = |key: &str| transition.get(key).and_then(Value::as_str).map(Arc::from);
            let transition = Transition {
                request_id: Arc::from(s(&transition["request_id"])),
                agent_id: Arc::from(s(&transition["agent_id"])),
                household_id: text("household_id"),
                support_kind: Arc::from(s(&transition["support_kind"])),
                status,
                staff_id: text("staff_id"),
                causation_id: text("causation_id"),
                source_ids,
                outcome: None,
            };
            let written = tick.cell.with(|world| care_receipts::append(world, transition))?;
            tick.written(written)
        })?;
    }
    Ok(())
}

/// What the Python rule made of it (it has none left, lane L10), by the digest of the world:
/// `sha256(canonical_json)[:16]`, which the pytest test of the same name pinned (the core writes
/// the same bytes); sorted sources without repeats, with the producer's own.
#[test]
fn a_care_transition_is_python_s() {
    let call = &calls("a_care_transition_is_python_s", 1)[0];
    let mut loading = Loading::of(&call["world"]);
    append_transitions(&mut loading, &call["transitions"]).unwrap();
    assert_eq!(&sha256_hex(loading.canonical().as_bytes())[..16], "4d4cd613a943dd2a");
    let document = loading.document();
    check_facts(&document, &call["expect"]);
    let head = at(&document, &[Value::from("model_state"), "shared_operations_v1".into(), "support_requests".into(), "support-t1".into()]).unwrap();
    let last = at(&document, &[Value::from("event_log"), Value::from(-1)]).unwrap();
    assert_eq!(head["latest_event_id"], last["event"]["event_id"]);
}

/// A transition the core refuses or the rule turns down with its text changes nothing, its
/// counters neither: the Python rule appended the event and took its sequence first.
fn refused_transitions(group: &str, count: usize) {
    for call in calls(group, count) {
        let mut loading = Loading::of(&call["world"]);
        let before = loading.parts();
        assert_eq!(append_transitions(&mut loading, &call["transitions"]), Err(error_of(&call["error"])));
        if call["transitions"].as_array().unwrap().len() == 1 {
            unchanged(&before, &loading);
        }
    }
}

#[test]
fn what_a_care_transition_refuses_changes_nothing() {
    refused_transitions("what_a_care_transition_refuses_changes_nothing", 3);
}

#[test]
fn a_transition_out_of_order_has_the_python_text_and_changes_nothing() {
    refused_transitions("a_transition_out_of_order_has_the_python_text_and_changes_nothing", 1);
}

#[test]
fn a_request_id_used_before_is_python_s_error() {
    refused_transitions("a_request_id_used_before_is_python_s_error", 1);
}

#[test]
fn without_the_namespace_nothing_happens() {
    let call = &calls("without_the_namespace_nothing_happens", 1)[0];
    let mut loading = Loading::of(&call["world"]);
    let before = loading.parts();
    append_transitions(&mut loading, &call["transitions"]).unwrap();
    unchanged(&before, &loading);
}

// ---- seek_help and seek_information --------------------------------------------------------------

/// `_apply_effect` of the call's proposal (no observed state hash, as the pytest file's proposal
/// had none), and the digest of the world after: `sha256(canonical_json)[:16]`.
fn seek(call: &Value) -> (Applied, String, Value) {
    let mut loading = Loading::of(&call["world"]);
    let proposal = Proposal {
        action: Action::parse(s(&call["action"])).unwrap(),
        agent_id: Arc::from(s(&call["agent_id"])),
        next_decision_at: call["next_decision_at"].as_i64().unwrap(),
        target_id: None,
        observed_state_hash: None,
    };
    let applied = loading.tick(|tick| tick::propose::apply_alone(tick, proposal)).unwrap();
    let canonical = loading.canonical();
    (applied, sha256_hex(canonical.as_bytes())[..16].to_owned(), serde_json::from_str(&canonical).unwrap())
}

const REQUEST: &str = "support-9a4dff703809248129a56411";

fn asked(applied: &Applied) -> (Option<&str>, i64, bool) {
    (applied.target.as_deref(), applied.next_decision_at, applied.committed)
}

/// The world of each seek is pinned by `sha256(canonical_json)[:16]`, recorded from the core in
/// #225 lane M4; on the same kernel the pytest test of the same name in test_handler_rules.py,
/// with its `world_digest` (`b97278e44a15…`, `77396644fe07…`, `15ff1c01a90e…`, `1b1b58c7ce93…`,
/// `cddd80dc1bf4…`, `1b05e9d54a04…`, `76415054a733…`, `a149ba7dc717…`, in this order) passed.
#[test]
fn what_a_seek_reads_of_a_person_is_python_s() {
    let digests = [
        "3577412dda5be40c",
        "b2aec716721dd837",
        "30d581e60610ce75",
        "cfd933c69b47f61b",
        "1f49289412673da2",
        "24537a62519023b5",
        "d61fd2f8c5afdc44",
        "ab1c7cd0f610314c",
    ];
    for (call, wanted) in calls("what_a_seek_reads_of_a_person_is_python_s", 8).iter().zip(digests) {
        let (applied, digest, _) = seek(call);
        assert_eq!(asked(&applied), (Some(REQUEST), 12, true));
        assert_eq!(digest, wanted, "{}", call["world"]);
    }
}

/// Recorded from the core in #225 lane M4; on the same kernel the pytest test of the same name, with
/// its `world_digest` `5d20d2e22d7a…` (seek_help) and `9f3389377a05…` (seek_information), passed.
#[test]
fn a_seek_is_python_s() {
    let digests = ["46f9441bd237dc0b", "d26a051a688ff46c"];
    for (call, wanted) in calls("a_seek_is_python_s", 2).iter().zip(digests) {
        let (applied, digest, document) = seek(call);
        assert_eq!(asked(&applied), (Some(REQUEST), 12, true));
        let agent = &[Value::from("agents"), call["agent_id"].clone(), "activity".into()];
        assert_eq!(at(&document, agent), Some(&Value::from("seeking_help")));
        assert!(at(&document, &[Value::from("support_requests"), REQUEST.into()]).is_some());
        assert_eq!(digest, wanted, "{}", call["action"]);
    }
}

/// Recorded from the core in #225 lane M4; on the same kernel the pytest test of the same name, with
/// its `world_digest` `f53692cd3b44…`, passed.
#[test]
fn a_seek_in_cooldown_counts_as_rejected() {
    let call = &calls("a_seek_in_cooldown_counts_as_rejected", 1)[0];
    let (applied, digest, document) = seek(call);
    assert_eq!(asked(&applied), (None, 12, false));
    assert!(at(&document, &[Value::from("metric_state"), "support_request_rejected_count".into()]).unwrap().as_f64().unwrap() >= 1.0);
    assert_eq!(digest, "51b75ffbda42d798");
}

/// Recorded from the core in #225 lane M4; on the same kernel the pytest test of the same name, with
/// its `world_digest` `dbe1888452f6…`, passed.
#[test]
fn a_seek_reaches_the_information_rule_above_its_threshold() {
    let call = &calls("a_seek_reaches_the_information_rule_above_its_threshold", 1)[0];
    let (applied, digest, _) = seek(call);
    assert_eq!(asked(&applied), (Some(REQUEST), 12, true));
    assert_eq!(digest, "783cc67d6c33a53a");
}

// ---- the timeout ---------------------------------------------------------------------------------

/// The Python rule (`timeout_support_request`, gone in lane L10): unresolved, with a cooldown.
#[test]
fn a_timeout_is_python_s() {
    check_group(FILE, "a_timeout_is_python_s", 1);
}

/// An assigned request goes on in service: the Python rule's suppression.
#[test]
fn a_timeout_of_what_has_left_pending_says_python_s_text() {
    check_group(FILE, "a_timeout_of_what_has_left_pending_says_python_s_text", 1);
}

#[test]
fn a_timeout_of_another_person_s_request_is_python_s_error() {
    check_group(FILE, "a_timeout_of_another_person_s_request_is_python_s_error", 1);
}

// ---- a staff member's support ends ---------------------------------------------------------------

/// What `complete_support_task` did (the task gone, the staff member free, the request resolved with
/// the cooldown, the person idle, one more resolved request) and the care the deleted rule
/// recorded, with the body state's configured amounts: the facts the pytest test checked.
fn support_ends(group: &str, count: usize) {
    for call in calls(group, count) {
        let mut loading = Loading::of(&call["world"]);
        let records = handled(&mut loading, call);
        assert_eq!(records.len(), 1);
        let record: Value = serde_json::from_str(&Dumps::default().text(&records[0])).unwrap();
        assert_eq!(record["status"], "applied");
        check_facts(&record, &call["record"]);
        check_facts(&loading.document(), &call["expect"]);
    }
}

#[test]
fn the_end_of_support_resolves_and_records_the_care() {
    support_ends("the_end_of_support_resolves_and_records_the_care", 3);
}

/// The rule read and wrote the body only when it was a dict: without the body state (a Run of no
/// body model), emotional support relieves nothing and its care is not measured. Recorded once from
/// the deleted rule.
#[test]
fn the_end_of_support_without_a_body_state_measures_nothing() {
    support_ends("the_end_of_support_without_a_body_state_measures_nothing", 1);
}

#[test]
fn the_end_of_support_that_does_not_fit_is_python_s() {
    check_group(FILE, "the_end_of_support_that_does_not_fit_is_python_s", 1);
}

// ---- what follows in the dispatch phase ----------------------------------------------------------

/// The event's ID and due minute, and the payload's values of these keys that are str.
fn support_event(row: &EventRow, keys: &[&str]) -> care::SupportEvent {
    let text = |key: &str| match kit::payload(row, key) {
        Some(V::Str(text)) => Some(text.clone()),
        _ => None,
    };
    let wanted = |key: &str| if keys.contains(&key) { text(key) } else { None };
    care::SupportEvent {
        event_id: row.event_id.clone(),
        due_minute: row.due_minute,
        request_id: wanted("request_id"),
        agent_id: wanted("agent_id"),
        staff_id: wanted("staff_id"),
        task_id: wanted("task_id"),
    }
}

/// `_record_request_interrupted` (`started`: `_record_staff_support_started`) of the call's event, as
/// the dispatch phase runs it after the event applied: nothing for an event without a request.
fn record_receipt(call: &Value, started: bool) {
    let mut loading = Loading::of(&call["world"]);
    let before = loading.parts();
    let row = load::event(&call["event"]);
    let event = support_event(&row, if started { &["request_id", "staff_id"] } else { &["request_id"] });
    if event.request_id.is_some() {
        loading
            .tick(|tick| -> Result<(), Error> {
                let inputs = care::Inputs::read(tick.cell, tick.host)?;
                let written = tick.cell.with(|world| if started { care::record_started(world, &inputs, &event) } else { care::record_interrupted(world, &inputs, &event) })?;
                tick.written(written)
            })
            .unwrap();
    }
    match call.get("after") {
        Some(after) => loading.is(after),
        None => unchanged(&before, &loading),
    }
}

/// What the Python rule appended: the receipt's household and kind, the timeout as the cause.
#[test]
fn a_timeout_records_the_interruption_of_a_care_request() {
    for call in calls("a_timeout_records_the_interruption_of_a_care_request", 2) {
        record_receipt(call, false);
    }
}

/// Nothing without the request, for another staff member, for a request not there, or for a person
/// not seeking help; else what the Python rule appended: the assigned staff member, the request's
/// task as the cause.
#[test]
fn a_staff_arrival_records_the_start_of_care() {
    for call in calls("a_staff_arrival_records_the_start_of_care", 5) {
        record_receipt(call, true);
    }
}

#[test]
fn a_settled_request_without_a_receipt_adds_nothing() {
    record_receipt(&calls("a_settled_request_without_a_receipt_adds_nothing", 1)[0], false);
}

/// The request's own ValueError, as #90 saves it: the walk fails with the request's text.
#[test]
fn an_arrival_s_refused_toilet_request_is_its_text() {
    let call = &calls("an_arrival_s_refused_toilet_request_is_its_text", 1)[0];
    let mut loading = Loading::of(&call["world"]);
    let before = loading.parts();
    let refused = loading.tick(|tick| {
        /// The controller's toilet queue, as `_after_arrival` asks it (the dispatch phase's).
        struct Queue<'c, C>(&'c C);
        impl<C: WorldCell> travel_completion::Managers<TestHost> for Queue<'_, C> {
            fn own(&mut self, _toilet: bool) -> Result<bool, Error> {
                Ok(false)
            }

            fn toilet(&mut self, host: &mut TestHost, agent_id: &str, facility_id: &str) -> Result<(), Error> {
                let holding = kit::holding(self.0, host)?;
                let queues = holding.held("facility_queues_v1", "the world holds another queue state");
                let geo = host.floor_plan()?;
                let needs = kit::kept_section_value(self.0, host, &holding, SectionId::AccessibilityNeeds, |configs| &configs.accessibility_needs, AccessibilityNeeds::parse)?;
                let request = toilet::Request { agent_id, facility_id, queue_id: None, queues: &queues, geo: &geo, needs: &needs };
                Ok(self.0.with(|world| toilet::queue_up(world, &request).map(drop))?.map_err(Error::from)?)
            }

            fn resource(&mut self, _host: &mut TestHost, _agent_id: &str, _facility_id: &str) -> Result<Option<String>, Error> {
                unreachable!("not a resource walk")
            }
        }
        let has_geometry = travel_completion::has_geometry(tick.cell, tick.host).unwrap();
        assert!(!has_geometry);
        let arrival = travel_completion::Arrival {
            agent_id: s(&call["agent_id"]),
            purpose: "toilet_request",
            facility_id: s(&call["facility_id"]),
            destination: s(&call["destination"]),
            origin: None,
            has_geometry,
            resource_service: false,
            validates: false,
        };
        travel_completion::arrive(tick.cell, tick.host, &arrival, &mut Queue(tick.cell)).err()
    });
    assert_eq!(refused, Some(error_of(&call["error"])));
    unchanged(&before, &loading);
}

/// `_schedule_return_to_initial_zone` of the event, as the toilet's completion runs it: nothing for
/// an unknown person or one at home; else the controller's route home and a "return" trip on it,
/// what the Python rule did.
#[test]
fn the_journey_home_after_a_toilet_is_a_return_trip() {
    for call in calls("the_journey_home_after_a_toilet_is_a_return_trip", 3) {
        let mut loading = Loading::of(&call["world"]);
        let before = loading.parts();
        let row = load::event(&call["event"]);
        loading
            .tick(|tick| -> Result<(), Error> {
                let holding = tick.holding()?;
                let graph = tick::propose::graph(tick)?;
                let plan = tick.plan();
                let section = tick.value_section(&holding, SectionId::Movement, |configs| &configs.movement, Movement::parse)?;
                let movement = holding.held("movement_v1", "the world holds another movement state");
                let staff = holding.held("staff_operations_v1", "the world holds another document than the row's");
                let routing = travel::Routing { graph: &graph, geo: plan.get(), movement: &section, staff: &staff };
                let walking = travel::Walking { movement: &movement, geo: plan.get() };
                let returned = tick.cell.with(|world| travel::schedule_return(world, &routing, walking, &row))?;
                tick.written(returned)
            })
            .unwrap();
        match call.get("after") {
            Some(after) => loading.is(after),
            None => unchanged(&before, &loading),
        }
    }
}

// ==== integration/test_handler_rest_rules.py ======================================================

#[test]
fn a_failed_outage_changes_nothing_and_says_python_s_text() {
    check_group(FILE, "a_failed_outage_changes_nothing_and_says_python_s_text", 7);
}

#[test]
fn a_stale_proximity_sample_is_failed_in_python_s_text() {
    check_group(FILE, "a_stale_proximity_sample_is_failed_in_python_s_text", 1);
}

#[test]
fn a_contact_that_no_longer_applies_says_python_s_text() {
    check_group(FILE, "a_contact_that_no_longer_applies_says_python_s_text", 1);
}

#[test]
fn a_restoration_of_no_outage_says_python_s_text() {
    check_group(FILE, "a_restoration_of_no_outage_says_python_s_text", 1);
}

// ---- Care that a selected formula decides --------------------------------------------------------

/// What the Python rule gave, by care outcome formula and kind: the digest of what the kernel
/// returned and left (`records_digest`), which test_handler_rest_rules.py pinned from the deleted
/// rule (model version 2 re-recorded some, from the kernels of that model).
const CARE_DIGESTS: [(&str, &str, &str); 24] = [
    ("hygiene-support-fixed-relief-v1", "caregiver_assistance", "81e48ab9b68232ec"),
    ("hygiene-support-fixed-relief-v1", "dependent_assistance", "5574059f51f389dc"),
    ("hygiene-support-fixed-relief-v1", "hygiene_support", "b6f572df6044eddc"),
    ("hygiene-support-fixed-relief-v1", "language_access", "8786e5e4161be69d"),
    ("hygiene-support-fixed-relief-v1", "medical_assessment", "282d7186863ef39b"),
    ("hygiene-support-fixed-relief-v1", "medication_support", "4d9983de142b0360"),
    ("hygiene-support-fixed-relief-v1", "mobility_assistance", "487ba4781f32438f"),
    ("hygiene-support-fixed-relief-v1", "privacy_accommodation", "a77ef41a75d64664"),
    ("hygiene-support-proportional-relief-v1", "caregiver_assistance", "570b9808eee1ef34"),
    ("hygiene-support-proportional-relief-v1", "dependent_assistance", "cf7679c5677685cd"),
    ("hygiene-support-proportional-relief-v1", "hygiene_support", "5a5972846519c45f"),
    ("hygiene-support-proportional-relief-v1", "language_access", "f92d9feb6f767a72"),
    ("hygiene-support-proportional-relief-v1", "medical_assessment", "d28c133517b555e6"),
    ("hygiene-support-proportional-relief-v1", "medication_support", "09f886637923e75f"),
    ("hygiene-support-proportional-relief-v1", "mobility_assistance", "d48a1015592e3a1c"),
    ("hygiene-support-proportional-relief-v1", "privacy_accommodation", "54d5eeaf26d65d2a"),
    ("typed-care-support-response-v1", "caregiver_assistance", "15e48ae4645a2a9d"),
    ("typed-care-support-response-v1", "dependent_assistance", "ba1d90685a0ca96c"),
    ("typed-care-support-response-v1", "hygiene_support", "9eb2ecb9ebf798f2"),
    ("typed-care-support-response-v1", "language_access", "dc2ceb45740c9cca"),
    ("typed-care-support-response-v1", "medical_assessment", "350f4b14a0bf7801"),
    ("typed-care-support-response-v1", "medication_support", "822f2ec5a07501bd"),
    ("typed-care-support-response-v1", "mobility_assistance", "096a3c82e2d7234c"),
    ("typed-care-support-response-v1", "privacy_accommodation", "aa6ac71ad006d932"),
];

/// The care of a call, and the digest of what it returned and left.
fn care(call: &Value) -> (Vec<V>, String) {
    let mut loading = Loading::of(&call["world"]);
    let records = handled(&mut loading, call);
    assert_eq!(records.len(), 1);
    let digest = records_digest(records.clone(), &loading);
    (records, digest)
}

fn payload_of(record: &V) -> Value {
    let record: Value = serde_json::from_str(&Dumps::default().text(record)).unwrap();
    record["event"]["payload"].clone()
}

#[test]
fn a_kind_a_selected_formula_decides_is_python_s() {
    let calls = calls("a_kind_a_selected_formula_decides_is_python_s", 24);
    for (model, kind, wanted) in CARE_DIGESTS {
        let call = calls.iter().find(|call| call["model_id"] == model && call["kind"] == kind).unwrap();
        let (records, digest) = care(call);
        assert_eq!(payload_of(&records[0])["status"], "completed");
        assert_eq!(digest, wanted, "{model} {kind}");
    }
}

#[test]
fn care_with_a_dependent_is_python_s() {
    let call = &calls("care_with_a_dependent_is_python_s", 1)[0];
    let (records, digest) = care(call);
    assert_eq!(digest, "1aaa2e06433b2a25");
    let payload = payload_of(&records[0]);
    assert_eq!(payload["outcome_observation"], "observed_change");
    let people: std::collections::BTreeSet<&str> = payload["state_changes"].as_array().unwrap().iter().map(|change| s(&change["entity_id"])).collect();
    assert_eq!(people, call["people"].as_array().unwrap().iter().map(s).collect());
    assert!(payload["source_ids"].as_array().unwrap().contains(&Value::from("SRC-DEPENDENT")));
}

#[test]
fn a_care_delta_1_percent_off_shows() {
    let call = &calls("a_care_delta_1_percent_off_shows", 1)[0];
    let stress: Vec<Value> = call["worlds"]
        .as_array()
        .unwrap()
        .iter()
        .map(|world| {
            let mut loading = Loading::of(world);
            handled(&mut loading, call);
            at(&loading.document(), &[Value::from("agents"), call["agent_id"].clone(), "stress".into()]).unwrap().clone()
        })
        .collect();
    assert_ne!(stress[0], stress[1]);
}

/// What the Python rule gave without configured amounts, by kind: `records_digest`.
#[test]
fn care_without_configured_amounts_uses_the_formula_s_defaults() {
    let digests = [
        ("caregiver_assistance", "be980048d184e9ef"),
        ("dependent_assistance", "7c76ba100f33c6e0"),
        ("hygiene_support", "fe11fae3dc4a53c8"),
        ("language_access", "1a8694275defe196"),
        ("medical_assessment", "f0cff3adddb6b51c"),
        ("medication_support", "aebddd5ba5d6ee02"),
        ("mobility_assistance", "fa4a3e6c98a7f87d"),
        ("privacy_accommodation", "cd03d9dccf359e55"),
    ];
    let calls = calls("care_without_configured_amounts_uses_the_formula_s_defaults", 8);
    for (kind, wanted) in digests {
        let call = calls.iter().find(|call| call["kind"] == kind).unwrap();
        assert_eq!(care(call).1, wanted, "{kind}");
    }
}

// ---- What else the handlers ask ------------------------------------------------------------------

/// What the Python rule made of it (it has none left, lane L10), by the digest of the world, which
/// test_handler_rest_rules.py pinned: `sha256(json.dumps(canonical_text))[:16]`; recorded once:
/// the second time it is found.
#[test]
fn ingest_of_a_record_of_this_tick_is_python_s() {
    let call = &calls("ingest_of_a_record_of_this_tick_is_python_s", 1)[0];
    let mut loading = Loading::of(&call["world"]);
    let row = load::event(&call["event"]);
    let realized = call["realized_minute"].as_i64().unwrap();
    let ingest = |loading: &mut Loading| loading.tick(|tick| care_receipts::ingest_applied(tick.cell, tick.host, &row, realized)).unwrap();
    ingest(&mut loading);
    let document = loading.document();
    let last = at(&document, &[Value::from("model_state"), "shared_operations_v1".into(), "history".into(), Value::from(-1), "event_id".into()]);
    assert_eq!(last, Some(&call["event"]["event_id"]));
    assert_eq!(digest(&V::Str(Arc::from(loading.canonical()))), "df5f83d1b598fa8a");
    let once = loading.parts();
    ingest(&mut loading);
    unchanged(&once, &loading);
}

// ---- The other social events: their fixed amounts, which few runs reach --------------------------

/// The kernel applies the event and writes only the stress and trust of the people it names, whose
/// values each test wrote down from the deleted Python rule.
#[test]
fn a_complaint_response_is_python_s() {
    check_group(FILE, "a_complaint_response_is_python_s", 3);
}

#[test]
fn an_untyped_social_event_is_python_s() {
    check_group(FILE, "an_untyped_social_event_is_python_s", 2);
}

#[test]
fn cooperation_is_python_s() {
    check_group(FILE, "cooperation_is_python_s", 1);
}

/// The receipt and the world are the Python rule's: the digest of what it gave (lane L10), which
/// test_handler_rest_rules.py pinned. The zone is as clean as it gets already: the cleaning's
/// receipt measures, and nothing changed.
#[test]
fn a_finished_operation_is_python_s() {
    let digests = [("cleaning", "7bb434e07202a409", &[][..]), ("information_outreach", "989b5abd395d3d49", &["trust"][..]), ("patrol", "dc462459ee25cdcf", &["trust"][..])];
    let calls = calls("a_finished_operation_is_python_s", 3);
    for (kind, wanted, changed) in digests {
        let call = calls.iter().find(|call| call["kind"] == kind).unwrap();
        let mut loading = Loading::of(&call["world"]);
        let records = handled(&mut loading, call);
        let payload = payload_of(&records[0]);
        let paths: std::collections::BTreeSet<&str> = payload["state_changes"].as_array().unwrap().iter().map(|change| s(&change["field_path"])).collect();
        assert_eq!(paths, changed.iter().copied().collect(), "{kind}");
        assert_eq!(records_digest(records, &loading), wanted, "{kind}");
    }
}

// ---- What a draw hides ---------------------------------------------------------------------------

fn spread(fields: &Value) -> InformationSpread {
    InformationSpread::from_v(&load::v(fields)).unwrap()
}

fn bits(text: &Value) -> Vec<bool> {
    s(text).chars().map(|bit| bit == '1').collect()
}

/// 1% in a coefficient flips few draws: these settings put the probability near the middle. The
/// draws are Python's `CounterRNG` (the fixture's bits): the belief is 1.0 when the draw is below
/// the clamped probability, else 0.0; a correction is accepted below 0.95. Model version 2 keys the
/// draws by the minute and the people, the source and the message, without the event's ID.
#[test]
fn many_draws_of_a_claim_and_a_correction_are_python_s() {
    let call = &calls("many_draws_of_a_claim_and_a_correction_are_python_s", 1)[0];
    let loading = Loading::of(&call["world"]);
    let (believing, accepting) = (spread(&call["believing"]), spread(&call["accepting"]));
    let agent_id = s(&call["agent_id"]);
    let (beliefs, acceptances) = (bits(&call["beliefs"]), bits(&call["acceptances"]));
    assert_eq!(beliefs.len(), 4000);
    loading
        .loaded
        .cell
        .with(|world| {
            for (number, (believed, accepted)) in beliefs.iter().zip(&acceptances).enumerate() {
                let message_id = format!("claim-{number}");
                let strength = information_events::sample_strength(world, &believing, agent_id, "SRC-TEST", 7, &message_id).unwrap();
                assert_eq!(strength, if *believed { 1.0 } else { 0.0 }, "{message_id}");
                let label = "correction-acceptance-v1";
                let accepts = information_events::accepts(world, &accepting, label, 1.0, "SRC-TEST", 7, &["SRC-TEST", agent_id, &message_id]).unwrap();
                assert_eq!(accepts, *accepted, "{message_id}");
            }
        })
        .unwrap();
}

/// A Scenario's seed is any int from 0; CounterRNG keys its draws by `str(seed)`.
#[test]
fn a_claim_s_draw_keys_a_seed_past_64_bits_as_python_does() {
    let call = &calls("a_claim_s_draw_keys_a_seed_past_64_bits_as_python_does", 1)[0];
    let loading = Loading::of(&call["world"]);
    assert_eq!(loading.entry["world"]["seed"].to_string(), "1180591620717411303427");
    let believing = spread(&call["believing"]);
    let beliefs = bits(&call["beliefs"]);
    assert_eq!(beliefs.len(), 200);
    loading
        .loaded
        .cell
        .with(|world| {
            for (number, believed) in beliefs.iter().enumerate() {
                let message_id = format!("claim-{number}");
                let strength = information_events::sample_strength(world, &believing, s(&call["agent_id"]), "SRC-TEST", 7, &message_id).unwrap();
                assert_eq!(strength, if *believed { 1.0 } else { 0.0 }, "{message_id}");
            }
        })
        .unwrap();
}
