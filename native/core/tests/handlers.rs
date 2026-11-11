//! Lane d2: the rest of the event handlers (`rules::handlers`) on a world built here with no Python.
//! The PyO3 crate's handlers are glue over these, so the gate and the offline tests run them; these
//! say what each reads, decides, refuses and writes, and what it asks the host.

mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;
use sheltersim_core::entities::{Entities, FacilityRow, ZoneRow};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::writer::WriteError;
use sheltersim_core::events::{EventKind, EventRow};
use sheltersim_core::host::{Contract, Decision, DecisionSink, ErrorClass, Host, HostFailure, Llm, Phase, Stage, Trace, TraceLevel, TxnKind};
use sheltersim_core::row::ValueRow;
use sheltersim_core::rules::care_receipts::Made;
use sheltersim_core::rules::common_operations::Logged;
use sheltersim_core::rules::handlers::{self as handlers, FacilityIn, Infra, InfraIn, PopulationOp};
use sheltersim_core::rules::kit::Held;
use sheltersim_core::rules::metrics::MetricSample;
use sheltersim_core::cell::Shard;
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, Snapshot, World, WorldCell, ID_COUNTERS, MODEL_CONFIGS};
use support::{Call, TestHost};

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn s(value: &str) -> V {
    V::Str(text(value))
}

fn map(entries: Vec<(&str, V)>) -> V {
    V::Map(entries.into_iter().map(|(key, value)| (text(key), value)).collect())
}

fn entries(entries: Vec<(&str, V)>) -> IndexMap<Arc<str>, V> {
    let V::Map(map) = map(entries) else { unreachable!() };
    map
}

fn add(world: &mut World, name: &str, place: Place, value: V) {
    let index = world.slots.add(text(name), place).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(ValueRow(value)));
}

fn facility(id: &str, kind: &str, zone: &str, capacity: i64, units: i64) -> FacilityRow {
    FacilityRow {
        id: text(id),
        kind: text(kind),
        zone_id: text(zone),
        capacity,
        working_units: units,
        active_agent_ids: Vec::new(),
        queue_agent_ids: Vec::new(),
        resource_active_agent_ids: Vec::new(),
        resource_queue_agent_ids: Vec::new(),
    }
}

/// A world of 100 minutes at minute 10 (schema 6): zones z0 and z1, a water station in z1 and a
/// toilet of 2 units in z0; its queue, log, `id_counters` (event 40) and `model_configs`, and the
/// namespaces given.
fn world(namespaces: Vec<(&str, V)>) -> World {
    let mut world = World::default();
    let scalars = world.scalars.get_mut();
    scalars.duration_minutes = Fit::Ok(100);
    scalars.minute_index = Fit::Ok(10);
    scalars.schema_version = Fit::Ok(6);
    let mut entities = Entities::default();
    for id in ["z0", "z1"] {
        entities.zones.insert(text(id), ZoneRow { id: text(id), name: text(id), capacity: None, display_x: None, display_y: None });
    }
    entities.facilities.insert(text("water_1"), facility("water_1", "water_station", "z1", 1, 1));
    entities.facilities.insert(text("toilet_1"), facility("toilet_1", "toilet", "z0", 2, 2));
    *world.entities.get_mut() = entities;
    add(&mut world, ID_COUNTERS, Place::Field, map(vec![("event", V::Int(40))]));
    let configs = map(vec![("infrastructure", map(vec![("source_id", s("ASM-INFRA"))]))]);
    add(&mut world, MODEL_CONFIGS, Place::Field, configs);
    for (name, value) in namespaces {
        add(&mut world, name, Place::Value, value);
    }
    world.hold_queue(Arc::new(Queue::new()));
    world.hold_log(Arc::new(EventLog::new()));
    world
}

fn event(event_id: &str, sequence: i64, due: i64, event_type: &str, payload: Vec<(&str, V)>) -> EventRow {
    EventRow::new(text(event_id), EventKind::Exogenous, due, 0, sequence, text(event_type), entries(payload), Some(text("scenario")), None, IndexMap::new()).unwrap()
}

fn failed<T>(result: Result<T, Error>) -> String {
    match result {
        Err(Error::Outcome(_, text)) => text,
        other => panic!("not an outcome: {:?}", other.err()),
    }
}

fn state(world: &World, name: &str) -> V {
    world.slots.try_value(name).unwrap().unwrap().clone()
}

fn records(world: &World) -> Vec<Value> {
    let log = world.log().unwrap();
    (0..log.len()).map(|index| serde_json::from_slice(log.written(index).unwrap()).unwrap()).collect()
}

// ---- A host whose errors take the writer's -------------------------------------------------------

/// A test's error: the core's, or the writer's `DuplicateEventError`.
#[derive(Debug, PartialEq)]
enum Fault {
    Core(Error),
    Duplicate(String),
}

impl From<Error> for Fault {
    fn from(error: Error) -> Self {
        Fault::Core(error)
    }
}

impl From<WriteError> for Fault {
    fn from(error: WriteError) -> Self {
        match error {
            WriteError::Error(error) => Fault::Core(error),
            WriteError::Duplicate(text) => Fault::Duplicate(text),
        }
    }
}

impl HostFailure for Fault {
    fn class(&self) -> ErrorClass {
        match self {
            Fault::Core(error) => error.class(),
            Fault::Duplicate(_) => ErrorClass::Value,
        }
    }
}

/// `TestHost`, with the records `find_record` finds, which writes down the in-flight validations
/// and the slots the rule changed.
#[derive(Default)]
struct Writing {
    host: TestHost,
    in_flight: Vec<Arc<str>>,
    changed: Vec<u64>,
    found: Vec<(Arc<str>, Option<Logged>)>,
}

macro_rules! delegate {
    ($($name:ident($($arg:ident: $kind:ty),*) -> $out:ty;)*) => {
        $(fn $name(&mut self, $($arg: $kind),*) -> Result<$out, Fault> {
            Ok(self.host.$name($($arg),*)?)
        })*
    };
}

impl Host for Writing {
    type Error = Fault;

    delegate! {
        validate_candidate(kind: TxnKind, stage: Stage) -> ();
        validate_world() -> ();
        validate_extensions() -> ();
        contract(which: &Contract, input: &V) -> V;
        prioritize_support_request(request: &str, agent: &str, hash: &str) -> ();
        consider_support_request(agent: &str, request: &str) -> ();
        consider_complaint_response(agent: &str, request: &str) -> ();
        consider_cooperation(initiator: &str, participant: &str, trigger: &str) -> ();
        consider_information_reaction(agent: &str, participant: &str, message: &str, trigger: &str) -> ();
        capture_metric(sample: &MetricSample, minute: i64) -> ();
        decision(decision: Decision) -> ();
        trace(document: Trace) -> ();
        phase(phase: Phase, before: bool) -> ();
        space_refreshed() -> ();
    }

    fn canonicalized(&mut self, kind: TxnKind, before: &Snapshot) -> Result<(), Fault> {
        Ok(self.host.canonicalized(kind, before)?)
    }

    fn llm(&self) -> Result<Llm, Fault> {
        Ok(self.host.llm()?)
    }

    fn decision_sink(&self) -> Result<DecisionSink, Fault> {
        Ok(self.host.decision_sink()?)
    }

    fn trace_level(&self) -> Result<TraceLevel, Fault> {
        Ok(self.host.trace_level()?)
    }

    fn validate_in_flight(&mut self, event: &EventRow) -> Result<(), Fault> {
        self.in_flight.push(event.event_id.clone());
        Ok(())
    }

    fn slots_changed(&mut self, serials: &[u64]) -> Result<(), Fault> {
        self.changed.extend_from_slice(serials);
        Ok(())
    }

    fn find_record(&mut self, event_id: &str) -> Result<Option<Logged>, Fault> {
        Ok(self.found.iter().find(|(id, _)| &**id == event_id).and_then(|(_, logged)| logged.clone()))
    }
}

// ---- Water and staff --------------------------------------------------------------------------------

fn operational() -> V {
    map(vec![
        (
            "water_outages",
            map(vec![(
                "o1",
                map(vec![("outage_event_id", s("water-down")), ("restore_event_id", s("water-up")), ("target_kind", s("zone")), ("target_id", s("z1")), ("active", V::Bool(false))]),
            )]),
        ),
        (
            "staff_status",
            map(vec![(
                "s1",
                map(vec![("available", V::Bool(false)), ("unavailability_event_id", s("gone")), ("until_minute", V::Int(50)), ("restore_event_id", s("back"))]),
            )]),
        ),
        ("shipments", map(vec![("sh1", map(vec![("event_id", s("delivery-1")), ("resource_id", s("water_ml")), ("due_minute", V::Int(30))]))])),
        ("cancelled_staff_task_ids", V::List(Vec::new())),
    ])
}

#[test]
fn a_water_outage_is_checked_in_the_rule_s_order_and_marks_its_record() {
    let world = world(vec![(handlers::OPERATIONAL, operational())]);
    let held = Held::Held;
    let down = event("water-down", 1, 10, "water_outage", vec![("outage_id", s("o1")), ("zone_id", s("z1"))]);
    let wrong = event("water-down", 1, 10, "water_outage", vec![("outage_id", s("o1")), ("zone_id", s("z0"))]);
    assert_eq!(failed(handlers::water_outage(&world, &held, &wrong)), "water outage target does not match operational state");
    let fields = event("water-down", 1, 10, "water_outage", vec![("outage_id", s("o1"))]);
    assert_eq!(failed(handlers::water_outage(&world, &held, &fields)), "water outage payload fields or source are invalid");
    let other = event("other", 1, 10, "water_outage", vec![("outage_id", s("o1")), ("zone_id", s("z1"))]);
    assert_eq!(failed(handlers::water_outage(&world, &held, &other)), "water outage does not match operational state");
    handlers::water_outage(&world, &held, &down).unwrap();
    let record = state(&world, handlers::OPERATIONAL);
    let V::Map(outages) = &record else { panic!() };
    assert_eq!(outages["water_outages"], map(vec![(
        "o1",
        map(vec![("outage_event_id", s("water-down")), ("restore_event_id", s("water-up")), ("target_kind", s("zone")), ("target_id", s("z1")), ("active", V::Bool(true))]),
    )]));
    assert_eq!(failed(handlers::water_outage(&world, &held, &down)), "water outage is already active");
    let up = event("water-up", 2, 10, "water_restoration", vec![("outage_id", s("o1"))]);
    handlers::water_restoration(&world, &held, &up).unwrap();
    assert_eq!(failed(handlers::water_restoration(&world, &held, &up)), "water outage is not active");
    // Without the namespace.
    let bare = self::world(Vec::new());
    assert_eq!(failed(handlers::water_outage(&bare, &Held::Absent, &down)), "water outage does not match operational state");
}

#[test]
fn a_staff_restoration_reads_its_token() {
    let world = world(vec![(handlers::OPERATIONAL, operational())]);
    let held = Held::Held;
    let older = event("back", 3, 10, "staff_restored", vec![("staff_id", s("s1")), ("unavailability_event_id", s("older"))]);
    assert_eq!(failed(handlers::staff_restoration(&world, &held, &older)), "staff restoration belongs to an older unavailability");
    let token = event("not-back", 3, 10, "staff_restored", vec![("staff_id", s("s1")), ("unavailability_event_id", s("gone"))]);
    assert_eq!(failed(handlers::staff_restoration(&world, &held, &token)), "staff restoration event ID does not match its token");
    let unknown = event("back", 3, 10, "staff_restored", vec![("staff_id", s("s9")), ("unavailability_event_id", s("gone"))]);
    assert_eq!(failed(handlers::staff_restoration(&world, &held, &unknown)), "staff restoration references unknown staff");
    let back = event("back", 3, 10, "staff_restored", vec![("staff_id", s("s1")), ("unavailability_event_id", s("gone"))]);
    handlers::staff_restoration(&world, &held, &back).unwrap();
    assert_eq!(failed(handlers::staff_restoration(&world, &held, &back)), "staff is already available");
}

#[test]
fn a_delivery_delay_moves_the_pending_delivery_and_keeps_its_identity() {
    let world = world(vec![(handlers::OPERATIONAL, operational())]);
    let delivery = event("delivery-1", 7, 30, "resource_delivery", vec![("warehouse_id", s("w")), ("resource_id", s("water_ml")), ("quantity", V::Int(5)), ("shipment_id", s("sh1"))]);
    assert_eq!(world.queue_mut().unwrap().schedule_row(delivery), 0);
    let mut marker = entries(vec![("program_id", s("p")), ("guard", s("g")), ("phase", s("start"))]);
    marker.insert(text("effect_id"), s("e"));
    let mut provenance = IndexMap::new();
    provenance.insert(text("effect_program_v1"), V::Map(marker));
    let delay = EventRow::new(text("delay-1"), EventKind::Exogenous, 10, 0, 8, text("delivery_delayed"), entries(vec![("shipment_id", s("sh1")), ("to_minute", V::Int(60))]), Some(text("scenario")), None, provenance).unwrap();
    let early = EventRow::new(text("delay-1"), EventKind::Exogenous, 10, 0, 8, text("delivery_delayed"), entries(vec![("shipment_id", s("sh1")), ("to_minute", V::Int(20))]), Some(text("scenario")), None, IndexMap::new()).unwrap();
    let mut taken = Vec::new();
    assert_eq!(failed(handlers::delivery_delay(&world, &Held::Held, &early, &mut taken)), "shipment delay must move a not-yet-due delivery forward");
    handlers::delivery_delay(&world, &Held::Held, &delay, &mut taken).unwrap();
    // The slot has a new row, whose object the PyO3 crate makes again.
    assert_eq!(taken.len(), 1);
    let queue = world.queue().unwrap();
    let serial = queue.serial_of("delivery-1").unwrap();
    assert_eq!(taken[0], serial);
    let moved = queue.slot(serial).row.clone().unwrap();
    assert_eq!((moved.due_minute, moved.sequence, moved.causation_id.as_deref()), (60, 7, Some("delay-1")));
    // The marker is derived: no guard, phase `derived`, the order kept.
    let expected = map(vec![
        ("last_delay_event_id", s("delay-1")),
        ("effect_program_v1", map(vec![("program_id", s("p")), ("phase", s("derived")), ("effect_id", s("e"))])),
    ]);
    assert!(moved.provenance.same_in_order(&expected), "{:?}", moved.provenance);
    drop(queue);
    let V::Map(state) = state(&world, handlers::OPERATIONAL) else { panic!() };
    assert_eq!(state["shipments"], map(vec![("sh1", map(vec![("event_id", s("delivery-1")), ("resource_id", s("water_ml")), ("due_minute", V::Int(60))]))]));
    // Its delivery is due later now: a second delay must be later still.
    let again = EventRow::new(text("delay-2"), EventKind::Exogenous, 10, 0, 9, text("delivery_delayed"), entries(vec![("shipment_id", s("sh1")), ("to_minute", V::Int(50))]), Some(text("scenario")), None, IndexMap::new()).unwrap();
    assert_eq!(failed(handlers::delivery_delay(&world, &Held::Held, &again, &mut taken)), "shipment delay must move a not-yet-due delivery forward");
}

// ---- Facilities --------------------------------------------------------------------------------------

#[test]
fn a_facility_outage_takes_units_and_its_base_units() {
    let infrastructure = map(vec![("outages", V::Map(IndexMap::new())), ("base_working_units", map(vec![("toilet_1", V::Int(2))]))]);
    let world = world(vec![(handlers::INFRASTRUCTURE, infrastructure)]);
    let (held, absent) = (Held::Held, Held::Absent);
    let inputs = FacilityIn { program: &absent, infrastructure: &held, shared_absent: true };
    let missing = event("out", 1, 10, "facility_outage", vec![("facility_id", s("nope")), ("units_out", V::Int(1))]);
    assert_eq!(failed(handlers::facility_units(&world, &inputs, &missing, false)), "facility 'nope' does not exist");
    let zero = event("out", 1, 10, "facility_outage", vec![("facility_id", s("toilet_1")), ("units_out", V::Int(0))]);
    assert!(matches!(handlers::facility_units(&world, &inputs, &zero, false), Err(Error::Outcome(sheltersim_core::error::Outcome::Suppressed, ref text)) if text == "zero-unit facility outage has no effect"));
    let negative = event("out", 1, 10, "facility_outage", vec![("facility_id", s("toilet_1")), ("units_out", V::Big("-99999999999999999999".into()))]);
    assert_eq!(failed(handlers::facility_units(&world, &inputs, &negative, false)), "facility outage units_out cannot be negative");
    let too_many = event("out", 1, 10, "facility_outage", vec![("facility_id", s("toilet_1")), ("units_out", V::Int(3))]);
    assert_eq!(failed(handlers::facility_units(&world, &inputs, &too_many, false)), "facility outage exceeds currently working capacity");
    let out = event("out", 1, 10, "facility_outage", vec![("facility_id", s("toilet_1")), ("units_out", V::Int(2))]);
    assert!(handlers::facility_units(&world, &inputs, &out, false).unwrap().is_none());
    assert_eq!(world.rows().unwrap().facilities.get("toilet_1").unwrap().working_units, 0);
    let V::Map(state) = state(&world, handlers::INFRASTRUCTURE) else { panic!() };
    assert_eq!(state["base_working_units"], map(vec![("toilet_1", V::Int(0))]));
    let back = event("back", 2, 10, "facility_recovery", vec![("facility_id", s("toilet_1")), ("units_restored", V::Int(3))]);
    assert_eq!(failed(handlers::facility_units(&world, &inputs, &back, true)), "facility recovery exceeds installed capacity");
}

// ---- The end of an operational transaction -------------------------------------------------------------

#[test]
fn settling_with_records_takes_the_event_out_and_logs_it_and_its_records() {
    let world = Shard::new(world(Vec::new()));
    let pending = event("cause", 3, 10, "staff_unavailable", vec![("staff_id", s("s1")), ("until_minute", V::Int(20))]);
    world.with(|world| assert_eq!(world.queue_mut().unwrap().schedule_row(pending.clone()), 0)).unwrap();
    let mut host = Writing::default();
    let mut taken = Vec::new();
    // Without records: the world is validated with the event in flight, and nothing changes.
    handlers::settle(&world, &mut host, &Held::Absent, &pending, &[], &mut taken).unwrap();
    assert_eq!(host.in_flight, vec![text("cause")]);
    assert!(taken.is_empty());
    let follow = event("follow-41", 41, 10, "facility_recovery_transition", vec![("record_type", s("operations"))]);
    let made = Made { event: follow, minute: 10, detail: "receipt recorded".to_owned() };
    handlers::settle(&world, &mut host, &Held::Absent, &pending, &[made], &mut taken).unwrap();
    assert_eq!(taken.len(), 1);
    assert_eq!(host.host.calls, vec![Call::ValidateWorld]);
    world
        .with(|world| {
            assert!(!world.queue().unwrap().has_event("cause"));
            let logged = records(world);
            assert_eq!(logged.len(), 2);
            assert_eq!((logged[0]["event"]["event_id"].as_str(), logged[0]["status"].as_str(), &logged[0]["detail"]), (Some("cause"), Some("applied"), &Value::Null));
            assert_eq!((logged[1]["event"]["event_id"].as_str(), logged[1]["detail"].as_str()), (Some("follow-41"), Some("receipt recorded")));
        })
        .unwrap();
}

// ---- Infrastructure ------------------------------------------------------------------------------

fn infrastructure_state() -> V {
    map(vec![
        ("outages", V::Map(IndexMap::new())),
        ("base_working_units", map(vec![("water_1", V::Int(1)), ("toilet_1", V::Int(2))])),
        (
            "environmental_stressors",
            map(vec![("heat", map(vec![("start_event_id", s("heat-on")), ("zone_id", s("z1")), ("active", V::Bool(false))]))]),
        ),
        ("route_interruptions", V::Map(IndexMap::new())),
    ])
}

#[test]
fn a_stressor_reads_the_spec_through_the_host_and_schedules_its_end() {
    let world = Shard::new(world(vec![(handlers::INFRASTRUCTURE, infrastructure_state())]));
    let (held, absent) = (Held::Held, Held::Absent);
    let inputs = InfraIn { infrastructure: &held, operational: &absent, program: &absent };
    let (kind, label) = Infra::of("extreme_heat").unwrap();
    assert_eq!(label, "infrastructure.start_stressor");
    assert_eq!(failed(Infra::of("meteor").map(drop)), "unsupported infrastructure event type 'meteor'");
    let heat = event("heat-on", 1, 10, "extreme_heat", vec![("stressor_id", s("heat")), ("zone_id", s("z1")), ("until_minute", V::Int(30))]);
    let mut host = Writing::default();
    let mut taken = Vec::new();
    let followed = handlers::infrastructure(&world, &mut host, &inputs, &heat, kind, &mut taken).unwrap();
    assert!(followed.is_empty());
    // The spec's contract, once, at the end's scheduling.
    assert_eq!(host.host.calls, vec![Call::Contract(Contract::Infrastructure, map(vec![("source_id", s("ASM-INFRA"))]))]);
    world
        .with(|world| {
            let queue = world.queue().unwrap();
            let end = queue.slot(queue.serial_of("environmental-stressor-end-40").unwrap()).row.clone().unwrap();
            assert_eq!((end.due_minute, end.priority, end.sequence, end.causation_id.as_deref()), (30, 10, 40, Some("heat-on")));
            assert_eq!(end.payload, map(vec![("stressor_id", s("heat")), ("zone_id", s("z1"))]));
            drop(queue);
            let V::Map(state) = state(world, handlers::INFRASTRUCTURE) else { panic!() };
            assert_eq!(state["environmental_stressors"], map(vec![(
                "heat",
                map(vec![("start_event_id", s("heat-on")), ("zone_id", s("z1")), ("active", V::Bool(true)), ("end_event_id", s("environmental-stressor-end-40")), ("until_minute", V::Int(30))]),
            )]));
            assert_eq!(world.id_counter("event").unwrap(), Some(41));
        })
        .unwrap();
    // Again: the record is active.
    let mut host = Writing::default();
    let again = handlers::infrastructure(&world, &mut host, &inputs, &heat, kind, &mut taken);
    assert!(matches!(again, Err(Fault::Core(Error::Outcome(_, ref text))) if text == "environmental stressor payload or duration is invalid"));
    assert!(host.host.calls.is_empty(), "the checks come before the spec");
}

#[test]
fn an_outage_sets_the_dependent_facilities_to_zero() {
    let mut initial = infrastructure_state();
    let V::Map(fields) = &mut initial else { unreachable!() };
    fields.insert(
        text("outages"),
        map(vec![("p1", map(vec![("start_event_id", s("power-down")), ("event_type", s("power_outage")), ("affected_facility_ids", V::List(vec![s("water_1")])), ("active", V::Bool(false))]))]),
    );
    let world = Shard::new(world(vec![(handlers::INFRASTRUCTURE, initial)]));
    let (held, absent) = (Held::Held, Held::Absent);
    let inputs = InfraIn { infrastructure: &held, operational: &absent, program: &absent };
    let down = event("power-down", 1, 10, "power_outage", vec![("outage_id", s("p1")), ("component_id", s("water_1")), ("until_minute", V::Int(100))]);
    let previous = world.with(|world| handlers::working_units(world).unwrap()).unwrap();
    let mut host = Writing::default();
    let mut taken = Vec::new();
    handlers::infrastructure(&world, &mut host, &inputs, &down, Infra::StartOutage, &mut taken).unwrap();
    world
        .with(|world| {
            assert_eq!(world.rows().unwrap().facilities.get("water_1").unwrap().working_units, 0);
            // At the Run's end: no restoration is scheduled.
            assert!(world.queue().unwrap().is_empty());
            let V::Map(state) = state(world, handlers::INFRASTRUCTURE) else { panic!() };
            let V::Map(outages) = &state["outages"] else { panic!() };
            let V::Map(record) = &outages["p1"] else { panic!() };
            assert_eq!((&record["active"], &record["until_minute"], &record["units_out"], &record["restore_event_id"]), (&V::Bool(true), &V::Int(100), &V::Null, &V::Null));
            // Without the shared contract there is no receipt.
            assert!(handlers::infrastructure_receipts(world, true, &previous, &down).unwrap().is_empty());
        })
        .unwrap();
}

// ---- Population primitives ----------------------------------------------------------------------------

#[test]
fn a_population_primitive_must_be_in_its_program() {
    let world = world(Vec::new());
    let add = event("pop-1", 1, 10, "population_add", vec![("program_id", s("p")), ("effect_id", s("e")), ("op", s("add_evacuees")), ("zone_id", s("z0")), ("count", V::Int(2)), ("household_size", V::Int(1)), ("mobility", s("independent")), ("language", s("ja"))]);
    assert_eq!(failed(handlers::population_checks(&world, &Held::Absent, &add)), "population primitive is not in the approved program");
    let odd = event("pop-1", 1, 10, "population_add", vec![("op", s("teleport"))]);
    assert_eq!(failed(handlers::population_checks(&world, &Held::Absent, &odd)), "population primitive operation is invalid");
    let short = event("pop-1", 1, 10, "population_add", vec![("program_id", s("p")), ("effect_id", s("e")), ("op", s("depart_evacuees")), ("zone_id", s("z0"))]);
    assert_eq!(failed(handlers::population_checks(&world, &Held::Absent, &short)), "population primitive payload fields do not match");
    assert_eq!(handlers::service_cancellation(&world), Ok(()));
    let _ = PopulationOp::Add;
}

// ---- Proximity ---------------------------------------------------------------------------------------

#[test]
fn a_proximity_contact_without_its_config_or_positions_is_refused_by_its_texts() {
    let world = world(Vec::new());
    let contact = EventRow::new(
        text("contact-1"),
        EventKind::Endogenous,
        10,
        0,
        1,
        text("proximity_contact_update"),
        IndexMap::new(),
        Some(text("ASM-PROXIMITY-CONTACT-V1")),
        None,
        IndexMap::new(),
    )
    .unwrap();
    assert_eq!(failed(handlers::proximity(&world, None, &Held::Absent, &contact)), "proximity contact provenance or time is invalid");
    let config = sheltersim_core::config::proximity::Proximity::from_v(&map(vec![("policy_version", s("v1")), ("max_distance_m", V::Float(2.0))])).unwrap().unwrap();
    assert_eq!(failed(handlers::proximity(&world, Some(&config), &Held::Absent, &contact)), "proximity contact provenance or time is invalid");
}

// ---- Staff unavailable ---------------------------------------------------------------------------------

#[test]
fn an_idle_member_s_unavailability_schedules_the_restoration_and_marks_the_status() {
    use sheltersim_core::entities::{Skills, StaffRow};
    use sheltersim_core::rules::handlers::{StaffIn, Unread};
    use sheltersim_core::rules::staff_operation_events::Inputs;

    let mut world = world(vec![(handlers::OPERATIONAL, operational())]);
    world.rows_mut().unwrap().staff.insert(
        text("s1"),
        StaffRow { id: text("s1"), zone_id: text("z0"), skills: Skills(Vec::new()), work_queue: Vec::new(), busy_until: 0, active_task_id: None },
    );
    let absent = Held::Absent;
    let ops = Inputs { staff: &absent, movement: &absent, body: &absent, program: &absent, shared_absent: true, geo: None, formula: None };
    let held = Held::Held;
    let inputs = StaffIn { operational: &held, ops: &ops, service: Err(Unread::Raised), cooldown: None, metric_state_held: true };
    let mut provenance = IndexMap::new();
    provenance.insert(text("effect_program_v1"), map(vec![("program_id", s("p")), ("guard", s("g"))]));
    let gone = EventRow::new(text("gone-2"), EventKind::Exogenous, 10, 0, 5, text("staff_unavailable"), entries(vec![("staff_id", s("s1")), ("until_minute", V::Int(40))]), Some(text("scenario")), None, provenance).unwrap();
    let mut taken = Vec::new();
    let done = handlers::staff_unavailable(&mut world, &inputs, &gone, &mut taken).ok().unwrap();
    assert!(done.records.is_empty() && done.applied.is_none() && taken.is_empty());
    let queue = world.queue().unwrap();
    let restore = queue.slot(queue.serial_of("staff-restored-40").unwrap()).row.clone().unwrap();
    assert_eq!((restore.due_minute, restore.priority, restore.sequence, restore.causation_id.as_deref()), (40, 10, 40, Some("gone-2")));
    assert_eq!(restore.payload, map(vec![("staff_id", s("s1")), ("unavailability_event_id", s("gone-2"))]));
    assert_eq!(
        restore.provenance,
        map(vec![
            ("source_type", s("assumption")),
            ("source_id", s("ASM-H03-OPERATIONAL-EVENTS-V1")),
            ("policy_version", s("operational-events-v1")),
            ("effect_program_v1", map(vec![("program_id", s("p")), ("phase", s("derived"))])),
        ])
    );
    drop(queue);
    assert_eq!(world.id_counter("event").unwrap(), Some(41));
    let V::Map(state) = state(&world, handlers::OPERATIONAL) else { panic!() };
    assert_eq!(state["staff_status"], map(vec![(
        "s1",
        map(vec![("available", V::Bool(false)), ("unavailability_event_id", s("gone-2")), ("until_minute", V::Int(40)), ("restore_event_id", s("staff-restored-40"))]),
    )]));
    // The same absence again is suppressed; an unknown member fails.
    let again = handlers::staff_unavailable(&mut world, &inputs, &gone, &mut taken);
    assert!(matches!(again, Err(sheltersim_core::rules::service::Failure::Write(WriteError::Error(Error::Outcome(_, ref text)))) if text == "staff unavailability was already applied"));
}
