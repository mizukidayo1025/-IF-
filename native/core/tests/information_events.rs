//! Lane P8 d1: the information events (`rules::information_events`) and the social events
//! (`rules::social_interaction`) in the core, on worlds built here. The PyO3 crate's files of the
//! same names are glue over these, so the gate and the offline tests run them on every Run; these
//! say what each does with no Python.

mod support;

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::config::information::InformationSpread;
use sheltersim_core::config::{Fault, FaultClass, Selection};
use sheltersim_core::entities::{Activity, AgentRow, AnyTable, EdgeRow, Entities, Mobility, NeedRow, ProfileRow, RequestRow, RequestStatus, Table, ZoneRow};
use sheltersim_core::error::{Error, Outcome};
use sheltersim_core::events::{EventKind, EventRow, EventStatus};
use sheltersim_core::host::{Contract, Decision, DecisionSink, Host, Llm, Phase, Stage, Trace, TraceLevel, TxnKind};
use sheltersim_core::rng::Draws;
use sheltersim_core::row::{IdMap, Row};
use sheltersim_core::rules::common_operations::Logged;
use sheltersim_core::rules::information_events::{self as info, Applied, Inputs, NOT_ENABLED, OLDER_STATE};
use sheltersim_core::rules::kit::{Held, Kept};
use sheltersim_core::rules::metrics::MetricSample;
use sheltersim_core::rules::social_interaction::{self as social, Inputs as SocialInputs};
use sheltersim_core::cell::Shard;
use sheltersim_core::state::information::{BeliefRow, ContactRow, CountersRow, InformationRow, MessageRow, NAME};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, Seed, Snapshot, World, WorldCell};
use support::TestHost;

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn map(entries: &[(&str, V)]) -> V {
    V::Map(entries.iter().map(|(key, value)| (text(key), value.clone())).collect())
}

fn string(value: &str) -> V {
    V::Str(text(value))
}

fn unsupported(reason: &str) -> Error {
    Error::Unsupported(reason.to_owned().into())
}

fn needs(value: f64) -> NeedRow {
    NeedRow { toilet_urgency: value, hunger: value, fatigue: value, hydration_need: value }
}

fn agent(id: &str, zone: &str, group: Option<&str>) -> AgentRow {
    AgentRow {
        profile: ProfileRow {
            id: text(id),
            group_id: group.map(text),
            initial_zone_id: text(zone),
            mobility: Mobility::Independent,
            support_need: 0.0,
            stress_sensitivity: 1.0,
            rng_key: text(id),
            initial_needs: needs(0.0),
            provenance: V::Map(IndexMap::new()),
        },
        zone_id: text(zone),
        activity: Activity::Idle,
        target_id: None,
        needs: needs(0.25),
        stress: 0.5,
        trust: 0.75,
        next_decision_at: 7,
        support_request_id: None,
        support_cooldown_until: 0,
    }
}

fn put<R: Clone>(table: &mut Table<R>, id: &str, row: R) {
    let slot = table.reserve_slot();
    table.fill_slot(slot, row);
    assert_eq!(table.bind(text(id), slot), None);
}

fn empty_state(people: &[&str]) -> InformationRow {
    let mut beliefs = IdMap::default();
    let mut pressure = IdMap::default();
    for person in people {
        beliefs.insert(text(person), IdMap::default());
        pressure.insert(text(person), 0.0);
    }
    InformationRow {
        schema_version: 2,
        messages: IdMap::default(),
        beliefs,
        contacts: IdMap::<ContactRow>::default(),
        query_pressure: pressure,
        support_requests: Vec::new(),
        counters: CountersRow { contact_attempts: 0, contact_deliveries: 0, correction_deliveries: 0, support_requests: 0 },
    }
}

/// a2, a1 (a household, `g1`) and a4 in z0, a3 in z1; an open edge from z0 to z1 and a closed one
/// from z0 to z2. Seed 7, minute 10 of 100, the information state of the four people.
fn world() -> World {
    let mut entities = Entities::default();
    put(&mut entities.agents, "a2", agent("a2", "z0", Some("g1")));
    put(&mut entities.agents, "a1", agent("a1", "z0", Some("g1")));
    put(&mut entities.agents, "a4", agent("a4", "z0", None));
    put(&mut entities.agents, "a3", agent("a3", "z1", None));
    for zone in ["z0", "z1", "z2"] {
        put(&mut entities.zones, zone, ZoneRow { id: text(zone), name: text(zone), capacity: None, display_x: None, display_y: None });
    }
    put(&mut entities.zone_edges, "e01", EdgeRow { id: text("e01"), from_zone_id: text("z0"), to_zone_id: text("z1"), travel_minutes: 1, closed: false });
    put(&mut entities.zone_edges, "e02", EdgeRow { id: text("e02"), from_zone_id: text("z0"), to_zone_id: text("z2"), travel_minutes: 1, closed: true });
    put(
        &mut entities.support_requests,
        "r1",
        RequestRow {
            id: text("r1"),
            agent_id: text("a1"),
            requested_minute: 0,
            timeout_minute: 60,
            status: RequestStatus::Pending,
            assigned_staff_id: None,
            task_id: None,
            resolved_minute: None,
            cooldown_until: 0,
            last_assignment_attempt_minute: None,
            last_assignment_wait_reason_code: None,
        },
    );
    let mut world = World::default();
    *world.entities.get_mut() = entities;
    let scalars = world.scalars.get_mut();
    scalars.duration_minutes = Fit::Ok(100);
    scalars.minute_index = Fit::Ok(10);
    scalars.seed = Fit::Ok(Seed::from_text("7").expect("a seed"));
    add_row(&mut world, NAME, Place::Namespace, empty_state(&["a1", "a2", "a3", "a4"]));
    world
}

fn add_row<T: Row>(world: &mut World, name: &str, place: Place, row: T) {
    let index = world.slots.add(text(name), place).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(row));
}

fn state(world: &World) -> InformationRow {
    world.row::<InformationRow>(NAME).unwrap().clone()
}

fn written(world: &mut World) -> Vec<String> {
    let names: Vec<String> = world.slots.written_names().iter().map(|name| name.to_string()).collect();
    world.slots.take_written();
    names
}

fn config(enabled: bool) -> InformationSpread {
    let config = map(&[
        ("enabled", V::Bool(enabled)),
        ("minimum_contact_minutes", V::Int(2)),
        ("default_source_reliability", V::Float(0.5)),
        ("correction_source_reliability_by_id", map(&[("trusted", V::Float(0.9))])),
        ("rumor_belief_probability", V::Float(0.5)),
        ("rumor_trust_sensitivity", V::Float(0.4)),
        ("rumor_source_sensitivity", V::Float(0.3)),
        ("query_pressure_per_claim", V::Float(0.2)),
        ("query_pressure_relief_per_correction", V::Float(0.3)),
        ("correction_acceptance_probability", V::Float(0.6)),
        ("correction_trust_sensitivity", V::Float(0.2)),
        ("correction_source_sensitivity", V::Float(0.1)),
        ("present", V::Bool(true)),
    ]);
    InformationSpread::from_v(&config).expect("a config")
}

fn selection(model: &str) -> Kept<Selection> {
    Ok(Arc::new(Selection::new(vec![(text("information"), text(model))])))
}

const PLAIN: &str = "information-spread-v1";
const AWARE: &str = "evidence-aware-rumor-v1";

#[allow(clippy::too_many_arguments)]
fn event(id: &str, kind: EventKind, due: i64, event_type: &str, payload: V, source: Option<&str>, causation: Option<&str>, provenance: V) -> EventRow {
    EventRow {
        event_id: text(id),
        kind,
        due_minute: due,
        priority: 0,
        sequence: 1,
        event_type: text(event_type),
        payload,
        source_id: source.map(text),
        causation_id: causation.map(text),
        provenance,
    }
}

fn rumor_event(id: &str, message: &str, truth: &str, origin: &str) -> EventRow {
    let payload = map(&[("message_id", string(message)), ("truth_status", string(truth)), ("origin_agent_id", string(origin))]);
    event(id, EventKind::Exogenous, 10, "information_rumor", payload, Some("trusted"), None, map(&[]))
}

/// Run f on a shard of the world and give the world back.
fn on<R>(world: &mut World, f: impl FnOnce(&Shard<World>) -> R) -> R {
    let cell = Shard::new(std::mem::take(world));
    let result = f(&cell);
    *world = cell.into_inner();
    result
}

fn clone_world(world: &World) -> World {
    world.fork().expect("a fork")
}

fn inputs<'a>(config: &'a InformationSpread, held: &'a Held, selection: &'a Kept<Selection>) -> Inputs<'a> {
    Inputs { config, held, selection }
}

// ---- Rumors -------------------------------------------------------------------------------------

#[test]
fn a_rumor_seeds_its_origin_and_marks_the_namespace() {
    let mut world = world();
    let (config, selection) = (config(true), selection(PLAIN));
    let applied = on(&mut world, |cell| info::rumor(cell, &mut TestHost::default(), &inputs(&config, &Held::Held, &selection), &rumor_event("ev-r1", "m1", "false", "a1")));
    assert_eq!(applied, Ok(()));
    assert_eq!(written(&mut world), vec![NAME.to_owned()]);
    let state = state(&world);
    let message = &state.messages["m1"];
    assert_eq!(
        (&*message.id, &*message.truth_status, &*message.source_id, &*message.zone_id, message.time, &*message.origin_event_id, &*message.origin_agent_id),
        ("m1", "false", "trusted", "z0", 10, "ev-r1", "a1")
    );
    let belief = &state.beliefs["a1"]["m1"];
    assert_eq!((&*belief.belief, &*belief.source, belief.last_contact, &*belief.last_event_id), ("heard", "a1", 10, "ev-r1"));
    assert_eq!((belief.exposed, belief.belief_strength, belief.share_propensity), (true, 0.5, 0.5));
    // A false claim at 0.5 is not believed.
    assert!(!belief.ever_misbelieved && !belief.correction_exposed && !belief.ever_forgotten);
    assert_eq!(state.query_pressure["a1"], 0.2);
    // A second claim comes after the first, as a dict keeps it.
    on(&mut world, |cell| info::rumor(cell, &mut TestHost::default(), &inputs(&config, &Held::Held, &selection), &rumor_event("ev-r0", "m0", "true", "a1"))).unwrap();
    assert_eq!(state_names(&world), ["m1", "m0"]);
    assert_eq!(state_pressure(&world, "a1"), 0.4);
}

fn state_names(world: &World) -> Vec<String> {
    state(world).messages.keys().map(|key| key.to_string()).collect()
}

fn state_pressure(world: &World, agent: &str) -> f64 {
    state(world).query_pressure[agent]
}

#[test]
fn an_evidence_aware_rumor_draws_its_strength_from_the_seed() {
    let mut world = world();
    let (config, selection) = (config(true), selection(AWARE));
    on(&mut world, |cell| info::rumor(cell, &mut TestHost::default(), &inputs(&config, &Held::Held, &selection), &rumor_event("ev-r1", "m1", "true", "a1"))).unwrap();
    let belief = state(&world).beliefs["a1"]["m1"].clone();
    // trust 0.75 and the source's 0.9: 0.5 + 0.8 * 0.25 + 0.6 * 0.4 = 0.94, against the draw of
    // ("rumor-belief-acceptance-v1", 10, "a1", "m1") under seed 7 (`claim_strength_of`).
    let expected = on(&mut world, |cell| cell.with(|world| info::sample_strength(world, &config, "a1", "trusted", 10, "m1"))).unwrap().unwrap();
    assert_eq!(belief.belief_strength, expected);
    assert_eq!(expected, EXPECTED_STRENGTH);
    assert_eq!(belief.ever_misbelieved, EXPECTED_STRENGTH < 0.5);
}

/// `CounterRNG(7).random("rumor-belief-acceptance-v1", 10, "a1", "m1")` of `core/rng.py`, below
/// the probability 0.94: the claim is believed.
const RUMOR_DRAW: f64 = 0.6913947174306129;
const EXPECTED_STRENGTH: f64 = 1.0;

#[test]
fn a_rumor_s_texts_come_in_the_python_rule_s_order() {
    let world = world();
    let config = config(true);
    let selection = selection(PLAIN);
    let fail = |text: &str| Err(Error::Outcome(Outcome::Failed, text.to_owned()));
    let run = |held: &Held, config: &InformationSpread, row: &EventRow| {
        let mut world = clone_world(&world);
        on(&mut world, |cell| info::rumor(cell, &mut TestHost::default(), &inputs(config, held, &selection), row))
    };
    let good = rumor_event("ev-r1", "m1", "true", "a1");
    assert_eq!(run(&Held::Held, &config_off(), &good), fail(NOT_ENABLED));
    assert_eq!(run(&Held::Absent, &config, &good), fail(NOT_ENABLED));
    assert_eq!(run(&Held::Other("another document"), &config, &good), Err(unsupported("another document")));
    let mut extra = good.clone();
    extra.payload = map(&[("message_id", string("m1")), ("truth_status", string("true")), ("x", V::Int(1))]);
    assert_eq!(run(&Held::Held, &config, &extra), fail("information_rumor payload fields do not match"));
    let mut sourceless = good.clone();
    sourceless.source_id = None;
    assert_eq!(run(&Held::Held, &config, &sourceless), fail("information_rumor requires a source_id"));
    let mut numbered = good.clone();
    numbered.payload = map(&[("message_id", V::Int(5)), ("truth_status", string("true")), ("origin_agent_id", string("a1"))]);
    assert_eq!(run(&Held::Held, &config, &numbered), fail("information_rumor requires stable message and Agent IDs"));
    assert_eq!(run(&Held::Held, &config, &rumor_event("ev-r1", "m1", "maybe", "a1")), fail("information_rumor truth_status is invalid"));
    assert_eq!(run(&Held::Held, &config, &rumor_event("ev-r1", "m1", "true", "nobody")), fail("information_rumor references an unknown Agent"));
    let mut taken = clone_world(&world);
    on(&mut taken, |cell| info::rumor(cell, &mut TestHost::default(), &inputs(&config, &Held::Held, &selection), &good)).unwrap();
    let again = on(&mut taken, |cell| info::rumor(cell, &mut TestHost::default(), &inputs(&config, &Held::Held, &selection), &good));
    assert_eq!(again, fail("information message ID already exists"));
    // An ID the Python rule would turn down is left to it, with nothing written.
    let mut odd = clone_world(&world);
    let refused = on(&mut odd, |cell| info::rumor(cell, &mut TestHost::default(), &inputs(&config, &Held::Held, &selection), &rumor_event("ev r1", "m1", "true", "a1")));
    assert_eq!(refused, Err(unsupported("an ID for the Python rule to judge")));
    assert!(written(&mut odd).is_empty());
}

// ---- Corrections --------------------------------------------------------------------------------

fn correction_event(message: &str, zone: &str, belief: &str, causation: &str) -> EventRow {
    let payload = map(&[("message_id", string(message)), ("zone_id", string(zone)), ("belief", string(belief))]);
    event("ev-c1", EventKind::Exogenous, 12, "information_correction", payload, Some("trusted"), Some(causation), map(&[]))
}

#[test]
fn a_correction_reaches_everyone_in_the_zone_by_id() {
    let mut world = world();
    let (config, selection) = (config(true), selection(PLAIN));
    on(&mut world, |cell| info::rumor(cell, &mut TestHost::default(), &inputs(&config, &Held::Held, &selection), &rumor_event("ev-r1", "m1", "unknown", "a2"))).unwrap();
    written(&mut world);
    on(&mut world, |cell| info::correction(cell, &mut TestHost::default(), &inputs(&config, &Held::Held, &selection), &correction_event("m1", "z0", "rejected", "ev-r1"))).unwrap();
    assert_eq!(written(&mut world), vec![NAME.to_owned()]);
    let state = state(&world);
    // An unknown claim takes the correction's truth.
    assert_eq!(&*state.messages["m1"].truth_status, "false");
    for person in ["a1", "a2", "a4"] {
        let belief = &state.beliefs[person]["m1"];
        assert_eq!((&*belief.belief, &*belief.source, belief.belief_strength, belief.last_contact), ("rejected", "trusted", 0.0, 12), "{person}");
        assert!(belief.correction_exposed && belief.correction_accepted && belief.last_correction_accepted);
    }
    assert!(!state.beliefs["a3"].contains_key("m1"));
    // a2 had 0.2; everyone's pressure is relieved down to 0.0, and each accepted delivery counts.
    assert_eq!([state.query_pressure["a1"], state.query_pressure["a2"], state.query_pressure["a4"]], [0.0, 0.0, 0.0]);
    assert_eq!(state.counters.correction_deliveries, 3);
    // The other correction now conflicts.
    let mut verified = correction_event("m1", "z0", "verified", "ev-r1");
    verified.event_id = text("ev-c2");
    let conflict = on(&mut world, |cell| info::correction(cell, &mut TestHost::default(), &inputs(&config, &Held::Held, &selection), &verified));
    assert_eq!(conflict, Err(Error::Outcome(Outcome::Failed, "information correction conflicts with message truth".to_owned())));
}

#[test]
fn a_correction_s_texts_come_in_the_python_rule_s_order() {
    let mut world = world();
    let (config, selection) = (config(true), selection(PLAIN));
    on(&mut world, |cell| info::rumor(cell, &mut TestHost::default(), &inputs(&config, &Held::Held, &selection), &rumor_event("ev-r1", "m1", "false", "a2"))).unwrap();
    let fail = |text: &str| Err(Error::Outcome(Outcome::Failed, text.to_owned()));
    let run = |row: &EventRow| {
        let mut world = clone_world(&world);
        on(&mut world, |cell| info::correction(cell, &mut TestHost::default(), &inputs(&config, &Held::Held, &selection), row))
    };
    assert_eq!(run(&correction_event("m1", "z0", "maybe", "ev-r1")), fail("information_correction belief is invalid"));
    assert_eq!(run(&correction_event("m1", "z0", "heard", "ev-r1")), fail("information_correction must verify or reject a claim"));
    assert_eq!(run(&correction_event("m1", "nowhere", "rejected", "ev-r1")), fail("information_correction references an unknown Zone"));
    assert_eq!(run(&correction_event("m9", "z0", "rejected", "ev-r1")), fail("information_correction references an unknown message"));
    assert_eq!(run(&correction_event("m1", "z0", "rejected", "other")), fail("information_correction must reference its rumor event"));
    assert_eq!(run(&correction_event("m1", "z0", "verified", "ev-r1")), fail("information correction conflicts with message truth"));
    assert_eq!(run(&correction_event("m1", "z0", "rejected", "ev-r1")), Ok(()));
}

// ---- Contacts -------------------------------------------------------------------------------------

fn contact_event(sender: &str, recipient: &str, belief: &str, basis: &str, minutes: V) -> EventRow {
    let payload = map(&[
        ("message_id", string("m1")),
        ("sender_agent_id", string(sender)),
        ("recipient_agent_id", string(recipient)),
        ("belief", string(belief)),
        ("contact_basis", string(basis)),
        ("contact_minutes", minutes),
    ]);
    let provenance = map(&[
        ("source_type", string("assumption")),
        ("source_id", string("ASM-INFORMATION-SPREAD-V1")),
        ("policy_version", string(PLAIN)),
        ("transmission_probability", V::Float(0.5)),
    ]);
    event("ev-k1", EventKind::Endogenous, 10, "information_contact_update", payload, Some("trusted"), Some("ev-r1"), provenance)
}

#[test]
fn a_contact_passes_a_claim_on_and_counts_it() {
    let mut world = world();
    let (config, selection) = (config(true), selection(PLAIN));
    on(&mut world, |cell| info::rumor(cell, &mut TestHost::default(), &inputs(&config, &Held::Held, &selection), &rumor_event("ev-r1", "m1", "false", "a2"))).unwrap();
    // a2 and a1 are a household (their profiles' group): the basis is family.
    let applied = on(&mut world, |cell| info::contact(cell, &mut TestHost::default(), &inputs(&config, &Held::Held, &selection), &contact_event("a2", "a1", "heard", "family", V::Int(5))));
    assert_eq!(applied, Ok(Applied::Applied));
    let state = state(&world);
    let belief = &state.beliefs["a1"]["m1"];
    assert_eq!((&*belief.belief, &*belief.source, &*belief.last_event_id, belief.belief_strength), ("heard", "a2", "ev-k1", 0.5));
    assert_eq!(state.query_pressure["a1"], 0.2);
    assert_eq!((state.counters.contact_deliveries, state.counters.correction_deliveries), (1, 0));
    // A second time, a1's belief is as strong.
    let again = on(&mut world, |cell| info::contact(cell, &mut TestHost::default(), &inputs(&config, &Held::Held, &selection), &contact_event("a2", "a1", "heard", "family", V::Int(5))));
    assert_eq!(again, Ok(Applied::Suppressed("recipient already has an equally strong belief")));
}

#[test]
fn a_contact_s_texts_come_in_the_python_rule_s_order() {
    let mut world = world();
    let (config, selection) = (config(true), selection(PLAIN));
    on(&mut world, |cell| info::rumor(cell, &mut TestHost::default(), &inputs(&config, &Held::Held, &selection), &rumor_event("ev-r1", "m1", "false", "a2"))).unwrap();
    let fail = |text: &str| Err(Error::Outcome(Outcome::Failed, text.to_owned()));
    let suppressed = |text: &'static str| Ok(Applied::Suppressed(text));
    let run = |row: &EventRow| {
        let mut world = clone_world(&world);
        on(&mut world, |cell| info::contact(cell, &mut TestHost::default(), &inputs(&config, &Held::Held, &selection), row))
    };
    let mut exogenous = contact_event("a2", "a4", "heard", "same_zone", V::Int(5));
    exogenous.kind = EventKind::Exogenous;
    assert_eq!(run(&exogenous), fail("information contact updates must be endogenous"));
    let mut late = contact_event("a2", "a4", "heard", "same_zone", V::Int(5));
    late.due_minute = 11;
    assert_eq!(run(&late), fail("information contact update must run at its due minute"));
    assert_eq!(run(&contact_event("a2", "a2", "heard", "same_zone", V::Int(5))), fail("information contact requires two different Agents"));
    assert_eq!(run(&contact_event("a2", "nobody", "heard", "same_zone", V::Int(5))), fail("information contact references unknown state"));
    assert_eq!(run(&contact_event("a4", "a2", "heard", "same_zone", V::Int(5))), suppressed("contact sender no longer knows the message"));
    assert_eq!(run(&contact_event("a2", "a4", "maybe", "same_zone", V::Int(5))), fail("information contact belief is invalid"));
    assert_eq!(run(&contact_event("a2", "a4", "verified", "same_zone", V::Int(5))), suppressed("contact sender belief changed before transmission"));
    assert_eq!(run(&contact_event("a2", "a4", "heard", "family", V::Int(5))), suppressed("sampled contact basis is no longer current"));
    // a3 is in z1, through the open edge: an adjacent zone.
    assert_eq!(run(&contact_event("a2", "a3", "heard", "adjacent_zone", V::Int(5))), Ok(Applied::Applied));
    assert_eq!(run(&contact_event("a2", "a4", "heard", "same_zone", V::Int(1))), fail("information contact duration is below its configured minimum"));
    assert_eq!(run(&contact_event("a2", "a4", "heard", "same_zone", V::Bool(true))), fail("information contact duration is below its configured minimum"));
    let mut improbable = contact_event("a2", "a4", "heard", "same_zone", V::Int(5));
    improbable.provenance = map(&[
        ("source_type", string("assumption")),
        ("source_id", string("ASM-INFORMATION-SPREAD-V1")),
        ("policy_version", string(PLAIN)),
        ("transmission_probability", V::Int(0)),
    ]);
    assert_eq!(run(&improbable), fail("information contact probability is invalid"));
    let mut other_source = contact_event("a2", "a4", "heard", "same_zone", V::Int(5));
    other_source.source_id = Some(text("OTHER"));
    assert_eq!(run(&other_source), fail("information contact provenance does not match sender state"));
    let mut observed = contact_event("a2", "a4", "heard", "same_zone", V::Int(5));
    observed.provenance = map(&[("source_type", string("observed"))]);
    assert_eq!(run(&observed), fail("information contact provenance is missing its assumption ID"));
}

#[test]
fn people_in_zones_with_only_a_closed_edge_are_not_in_contact() {
    let mut world = world();
    let (config, selection) = (config(true), selection(PLAIN));
    on(&mut world, |cell| info::rumor(cell, &mut TestHost::default(), &inputs(&config, &Held::Held, &selection), &rumor_event("ev-r1", "m1", "false", "a2"))).unwrap();
    on(&mut world, |cell| cell.with(|world| world.rows_mut().unwrap().agents.get_mut("a4").unwrap().zone_id = text("z2"))).unwrap();
    let said = on(&mut world, |cell| info::contact(cell, &mut TestHost::default(), &inputs(&config, &Held::Held, &selection), &contact_event("a2", "a4", "heard", "same_zone", V::Int(5))));
    assert_eq!(said, Ok(Applied::Suppressed("Agents are no longer in a modeled contact zone")));
}

// ---- An older state, upgraded by the host (policy E11) ---------------------------------------------

/// A host that does what the native host's upgrade does to the row: schema 2.
struct Upgrader<'a> {
    cell: &'a Shard<World>,
    calls: usize,
}

impl Host for Upgrader<'_> {
    type Error = Error;
    fn validate_candidate(&mut self, _kind: TxnKind, _stage: Stage) -> Result<(), Error> {
        Ok(())
    }
    fn validate_world(&mut self) -> Result<(), Error> {
        Ok(())
    }
    fn validate_extensions(&mut self) -> Result<(), Error> {
        Ok(())
    }
    fn contract(&mut self, _which: &Contract, input: &V) -> Result<V, Error> {
        Ok(input.clone())
    }
    fn llm(&self) -> Result<Llm, Error> {
        Ok(Llm::default())
    }
    fn prioritize_support_request(&mut self, _request_id: &str, _agent_id: &str, _observed_state_hash: &str) -> Result<(), Error> {
        Ok(())
    }
    fn consider_support_request(&mut self, _agent_id: &str, _request_id: &str) -> Result<(), Error> {
        Ok(())
    }
    fn consider_complaint_response(&mut self, _agent_id: &str, _request_id: &str) -> Result<(), Error> {
        Ok(())
    }
    fn consider_cooperation(&mut self, _initiator: &str, _participant: &str, _trigger_event_id: &str) -> Result<(), Error> {
        Ok(())
    }
    fn consider_information_reaction(&mut self, _agent_id: &str, _participant: &str, _message_id: &str, _trigger_event_id: &str) -> Result<(), Error> {
        Ok(())
    }
    fn capture_metric(&mut self, _sample: &MetricSample, _minute: i64) -> Result<(), Error> {
        Ok(())
    }
    fn decision_sink(&self) -> Result<DecisionSink, Error> {
        Ok(DecisionSink::Silent)
    }
    fn decision(&mut self, _decision: Decision) -> Result<(), Error> {
        Ok(())
    }
    fn trace_level(&self) -> Result<TraceLevel, Error> {
        Ok(TraceLevel::Off)
    }
    fn trace(&mut self, _document: Trace) -> Result<(), Error> {
        Ok(())
    }
    fn phase(&mut self, _phase: Phase, _before: bool) -> Result<(), Error> {
        Ok(())
    }
    fn space_refreshed(&mut self) -> Result<(), Error> {
        Ok(())
    }
    fn canonicalized(&mut self, _kind: TxnKind, _before: &Snapshot) -> Result<(), Error> {
        Ok(())
    }
    /// The world is not borrowed now (rule R6): this borrows it to write.
    fn upgrade_information_state(&mut self) -> Result<(), Error> {
        self.calls += 1;
        self.cell.with(|world| world.row_mut::<InformationRow>(NAME).map(|mut row| row.schema_version = 2))?
    }
}

#[test]
fn a_state_of_another_schema_is_upgraded_by_the_host_before_the_event_s_checks() {
    let mut world = world();
    on(&mut world, |cell| cell.with(|world| world.row_mut::<InformationRow>(NAME).unwrap().schema_version = 1)).unwrap();
    written(&mut world);
    let (config, selection) = (config(true), selection(PLAIN));
    // A host without Python refuses, and nothing is written.
    let refused = on(&mut world, |cell| info::rumor(cell, &mut TestHost::default(), &inputs(&config, &Held::Held, &selection), &rumor_event("ev-r1", "m1", "true", "a1")));
    assert_eq!(refused, Err(unsupported(OLDER_STATE)));
    assert!(written(&mut world).is_empty());
    // The model's checks come first: a model that is off is not upgraded.
    let cell = Shard::new(world);
    let mut host = Upgrader { cell: &cell, calls: 0 };
    let off = config_off();
    let said = info::rumor(&cell, &mut host, &inputs(&off, &Held::Held, &selection), &rumor_event("ev-r1", "m1", "true", "a1"));
    assert_eq!((said, host.calls), (Err(Error::Outcome(Outcome::Failed, NOT_ENABLED.to_owned())), 0));
    // Upgraded, then the event's checks: a payload that does not fit fails after the upgrade.
    let mut bad = rumor_event("ev-r1", "m1", "true", "a1");
    bad.payload = map(&[]);
    let said = info::rumor(&cell, &mut host, &inputs(&config, &Held::Held, &selection), &bad);
    assert_eq!((said, host.calls), (Err(Error::Outcome(Outcome::Failed, "information_rumor payload fields do not match".to_owned())), 1));
    assert_eq!(cell.with(|world| world.row::<InformationRow>(NAME).unwrap().schema_version).unwrap(), 2);
    // A state of schema 2 is not upgraded again.
    let said = info::rumor(&cell, &mut host, &inputs(&config, &Held::Held, &selection), &rumor_event("ev-r1", "m1", "true", "a1"));
    assert_eq!((said, host.calls), (Ok(()), 1));
}

fn config_off() -> InformationSpread {
    config(false)
}

#[test]
fn the_selection_s_errors_are_said_where_the_rule_reads_it() {
    let refused: Kept<Selection> = Err(Fault::new(FaultClass::Refused, "x"));
    assert_eq!(info::selected_information(&refused), Err(unsupported("not a selection")));
    let raised: Kept<Selection> = Err(Fault::new(FaultClass::Value, "bad"));
    assert_eq!(info::selected_information(&raised), Err(Error::corrupt("model_configs", info::SELECTION_RAISED)));
    let other: Kept<Selection> = Ok(Arc::new(Selection::new(vec![(text("behavior"), text("b"))])));
    assert_eq!(info::selected_information(&other), Err(Error::Key("information".to_owned())));
    // A rumor reads it only after its own checks, under the default model.
    let mut world = world();
    let config = config(true);
    let unknown = on(&mut world, |cell| info::rumor(cell, &mut TestHost::default(), &inputs(&config, &Held::Held, &raised), &rumor_event("ev-r1", "m1", "true", "nobody")));
    assert_eq!(unknown, Err(Error::Outcome(Outcome::Failed, "information_rumor references an unknown Agent".to_owned())));
    let corrupt = on(&mut world, |cell| info::rumor(cell, &mut TestHost::default(), &inputs(&config, &Held::Held, &raised), &rumor_event("ev-r1", "m1", "true", "a1")));
    assert_eq!(corrupt, Err(Error::corrupt("model_configs", info::SELECTION_RAISED)));
    assert!(written(&mut world).is_empty());
}

#[test]
fn a_correction_s_acceptance_and_the_numbers_of_a_payload() {
    let mut world = world();
    let config = config(true);
    // trust 0.75 and the source's 0.9: min(0.95, 0.6 + 0.4 * 0.25 + 0.2 * 0.4) = 0.78.
    assert_eq!(info::acceptance_probability(&config, 0.75, 0.9), Ok(0.6 + 2.0 * 0.2 * (0.75 - 0.5) + 2.0 * 0.1 * (0.9 - 0.5)));
    assert_eq!(info::acceptance_probability(&config, 1.0, 1.0), Ok(0.6 + 2.0 * 0.2 * 0.5 + 2.0 * 0.1 * 0.5));
    let accepted = on(&mut world, |cell| cell.with(|world| info::accepts(world, &config, "correction-acceptance-v1", 0.75, "trusted", 12, &["trusted", "a1", "m1"]))).unwrap();
    assert_eq!(accepted, Ok(EXPECTED_ACCEPTANCE));
    let draws = Draws::of_seed("7").unwrap();
    assert_eq!(draws.random("correction-acceptance-v1", 12, &["trusted", "a1", "m1"]), CORRECTION_DRAW);
    assert_eq!(draws.random("rumor-belief-acceptance-v1", 10, &["a1", "m1"]), RUMOR_DRAW);
    assert_eq!(info::int_not_bool(&V::Bool(true)), Ok(None));
    assert_eq!(info::int_not_bool(&V::Big("1".repeat(30).into_boxed_str())), Err(unsupported("an int is too large")));
    assert_eq!(info::real_number(&V::Int(1 << 54)), Err(unsupported("a number does not fit a float")));
    assert_eq!(info::real_number(&V::Int(1)), Ok(Some(1.0)));
}

/// `CounterRNG(7).random("correction-acceptance-v1", 12, "trusted", "a1", "m1")` of `core/rng.py`,
/// not below the chance 0.78: the correction is not accepted.
const CORRECTION_DRAW: f64 = 0.8073038381136849;
const EXPECTED_ACCEPTANCE: bool = false;

// ---- Social interactions ------------------------------------------------------------------------------

const SOCIAL_SOURCE: &str = "ASM-EVENT-TRIGGERED-SOCIAL-INTERACTIONS-V1";

fn social_event(payload: V) -> EventRow {
    let provenance = map(&[("source_id", string(SOCIAL_SOURCE)), ("source_type", string("assumption")), ("policy_version", string("social-v1"))]);
    event("ev-s1", EventKind::Endogenous, 10, "social_interaction", payload, Some("social-v1"), Some("ev-k0"), provenance)
}

fn action(kind: &str, actor: &str, participant: &str, act: &str, topic: &str) -> V {
    map(&[
        ("interaction_type", string(kind)),
        ("agent_id", string(actor)),
        ("participant_agent_id", string(participant)),
        ("zone_id", string("z0")),
        ("topic_id", string(topic)),
        ("action", string(act)),
        ("reason_short", string("autonomous_need_choice")),
        ("interaction_duration_minutes", V::Int(10)),
    ])
}

fn person(world: &World, id: &str) -> AgentRow {
    world.rows().unwrap().agents.get(id).unwrap().clone()
}

fn apply(world: &mut World, row: &EventRow, inputs: &SocialInputs) -> Result<social::Said, Error> {
    on(world, |cell| cell.with(|world| social::apply_event(world, row, inputs))).unwrap()
}

#[test]
fn a_social_action_relieves_the_participant_and_tires_a_helper() {
    let mut world = world();
    // No body state: the relief of affect 0.5 and 0.0 at the default transfer, 0.01.
    let said = apply(&mut world, &social_event(action("reassurance_exchange", "a1", "a2", "offer_reassurance", "emotional_check_in")), &SocialInputs::default());
    assert_eq!(said, Ok(None));
    let (actor, participant) = (person(&world, "a1"), person(&world, "a2"));
    assert_eq!((participant.stress, participant.trust), (0.49, 0.755));
    assert_eq!((actor.needs.fatigue, actor.trust), (0.25, 0.75));
    let said = apply(&mut world, &social_event(action("family_care", "a2", "a1", "provide_family_support", "family_care")), &SocialInputs::default());
    assert_eq!(said, Ok(None));
    let actor = person(&world, "a2");
    assert_eq!((actor.needs.fatigue, actor.trust), (0.252, 0.7575));
    // a4 is in no household with a1.
    let said = apply(&mut world, &social_event(action("family_care", "a4", "a1", "provide_family_support", "family_care")), &SocialInputs::default());
    assert_eq!(said, Ok(Some((Outcome::Suppressed, "family support target is no longer in the same household"))));
    // Equal needs: a1 does not need more than a4 by the margin.
    let said = apply(&mut world, &social_event(action("voluntary_support", "a4", "a1", "offer_practical_help", "mutual_aid")), &SocialInputs::default());
    assert_eq!(said, Ok(Some((Outcome::Suppressed, "the selected person no longer has a higher immediate need"))));
}

#[test]
fn a_social_action_s_texts_come_in_the_python_rule_s_order() {
    let world = world();
    let run = |row: &EventRow| apply(&mut clone_world(&world), row, &SocialInputs::default());
    let good = action("reassurance_exchange", "a1", "a2", "offer_reassurance", "emotional_check_in");
    let with = |key: &str, value: V| {
        let V::Map(mut payload) = good.clone() else { unreachable!() };
        payload.insert(text(key), value);
        social_event(V::Map(payload))
    };
    let failed = |text: &'static str| Ok(Some((Outcome::Failed, text)));
    let mut exogenous = social_event(good.clone());
    exogenous.kind = EventKind::Exogenous;
    assert_eq!(run(&exogenous), failed("typed social interaction provenance or time is invalid"));
    assert_eq!(run(&with("reason_short", V::Null)), failed("typed social interaction requires a reason code"));
    assert_eq!(run(&with("agent_id", V::Int(1))), failed("typed social interaction references an unknown Agent"));
    assert_eq!(run(&with("extra", V::Int(1))), failed("autonomous social action payload fields do not match"));
    assert_eq!(run(&with("participant_agent_id", string("a3"))), Ok(Some((Outcome::Suppressed, "social action participants are no longer co-located"))));
    assert_eq!(run(&with("participant_agent_id", string("nobody"))), Ok(Some((Outcome::Suppressed, "social action participants are no longer co-located"))));
    assert_eq!(run(&with("agent_id", string("nobody"))), failed("typed social interaction references an unknown Agent"));
    assert_eq!(run(&with("topic_id", string("other"))), failed("social action does not match its typed topic"));
    assert_eq!(run(&with("reason_short", string("other"))), failed("social action reason code is not allowed"));
    assert_eq!(run(&with("interaction_duration_minutes", V::Int(1441))), failed("social action duration must be within 1..1440 minutes"));
    assert_eq!(run(&with("conversation_state_context", map(&[]))), failed("social dialogue state context is invalid"));
    assert_eq!(run(&with("interaction_type", string("dancing"))), failed("typed social interaction type is not allowed"));
    assert_eq!(run(&with("interaction_type", V::List(vec![]))), Err(unsupported("another type of interaction is for the Python rule to judge")));
}

#[test]
fn an_untyped_interaction_and_a_complaint_response_change_the_actor() {
    let mut world = world();
    let untyped = social_event(map(&[("agent_id", string("a1")), ("request_id", string("r1")), ("action", string("offer_reassurance"))]));
    assert_eq!(apply(&mut world, &untyped, &SocialInputs::default()), Ok(None));
    assert_eq!((person(&world, "a1").stress, person(&world, "a1").trust), (0.47, 0.76));
    let wrong = social_event(map(&[("agent_id", string("a2")), ("request_id", string("r1")), ("action", string("offer_reassurance"))]));
    assert_eq!(apply(&mut world, &wrong, &SocialInputs::default()), Ok(Some((Outcome::Failed, "social interaction references an unknown request or Agent"))));
    let complaint = |action: &str, request: V| {
        let mut row = social_event(map(&[
            ("interaction_type", string("complaint_response")),
            ("agent_id", string("a1")),
            ("request_id", request),
            ("zone_id", string("z0")),
            ("stress_threshold", V::Float(0.4)),
            ("complaint_wait_minutes", V::Int(5)),
            ("action", string(action)),
            ("reason_short", string("long_wait")),
        ]));
        row.causation_id = Some(text("r1"));
        row
    };
    assert_eq!(apply(&mut world, &complaint("acknowledge_complaint", string("r1")), &SocialInputs::default()), Ok(None));
    assert_eq!((person(&world, "a1").stress, person(&world, "a1").trust), (0.44999999999999996, 0.77));
    assert_eq!(
        apply(&mut world, &complaint("dance", string("r1")), &SocialInputs::default()),
        Ok(Some((Outcome::Failed, "complaint response action is not allowed")))
    );
    // A mapping as a key has no hash, as the frozen payload's value has none.
    assert_eq!(
        apply(&mut world, &complaint("acknowledge_complaint", map(&[])), &SocialInputs::default()),
        Err(Error::Type("unhashable type: 'mappingproxy'".to_owned()))
    );
}

fn contact_record(sender: &str, recipient: &str, minute: i64) -> Logged {
    Logged { status: EventStatus::Applied, realized_minute: minute, event: contact_event(sender, recipient, "heard", "family", V::Int(5)) }
}

fn answering(kind: &str, extra: &[(&str, V)]) -> EventRow {
    let mut fields = vec![
        ("interaction_type", string(kind)),
        ("agent_id", string("a1")),
        ("participant_agent_id", string("a2")),
        ("zone_id", string("z0")),
        ("trigger_event_id", string("ev-k1")),
        ("reason_short", string("rule_fallback")),
    ];
    fields.extend(extra.iter().cloned());
    let mut row = social_event(map(&fields));
    row.causation_id = Some(text("ev-k1"));
    row
}

#[test]
fn a_cooperation_answers_an_applied_contact_a_minute_ago() {
    let mut world = world();
    let cooperation = answering(
        "cooperation",
        &[("stress_threshold", V::Float(0.4)), ("relationship", string("same_group")), ("action", string("offer_mutual_support"))],
    );
    let found = |record: Option<Logged>| SocialInputs { trigger: Some((text("ev-k1"), record)), information: None };
    // Not looked up: the glue's bug, not the rule's text.
    assert!(matches!(apply(&mut world, &cooperation, &SocialInputs::default()), Err(Error::Runtime(_))));
    assert_eq!(apply(&mut world, &cooperation, &found(None)), Ok(Some((Outcome::Failed, "cooperation does not match an applied local contact"))));
    assert_eq!(
        apply(&mut world, &cooperation, &found(Some(contact_record("a1", "a2", 8)))),
        Ok(Some((Outcome::Failed, "cooperation does not match an applied local contact")))
    );
    assert_eq!(apply(&mut world, &cooperation, &found(Some(contact_record("a1", "a2", 9)))), Ok(None));
    for id in ["a1", "a2"] {
        assert_eq!((person(&world, id).stress, person(&world, id).trust), (0.48, 0.76));
    }
}

#[test]
fn an_information_reaction_raises_the_query_pressure() {
    let mut world = world();
    let (config, selection) = (config(true), selection(PLAIN));
    on(&mut world, |cell| info::rumor(cell, &mut TestHost::default(), &inputs(&config, &Held::Held, &selection), &rumor_event("ev-r1", "m1", "false", "a1"))).unwrap();
    written(&mut world);
    let reaction = answering("information_reaction", &[("message_id", string("m1")), ("action", string("seek_claim_verification"))]);
    let kept: Kept<InformationSpread> = Ok(Arc::new(config.clone()));
    let inputs = |held: Held| SocialInputs { trigger: Some((text("ev-k1"), Some(contact_record("a2", "a1", 9)))), information: Some((kept.clone(), held)) };
    assert_eq!(apply(&mut world, &reaction, &inputs(Held::Absent)), Ok(Some((Outcome::Suppressed, "information state is no longer enabled"))));
    assert_eq!(apply(&mut world, &reaction, &inputs(Held::Held)), Ok(None));
    assert_eq!(written(&mut world), vec![NAME.to_owned()]);
    // 0.2 from the rumor, 0.2 more.
    assert_eq!(state_pressure(&world, "a1"), 0.4);
    // The config's pressure: a configs without the key is refused.
    let mut absent = config.clone();
    absent.present = false;
    assert_eq!(social::pressure_gain(&Ok(Arc::new(absent))), Err(unsupported("the config is for the Python rule to read")));
    assert_eq!(social::pressure_gain(&kept), Ok(0.2));
}

#[test]
fn the_inputs_a_social_event_needs_are_named_by_its_payload() {
    let cooperation = answering("cooperation", &[]);
    assert_eq!(social::interaction_type(&cooperation), Some("cooperation"));
    assert_eq!(social::trigger_of(&cooperation).map(|id| &**id), Some("ev-k1"));
    let action = social_event(action("reassurance_exchange", "a1", "a2", "offer_reassurance", "emotional_check_in"));
    assert_eq!(social::trigger_of(&action), None);
    let _ = (MessageRow::NAMES, BeliefRow::NAMES);
}
