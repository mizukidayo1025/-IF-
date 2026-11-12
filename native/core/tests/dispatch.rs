//! Lane P9-1c: the dispatch phase in the core (`sheltersim_core::rules::dispatch`), on worlds built
//! here, with no Python: which due event runs (gone, the same row, or another row of the same event
//! by value, as `current.to_dict() == event.to_dict()` asks), the handler's outcome and records, the
//! settle's compare of a record the handler wrote, and the order in which the host is asked (a test
//! handler through `apply_foreign`, `space_refreshed` after the records of what applied, the social
//! policy's `consider_*` at the end), across a batch whose rows a transaction put in canonical order.

mod support;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::cell::Shard;
use sheltersim_core::entities::{Activity, AgentRow, AnyTable, Entities, Mobility, NeedRow, ProfileRow, Table};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::{EventKind, EventRow, EventStatus};
use sheltersim_core::host::{Foreign, ForeignRecord};
use sheltersim_core::rules::common_operations::Logged;
use sheltersim_core::rules::dispatch::{self, py_eq, same_event, FOLLOW_UP_DISAGREES, RECORD_DISAGREES};
use sheltersim_core::rules::tick::{Seed, Tick, ZoneNeighbors};
use sheltersim_core::run::dispatch::{Dispatch, Handler};
use sheltersim_core::run::held::{Class, Held, Obj};
use sheltersim_core::run::social::Social;
use sheltersim_core::run::{Policies, Run, RunConfig};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, World, WorldCell};

use support::{Call, TestHost};

const MINUTE: i64 = 10;

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn map(entries: &[(&str, V)]) -> V {
    V::Map(entries.iter().map(|(key, value)| (text(key), value.clone())).collect())
}

fn event(id: &str, sequence: i64, event_type: &str, payload: &[(&str, V)]) -> EventRow {
    let V::Map(payload) = map(payload) else { unreachable!() };
    let provenance = IndexMap::from([(text("source_type"), V::Str(text("assumption")))]);
    EventRow::new(text(id), EventKind::Endogenous, MINUTE, 0, sequence, text(event_type), payload, Some(text("probe-1")), None, provenance).unwrap()
}

fn needs() -> NeedRow {
    NeedRow { toilet_urgency: 0.1, hunger: 0.1, fatigue: 0.1, hydration_need: 0.1 }
}

fn agent(id: &str, group: Option<&str>, stress: f64) -> AgentRow {
    AgentRow {
        profile: ProfileRow {
            id: text(id),
            group_id: group.map(text),
            initial_zone_id: text("z0"),
            mobility: Mobility::Independent,
            support_need: 0.0,
            stress_sensitivity: 1.0,
            rng_key: text(id),
            initial_needs: needs(),
            provenance: V::Map(IndexMap::new()),
        },
        zone_id: text("z0"),
        activity: Activity::Idle,
        target_id: None,
        needs: needs(),
        stress,
        trust: 0.5,
        next_decision_at: 0,
        support_request_id: None,
        support_cooldown_until: 0,
    }
}

fn put<R: Clone>(table: &mut Table<R>, id: &str, row: R) {
    let slot = table.reserve_slot();
    table.fill_slot(slot, row);
    assert_eq!(table.bind(text(id), slot), None);
}

/// a1 and a2 of household g1; the world's queue holds `events` and its log is empty.
fn world(events: Vec<EventRow>) -> Rc<Shard<World>> {
    let mut entities = Entities::default();
    put(&mut entities.agents, "a1", agent("a1", Some("g1"), 0.2));
    put(&mut entities.agents, "a2", agent("a2", Some("g1"), 0.7));
    let mut world = World::default();
    *world.entities.get_mut() = entities;
    let scalars = world.scalars.get_mut();
    scalars.minute_index = Fit::Ok(MINUTE);
    scalars.duration_minutes = Fit::Ok(100);
    scalars.schema_version = Fit::Ok(6);
    let mut queue = Queue::new();
    for row in events {
        assert_eq!(queue.schedule_row(row), 0);
    }
    world.hold_queue(Arc::new(queue));
    world.hold_log(Arc::new(EventLog::new()));
    Rc::new(Shard::new(world))
}

fn object(attrs: Vec<(&str, Held)>) -> Arc<Obj> {
    let mut object = Obj::new(Class { module: None, qualname: None, resolves: false });
    for (name, held) in attrs {
        object.attrs.insert(text(name), Ok(held));
    }
    Arc::new(object)
}

/// The Run's policies with these event types' handlers the host's (a test's), and a social policy
/// whose stress threshold is 0.5.
fn run(foreign: &[&str]) -> (RefCell<Run>, Arc<Policies>) {
    let run = RefCell::new(Run::new(RunConfig { route_capacities: Err("no capacities") }));
    let mut policies = Policies::clone(&run.borrow().policies);
    let handlers: IndexMap<Arc<str>, Handler> = foreign.iter().map(|event_type| (text(event_type), Handler::Foreign)).collect();
    policies.dispatch = Dispatch { known: Ok(true), handlers: Ok(Arc::new(handlers)) };
    let limits = object(vec![("stress_threshold", Held::Value(V::Float(0.5)))]);
    let policy = object(vec![("policy_version", Held::Value(V::Str(text("social-1")))), ("limits", Held::Object(limits))]);
    policies.social = Social { policy: Ok(Some(policy)) };
    (run, Arc::new(policies))
}

fn neighbors() -> Arc<ZoneNeighbors> {
    Arc::new(ZoneNeighbors { social: Ok(HashMap::new()), information: Ok(HashMap::new()) })
}

/// The phase at `MINUTE` with this host.
fn dispatched(cell: &Shard<World>, host: &mut TestHost, foreign: &[&str]) -> Result<(), Error> {
    let (run, policies) = run(foreign);
    let mut tick = Tick { cell, host, run: &run, policies, minute: MINUTE, seed: Seed::Other, neighbors: neighbors() };
    dispatch::run(&mut tick)
}

/// The log's records as `(event_id, status, minute)`, in their order.
fn records(cell: &Shard<World>) -> Vec<(String, EventStatus, i64)> {
    cell.with(|world| {
        let log = world.log().unwrap();
        (0..log.len())
            .map(|index| {
                let raw = log.written(index).expect("a record's bytes");
                let (row, minute, status, _) = EventRow::from_saved_record(raw).expect("a record");
                (row.event_id.to_string(), status, minute)
            })
            .collect()
    })
    .unwrap()
}

fn detail_of(cell: &Shard<World>, event_id: &str) -> Option<String> {
    cell.with(|world| {
        let log = world.log().unwrap();
        (0..log.len()).find_map(|index| {
            let (row, _, _, detail) = EventRow::from_saved_record(log.written(index)?)?;
            (&*row.event_id == event_id).then(|| detail.map(|detail| detail.to_string()))?
        })
    })
    .unwrap()
}

fn pending(cell: &Shard<World>) -> Vec<String> {
    cell.with(|world| world.queue().unwrap().pending_ids(None).iter().map(|id| id.to_string()).collect()).unwrap()
}

fn foreign_calls(host: &TestHost) -> Vec<Call> {
    host.calls.iter().filter(|call| matches!(call, Call::Foreign(_) | Call::SpaceRefreshed | Call::Cooperation { .. } | Call::InformationReaction { .. })).cloned().collect()
}

fn applied() -> Result<Foreign, Error> {
    Ok(Foreign { status: EventStatus::Applied, detail: None, records: Vec::new() })
}

// ---- Equality by value ------------------------------------------------------------------------------

#[test]
fn values_are_equal_as_python_compares_them() {
    // Numbers by value: an int and its float, -0.0 and 0.0, True and 1; an int past 64 bits and its
    // float.
    assert!(py_eq(&V::Int(1), &V::Float(1.0)));
    assert!(py_eq(&V::Float(-0.0), &V::Float(0.0)));
    assert!(py_eq(&V::Float(-0.0), &V::Int(0)));
    assert!(py_eq(&V::Bool(true), &V::Int(1)));
    assert!(py_eq(&V::Big("1180591620717411303424".into()), &V::Float(1180591620717411303424.0)));
    assert!(!py_eq(&V::Big("1180591620717411303425".into()), &V::Float(1.5)));
    assert!(!py_eq(&V::Int(1), &V::Float(1.5)));
    assert!(!py_eq(&V::Int(1), &V::Str(text("1"))));
    assert!(!py_eq(&V::Null, &V::Int(0)));
    // A dict by its keys in any order, nested; a list item by item.
    let first = map(&[("a", V::Int(1)), ("b", map(&[("x", V::List(vec![V::Int(2), V::Float(0.0)]))]))]);
    let second = map(&[("b", map(&[("x", V::List(vec![V::Float(2.0), V::Float(-0.0)]))])), ("a", V::Float(1.0))]);
    assert!(py_eq(&first, &second));
    let third = map(&[("b", map(&[("x", V::List(vec![V::Float(2.0)]))])), ("a", V::Float(1.0))]);
    assert!(!py_eq(&first, &third));
    assert!(!py_eq(&first, &map(&[("a", V::Int(1))])));
}

#[test]
fn rows_are_the_same_event_as_their_to_dict_compares() {
    let row = event("e1", 1, "probe", &[("n", V::Int(1)), ("x", V::Float(-0.0))]);
    let floats = event("e1", 1, "probe", &[("x", V::Float(0.0)), ("n", V::Float(1.0))]);
    assert!(same_event(&row, &floats));
    let other = event("e1", 1, "probe", &[("n", V::Int(2)), ("x", V::Float(0.0))]);
    assert!(!same_event(&row, &other));
    let later = event("e1", 2, "probe", &[("n", V::Int(1)), ("x", V::Float(0.0))]);
    assert!(!same_event(&row, &later));
}

// ---- Which due event runs -------------------------------------------------------------------------

/// The first event's handler puts another row of the second event in the queue: an int of the
/// payload as its float runs it (the same event), another value skips it, and the replacing row
/// stays pending.
#[test]
fn a_replaced_row_runs_only_if_it_is_the_same_event_by_value() {
    for (replacement, runs) in [(V::Float(1.0), true), (V::Int(2), false)] {
        let cell = world(vec![event("first", 1, "probe", &[]), event("second", 2, "probe", &[("n", V::Int(1))])]);
        let mut host = TestHost::default();
        let held = cell.clone();
        let replacement = replacement.clone();
        host.foreign = Some(Box::new(move |row: &EventRow| {
            if &*row.event_id == "first" {
                held.with(|world| {
                    let mut queue = world.queue_mut().unwrap();
                    queue.take_slot("second").expect("pending");
                    assert_eq!(queue.schedule_row(event("second", 2, "probe", &[("n", replacement.clone())])), 0);
                })
                .unwrap();
            }
            applied()
        }));
        dispatched(&cell, &mut host, &["probe"]).unwrap();
        let ran: Vec<Call> = foreign_calls(&host);
        if runs {
            assert_eq!(ran, vec![Call::Foreign(text("probe")), Call::Foreign(text("probe"))]);
            assert_eq!(records(&cell), vec![("first".to_owned(), EventStatus::Applied, MINUTE), ("second".to_owned(), EventStatus::Applied, MINUTE)]);
            assert!(pending(&cell).is_empty());
        } else {
            assert_eq!(ran, vec![Call::Foreign(text("probe"))]);
            assert_eq!(records(&cell), vec![("first".to_owned(), EventStatus::Applied, MINUTE)]);
            assert_eq!(pending(&cell), vec!["second".to_owned()]);
        }
    }
}

#[test]
fn an_event_that_left_the_queue_is_skipped_and_one_without_a_handler_fails_with_python_s_text() {
    let cell = world(vec![event("first", 1, "probe", &[]), event("second", 2, "probe", &[]), event("third", 3, "no_such_event", &[])]);
    let mut host = TestHost::default();
    let held = cell.clone();
    host.foreign = Some(Box::new(move |row: &EventRow| {
        if &*row.event_id == "first" {
            held.with(|world| world.queue_mut().unwrap().take_slot("second").map(drop)).unwrap();
        }
        applied()
    }));
    dispatched(&cell, &mut host, &["probe"]).unwrap();
    assert_eq!(records(&cell), vec![("first".to_owned(), EventStatus::Applied, MINUTE), ("third".to_owned(), EventStatus::Failed, MINUTE)]);
    assert_eq!(detail_of(&cell, "third").as_deref(), Some("no handler registered for event type 'no_such_event'"));
}

// ---- Records ------------------------------------------------------------------------------------------

/// A handler that wrote the event's own record: the settle compares it (through the host) and
/// appends nothing; one of another status is Python's `ValueError`.
#[test]
fn a_record_the_handler_wrote_is_compared_with_the_outcome() {
    for (status, agrees) in [(EventStatus::Applied, true), (EventStatus::Failed, false)] {
        let row = event("first", 1, "probe", &[]);
        let cell = world(vec![row.clone()]);
        let mut host = TestHost::default();
        host.records.insert(text("first"), Logged { status, realized_minute: MINUTE, event: row.clone() });
        let held = cell.clone();
        host.foreign = Some(Box::new(move |row: &EventRow| {
            held.with(|world| sheltersim_core::rules::kit::log_record(world, row, MINUTE, status, None).unwrap()).unwrap();
            applied()
        }));
        let done = dispatched(&cell, &mut host, &["probe"]);
        if agrees {
            done.unwrap();
        } else {
            assert_eq!(done, Err(Error::Value(RECORD_DISAGREES.to_owned())));
        }
        assert_eq!(records(&cell).len(), 1);
        assert!(host.calls.contains(&Call::FindRecord(text("first"))));
    }
}

/// The handler's follow-up records come after the event's own; one the log holds already must be
/// the same event with the same status.
#[test]
fn follow_up_records_are_appended_after_the_event_s_own_or_compared() {
    let receipt = event("receipt-1", 9, "probe_receipt", &[("n", V::Int(1))]);
    for (logged, agrees) in [(None, true), (Some(receipt.clone()), true), (Some(event("receipt-1", 9, "probe_receipt", &[("n", V::Int(2))])), false)] {
        let cell = world(vec![event("first", 1, "probe", &[])]);
        let mut host = TestHost::default();
        if let Some(logged) = &logged {
            cell.with(|world| sheltersim_core::rules::kit::log_record(world, logged, MINUTE, EventStatus::Applied, Some("before")).unwrap()).unwrap();
            host.records.insert(text("receipt-1"), Logged { status: EventStatus::Applied, realized_minute: MINUTE, event: logged.clone() });
        }
        let given = receipt.clone();
        host.foreign = Some(Box::new(move |_: &EventRow| {
            let record = ForeignRecord { event: given.clone(), realized_minute: MINUTE, status: EventStatus::Applied, detail: Some("made".to_owned()) };
            Ok(Foreign { status: EventStatus::Applied, detail: None, records: vec![record] })
        }));
        let done = dispatched(&cell, &mut host, &["probe"]);
        if !agrees {
            assert_eq!(done, Err(Error::Value(FOLLOW_UP_DISAGREES.to_owned())));
            continue;
        }
        done.unwrap();
        let ids: Vec<String> = records(&cell).into_iter().map(|(id, _, _)| id).collect();
        match logged {
            None => assert_eq!(ids, vec!["first".to_owned(), "receipt-1".to_owned()]),
            Some(_) => assert_eq!(ids, vec!["receipt-1".to_owned(), "first".to_owned()]),
        }
    }
}

// ---- The order of the host's calls --------------------------------------------------------------

/// A batch in which the first handler puts the world in canonical order, as a rule's transaction in
/// place does (every later row of the queue is then another row of the same event): every event still
/// runs, in the queue's order, each record in turn; the space is refreshed once, after the events'
/// records, for the structure's change; the social policy is asked last, for the contact of two people
/// of one household above the stress threshold.
#[test]
fn the_host_is_asked_in_the_phase_s_order_across_a_canonical_batch() {
    let contact = event("contact-1", 3, "information_contact_update", &[("sender_agent_id", V::Str(text("a1"))), ("recipient_agent_id", V::Str(text("a2")))]);
    let cell = world(vec![
        event("first", 1, "probe", &[("b", V::Int(1)), ("a", V::Int(2))]),
        event("structure-1", 2, "effect_program_structure", &[("b", V::Int(1)), ("a", V::Int(2))]),
        contact,
        event("unknown-1", 4, "nothing", &[]),
    ]);
    let mut host = TestHost::default();
    host.llm.social_policy = true;
    let held = cell.clone();
    let row_of = |world: &World, id: &str| {
        let queue = world.queue().unwrap();
        queue.slot(queue.serial_of(id).unwrap()).row.clone().unwrap()
    };
    let before = cell.with(|world| row_of(world, "structure-1")).unwrap();
    let replaced = Rc::new(RefCell::new(false));
    let seen = replaced.clone();
    host.foreign = Some(Box::new(move |row: &EventRow| {
        if &*row.event_id == "first" {
            held.with(|world| {
                world.canonicalize();
                // Another row of the same event: its payload's keys are in canonical order now.
                let after = row_of(world, "structure-1");
                let keys = |row: &EventRow| match &row.payload {
                    V::Map(payload) => payload.keys().map(|key| key.to_string()).collect::<Vec<_>>(),
                    _ => Vec::new(),
                };
                *seen.borrow_mut() = !Arc::ptr_eq(&after, &before) && same_event(&after, &before) && keys(&after) != keys(&before);
            })
            .unwrap();
        }
        applied()
    }));
    dispatched(&cell, &mut host, &["probe", "effect_program_structure", "information_contact_update"]).unwrap();
    // The canonical order gave the pending events other rows: they ran as the same events by value.
    assert!(*replaced.borrow());
    assert_eq!(
        foreign_calls(&host),
        vec![
            Call::Foreign(text("probe")),
            Call::Foreign(text("effect_program_structure")),
            Call::Foreign(text("information_contact_update")),
            Call::SpaceRefreshed,
            Call::Cooperation { initiator: text("a1"), participant: text("a2"), trigger: text("contact-1") },
        ]
    );
    assert_eq!(
        records(&cell),
        vec![
            ("first".to_owned(), EventStatus::Applied, MINUTE),
            ("structure-1".to_owned(), EventStatus::Applied, MINUTE),
            ("contact-1".to_owned(), EventStatus::Applied, MINUTE),
            ("unknown-1".to_owned(), EventStatus::Failed, MINUTE),
        ]
    );
    // The records keep the events' sequences, in the queue's order.
    let sequences = cell
        .with(|world| {
            let log = world.log().unwrap();
            (0..log.len()).map(|index| EventRow::from_saved_record(log.written(index).unwrap()).unwrap().0.sequence).collect::<Vec<_>>()
        })
        .unwrap();
    assert_eq!(sequences, vec![1, 2, 3, 4]);
    assert!(pending(&cell).is_empty());
}

/// Without a social policy the contact asks nothing, nor does a contact of a person with themself;
/// two people of one household, one above the stress threshold, ask for cooperation.
#[test]
fn the_contact_gate_asks_only_with_a_social_policy_between_two_people_of_a_household() {
    for (policy, sender, asks) in [(false, "a1", false), (true, "a1", true), (true, "a2", false)] {
        let payload = [("sender_agent_id", V::Str(text(sender))), ("recipient_agent_id", V::Str(text("a2")))];
        let cell = world(vec![event("contact-1", 1, "information_contact_update", &payload)]);
        let mut host = TestHost::default();
        host.llm.social_policy = policy;
        host.foreign = Some(Box::new(|_: &EventRow| applied()));
        dispatched(&cell, &mut host, &["information_contact_update"]).unwrap();
        let asked = host.calls.iter().any(|call| matches!(call, Call::Cooperation { .. }));
        assert_eq!(asked, asks, "policy {policy}, sender {sender}");
    }
}

/// A due event no row holds stops the phase before any event: the core runs rows only (H6).
#[test]
fn a_due_event_without_a_row_refuses_before_any_event() {
    let cell = world(vec![event("first", 1, "probe", &[])]);
    cell.with(|world| {
        world.queue_mut().unwrap().add(text("objectonly"), (MINUTE, 0, 2), None);
    })
    .unwrap();
    let mut host = TestHost::default();
    host.foreign = Some(Box::new(|_: &EventRow| applied()));
    assert_eq!(dispatched(&cell, &mut host, &["probe"]), Err(Error::Unsupported(dispatch::NO_ROW.into())));
    assert!(records(&cell).is_empty());
    assert!(!host.calls.iter().any(|call| matches!(call, Call::Foreign(_))));
}

// ---- A dispatcher a caller holds alone (decision D3, lane P9-3b) -------------------------------------

/// `EventDispatcher.dispatch_one` and `settle` of a dispatcher alone, at `MINUTE`.
fn alone<T>(cell: &Shard<World>, host: &mut TestHost, foreign: &[&str], f: impl FnOnce(&mut Tick<'_, Shard<World>, TestHost, RefCell<Run>>) -> T) -> T {
    let (run, policies) = run(foreign);
    let mut tick = Tick { cell, host, run: &run, policies, minute: MINUTE, seed: Seed::Other, neighbors: neighbors() };
    f(&mut tick)
}

/// One event of a snapshot: none once it left the queue or the queue holds another event of its ID;
/// else the handler's (the host's) outcome, the event settled, and its follow-up records appended or
/// compared, without any of the phase's steps after the events (no receipts taken in, no space
/// refreshed, nothing asked of the social policy). No handler is the FAILED outcome.
#[test]
fn an_event_of_a_dispatcher_alone_runs_as_python_s_dispatch_one() {
    let row = event("first", 1, "probe", &[("n", V::Int(1))]);
    let receipt = event("receipt-1", 9, "probe_receipt", &[]);
    let cell = world(vec![row.clone(), event("second", 2, "unknown", &[])]);
    let mut host = TestHost::default();
    let given = receipt.clone();
    host.foreign = Some(Box::new(move |_: &EventRow| {
        let record = ForeignRecord { event: given.clone(), realized_minute: MINUTE, status: EventStatus::Applied, detail: Some("made".to_owned()) };
        Ok(Foreign { status: EventStatus::Applied, detail: None, records: vec![record] })
    }));
    // Another event of the same ID (an unequal payload) is not this one.
    let other = event("first", 1, "probe", &[("n", V::Int(2))]);
    assert_eq!(alone(&cell, &mut host, &["probe"], |tick| dispatch::one_alone(tick, &other)), Ok(None));
    assert!(records(&cell).is_empty());
    // An equal one by value (an int and its float) is.
    let equal = event("first", 1, "probe", &[("n", V::Float(1.0))]);
    let ids = alone(&cell, &mut host, &["probe"], |tick| dispatch::one_alone(tick, &equal)).unwrap();
    assert_eq!(ids, Some(vec![text("first"), text("receipt-1")]));
    assert_eq!(records(&cell), vec![("first".to_owned(), EventStatus::Applied, MINUTE), ("receipt-1".to_owned(), EventStatus::Applied, MINUTE)]);
    assert_eq!(pending(&cell), vec!["second".to_owned()]);
    assert_eq!(foreign_calls(&host), vec![Call::Foreign(text("probe"))]);
    // It left the queue: nothing.
    assert_eq!(alone(&cell, &mut host, &["probe"], |tick| dispatch::one_alone(tick, &row)), Ok(None));
    // No handler for its type.
    let unknown = event("second", 2, "unknown", &[]);
    assert_eq!(alone(&cell, &mut host, &["probe"], |tick| dispatch::one_alone(tick, &unknown)), Ok(Some(vec![text("second")])));
    assert_eq!(detail_of(&cell, "second"), Some("no handler registered for event type 'unknown'".to_owned()));
    assert!(pending(&cell).is_empty());
}

/// `settle` of a dispatcher alone: the event leaves the queue and is recorded, or the record its
/// handler wrote is compared.
#[test]
fn a_dispatcher_alone_settles_as_python_s_settle() {
    let row = event("first", 1, "probe", &[]);
    let cell = world(vec![row.clone()]);
    let mut host = TestHost::default();
    alone(&cell, &mut host, &[], |tick| dispatch::settle_alone(tick, &row, EventStatus::Failed, Some("no"))).unwrap();
    assert_eq!(records(&cell), vec![("first".to_owned(), EventStatus::Failed, MINUTE)]);
    assert_eq!(detail_of(&cell, "first"), Some("no".to_owned()));
    assert!(pending(&cell).is_empty());
    host.records.insert(text("first"), Logged { status: EventStatus::Failed, realized_minute: MINUTE, event: row.clone() });
    alone(&cell, &mut host, &[], |tick| dispatch::settle_alone(tick, &row, EventStatus::Failed, None)).unwrap();
    let disagrees = alone(&cell, &mut host, &[], |tick| dispatch::settle_alone(tick, &row, EventStatus::Applied, None));
    assert_eq!(disagrees, Err(Error::Value(RECORD_DISAGREES.to_owned())));
    assert_eq!(records(&cell).len(), 1);
}
