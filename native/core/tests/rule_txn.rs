//! Lane P9-1b: the rule transactions in the core (`txn::rule`, policy E16) and the handlers that
//! open one (`effects::*_event`, `handlers::*_event`), on worlds built here with no Python.
//!
//! A recording host writes down each step of the transaction and of the world's checks, in order,
//! and can make the check of the changed world fail with an outcome. Each handler that opens a
//! transaction is run to that failure: the world (its rows, slots, queue and EventLog, which holds
//! the log's mark and the sequence counter) is then what it was before, and the steps are the
//! transaction's: the host's check, the two validations of its start around the canonical order,
//! the rule's own checks, and the rollback.

mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::cell::Shard;
use sheltersim_core::config::{Configs, Filled, SectionId};
use sheltersim_core::entities::{Activity, AgentRow, Entities, FacilityRow, Mobility, NeedRow, ProfileRow, Skills, StaffRow, ZoneRow};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::writer::WriteError;
use sheltersim_core::events::{EventKind, EventRow};
use sheltersim_core::host::{Contract, Decision, DecisionSink, ErrorClass, Holding, Host, HostFailure, Llm, Phase, Stage, Trace, TraceLevel, TxnKind};
use sheltersim_core::json::Encoder;
use sheltersim_core::row::{IdMap, ValueRow};
use sheltersim_core::rules::effects;
use sheltersim_core::rules::handlers::{self, Infra};
use sheltersim_core::rules::metrics::MetricSample;
use sheltersim_core::state::effect_program::{EffectProgramRow, Program, NAME as PROGRAM};
use sheltersim_core::txn::{self, TxnEnd};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, Snapshot, World, WorldCell, ID_COUNTERS, MODEL_CONFIGS};
use support::TestHost;

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

// ---- A host that writes down the steps and can fail the changed world's check --------------------

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

    fn text(&self) -> String {
        match self {
            Fault::Core(error) => error.text(),
            Fault::Duplicate(text) => text.clone(),
        }
    }

    fn is_outcome(&self) -> bool {
        matches!(self, Fault::Core(error) if error.is_outcome())
    }
}

/// `TestHost`, which writes down the steps of a transaction and of the world's checks in `steps`,
/// fails the check of the changed world (`validate_world`, `validate_in_flight`) with `fail`, and
/// refuses the transaction's check with `refuse`.
#[derive(Default)]
struct Recording {
    host: TestHost,
    steps: Vec<String>,
    fail: Option<Error>,
    refuse: Option<&'static str>,
}

impl Recording {
    fn failing() -> Self {
        Self { fail: Some(Error::failed("the changed world fails its check")), ..Self::default() }
    }

    fn step(&mut self, name: impl Into<String>) {
        self.steps.push(name.into());
    }

    fn checked(&mut self, name: &str) -> Result<(), Fault> {
        self.step(name);
        self.fail.clone().map_or(Ok(()), |error| Err(Fault::Core(error)))
    }
}

macro_rules! delegate {
    ($($name:ident($($arg:ident: $kind:ty),*) -> $out:ty;)*) => {
        $(fn $name(&mut self, $($arg: $kind),*) -> Result<$out, Fault> {
            Ok(self.host.$name($($arg),*)?)
        })*
    };
}

impl Host for Recording {
    type Error = Fault;

    delegate! {
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
        holding() -> Option<Arc<Holding>>;
    }

    fn txn_check(&mut self) -> Result<(), Fault> {
        self.step("check");
        self.refuse.map_or(Ok(()), |reason| Err(Fault::Core(Error::unsupported(reason))))
    }

    fn validate_candidate(&mut self, kind: TxnKind, stage: Stage) -> Result<(), Fault> {
        self.step(format!("validate {kind:?} {stage:?}"));
        Ok(())
    }

    fn canonicalized(&mut self, _kind: TxnKind, _before: &Snapshot) -> Result<(), Fault> {
        self.step("canonicalized");
        Ok(())
    }

    fn restored(&mut self, _kind: TxnKind) -> Result<(), Fault> {
        self.step("restored");
        Ok(())
    }

    fn committed(&mut self, _kind: TxnKind) -> Result<(), Fault> {
        self.step("committed");
        Ok(())
    }

    fn validate_world(&mut self) -> Result<(), Fault> {
        self.checked("validate_world")
    }

    fn validate_in_flight(&mut self, _event: &EventRow) -> Result<(), Fault> {
        self.checked("validate_in_flight")
    }

    fn fill_section(&mut self, configs: &Arc<Configs>, which: SectionId) -> Result<Filled, Fault> {
        Ok(self.host.fill_section(configs, which)?)
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
}

/// The steps of a rolled-back transaction of `kind`, the check of the changed world among them.
fn rolled_back(kind: &str, check: &str) -> Vec<String> {
    vec!["check".into(), format!("validate {kind} World"), "canonicalized".into(), format!("validate {kind} Candidate"), check.into(), "restored".into()]
}

// ---- What a world is, to compare it before and after ---------------------------------------------

fn entity_json<T: sheltersim_core::entities::EntityRow>(row: &T) -> Vec<u8> {
    let mut encoder = Encoder { out: Vec::new() };
    row.write_json(&mut encoder, 6).unwrap();
    encoder.out
}

/// Everything a rule writes: the entities in their order, every slot's row and stamp, the queue,
/// and the EventLog (its records, its mark and the next sequence).
#[derive(Debug, PartialEq)]
struct Fingerprint {
    entities: Vec<(String, Vec<u8>)>,
    slots: Vec<(String, Option<Vec<u8>>, u64)>,
    queue: String,
    log: EventLog,
    in_transaction: bool,
}

fn fingerprint(world: &World) -> Fingerprint {
    let rows = world.rows().unwrap();
    let mut entities = Vec::new();
    for (id, row) in rows.agents.iter() {
        entities.push((format!("agent {id}"), entity_json(row)));
    }
    for (id, row) in rows.facilities.iter() {
        entities.push((format!("facility {id}"), entity_json(row)));
    }
    for (id, row) in rows.staff.iter() {
        entities.push((format!("staff {id}"), entity_json(row)));
    }
    drop(rows);
    let names: Vec<Arc<str>> = world.slots.names().cloned().collect();
    let slots = names
        .iter()
        .map(|name| {
            let slot = world.slots.get(name).unwrap().try_borrow().unwrap();
            let bytes = slot.row.as_deref().map(|row| {
                let mut encoder = Encoder { out: Vec::new() };
                row.write_json(&mut encoder).unwrap();
                encoder.out
            });
            (name.to_string(), bytes, slot.generation.get())
        })
        .collect();
    Fingerprint {
        entities,
        slots,
        queue: format!("{:?}", *world.queue().unwrap()),
        log: world.log().unwrap().clone(),
        in_transaction: world.in_transaction(),
    }
}

fn print(cell: &Shard<World>) -> Fingerprint {
    cell.with(|world| fingerprint(world)).unwrap()
}

fn outcome<T>(result: Result<T, Fault>) -> String {
    match result {
        Err(Fault::Core(Error::Outcome(_, text))) => text,
        Err(other) => panic!("not an outcome: {other:?}"),
        Ok(_) => panic!("not an outcome: it passed"),
    }
}

// ---- Worlds ------------------------------------------------------------------------------------------

fn add_value(world: &mut World, name: &str, place: Place, value: V) {
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

fn needs() -> NeedRow {
    NeedRow { toilet_urgency: 0.0, hunger: 0.0, fatigue: 0.0, hydration_need: 0.0 }
}

fn agent(id: &str) -> AgentRow {
    AgentRow {
        profile: ProfileRow {
            id: text(id),
            group_id: None,
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
        stress: 0.0,
        trust: 0.5,
        next_decision_at: 0,
        support_request_id: None,
        support_cooldown_until: 0,
    }
}

/// `sha256(f"{program}:{effect}")`'s modifier ID, as the program's scheduler wrote it.
const MODIFIER: &str = "modifier-62ee1868ced5ead06966d06e64007cd0";

/// The approved program `p1` of one effect `e1` of `op`, scheduled as `ev1`.
fn program(op: &str) -> EffectProgramRow {
    let mut operators: IdMap<Arc<str>> = IdMap::default();
    operators.insert(text("e1"), text(op));
    let mut programs: IdMap<Program> = IdMap::default();
    programs.insert(
        text("p1"),
        Program {
            program_hash: text(&"a".repeat(64)),
            source_text_hash: text(&"b".repeat(64)),
            narrative: text("a narrative"),
            compiler_provenance: map(vec![("compiler_kind", s("offline_rule")), ("compiler_version", s("v1"))]),
            assumptions: V::List(Vec::new()),
            support_mode: text("native"),
            effect_ids: vec![text("e1")],
            operators,
            scheduled_event_ids: vec![text("ev1")],
        },
    );
    EffectProgramRow { schema_version: 1, programs, active_facility_modifiers: IdMap::default() }
}

/// A world of 100 minutes at minute 10 (schema 6): zones z0 and z1, a toilet of 3 units with a
/// person in it and a water station in z0, two people, staff s1 in z0; its queue, log (one record),
/// `id_counters` (event 40), `model_configs`, the program of `op` (none for None) and the namespaces
/// given as values.
fn world(op: Option<&str>, namespaces: Vec<(&str, V)>) -> World {
    let mut world = World::default();
    let scalars = world.scalars.get_mut();
    scalars.duration_minutes = Fit::Ok(100);
    scalars.minute_index = Fit::Ok(10);
    scalars.schema_version = Fit::Ok(6);
    let mut entities = Entities::default();
    for id in ["z0", "z1"] {
        entities.zones.insert(text(id), ZoneRow { id: text(id), name: text(id), capacity: None, display_x: None, display_y: None });
    }
    let mut toilet = facility("toilet_1", "toilet", "z0", 3, 3);
    toilet.active_agent_ids = vec![text("a1")];
    entities.facilities.insert(text("toilet_1"), toilet);
    entities.facilities.insert(text("water_1"), facility("water_1", "water_station", "z1", 1, 1));
    for id in ["a1", "a2"] {
        entities.agents.insert(text(id), agent(id));
    }
    entities.staff.insert(
        text("s1"),
        StaffRow { id: text("s1"), zone_id: text("z0"), skills: Skills(vec![text("support")]), work_queue: Vec::new(), busy_until: 0, active_task_id: None },
    );
    *world.entities.get_mut() = entities;
    add_value(&mut world, ID_COUNTERS, Place::Field, map(vec![("event", V::Int(40))]));
    add_value(&mut world, MODEL_CONFIGS, Place::Field, map(vec![("infrastructure", map(vec![("source_id", s("ASM-INFRA"))]))]));
    if let Some(op) = op {
        let index = world.slots.add(text(PROGRAM), Place::Value).unwrap();
        world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(program(op)));
    }
    for (name, value) in namespaces {
        add_value(&mut world, name, Place::Value, value);
    }
    world.hold_queue(Arc::new(Queue::new()));
    world.hold_log(Arc::new(EventLog::new()));
    let earlier = EventRow::new(text("earlier"), EventKind::Exogenous, 5, 0, 3, text("note"), IndexMap::new(), Some(text("scenario")), None, IndexMap::new()).unwrap();
    sheltersim_core::rules::kit::log_record(&world, &earlier, 5, sheltersim_core::events::EventStatus::Applied, None).unwrap();
    world
}

/// The marker of a start of `op`.
fn marker(op: &str, dependencies: &[&str]) -> V {
    map(vec![
        ("program_id", s("p1")),
        ("effect_id", s("e1")),
        ("op", s(op)),
        ("source_text_hash", s(&"b".repeat(64))),
        ("compiler_provenance", map(vec![("compiler_kind", s("offline_rule")), ("compiler_version", s("v1"))])),
        ("assumption_ids", V::List(vec![s("ASM-A")])),
        ("policy_dependencies", V::List(dependencies.iter().map(|item| s(item)).collect())),
        ("units", s("count")),
        ("phase", s("start")),
    ])
}

fn effect_event(event_id: &str, event_type: &str, payload: Vec<(&str, V)>, marker: V) -> EventRow {
    EventRow::new(
        text(event_id),
        EventKind::Intervention,
        10,
        0,
        41,
        text(event_type),
        entries(payload),
        Some(text("SRC-1")),
        None,
        entries(vec![("effect_program_v1", marker)]),
    )
    .unwrap()
}

fn exogenous(event_id: &str, sequence: i64, event_type: &str, payload: Vec<(&str, V)>) -> EventRow {
    EventRow::new(text(event_id), EventKind::Exogenous, 10, 0, sequence, text(event_type), entries(payload), Some(text("scenario")), None, IndexMap::new()).unwrap()
}

/// Run `handler` on `world` with a host that fails the changed world's check: the outcome's text,
/// with the world as it was and the steps given.
fn rolls_back<T>(world: World, steps: Vec<String>, handler: impl FnOnce(&Shard<World>, &mut Recording) -> Result<T, Fault>) -> String {
    let cell = Shard::new(world);
    let before = print(&cell);
    let mut host = Recording::failing();
    let text = outcome(handler(&cell, &mut host));
    assert_eq!(print(&cell), before);
    assert_eq!(host.steps, steps);
    text
}

const FAILS: &str = "the changed world fails its check";

// ---- The rule transaction --------------------------------------------------------------------------

#[test]
fn a_rule_transaction_checks_the_host_then_validates_twice_and_counts() {
    let cell = Shard::new(world(None, Vec::new()));
    let counts = |name: &str| txn::counts().iter().find(|(key, _)| *key == name).unwrap().1;
    let in_place = counts("in_place");
    let mut host = Recording::default();
    let kept = txn::rule(&cell, &mut host, TxnKind::Effect, |cell, host| {
        assert_eq!(host.steps, ["check", "validate Effect World", "canonicalized", "validate Effect Candidate"]);
        cell.with(|world| world.rows_mut().unwrap().agents.get_mut("a1").unwrap().stress = 0.7)?;
        Ok(TxnEnd::Commit(3))
    });
    assert_eq!(kept, Ok(3));
    assert_eq!(host.steps.last().map(String::as_str), Some("committed"));
    assert!(counts("in_place") > in_place);
    assert_eq!(cell.with(|world| world.rows().unwrap().agents.get("a1").unwrap().stress).unwrap(), 0.7);

    // A refused check: nothing is validated, nothing changes.
    let before = print(&cell);
    let mut refusing = Recording { refuse: Some("the entity store is tainted"), ..Recording::default() };
    let refused: Result<(), Fault> = txn::rule(&cell, &mut refusing, TxnKind::Full, |_, _| panic!("the body ran"));
    assert_eq!(refused, Err(Fault::Core(Error::unsupported("the entity store is tainted"))));
    assert_eq!(refusing.steps, ["check"]);
    assert_eq!(print(&cell), before);

    // A body that asks for a rollback gives its value back, with the world as it was.
    let mut host = Recording::default();
    let back = txn::rule(&cell, &mut host, TxnKind::Full, |cell, _| {
        cell.with(|world| world.scalars.get_mut().minute_index = Fit::Ok(77))?;
        Ok(TxnEnd::Rollback("deferred"))
    });
    assert_eq!(back, Ok("deferred"));
    assert_eq!(print(&cell), before);
}

#[test]
fn a_rule_inside_an_open_transaction_validates_as_a_start_and_leaves_its_end_to_the_outer_one() {
    let cell = Shard::new(world(None, Vec::new()));
    let before = print(&cell);
    let nested = || txn::counts().iter().find(|(key, _)| *key == "nested").unwrap().1;
    let mut host = Recording::default();
    let outer: Result<(), Fault> = txn::rule(&cell, &mut host, TxnKind::Full, |cell, host| {
        host.steps.clear();
        let count = nested();
        // The inner rule asks for a rollback: what it wrote stays for the outer transaction.
        let inner = txn::rule(cell, host, TxnKind::Full, |cell, _| {
            cell.with(|world| world.scalars.get_mut().minute_index = Fit::Ok(55))?;
            Ok(TxnEnd::Rollback(()))
        });
        assert_eq!(inner, Ok(()));
        assert_eq!(host.steps, ["validate Full World", "validate Full Candidate"]);
        assert!(nested() > count);
        assert_eq!(cell.with(|world| world.minute_index()).unwrap(), Ok(55));
        Err(Fault::Core(Error::failed("the outer rule fails")))
    });
    assert_eq!(outcome(outer), "the outer rule fails");
    assert_eq!(host.steps.last().map(String::as_str), Some("restored"));
    assert_eq!(print(&cell), before);
}

// ---- The effect handlers ------------------------------------------------------------------------------

#[test]
fn a_facility_modifier_rolls_back_on_the_world_s_check() {
    let start = effect_event(
        "ev1",
        "effect_program_apply",
        vec![("program_id", s("p1")), ("effect_id", s("e1")), ("modifier_id", s(MODIFIER)), ("facility_id", s("toilet_1")), ("working_units", V::Int(2))],
        V::Null,
    );
    let steps = rolled_back("Effect", "validate_world");
    let text = rolls_back(world(Some("set_facility_availability"), Vec::new()), steps, |cell, host| effects::facility_event(cell, host, &start, false));
    assert_eq!(text, FAILS);
    // With a check that passes, the modifier is written and kept.
    let cell = Shard::new(world(Some("set_facility_availability"), Vec::new()));
    effects::facility_event(&cell, &mut Recording::default(), &start, false).unwrap();
    assert_eq!(cell.with(|world| world.rows().unwrap().facilities.get("toilet_1").unwrap().working_units).unwrap(), 2);
}

#[test]
fn an_environment_modifier_rolls_back_on_the_world_s_check() {
    let start = effect_event(
        "ev1",
        "effect_program_environment",
        vec![
            ("program_id", s("p1")),
            ("effect_id", s("e1")),
            ("modifier_id", s(MODIFIER)),
            ("zone_id", s("z1")),
            ("op", s("set_ambient_noise")),
            ("parameters", map(vec![("noise_db", V::Float(55.5))])),
            ("end_minute", V::Int(60)),
        ],
        marker("set_ambient_noise", &["zone_environment_v1"]),
    );
    let built = world(Some("set_ambient_noise"), vec![("zone_environment_v1", map(vec![("modifiers", map(Vec::new()))]))]);
    let expiry = EventRow::new(text("ev-end"), EventKind::Intervention, 60, 0, 50, text("effect_program_environment_expire"), entries(vec![("modifier_id", s(MODIFIER))]), Some(text("SRC-1")), None, IndexMap::new()).unwrap();
    built.queue_mut().unwrap().add(text("ev-end"), (60, 0, 50), Some(Arc::new(expiry)));
    // The environment wraps the check's value error; an outcome goes on as it is.
    let text = rolls_back(built, rolled_back("Effect", "validate_world"), |cell, host| effects::environment_event(cell, host, &start, false));
    assert_eq!(text, FAILS);
}

#[test]
fn a_temporary_space_rolls_back_on_the_world_s_check() {
    let spaces = map(vec![("spaces", map(Vec::new())), ("history", V::List(Vec::new()))]);
    let start = effect_event(
        "ev1",
        "effect_program_temporary_space",
        vec![
            ("program_id", s("p1")),
            ("effect_id", s("e1")),
            ("modifier_id", s(MODIFIER)),
            ("zone_id", s("z1")),
            ("op", s("add_temporary_space")),
            ("parameters", map(vec![("people_capacity", V::Int(4)), ("sleeping_capacity", V::Int(2))])),
            ("end_minute", V::Int(50)),
        ],
        marker("add_temporary_space", &["temporary_spaces_v1"]),
    );
    let built = world(Some("add_temporary_space"), vec![("temporary_spaces_v1", spaces)]);
    let text = rolls_back(built, rolled_back("Effect", "validate_world"), |cell, host| effects::temporary_space_event(cell, host, &start, false));
    assert_eq!(text, FAILS);
}

fn structure_event(op: &str, extra: Vec<(&str, V)>) -> EventRow {
    let mut payload = vec![("program_id", s("p1")), ("effect_id", s("e1")), ("modifier_id", s(MODIFIER)), ("op", s(op))];
    payload.extend(extra);
    effect_event("ev1", "effect_program_structure", payload, marker(op, &[]))
}

#[test]
fn a_role_change_rolls_back_on_the_world_s_check() {
    let role = structure_event("set_staff_role", vec![("staff_id", s("s1")), ("skills", V::List(vec![s("zeta"), s("alpha")]))]);
    let text = rolls_back(world(Some("set_staff_role"), Vec::new()), rolled_back("Effect", "validate_world"), |cell, host| effects::role_event(cell, host, &role));
    assert_eq!(text, FAILS);
}

#[test]
fn added_staff_roll_back_on_the_world_s_check() {
    let added = structure_event("add_staff", vec![("zone_id", s("z1")), ("count", V::Int(2))]);
    let operational = map(vec![("staff_status", map(Vec::new()))]);
    let built = world(Some("add_staff"), vec![("operational_events_v1", operational)]);
    let text = rolls_back(built, rolled_back("Effect", "validate_world"), |cell, host| effects::add_event(cell, host, &added));
    assert_eq!(text, FAILS);
    let operational = map(vec![("staff_status", map(Vec::new()))]);
    let cell = Shard::new(world(Some("add_staff"), vec![("operational_events_v1", operational)]));
    assert_eq!(effects::add_event(&cell, &mut Recording::default(), &added), Ok("structure.add_staff"));
    assert_eq!(cell.with(|world| world.rows().unwrap().staff.len()).unwrap(), 3);
}

// ---- The operational handlers ---------------------------------------------------------------------

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

/// An operational event without records is checked with the event in flight, then rolled back.
fn operational_rolled_back() -> Vec<String> {
    rolled_back("Full", "validate_in_flight")
}

#[test]
fn a_water_outage_and_its_restoration_roll_back_on_the_world_s_check() {
    let down = exogenous("water-down", 1, "water_outage", vec![("outage_id", s("o1")), ("zone_id", s("z1"))]);
    let built = world(None, vec![(handlers::OPERATIONAL, operational())]);
    assert_eq!(rolls_back(built, operational_rolled_back(), |cell, host| handlers::water_outage_event(cell, host, &down)), FAILS);
    // Kept with a check that passes; the restoration then rolls back on its own check.
    let cell = Shard::new(world(None, vec![(handlers::OPERATIONAL, operational())]));
    handlers::water_outage_event(&cell, &mut Recording::default(), &down).unwrap();
    let up = exogenous("water-up", 2, "water_restoration", vec![("outage_id", s("o1"))]);
    let before = print(&cell);
    let mut host = Recording::failing();
    assert_eq!(outcome(handlers::water_restoration_event(&cell, &mut host, &up)), FAILS);
    assert_eq!(host.steps, operational_rolled_back());
    assert_eq!(print(&cell), before);
}

#[test]
fn a_staff_restoration_rolls_back_on_the_world_s_check() {
    let back = exogenous("back", 3, "staff_restored", vec![("staff_id", s("s1")), ("unavailability_event_id", s("gone"))]);
    let built = world(None, vec![(handlers::OPERATIONAL, operational())]);
    assert_eq!(rolls_back(built, operational_rolled_back(), |cell, host| handlers::staff_restoration_event(cell, host, &back)), FAILS);
}

#[test]
fn a_delivery_delay_rolls_back_on_the_world_s_check() {
    let built = world(None, vec![(handlers::OPERATIONAL, operational())]);
    let delivery = exogenous("delivery-1", 7, "resource_delivery", vec![("warehouse_id", s("w")), ("resource_id", s("water_ml")), ("quantity", V::Int(5)), ("shipment_id", s("sh1"))]);
    let delivery = EventRow { due_minute: 30, ..delivery };
    built.queue_mut().unwrap().schedule_row(delivery);
    let delay = exogenous("delay-1", 8, "delivery_delayed", vec![("shipment_id", s("sh1")), ("to_minute", V::Int(60))]);
    assert_eq!(rolls_back(built, operational_rolled_back(), |cell, host| handlers::delivery_delay_event(cell, host, &delay)), FAILS);
}

#[test]
fn a_staff_unavailability_rolls_back_on_the_world_s_check() {
    let built = world(None, vec![(handlers::OPERATIONAL, operational())]);
    let gone = exogenous("gone-2", 5, "staff_unavailable", vec![("staff_id", s("s1")), ("until_minute", V::Int(40))]);
    assert_eq!(rolls_back(built, operational_rolled_back(), |cell, host| handlers::staff_unavailable_event(cell, host, &gone)), FAILS);
}

#[test]
fn an_infrastructure_event_rolls_back_on_the_world_s_check() {
    let infrastructure = map(vec![
        ("outages", map(vec![("p1", map(vec![("start_event_id", s("power-down")), ("event_type", s("power_outage")), ("affected_facility_ids", V::List(vec![s("water_1")])), ("active", V::Bool(false))]))])),
        ("base_working_units", map(vec![("water_1", V::Int(1)), ("toilet_1", V::Int(3))])),
        ("environmental_stressors", map(Vec::new())),
        ("route_interruptions", map(Vec::new())),
    ]);
    let built = world(None, vec![(handlers::INFRASTRUCTURE, infrastructure)]);
    let down = exogenous("power-down", 1, "power_outage", vec![("outage_id", s("p1")), ("component_id", s("water_1")), ("until_minute", V::Int(100))]);
    let text = rolls_back(built, operational_rolled_back(), |cell, host| handlers::infrastructure_event(cell, host, &down, Infra::StartOutage));
    assert_eq!(text, FAILS);
}
