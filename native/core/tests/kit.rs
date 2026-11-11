//! P8-0a: the helpers the rules share (`rules::kit`), over worlds and values built here. The PyO3
//! crate's helpers of the same names are glue over these, so the gate and the offline tests run
//! them on every Run; these say what each does with no Python.

use std::cell::RefCell;
use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::entities::{Activity, AgentRow, AnyTable, Entities, Mobility, NeedRow, ProfileRow, Table};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::writer::WriteError;
use sheltersim_core::events::{EventKind, EventStatus};
use sheltersim_core::geometry::Pt;
use sheltersim_core::rng::{label, Draws, Key, KeyedDraws};
use sheltersim_core::row::{Atom, IdMap, TreeSink, ValueRow};
use sheltersim_core::rules::kit::{self, Admission, Counts, Events, NewEvent, NoTree, Placed};
use sheltersim_core::state::effect_program::{EffectProgramRow, FacilityModifiers, Modifier};
use sheltersim_core::state::metric_state::MetricRow;
use sheltersim_core::state::movement::{MovementRow, Point, Position};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, Seed, World, ID_COUNTERS, METRIC_STATE};

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn map(entries: &[(&str, V)]) -> V {
    V::Map(entries.iter().map(|(key, value)| (text(key), value.clone())).collect())
}

fn string(value: &str) -> V {
    V::Str(text(value))
}

fn list(items: &[V]) -> V {
    V::List(items.to_vec())
}

fn unsupported(reason: &str) -> Error {
    Error::Unsupported(reason.to_owned().into())
}

fn needs() -> NeedRow {
    NeedRow { toilet_urgency: 0.0, hunger: 0.0, fatigue: 0.0, hydration_need: 0.0 }
}

fn agent(id: &str, group: Option<&str>, mobility: Mobility) -> AgentRow {
    AgentRow {
        profile: ProfileRow {
            id: text(id),
            group_id: group.map(text),
            initial_zone_id: text("z0"),
            mobility,
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
        stress: 0.0,
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

fn people() -> Entities {
    let mut entities = Entities::default();
    put(&mut entities.agents, "a1", agent("a1", Some("g1"), Mobility::Wheelchair));
    put(&mut entities.agents, "a2", agent("a2", None, Mobility::Independent));
    entities
}

// ---- Counts --------------------------------------------------------------------------------------

#[test]
fn counts_keep_what_ran_and_the_refusals_by_reason() {
    let counts = Counts::new();
    assert_eq!(counts.counted("b", Ok(1)), Ok(1));
    assert_eq!(counts.counted("a", Ok(2)), Ok(2));
    counts.counted("a", Ok(())).unwrap();
    assert!(counts.counted::<()>("a", Err(unsupported("no row"))).is_err());
    assert!(counts.counted::<()>("a", Err(unsupported("no row"))).is_err());
    // An error that is not a refusal is not counted.
    assert!(counts.counted::<()>("a", Err(Error::Value("bad".into()))).is_err());
    assert_eq!(counts.ran_counts(), [("a", 2), ("b", 1)]);
    assert_eq!(counts.refusal_counts(), [("a: no row".to_owned(), 2)]);
}

// ---- Values and events -----------------------------------------------------------------------------

#[test]
fn values_are_read_as_python_reads_them() {
    let payload = map(&[("agent_id", string("a1")), ("count", V::Int(2))]);
    assert_eq!(kit::get(&payload, "agent_id"), Ok(&string("a1")));
    assert_eq!(kit::get(&payload, "missing"), Ok(&V::Null));
    assert_eq!(kit::get(&V::Int(1), "x"), Err(unsupported("a value is not a plain dict")));
    assert_eq!(kit::mapping_get(&payload, "count"), Ok(Some(&V::Int(2))));
    assert_eq!(kit::mapping_get(&payload, "missing"), Ok(None));
    assert_eq!(kit::mapping_get(&list(&[]), "x"), Err(unsupported("a payload cannot be read")));
    assert_eq!(kit::exact_int(&V::Int(3)), Ok(3));
    assert_eq!(kit::exact_int(&V::Bool(true)), Err(unsupported("a value is not an int")));
    assert_eq!(kit::exact_int(&V::Big("99999999999999999999".into())), Err(unsupported("an int is too large")));
    assert_eq!(kit::exact_str(&string("x")).map(|text| &**text), Ok("x"));
    assert_eq!(kit::optional_text(&V::Null), Ok(None));
    assert_eq!(kit::status_member("APPLIED"), Some(EventStatus::Applied));
    assert_eq!(kit::status_member("FAILED"), Some(EventStatus::Failed));
    assert_eq!(kit::status_member("SUPPRESSED"), Some(EventStatus::Suppressed));
    assert_eq!(kit::status_member("applied"), None);
}

fn new_event(event_id: &str, due: i64, sequence: i64) -> NewEvent {
    let mut payload = IndexMap::new();
    payload.insert(text("zeta"), V::Int(1));
    payload.insert(text("alpha"), string("a"));
    NewEvent {
        event_id: text(event_id),
        kind: EventKind::Endogenous,
        due_minute: due,
        priority: 0,
        sequence,
        event_type: text("staff_task"),
        payload,
        source_id: Some(text("SRC-1")),
        causation_id: None,
        provenance: kit::assumption(string("SRC-1"), "policy-v1"),
    }
}

#[test]
fn an_event_is_made_as_a_row_with_its_keys_in_the_order_given() {
    let row = kit::new_event(new_event("e-1", 5, 3)).unwrap();
    assert!(kit::kind_is(&row, EventKind::Endogenous) && !kit::kind_is(&row, EventKind::Completion));
    let V::Map(payload) = &row.payload else { panic!("a payload is a map") };
    assert_eq!(payload.keys().map(|key| &**key).collect::<Vec<_>>(), ["zeta", "alpha"]);
    assert_eq!(kit::payload(&row, "alpha"), Some(&string("a")));
    let V::Map(provenance) = &row.provenance else { panic!("a provenance is a map") };
    assert_eq!(provenance.keys().map(|key| &**key).collect::<Vec<_>>(), ["source_type", "source_id", "policy_version"]);
    assert_eq!(kit::provenance(&row, "source_type"), Some(&string("assumption")));
    assert_eq!(kit::provenance(&row, "policy_version"), Some(&string("policy-v1")));
    // What ScheduledEvent refuses: a bad ID, a negative minute, a float that is not finite.
    assert_eq!(kit::new_event(new_event("bad id", 5, 3)).err(), Some(unsupported("the event's values are not what an event takes")));
    assert!(kit::new_event(new_event("e-2", -1, 3)).is_err());
    let mut event = new_event("e-3", 5, 3);
    event.payload.insert(text("nan"), V::Float(f64::NAN));
    assert!(kit::new_event(event).is_err());
}

/// A world that holds its queue and log, of 100 minutes, with `id_counters`.
fn event_world(counter: Option<V>) -> World {
    let mut world = World::default();
    world.scalars.get_mut().duration_minutes = Fit::Ok(100);
    let index = world.slots.add(text(ID_COUNTERS), Place::Field).unwrap();
    let mut counters = IndexMap::new();
    if let Some(counter) = counter {
        counters.insert(text("event"), counter);
    }
    counters.insert(text("task"), V::Int(7));
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(ValueRow(V::Map(counters))));
    world.hold_queue(Arc::new(Queue::new()));
    world.hold_log(Arc::new(EventLog::new()));
    world
}

fn counters(world: &World) -> V {
    world.row::<ValueRow>(ID_COUNTERS).unwrap().0.clone()
}

#[test]
fn events_are_scheduled_and_recorded_on_the_worlds_queue_log_and_counter() {
    let world = event_world(Some(V::Int(10)));
    let events = Events::of(&world).unwrap();
    assert_eq!(events.duration(), 100);
    assert_eq!(events.next_sequence(), Ok(10));
    events.reserve(2);
    assert_eq!(events.next_sequence(), Ok(12));
    assert_eq!(events.allocate_reserved(), Ok(10));
    assert_eq!(events.allocate_reserved(), Ok(11));
    assert_eq!(events.next_sequence(), Ok(12));
    let sequence = events.allocate_sequence().unwrap();
    assert_eq!(sequence, 12);
    // The counter keeps its place among the keys, as a dict does.
    assert_eq!(counters(&world), map(&[("event", V::Int(13)), ("task", V::Int(7))]));

    // A row of a later sequence moves the counter past it.
    events.schedule(kit::new_event(new_event("e-a", 20, 30)).unwrap()).unwrap();
    assert_eq!(events.next_sequence(), Ok(31));
    assert!(events.is_pending("e-a").unwrap() && events.is_taken("e-a", None).unwrap());
    events.schedule(kit::new_event(new_event("e-b", 20, 2)).unwrap()).unwrap();
    assert_eq!(events.next_sequence(), Ok(31));

    let realized = kit::new_event(new_event("e-c", 4, 31)).unwrap();
    events.record(&realized, 4, EventStatus::Applied, None).unwrap();
    assert!(events.is_realized("e-c", None).unwrap());
    assert!(events.is_realized("other", Some(31)).unwrap());
    assert!(!events.is_taken("e-d", Some(32)).unwrap());
    assert_eq!(world.log().unwrap().len(), 1);
    assert_eq!(events.with_log(|log| log.has_event_id("e-c")), Ok(true));
    // `log_record` writes the same log.
    let failed = kit::new_event(new_event("e-d", 4, 32)).unwrap();
    kit::log_record(&world, &failed, 4, EventStatus::Failed, Some("why")).unwrap();
    assert_eq!(world.log().unwrap().len(), 2);

    // The errors of `schedule_event` and `EventLog.append`.
    assert_eq!(
        events.schedule(kit::new_event(new_event("e-a", 30, 40)).unwrap()),
        Err(WriteError::Duplicate("duplicate event ID 'e-a'".to_owned()))
    );
    assert_eq!(
        events.schedule(kit::new_event(new_event("e-c", 30, 41)).unwrap()),
        Err(WriteError::Duplicate("event 'e-c' already realized".to_owned()))
    );
    assert_eq!(
        events.schedule(kit::new_event(new_event("e-late", 100, 42)).unwrap()),
        Err(WriteError::Error(Error::Value("event 'e-late' is outside the scenario".to_owned())))
    );
    assert_eq!(
        events.record(&realized, 5, EventStatus::Applied, None),
        Err(WriteError::Duplicate("event 'e-c' already realized".to_owned()))
    );
    assert_eq!(
        kit::log_record(&world, &kit::new_event(new_event("e-e", 4, 50)).unwrap(), -1, EventStatus::Applied, None),
        Err(WriteError::Error(Error::Value("realized_minute must be a non-negative integer".to_owned())))
    );
    assert_eq!(world.queue().unwrap().pending_ids(None).iter().map(|id| &**id).collect::<Vec<_>>(), ["e-b", "e-a"]);
}

#[test]
fn a_world_whose_events_a_rule_cannot_write_is_refused_before_any_write() {
    // No counter yet: the first sequence is 0, and the counter is added at the end.
    let world = event_world(None);
    let events = Events::of(&world).unwrap();
    assert_eq!(events.allocate_sequence(), Ok(0));
    assert_eq!(counters(&world), map(&[("task", V::Int(7)), ("event", V::Int(1))]));

    let refused = |world: &World| Events::of(world).err();
    let reason = "the event counter is not an int a rule can add to";
    assert_eq!(refused(&event_world(Some(V::Float(1.0)))), Some(unsupported(reason)));
    assert_eq!(refused(&event_world(Some(V::Int((1 << 62) + 1)))), Some(unsupported(reason)));
    assert!(refused(&event_world(Some(V::Int(1 << 62)))).is_none());
    let mut world = event_world(Some(V::Int(0)));
    world.scalars.get_mut().duration_minutes = Fit::Misfit;
    assert_eq!(refused(&world), Some(unsupported("the duration is not an int")));
    let mut world = event_world(Some(V::Int(0)));
    world.release_log();
    assert_eq!(refused(&world), Some(unsupported("the event log is not in Rust")));
    world.release_queue();
    assert_eq!(refused(&world), Some(unsupported("the event queue is not in Rust")));
    let mut bare = World::default();
    bare.hold_queue(Arc::new(Queue::new()));
    bare.hold_log(Arc::new(EventLog::new()));
    assert_eq!(refused(&bare), Some(unsupported(reason)));
}

// ---- Metrics -------------------------------------------------------------------------------------

/// A tree that writes down what it was given.
#[derive(Default)]
struct Written(RefCell<Vec<String>>);

impl TreeSink for Written {
    type Error = ();

    fn set_field(&self, key: &'static str, value: Atom<'_>) -> Result<(), ()> {
        self.0.borrow_mut().push(format!("field {key} {value:?}"));
        Ok(())
    }

    fn set_entry(&self, key: &str, value: Atom<'_>) -> Result<(), ()> {
        self.0.borrow_mut().push(format!("entry {key} {value:?}"));
        Ok(())
    }
}

#[test]
fn a_metric_counts_one_more_on_the_row_and_the_tree() {
    let mut row = MetricRow::default();
    row.insert(text("b"), 2.0);
    let tree = Written::default();
    kit::add_one(&mut row, &tree, "a").unwrap();
    kit::add_one(&mut row, &tree, "b").unwrap();
    kit::add_one(&mut row, &tree, "a").unwrap();
    assert_eq!(row.iter().map(|(key, value)| (&**key, *value)).collect::<Vec<_>>(), [("b", 3.0), ("a", 2.0)]);
    assert_eq!(*tree.0.borrow(), ["entry a Float(1.0)", "entry b Float(3.0)", "entry a Float(2.0)"]);
    let Ok(()) = kit::add_one(&mut row, &NoTree, "c");

    let mut world = World::default();
    assert_eq!(kit::add_metric(&world, "x"), Err(unsupported("the namespace is not owned")));
    let index = world.slots.add(text(METRIC_STATE), Place::Field).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(MetricRow::default()));
    kit::add_metric(&world, "x").unwrap();
    kit::add_metric(&world, "x").unwrap();
    assert_eq!(kit::metrics_of(&world).unwrap().get("x"), Some(&2.0));
    // The row alone was written: its slot is marked, for the tree to follow (lane P8-0c).
    assert_eq!(world.slots.take_written(), [index]);
}

#[test]
fn an_event_sequence_marks_the_counters_slot_and_a_read_does_not() {
    let mut world = event_world(Some(V::Int(4)));
    let events = Events::of(&world).unwrap();
    assert_eq!(events.next_sequence(), Ok(4));
    assert!(events.is_taken("e-x", None).is_ok());
    assert!(world.slots.written_names().is_empty());
    assert_eq!(events.allocate_sequence(), Ok(4));
    assert_eq!(world.slots.written_names().iter().map(|name| &**name).collect::<Vec<_>>(), [ID_COUNTERS]);
    assert_eq!(world.slots.take_written(), [0]);
}

// ---- People --------------------------------------------------------------------------------------

#[test]
fn a_persons_activity_target_and_mobility() {
    let mut entities = people();
    kit::set_agent(&mut entities, "a1", Activity::Moving, Some("f1")).unwrap();
    let row = entities.agents.get("a1").unwrap();
    assert_eq!((row.activity, row.target_id.as_deref()), (Activity::Moving, Some("f1")));
    kit::set_agent(&mut entities, "a1", Activity::Idle, None).unwrap();
    assert_eq!(entities.agents.get("a1").unwrap().target_id, None);
    assert_eq!(kit::set_agent(&mut entities, "nobody", Activity::Idle, None), Err(unsupported("no such agent")));
    assert_eq!(kit::mobility_text(entities.agents.get("a1").unwrap()), "wheelchair");
    assert_eq!(kit::mobility_text(entities.agents.get("a2").unwrap()), "independent");
}

#[test]
fn a_household_is_the_ledgers_or_else_the_profiles_group() {
    let entities = people();
    let ledger = map(&[(
        "people",
        map(&[
            ("a1", map(&[("household_id", string("h9"))])),
            ("a2", map(&[("household_id", V::Null)])),
            ("a3", map(&[])),
        ]),
    )]);
    let of = |presence: Option<&V>, agent: &str| kit::household_of(presence, &entities, agent);
    assert_eq!(of(Some(&ledger), "a1"), Ok(Some(text("h9"))));
    // The ledger's None is the answer; a person it does not know has none either.
    assert_eq!(of(Some(&ledger), "a2"), Ok(None));
    assert_eq!(of(Some(&ledger), "a4"), Ok(None));
    // No ledger, or one that is not a dict: the profile's group.
    assert_eq!(of(None, "a1"), Ok(Some(text("g1"))));
    assert_eq!(of(Some(&V::Null), "a1"), Ok(Some(text("g1"))));
    assert_eq!(of(None, "nobody"), Ok(None));
    assert_eq!(of(Some(&map(&[])), "a1"), Ok(None));
    assert_eq!(kit::presence_household(None, "a1"), Ok(None));
    assert_eq!(
        of(Some(&map(&[("people", list(&[]))])), "a1"),
        Err(unsupported("the presence state's people are for the Python rule"))
    );
    assert_eq!(
        of(Some(&map(&[("people", map(&[("a1", list(&[]))]))])), "a1"),
        Err(unsupported("a presence record is for the Python rule"))
    );
    assert_eq!(
        of(Some(&map(&[("people", map(&[("a1", map(&[("household_id", V::Int(1))]))]))])), "a1"),
        Err(unsupported("a value is not a str"))
    );

    // Over a world's rows.
    let mut world = World::default();
    *world.entities.get_mut() = people();
    assert_eq!(kit::household_in(&world, "a1"), Ok(Some(text("g1"))));
    let index = world.slots.add(text(kit::PRESENCE), Place::Namespace).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(ValueRow(ledger)));
    assert_eq!(kit::household_in(&world, "a1"), Ok(Some(text("h9"))));
}

// ---- Facilities ----------------------------------------------------------------------------------

#[test]
fn a_toilet_is_known_by_its_folded_kind() {
    for (kind, toilet) in [("toilet", true), ("Accessible_TOILET", true), ("ﬆoilet", true), ("TOİLET", false), ("water", false)] {
        assert_eq!(kit::is_toilet(kind), toilet, "{kind}");
    }
}

#[test]
fn an_outage_is_an_active_record_that_names_the_facility() {
    let outage = |active: V, ids: &[&str]| {
        map(&[("active", active), ("affected_facility_ids", list(&ids.iter().map(|id| string(id)).collect::<Vec<_>>()))])
    };
    let state = map(&[(
        "outages",
        map(&[("o1", outage(V::Bool(false), &["f1"])), ("o2", outage(V::Bool(true), &["f2", "f3"]))]),
    )]);
    assert_eq!(kit::active_outage(Some(&state), "f1"), Ok(false));
    assert_eq!(kit::active_outage(Some(&state), "f3"), Ok(true));
    assert_eq!(kit::active_outage(Some(&state), "f9"), Ok(false));
    assert_eq!(kit::active_outage(None, "f3"), Ok(false));
    assert_eq!(kit::active_outage(Some(&V::Null), "f3"), Ok(false));
    // The KeyError of Python's reads.
    assert_eq!(kit::active_outage(Some(&map(&[])), "f3"), Err(Error::Key("outages".to_owned())));
    let missing = map(&[("outages", map(&[("o1", map(&[("active", V::Bool(true))]))]))]);
    assert_eq!(kit::active_outage(Some(&missing), "f3"), Err(Error::Key("affected_facility_ids".to_owned())));
    // An inactive record is not read further.
    let inactive = map(&[("outages", map(&[("o1", map(&[("active", V::Int(0))]))]))]);
    assert_eq!(kit::active_outage(Some(&inactive), "f3"), Ok(false));
}

#[test]
fn a_modifier_is_active_while_its_facility_has_any() {
    let modifier = Modifier { modifier_id: text("m"), program_id: text("p"), effect_id: text("e"), working_units: 1, start_minute: 0, sequence: 0 };
    let mut active = IdMap::default();
    active.insert(text("f1"), FacilityModifiers { baseline_working_units: 2, modifiers: vec![modifier] });
    active.insert(text("f2"), FacilityModifiers { baseline_working_units: 2, modifiers: Vec::new() });
    let row = EffectProgramRow { schema_version: 1, programs: IdMap::default(), active_facility_modifiers: active };
    assert!(kit::has_active_modifier(Some(&row), "f1"));
    assert!(!kit::has_active_modifier(Some(&row), "f2"));
    assert!(!kit::has_active_modifier(Some(&row), "f3"));
    assert!(!kit::has_active_modifier(None, "f1"));
}

// ---- Places --------------------------------------------------------------------------------------

fn position(zone: &str, point: Option<(f64, f64)>, kind: &str, progress: f64) -> Position {
    Position {
        zone_id: text(zone),
        point: point.map(|(x, y)| Point { x, y }),
        edge_id: None,
        progress,
        position_kind: text(kind),
    }
}

#[test]
fn a_person_is_placed_at_their_bed_on_the_row() {
    let mut positions = IdMap::default();
    positions.insert(text("a1"), position("z0", None, "unavailable", 0.0));
    positions.insert(text("a2"), position("z1", Some((1.0, 2.0)), "physical", 1.0));
    let mut row = MovementRow {
        schema_version: 1,
        policy_version: text("v"),
        positions,
        staff_positions: IdMap::default(),
        journeys: IdMap::default(),
        edge_usage_minutes: IdMap::default(),
        last_updated_minute: 0,
    };
    let bed = Pt { x: 1.0, y: 2.0 };
    assert_eq!(kit::place_in_bed(&mut row, "a3", &text("z1"), bed), Placed::Absent);
    assert_eq!(kit::place_in_bed(&mut row, "a2", &text("z1"), bed), Placed::Same);
    assert_eq!(kit::place_in_bed(&mut row, "a1", &text("z1"), bed), Placed::Changed(text("a1")));
    // In place: the dict's order stays.
    assert_eq!(row.positions.keys().map(|key| &**key).collect::<Vec<_>>(), ["a1", "a2"]);
    assert!(kit::same_position(&row.positions["a1"], &position("z1", Some((1.0, 2.0)), "physical", 1.0)));
    // -0.0 and 0.0 are other bits.
    assert_eq!(kit::place_in_bed(&mut row, "a1", &text("z1"), Pt { x: -0.0, y: 2.0 }), Placed::Changed(text("a1")));
    assert!(!kit::same_position(&row.positions["a1"], &row.positions["a2"]));
}

// ---- Population ----------------------------------------------------------------------------------

#[test]
fn a_refusal_after_a_write_began_is_a_runtime_error() {
    assert_eq!(kit::writing(Ok(3)), Ok(3));
    assert_eq!(
        kit::writing::<()>(Err(unsupported("no row"))),
        Err(Error::Runtime("a kernel refused after it began writing: Unsupported: no row".to_owned()))
    );
    assert_eq!(kit::writing::<()>(Err(Error::Value("bad".into()))), Err(Error::Value("bad".into())));
}

/// An `Admission` that writes down what it was asked, and refuses a zone it is told to.
#[derive(Default)]
struct Asked {
    calls: Vec<String>,
    full: Option<&'static str>,
}

impl Admission for Asked {
    type Error = Error;

    fn error(error: Error) -> Error {
        error
    }

    fn begin(&mut self) -> Result<(), Error> {
        self.calls.push("begin".to_owned());
        Ok(())
    }

    fn room(&mut self, zone: &str) -> Result<(), Error> {
        self.calls.push(format!("room {zone}"));
        if self.full == Some(zone) {
            return Err(unsupported("no room"));
        }
        Ok(())
    }

    fn paths(&mut self, zones: &[&str]) -> Result<(), Error> {
        self.calls.push(format!("paths {}", zones.join(",")));
        Ok(())
    }
}

fn waiting(agent: &str, zone: &str, more: &[(&str, V)]) -> V {
    let mut entries = vec![
        ("created_minute", V::Int(3)),
        ("event_id", string(&format!("arrival-{agent}"))),
        ("zone_id", string(zone)),
        ("profile", map(&[("id", string(agent)), ("group_id", V::Null)])),
    ];
    entries.extend(more.iter().cloned());
    map(&entries)
}

fn dynamics(pending: Vec<V>, arrivals: Vec<V>) -> V {
    map(&[("pending_arrivals", V::List(pending)), ("arrivals", V::List(arrivals))])
}

fn check(state: Option<&V>) -> (Result<(), Error>, Vec<String>) {
    let mut asked = Asked::default();
    let result = kit::check_waiting_for_admission(state, &mut asked);
    (result, asked.calls)
}

#[test]
fn the_waiting_are_checked_in_the_order_admission_reads_them() {
    // Nothing waiting: nothing asked.
    assert_eq!(check(None), (Ok(()), vec![]));
    assert_eq!(check(Some(&dynamics(vec![], vec![]))), (Ok(()), vec![]));

    let state = dynamics(
        vec![
            waiting("p1", "z1", &[("admission_zone_ids", list(&[string("z2"), string("z3")]))]),
            waiting("p2", "z2", &[("traits", map(&[("household_id", string("h1"))])), ("approach_route_id", string("r1"))]),
        ],
        vec![map(&[("requested_count", V::Int(2)), ("event_id", string("e1"))]), map(&[("agent_id", string("x"))])],
    );
    let (result, calls) = check(Some(&state));
    assert_eq!(result, Ok(()));
    assert_eq!(calls, ["begin", "room z1", "room z2", "room z3", "room z2", "paths z1,z2,z3,z2"]);

    // A refusal of a zone stops there.
    let mut asked = Asked { full: Some("z2"), ..Asked::default() };
    assert_eq!(kit::check_waiting_for_admission(Some(&state), &mut asked), Err(unsupported("no room")));
    assert_eq!(asked.calls, ["begin", "room z1", "room z2"]);
}

#[test]
fn a_waiting_arrival_the_admission_could_not_take_is_refused_first() {
    let with = |item: V| dynamics(vec![item], vec![]);
    // Python's KeyError of a missing key.
    assert_eq!(check(Some(&map(&[("arrivals", list(&[]))]))).0, Err(Error::Key("pending_arrivals".to_owned())));
    let no_minute = map(&[("event_id", string("e")), ("zone_id", string("z1"))]);
    assert_eq!(check(Some(&with(no_minute))), (Err(Error::Key("created_minute".to_owned())), vec!["begin".to_owned()]));
    assert_eq!(check(Some(&dynamics(vec![waiting("p1", "z1", &[])], vec![]))).0, Ok(()));
    let no_arrivals = map(&[("pending_arrivals", list(&[waiting("p1", "z1", &[])]))]);
    assert_eq!(check(Some(&no_arrivals)).0, Err(Error::Key("arrivals".to_owned())));

    let refused = |more: &[(&str, V)], zone: &str| check(Some(&with(waiting("p1", zone, more)))).0;
    assert_eq!(refused(&[("created_minute", V::Float(3.0))], "z1"), Err(unsupported("a value is not an int")));
    assert_eq!(refused(&[("approach_route_id", V::Null)], "z1"), Err(unsupported("a value is not a str")));
    assert_eq!(refused(&[("admission_zone_ids", V::Null)], "z1"), Err(unsupported("the admission zones are not a list")));
    assert_eq!(refused(&[("admission_zone_ids", list(&[V::Int(1)]))], "z1"), Err(unsupported("a value is not a str")));
    assert_eq!(refused(&[], "bad zone"), Err(unsupported("a value is not an Identifier")));
    assert_eq!(
        refused(&[("traits", map(&[("household_id", string("not an id"))]))], "z1"),
        Err(unsupported("a value is not an Identifier"))
    );
    // A trait that is not a dict names no household; a group then does.
    assert_eq!(refused(&[("traits", string("x"))], "z1"), Ok(()));
    let grouped = waiting("p1", "z1", &[("profile", map(&[("id", string("p1")), ("group_id", string("bad group"))]))]);
    assert_eq!(check(Some(&with(grouped))).0, Err(unsupported("a value is not an Identifier")));
    let no_group = waiting("p1", "z1", &[("profile", map(&[("id", string("p1"))]))]);
    assert_eq!(check(Some(&with(no_group))).0, Err(Error::Key("group_id".to_owned())));
    // The profile's source, or the population's.
    let sourced = |provenance: V| {
        waiting("p1", "z1", &[("profile", map(&[("id", string("p1")), ("group_id", V::Null), ("provenance", provenance)]))])
    };
    assert_eq!(check(Some(&with(sourced(map(&[("source_id", string("SRC-1"))]))))).0, Ok(()));
    assert_eq!(check(Some(&with(sourced(map(&[]))))).0, Ok(()));
    assert_eq!(check(Some(&with(sourced(map(&[("source_id", V::Null)]))))).0, Err(unsupported("a value is not a str")));
    assert_eq!(check(Some(&with(sourced(V::Null)))).0, Err(unsupported("a provenance is not a plain dict")));
    // The arrivals that hold a count.
    let counted = dynamics(vec![waiting("p1", "z1", &[])], vec![map(&[("requested_count", string("2"))])]);
    assert_eq!(check(Some(&counted)).0, Err(unsupported("a value is not an int")));
}

// ---- Draws ---------------------------------------------------------------------------------------

#[test]
fn draws_are_counter_rngs_of_the_seed() {
    let draws = Draws::of_seed("2026").unwrap();
    let keyed = KeyedDraws::new("2026");
    let mut key = Key::labelled(label::BEHAVIOR);
    key.int(12).text("a1");
    let expected = key.clone().draw(&keyed);
    assert_eq!(draws.random(label::BEHAVIOR, 12, &["a1"]).to_bits(), expected.to_bits());
    assert_eq!(draws.draw(&mut key).to_bits(), expected.to_bits());
    assert_eq!(Key::labelled(label::BEHAVIOR).int(12).text("a1").draw(draws.keyed()).to_bits(), expected.to_bits());
    // A seed past 64 bits keys its draws by its text.
    assert!(Draws::of_seed("123456789012345678901234567890").is_ok());
    assert_eq!(Draws::of_seed("-1").err(), Some(unsupported("a negative seed")));
    assert_eq!(Draws::of_seed("1.5").err(), Some(unsupported("a seed is not an int")));

    let mut world = World::default();
    world.scalars.get_mut().seed = Fit::Ok(Seed::from_text("2026").unwrap());
    assert_eq!(Draws::of_world(&world).unwrap().random(label::BEHAVIOR, 12, &["a1"]).to_bits(), expected.to_bits());
}
