//! Lane d3: the rules of an EffectProgram's events (`rules::effects`) on a world built here with no
//! Python. The PyO3 crate's effects are glue over these, so the gate and the offline tests run them
//! against the texts and digests of the deleted Python rules; these say what each reads, decides,
//! refuses and writes, and what it asks the host.

mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::cell::Shard;
use sheltersim_core::entities::{Activity, AgentRow, Entities, FacilityRow, Mobility, NeedRow, ProfileRow, Skills, StaffRow, ZoneRow};
use sheltersim_core::error::Error;
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::{EventKind, EventRow};
use sheltersim_core::host::{Contract, Decision, DecisionSink, ErrorClass, Host, HostFailure, Llm, Phase, Stage, Trace, TraceLevel, TxnKind};
use sheltersim_core::row::{IdMap, ValueRow};
use sheltersim_core::rules::effects::{self as effects, EnvironmentIn, EnvironmentPlan, GuardIn, SpaceIn, SpacePlan, Structure, StructureIn};
use sheltersim_core::rules::kit::Held;
use sheltersim_core::rules::metrics::MetricSample;
use sheltersim_core::config::environment::ZoneEnvironmentSpec;
use sheltersim_core::state::effect_program::{EffectProgramRow, FacilityModifiers, Modifier, Program, NAME as PROGRAM};
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

fn add_value(world: &mut World, name: &str, value: V) {
    let index = world.slots.add(text(name), Place::Value).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(ValueRow(value)));
}

fn facility(id: &str, zone: &str, capacity: i64, units: i64) -> FacilityRow {
    FacilityRow {
        id: text(id),
        kind: text("toilet"),
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

/// A world of 100 minutes at minute 10 (schema 6): zones z0 and z1, a toilet of 3 units and two
/// people in z0, staff s1 in z0; its queue, log, `id_counters` (event 40), `model_configs`, the
/// program of `op`, and the namespaces given as values.
fn world(op: &str, namespaces: Vec<(&str, V)>) -> World {
    let mut world = World::default();
    let scalars = world.scalars.get_mut();
    scalars.duration_minutes = Fit::Ok(100);
    scalars.minute_index = Fit::Ok(10);
    scalars.schema_version = Fit::Ok(6);
    let mut entities = Entities::default();
    for id in ["z0", "z1"] {
        entities.zones.insert(text(id), ZoneRow { id: text(id), name: text(id), capacity: None, display_x: None, display_y: None });
    }
    let mut toilet = facility("toilet_1", "z0", 3, 3);
    toilet.active_agent_ids = vec![text("a1")];
    entities.facilities.insert(text("toilet_1"), toilet);
    for id in ["a1", "a2"] {
        entities.agents.insert(text(id), agent(id));
    }
    entities.staff.insert(
        text("s1"),
        StaffRow { id: text("s1"), zone_id: text("z0"), skills: Skills(vec![text("support")]), work_queue: Vec::new(), busy_until: 0, active_task_id: None },
    );
    *world.entities.get_mut() = entities;
    add_value(&mut world, ID_COUNTERS, map(vec![("event", V::Int(40))]));
    let index = world.slots.add(text(MODEL_CONFIGS), Place::Field).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(ValueRow(map(Vec::new()))));
    let index = world.slots.add(text(PROGRAM), Place::Value).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(program(op)));
    for (name, value) in namespaces {
        add_value(&mut world, name, value);
    }
    world.hold_queue(Arc::new(Queue::new()));
    world.hold_log(Arc::new(EventLog::new()));
    world
}

/// The marker of a start of `op`, with a guard if one is given.
fn marker(op: &str, guard: Option<V>, dependencies: &[&str]) -> V {
    let mut fields = vec![
        ("program_id", s("p1")),
        ("effect_id", s("e1")),
        ("op", s(op)),
        ("source_text_hash", s(&"b".repeat(64))),
        ("compiler_provenance", map(vec![("compiler_kind", s("offline_rule")), ("compiler_version", s("v1"))])),
        ("assumption_ids", V::List(vec![s("ASM-B"), s("ASM-A"), s("ASM-B")])),
        ("policy_dependencies", V::List(dependencies.iter().map(|item| s(item)).collect())),
        ("units", s("count")),
        ("phase", s("start")),
    ];
    if let Some(guard) = guard {
        fields.push(("guard", guard));
    }
    map(fields)
}

fn event(event_id: &str, due: i64, event_type: &str, payload: Vec<(&str, V)>, marker: V) -> EventRow {
    EventRow::new(
        text(event_id),
        EventKind::Intervention,
        due,
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

fn failed<T>(result: Result<T, Error>) -> String {
    match result {
        Err(Error::Outcome(_, text)) => text,
        Err(other) => panic!("not an outcome: {other:?}"),
        Ok(_) => panic!("not an outcome: it passed"),
    }
}

fn state(world: &World, name: &str) -> V {
    world.slots.try_value(name).unwrap().unwrap().clone()
}

// ---- A host whose contract or validation fails -------------------------------------------------------

/// `TestHost`, whose contract and whole-world validation raise the errors given.
#[derive(Default)]
struct Failing {
    host: TestHost,
    contract: Option<Error>,
    validation: Option<Error>,
}

macro_rules! delegate {
    ($($name:ident($($arg:ident: $kind:ty),*) -> $out:ty;)*) => {
        $(fn $name(&mut self, $($arg: $kind),*) -> Result<$out, Error> {
            self.host.$name($($arg),*)
        })*
    };
}

impl Host for Failing {
    type Error = Error;

    delegate! {
        validate_candidate(kind: TxnKind, stage: Stage) -> ();
        validate_extensions() -> ();
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

    fn canonicalized(&mut self, kind: TxnKind, before: &Snapshot) -> Result<(), Error> {
        self.host.canonicalized(kind, before)
    }

    fn validate_world(&mut self) -> Result<(), Error> {
        self.host.validate_world()?;
        self.validation.clone().map_or(Ok(()), Err)
    }

    fn contract(&mut self, which: &Contract, input: &V) -> Result<V, Error> {
        let given = self.host.contract(which, input)?;
        self.contract.clone().map_or(Ok(given), Err)
    }

    fn llm(&self) -> Result<Llm, Error> {
        self.host.llm()
    }

    fn decision_sink(&self) -> Result<DecisionSink, Error> {
        self.host.decision_sink()
    }

    fn trace_level(&self) -> Result<TraceLevel, Error> {
        self.host.trace_level()
    }
}

fn guard_in() -> (Held, Held) {
    (Held::Held, Held::Absent)
}

// ---- The guard ------------------------------------------------------------------------------------

#[test]
fn a_guard_reads_the_rows_and_a_guard_it_cannot_read_is_the_contract_s() {
    let cell = Shard::new(world("set_facility_availability", Vec::new()));
    let (program, ledger) = guard_in();
    let ins = GuardIn { program: &program, ledger: &ledger };
    let leaf = |field: &str, operator: &str, value: V| map(vec![("operator", s(operator)), ("field", s(field)), ("value", value)]);
    let guarded = |guard: V| event("ev1", 10, "effect_program_apply", vec![("facility_id", s("toilet_1"))], marker("set_facility_availability", Some(guard), &[]));
    let mut host = TestHost::default();
    // The minute, a facility's units (3) and the zone's people (2), compared by value.
    assert_eq!(effects::guard(&cell, &mut host, &ins, &guarded(leaf("simulation.minute", "eq", V::Float(10.0)))), Ok(true));
    assert_eq!(effects::guard(&cell, &mut host, &ins, &guarded(leaf("target.working_units", "gt", V::Int(3)))), Ok(false));
    let both = map(vec![("operator", s("and")), ("operands", V::List(vec![leaf("zone.occupancy", "gte", V::Int(2)), leaf("staff.available", "eq", V::Bool(true))]))]);
    // `staff.available` reads the payload's `staff_id or target`: the facility is no staff member.
    assert_eq!(failed(effects::guard(&cell, &mut host, &ins, &guarded(both))), "guard staff.available has no known staff target");
    assert!(host.calls.is_empty());
    // False suppresses.
    let suppressed = effects::guard_or_suppress(&cell, &mut host, &ins, &guarded(leaf("target.capacity", "lt", V::Int(3))));
    assert_eq!(suppressed, Err(Error::suppressed("EffectProgram guard evaluated false")));
    // A guard the kernel doesn't read goes to the contract: its ValueError is the failure's text, and
    // one the contract takes is a refusal.
    let odd = leaf("target.capacity", "eq", V::List(Vec::new()));
    let mut refusing = Failing { contract: Some(Error::Value("1 validation error for GuardExpression".to_owned())), ..Failing::default() };
    assert_eq!(failed(effects::guard(&cell, &mut refusing, &ins, &guarded(odd.clone()))), "EffectProgram guard is invalid: 1 validation error for GuardExpression");
    assert_eq!(refusing.host.calls, [Call::Contract(Contract::Guard, odd.clone())]);
    assert!(matches!(effects::guard(&cell, &mut host, &ins, &guarded(odd)), Err(Error::Unsupported(_))));
}

#[test]
fn what_other_rules_ask_of_the_modifiers() {
    let built = world("set_facility_availability", vec![("infrastructure_v1", map(vec![("active_events", map(Vec::new()))]))]);
    let pending = event("ev2", 50, "effect_program_expire", vec![("facility_id", s("toilet_1"))], V::Null);
    built.queue_mut().unwrap().add(text("ev2"), (50, 0, 41), Some(Arc::new(pending)));
    assert_eq!(effects::has_pending_modifier(&built, "toilet_1"), Ok(true));
    assert_eq!(effects::has_pending_modifier(&built, "toilet_2"), Ok(false));
    assert_eq!(effects::has_active_modifier(&built, &Held::Held, "toilet_1"), Ok(false));
    assert_eq!(effects::has_active_modifier(&built, &Held::Absent, "toilet_1"), Ok(false));
    // A pending event without its row: the PyO3 crate reads the rows and asks again.
    built.queue_mut().unwrap().add(text("ev3"), (60, 0, 42), None);
    assert_eq!(effects::has_pending_modifier(&built, "toilet_2"), Err(Error::Unsupported(effects::PENDING_UNREAD.into())));
}

// ---- Facility modifiers ------------------------------------------------------------------------------

#[test]
fn a_facility_modifier_keeps_a_stack_and_the_lowest_units() {
    let cell = Shard::new(world("set_facility_availability", Vec::new()));
    let payload = |units: Option<i64>| {
        let mut fields = vec![("program_id", s("p1")), ("effect_id", s("e1")), ("modifier_id", s(MODIFIER)), ("facility_id", s("toilet_1"))];
        if let Some(units) = units {
            fields.push(("working_units", V::Int(units)));
        }
        fields
    };
    let start = event("ev1", 10, "effect_program_apply", payload(Some(2)), V::Null);
    let plan = cell.with(|world| effects::plan_facility(world, &Held::Held, &Held::Absent, &start, false)).unwrap().unwrap();
    let mut host = TestHost::default();
    effects::write_facility(&cell, &mut host, &plan).unwrap();
    assert_eq!(host.calls, [Call::ValidateWorld]);
    cell.with(|world| {
        assert_eq!(world.rows().unwrap().facilities.get("toilet_1").unwrap().working_units, 2);
        let row = world.row::<EffectProgramRow>(PROGRAM).unwrap();
        let FacilityModifiers { baseline_working_units, modifiers } = row.active_facility_modifiers.get("toilet_1").unwrap();
        assert_eq!(*baseline_working_units, 3);
        let Modifier { modifier_id, working_units, start_minute, sequence, .. } = &modifiers[0];
        assert_eq!((&**modifier_id, *working_units, *start_minute, *sequence), (MODIFIER, 2, 10, 41));
        // The same modifier again is suppressed; fewer units than the people in it fail.
        assert_eq!(effects::plan_facility(world, &Held::Held, &Held::Absent, &start, false).err(), Some(Error::suppressed("facility availability modifier is already active")));
        let below = event("ev1", 10, "effect_program_apply", payload(Some(0)), V::Null);
        assert_eq!(failed(effects::plan_facility(world, &Held::Held, &Held::Absent, &below, false)), "facility availability modifier violates capacity or occupancy");
    })
    .unwrap();
    // The expiry restores the baseline and the stack goes with its last modifier.
    let expiry = event("ev9", 20, "effect_program_expire", payload(None), V::Null);
    let plan = cell.with(|world| effects::plan_facility(world, &Held::Held, &Held::Absent, &expiry, true)).unwrap().unwrap();
    effects::write_facility(&cell, &mut host, &plan).unwrap();
    cell.with(|world| {
        assert_eq!(world.rows().unwrap().facilities.get("toilet_1").unwrap().working_units, 3);
        assert!(world.row::<EffectProgramRow>(PROGRAM).unwrap().active_facility_modifiers.is_empty());
        assert_eq!(effects::plan_facility(world, &Held::Held, &Held::Absent, &expiry, true).err(), Some(Error::suppressed("facility availability modifier is not active")));
    })
    .unwrap();
}

// ---- Environment modifiers ---------------------------------------------------------------------------

fn environment_world() -> World {
    let built = world("set_ambient_noise", vec![("zone_environment_v1", map(vec![("modifiers", map(Vec::new()))]))]);
    let expiry = event("ev-end", 60, "effect_program_environment_expire", vec![("modifier_id", s(MODIFIER))], V::Null);
    built.queue_mut().unwrap().add(text("ev-end"), (60, 0, 50), Some(Arc::new(expiry)));
    built
}

fn environment_start() -> EventRow {
    event(
        "ev1",
        10,
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
        marker("set_ambient_noise", None, &["zone_environment_v1"]),
    )
}

#[test]
fn an_environment_modifier_records_the_contract_s_dump_and_wraps_the_check_s_value_error() {
    let cell = Shard::new(environment_world());
    let spec = ZoneEnvironmentSpec { policy_version: text("zone-environment-v2"), source_id: text("ASM-ENV") };
    let (program, environment) = (Held::Held, Held::Held);
    let ins = EnvironmentIn { program: &program, environment: &environment, service_present: false, config: Some(&spec) };
    let start = environment_start();
    let mut host = TestHost::default();
    let plan = effects::plan_environment(&cell, &mut host, &ins, &start, false).unwrap();
    let parameters = map(vec![("noise_db", V::Float(55.5))]);
    assert_eq!(host.calls, [Call::Contract(Contract::EffectParameters(text("set_ambient_noise")), parameters.clone())]);
    let EnvironmentPlan::Apply { record, .. } = &plan else { panic!("not a start") };
    let V::Map(fields) = record else { panic!("not a record") };
    let keys: Vec<&str> = fields.keys().map(|key| &**key).collect();
    assert_eq!(
        keys,
        ["modifier_id", "program_id", "effect_id", "op", "zone_id", "parameters", "start_event_id", "end_event_id", "start_minute", "end_minute", "source_id", "assumption_ids"]
    );
    assert_eq!(fields["end_event_id"], s("ev-end"));
    assert_eq!(fields["assumption_ids"], V::List(vec![s("ASM-A"), s("ASM-B")]));
    assert_eq!(fields["parameters"], parameters);
    // The check's ValueError is the rule's text; the record was written (the transaction rolls back).
    let mut failing = Failing { validation: Some(Error::Value("bad state".to_owned())), ..Failing::default() };
    assert_eq!(failed(effects::write_environment(&cell, &mut failing, &plan)), "zone environment modifier is invalid: bad state");
    // An outcome is let through as it is.
    let mut outcome = Failing { validation: Some(Error::suppressed("held back")), ..Failing::default() };
    assert_eq!(effects::write_environment(&cell, &mut outcome, &EnvironmentPlan::Expire { modifier: text(MODIFIER) }), Err(Error::suppressed("held back")));
    // A contract that refuses the parameters, and a config whose read raised.
    let mut refusing = Failing { contract: Some(Error::Value("1 validation error".to_owned())), ..Failing::default() };
    let fresh = Shard::new(environment_world());
    assert_eq!(failed(effects::plan_environment(&fresh, &mut refusing, &ins, &start, false)), "zone environment parameters are invalid: 1 validation error");
    let unread = EnvironmentIn { config: None, ..ins };
    assert_eq!(effects::plan_environment(&fresh, &mut host, &unread, &start, false), Err(Error::Unsupported(effects::CONFIG_UNREAD.into())));
}

#[test]
fn an_environment_expiry_pops_its_record() {
    let cell = Shard::new(environment_world());
    let spec = ZoneEnvironmentSpec { policy_version: text("zone-environment-v1"), source_id: text("ASM-ENV") };
    let (program, environment) = (Held::Held, Held::Held);
    let ins = EnvironmentIn { program: &program, environment: &environment, service_present: false, config: Some(&spec) };
    let mut host = TestHost::default();
    let plan = effects::plan_environment(&cell, &mut host, &ins, &environment_start(), false).unwrap();
    effects::write_environment(&cell, &mut host, &plan).unwrap();
    let mut expiry = event("ev-end", 60, "effect_program_environment_expire", vec![("program_id", s("p1")), ("effect_id", s("e1")), ("modifier_id", s(MODIFIER)), ("zone_id", s("z1"))], marker("set_ambient_noise", None, &["zone_environment_v1"]));
    // Not the start's cause: the record does not match.
    assert_eq!(failed(effects::plan_environment(&cell, &mut host, &ins, &expiry, true)), "zone environment expiry does not match its start");
    expiry.causation_id = Some(text("ev1"));
    let plan = effects::plan_environment(&cell, &mut host, &ins, &expiry, true).unwrap();
    assert_eq!(plan, EnvironmentPlan::Expire { modifier: text(MODIFIER) });
    effects::write_environment(&cell, &mut host, &plan).unwrap();
    cell.with(|world| assert_eq!(state(world, "zone_environment_v1"), map(vec![("modifiers", map(Vec::new()))]))).unwrap();
    assert_eq!(effects::plan_environment(&cell, &mut host, &ins, &expiry, true).err(), Some(Error::suppressed("zone environment modifier is not active")));
}

// ---- Temporary spaces --------------------------------------------------------------------------------

#[test]
fn a_temporary_space_registers_its_record_and_wraps_the_registration_s_error() {
    let spaces = map(vec![("spaces", map(Vec::new())), ("history", V::List(Vec::new()))]);
    let cell = Shard::new(world("add_temporary_space", vec![("temporary_spaces_v1", spaces)]));
    let (program, ledger, held) = (Held::Held, Held::Absent, Held::Held);
    let guard = GuardIn { program: &program, ledger: &ledger };
    let ins = SpaceIn { guard: &guard, spaces: &held };
    let start = event(
        "ev1",
        10,
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
        marker("add_temporary_space", None, &["temporary_spaces_v1"]),
    );
    let mut host = TestHost::default();
    let plan = effects::plan_temporary_space(&cell, &mut host, &ins, &start, false).unwrap();
    let SpacePlan::Register { record } = &plan else { panic!("not a start") };
    let V::Map(fields) = record else { panic!("not a record") };
    assert_eq!(fields["source_ids"], V::List(vec![s("ASM-A"), s("ASM-B"), s("SRC-1")]));
    assert_eq!((&fields["people_capacity"], &fields["sleeping_capacity"]), (&V::Int(4), &V::Int(2)));
    effects::write_temporary_space(&cell, &mut host, &held, &start, &plan).unwrap();
    // A second registration of the same space is the registration's ValueError, as the event's text.
    assert_eq!(failed(effects::write_temporary_space(&cell, &mut host, &held, &start, &plan)), "temporary space ID already exists");
    // Without the namespace the registration refuses: its `KernelRefusal`'s text.
    let empty = Shard::new(world("add_temporary_space", vec![("temporary_spaces_v1", map(vec![("spaces", V::Int(1)), ("history", V::List(Vec::new()))]))]));
    let text = failed(effects::write_temporary_space(&empty, &mut host, &held, &start, &plan));
    assert!(text.starts_with("overlay.register_temporary_space refused: "), "{text}");
}

// ---- Structures -------------------------------------------------------------------------------------

fn structure_event(op: &str, extra: Vec<(&str, V)>) -> EventRow {
    let mut payload = vec![("program_id", s("p1")), ("effect_id", s("e1")), ("modifier_id", s(MODIFIER)), ("op", s(op))];
    payload.extend(extra);
    event("ev1", 10, "effect_program_structure", payload, marker(op, None, &[]))
}

#[test]
fn a_structure_s_branch_is_told_by_its_fields() {
    assert_eq!(effects::structure_of(&structure_event("staff_task_order", vec![("staff_id", s("s1")), ("parameters", map(Vec::new()))])), Ok(Structure::TaskOrder));
    assert_eq!(effects::structure_of(&structure_event("staff_handoff", vec![("staff_id", s("s1")), ("parameters", map(Vec::new()))])), Ok(Structure::Handoff));
    assert_eq!(effects::structure_of(&structure_event("facility_repair", vec![("facility_id", s("toilet_1")), ("parameters", map(Vec::new()))])), Ok(Structure::Repair));
    assert_eq!(effects::structure_of(&structure_event("set_staff_role", vec![("staff_id", s("s1")), ("skills", V::List(Vec::new()))])), Ok(Structure::Role));
    assert_eq!(effects::structure_of(&structure_event("add_staff", vec![("zone_id", s("z0")), ("count", V::Int(1))])), Ok(Structure::Add));
}

#[test]
fn a_role_change_sorts_the_contract_s_skills() {
    let cell = Shard::new(world("set_staff_role", Vec::new()));
    let (program, ledger, absent) = (Held::Held, Held::Absent, Held::Absent);
    let guard = GuardIn { program: &program, ledger: &ledger };
    let ins = StructureIn { guard: &guard, shared: false, infrastructure: &absent };
    let skills = V::List(vec![s("zeta"), s("alpha")]);
    let role = structure_event("set_staff_role", vec![("staff_id", s("s1")), ("skills", skills.clone())]);
    let mut host = TestHost::default();
    let plan = effects::plan_role(&cell, &mut host, &ins, &role).unwrap();
    // The rule builds the contract's input around the payload's skills.
    assert_eq!(host.calls, [Call::Contract(Contract::EffectParameters(text("set_staff_role")), map(vec![("skills", skills)]))]);
    effects::write_role(&cell, &mut host, &absent, &plan).unwrap();
    cell.with(|world| assert_eq!(world.rows().unwrap().staff.get("s1").unwrap().skills.0, vec![text("alpha"), text("zeta")])).unwrap();
    let mut refusing = Failing { contract: Some(Error::Value("staff role skills must be unique".to_owned())), ..Failing::default() };
    assert_eq!(failed(effects::plan_role(&cell, &mut refusing, &ins, &role)), "staff-role event skills are invalid: staff role skills must be unique");
    let unknown = structure_event("set_staff_role", vec![("staff_id", s("s9")), ("skills", V::List(Vec::new()))]);
    assert_eq!(failed(effects::plan_role(&cell, &mut host, &ins, &unknown)), "staff-role event does not reference an approved staff target");
}

#[test]
fn added_staff_get_their_records_where_the_world_has_the_namespace() {
    let operational = map(vec![("staff_status", map(Vec::new()))]);
    let cell = Shard::new(world("add_staff", vec![("operational_events_v1", operational)]));
    let added = structure_event("add_staff", vec![("zone_id", s("z1")), ("count", V::Int(2))]);
    let plan = cell.with(|world| effects::plan_add(world, &Held::Held, &added)).unwrap().unwrap();
    let (held, absent) = (Held::Held, Held::Absent);
    let schema = Ok(Arc::new(None));
    let policy = Ok(Arc::new("agent-dynamics-v1"));
    let ins = effects::AddIn {
        operational: &held,
        dynamics: &absent,
        operations: &absent,
        movement: &absent,
        overlay: &absent,
        overlay_schema: &schema,
        policy: &policy,
        geo: None,
        staff_ops: None,
    };
    let mut host = TestHost::default();
    assert_eq!(effects::write_add(&cell, &mut host, &ins, &plan), Ok("structure.add_staff"));
    cell.with(|world| {
        let rows = world.rows().unwrap();
        let ids: Vec<&str> = rows.staff.iter().map(|(id, _)| &**id).collect();
        let suffix = &MODIFIER["modifier-".len()..];
        assert_eq!(ids, ["s1", &format!("effect-staff-{suffix}-1"), &format!("effect-staff-{suffix}-2")]);
        let V::Map(operational) = state(world, "operational_events_v1") else { panic!("no state") };
        let Some(V::Map(statuses)) = operational.get("staff_status") else { panic!("no statuses") };
        assert_eq!(statuses.len(), 2);
        let record = statuses.values().next().unwrap();
        assert_eq!(record, &map(vec![("available", V::Bool(true)), ("unavailability_event_id", V::Null), ("until_minute", V::Null), ("restore_event_id", V::Null)]));
    })
    .unwrap();
    // Past the bound of 100 staff.
    let many = structure_event("add_staff", vec![("zone_id", s("z1")), ("count", V::Int(98))]);
    let plan = cell.with(|world| effects::plan_add(world, &Held::Held, &many)).unwrap().unwrap();
    assert_eq!(failed(effects::write_add(&cell, &mut host, &ins, &plan)), "adding staff would exceed the 100-staff runtime bound");
    let unknown = structure_event("add_staff", vec![("zone_id", s("z9")), ("count", V::Int(1))]);
    cell.with(|world| assert_eq!(failed(effects::plan_add(world, &Held::Held, &unknown)), "structural primitive target or operation is invalid")).unwrap();
}

// ---- The state's check --------------------------------------------------------------------------------

#[test]
fn the_tick_s_check_of_the_state_passes_or_refuses() {
    let cell = Shard::new(world("set_facility_availability", Vec::new()));
    let mut host = TestHost::default();
    assert_eq!(effects::check_state(&cell, &mut host, &Held::Held, false), Ok(()));
    // Without the state, a pending event with a marker is for Python to word.
    let built = world("set_facility_availability", Vec::new());
    let marked = event("ev1", 10, "effect_program_apply", Vec::new(), marker("set_facility_availability", None, &[]));
    built.queue_mut().unwrap().add(text("ev1"), (10, 0, 41), Some(Arc::new(marked)));
    let cell = Shard::new(built);
    assert_eq!(effects::check_state(&cell, &mut host, &Held::Absent, false), Err(Error::Unsupported("marked events without the state".into())));
    assert_eq!(effects::expected_event_type("expiry", "add_temporary_space"), Some("effect_program_temporary_space_expire"));
    assert_eq!(effects::OPERATORS.len(), 27);
}

#[test]
fn a_host_error_says_its_class_and_text() {
    assert_eq!(Error::Key("x".to_owned()).text(), "x");
    assert!(Error::failed("no").is_outcome());
    assert!(!Error::Value("no".to_owned()).is_outcome());
    assert_eq!(Error::Refused { label: "overlay.x".to_owned(), reason: "why".into() }.class(), ErrorClass::Value);
}
