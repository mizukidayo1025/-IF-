//! The view of lane P8 a1 (`rules::world_view`, `person_context`, `zone_environment`) over a world
//! built here, with no Python.
//!
//! The world is the one `view_world.py` built in Python (lane notes of P8 a1): the expected hashes
//! and values are what the build before this lane gave for it (`view_for_proposals` at `04fb2fb`),
//! so the core writes the audit hash's bytes as the PyO3 kernel did.

use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::{json, Value};
use sheltersim_core::config::{Fault, Selection};
use sheltersim_core::entities::{
    Activity, AgentRow, AnyTable, FacilityRow, Mobility, NeedRow, ProfileRow, RequestRow, RequestStatus, Table, TaskRow,
    ZoneRow,
};
use sheltersim_core::error::Error;
use sheltersim_core::row::{IdMap, ValueRow};
use sheltersim_core::rules::person_context::{self, PersonFields};
use sheltersim_core::rules::world_view::{self as view, Inputs, ViewCache, ViewData};
use sheltersim_core::rules::zone_environment::ZoneInputs;
use sheltersim_core::state::information::{BeliefRow, CountersRow, InformationRow, MessageRow};
use sheltersim_core::state::psychological_distress::{AgentRecord, DistressRow};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, World, MODEL_CONFIGS};

/// What `view_for_proposals` of the build before lane P8 a1 gave for the two worlds.
const FULL_HASH: &str = "87ae1dbd2306b2aae8ff53ddc91971d1d2a9a4a30b80cc27e02d40b7f9da7da8";
const PLAIN_HASH: &str = "e5fc189653915bcd1b9501b5c5c575ddacbccf40e085309cfe4d9b7b460b0099";

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn v(value: Value) -> V {
    match value {
        Value::Null => V::Null,
        Value::Bool(flag) => V::Bool(flag),
        Value::Number(number) => match number.as_i64() {
            Some(int) if !number.to_string().contains(['.', 'e', 'E']) => V::Int(int),
            _ => V::Float(number.as_f64().expect("a float")),
        },
        Value::String(text) => V::Str(Arc::from(text)),
        Value::Array(items) => V::List(items.into_iter().map(v).collect()),
        Value::Object(map) => V::Map(map.into_iter().map(|(key, item)| (Arc::from(key), v(item))).collect::<IndexMap<_, _>>()),
    }
}

fn put<R: Clone>(table: &mut Table<R>, id: &str, row: R) {
    let slot = table.reserve_slot();
    table.fill_slot(slot, row);
    assert_eq!(table.bind(text(id), slot), None);
}

fn needs(toilet_urgency: f64, hunger: f64, fatigue: f64, hydration_need: f64) -> NeedRow {
    NeedRow { toilet_urgency, hunger, fatigue, hydration_need }
}

fn agent(id: &str, zone: &str, group: Option<&str>, stress: f64, needs: NeedRow, mobility: Mobility) -> AgentRow {
    AgentRow {
        profile: ProfileRow {
            id: text(id),
            group_id: group.map(text),
            initial_zone_id: text(zone),
            mobility,
            support_need: 0.25,
            stress_sensitivity: 1.5,
            rng_key: text(&format!("key-{id}")),
            initial_needs: NeedRow { toilet_urgency: 0.0, hunger: 0.0, fatigue: 0.0, hydration_need: 0.0 },
            provenance: V::Map(IndexMap::new()),
        },
        zone_id: text(zone),
        activity: Activity::Idle,
        target_id: None,
        needs,
        stress,
        trust: 0.5,
        next_decision_at: 0,
        support_request_id: None,
        support_cooldown_until: 0,
    }
}

fn facility(id: &str, kind: &str, zone: &str, active: &[&str], queue: &[&str]) -> FacilityRow {
    FacilityRow {
        id: text(id),
        kind: text(kind),
        zone_id: text(zone),
        capacity: 2,
        working_units: 2,
        active_agent_ids: active.iter().map(|id| text(id)).collect(),
        queue_agent_ids: queue.iter().map(|id| text(id)).collect(),
        resource_active_agent_ids: Vec::new(),
        resource_queue_agent_ids: Vec::new(),
    }
}

fn zone(id: &str, name: &str, capacity: Option<i64>) -> ZoneRow {
    ZoneRow { id: text(id), name: text(name), capacity, display_x: None, display_y: None }
}

fn add_value(world: &mut World, name: &str, place: Place, value: Value) -> usize {
    let index = world.slots.add(text(name), place).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(ValueRow(v(value))));
    index
}

fn information() -> InformationRow {
    let mut messages: IdMap<MessageRow> = IdMap::default();
    messages.insert(
        text("m1"),
        MessageRow {
            id: text("m1"),
            truth_status: text("false"),
            source_id: text("rumor"),
            zone_id: text("hall"),
            time: 3,
            origin_event_id: text("e-m1"),
            origin_agent_id: text("A1"),
        },
    );
    let mut believed: IdMap<BeliefRow> = IdMap::default();
    believed.insert(
        text("m1"),
        BeliefRow {
            message_id: text("m1"),
            belief: text("heard"),
            source: text("A1"),
            last_contact: 4,
            last_event_id: text("e-b1"),
            exposed: true,
            belief_strength: 0.75,
            share_propensity: 0.5,
            correction_exposed: false,
            correction_accepted: false,
            ever_misbelieved: true,
            ever_forgotten: false,
            last_correction_accepted: false,
        },
    );
    let mut beliefs: IdMap<IdMap<BeliefRow>> = IdMap::default();
    beliefs.insert(text("A1"), IdMap::default());
    beliefs.insert(text("A2"), believed);
    let mut query_pressure: IdMap<f64> = IdMap::default();
    query_pressure.insert(text("A1"), 0.125);
    query_pressure.insert(text("A2"), 0.5);
    InformationRow {
        schema_version: 2,
        messages,
        beliefs,
        contacts: IdMap::default(),
        query_pressure,
        support_requests: Vec::new(),
        counters: CountersRow { contact_attempts: 0, contact_deliveries: 0, correction_deliveries: 0, support_requests: 0 },
    }
}

/// The world of `view_world.py`: `full` with information, distress, the presence ledger and a
/// person context; without them, as its `plain` variant.
fn world(full: bool) -> World {
    let mut world = World::default();
    {
        let scalars = world.scalars.get_mut();
        scalars.minute_index = Fit::Ok(30);
        scalars.schema_version = Fit::Ok(6);
    }
    {
        let rows = world.entities.get_mut();
        let mut a2 = agent("A2", "hall", Some("g1"), 0.5, needs(0.1, 0.2, 0.3, 0.4), Mobility::Independent);
        a2.support_request_id = Some(text("r1"));
        a2.support_cooldown_until = 40;
        put(&mut rows.agents, "A2", a2);
        let mut a1 = agent("A1", "room", None, 0.0, needs(0.0, 0.5, 1.0, 0.0), Mobility::Wheelchair);
        a1.activity = Activity::Queued;
        a1.target_id = Some(text("f-toilet"));
        a1.next_decision_at = 31;
        put(&mut rows.agents, "A1", a1);
        put(&mut rows.zones, "room", zone("room", "Room", Some(5)));
        put(&mut rows.zones, "hall", zone("hall", "Hall", None));
        put(&mut rows.facilities, "f-water", facility("f-water", "Water_Station", "hall", &["A2"], &[]));
        put(&mut rows.facilities, "f-toilet", facility("f-toilet", "toilet", "room", &[], &["A1"]));
        put(
            &mut rows.support_requests,
            "r1",
            RequestRow {
                id: text("r1"),
                agent_id: text("A2"),
                requested_minute: 20,
                timeout_minute: 90,
                status: RequestStatus::Pending,
                assigned_staff_id: None,
                task_id: None,
                resolved_minute: None,
                cooldown_until: 0,
                last_assignment_attempt_minute: None,
                last_assignment_wait_reason_code: None,
            },
        );
        put(
            &mut rows.active_tasks,
            "t1",
            TaskRow {
                id: text("t1"),
                kind: text("toilet_queue"),
                agent_id: Some(text("A1")),
                resource_id: Some(text("f-toilet")),
                start_minute: 25,
                due_minute: 60,
                staff_id: None,
            },
        );
    }
    let mut configs = json!({
        "resource_service": {"a": 1.5, "b": [1, 2.5, "x"]},
        "information_spread": {"query_pressure_per_claim": 0.25},
        "policy_registry": {"schema_version": 2, "psychology": "life-state-feedback-v1", "zone_rest": "zone-rest-limiting-v1"},
        "zone_environment": {
            "policy_version": "zone-environment-v2",
            "temperature_stress_per_degree_minute": 0.001,
            "baseline_by_zone": {"hall": {"temperature_c": 30.0}}
        }
    });
    if full {
        configs["population_context"] =
            json!({"people": {"A1": {"roles": ["parent"], "usual_wake_minute": 420, "care_for_ids": ["A2"]}}});
    }
    add_value(&mut world, MODEL_CONFIGS, Place::Field, configs);
    if full {
        let index = world.slots.add(text("information_spread_v1"), Place::Namespace).unwrap();
        world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(Box::new(information()));
        let mut agents: IdMap<AgentRecord> = IdMap::default();
        agents.insert(
            text("A2"),
            AgentRecord {
                baseline_distress: 0.125,
                long_term_distress: 0.25,
                daily_acute_sum: 0.0,
                daily_high_distress_minutes: 0,
                daily_observed_minutes: 0,
                completed_days: 0,
            },
        );
        let index = world.slots.add(text("psychological_distress_v1"), Place::Namespace).unwrap();
        world.slots.at(index).unwrap().try_borrow_mut().unwrap().row =
            Some(Box::new(DistressRow { schema_version: 1, last_updated_minute: -1, agents }));
        add_value(&mut world, "population_presence_v1", Place::Namespace, json!({"people": {"A1": {"household_id": "h1"}}}));
    }
    add_value(
        &mut world,
        "infrastructure_v1",
        Place::Value,
        json!({
            "environmental_stressors": {
                "s2": {"active": true, "zone_id": "hall", "stress_per_minute": 0.1, "start_event_id": "e-s2", "source_id": "src-2"},
                "s1": {"active": true, "zone_id": "hall", "stress_per_minute": 0.2, "start_event_id": "e-s1", "source_id": "src-1"},
                "s3": {"active": false, "zone_id": "room", "stress_per_minute": 9.0, "start_event_id": "e-s3", "source_id": null}
            },
            "outages": {"o1": {"active": true, "affected_facility_ids": ["f-toilet"], "start_event_id": "e-o1"}}
        }),
    );
    add_value(
        &mut world,
        "operational_events_v1",
        Place::Value,
        json!({"water_outages": {"w1": {"outage_event_id": "e-w1", "active": true, "target_kind": "zone", "target_id": "hall"}}}),
    );
    add_value(
        &mut world,
        "zone_environment_v1",
        Place::Value,
        json!({"schema_version": 1, "modifiers": {"z1": {
            "zone_id": "room", "op": "set_lighting", "parameters": {"level": 0.25}, "start_minute": 0,
            "start_event_id": "e-z1", "source_id": "src-z", "assumption_ids": ["asm-z"]}}}),
    );
    world
}

fn life_state() -> Result<Arc<Selection>, Fault> {
    Ok(Arc::new(Selection::new(vec![(text("psychology"), text("life-state-feedback-v1"))])))
}

fn build(world: &World, cache: &mut ViewCache) -> Result<ViewData, Error> {
    let selection = life_state();
    let inputs = Inputs { selection: &selection, configs: None };
    Ok(view::build(world, &inputs, cache)?.finish(false))
}

fn texts(items: &[Arc<str>]) -> Vec<&str> {
    items.iter().map(|item| &**item).collect()
}

fn unsupported(reason: &str) -> Error {
    Error::Unsupported(reason.to_owned().into())
}

#[test]
fn the_view_of_a_small_world_has_the_values_and_hash_of_the_kernel_before() {
    let world = world(true);
    let data = build(&world, &mut ViewCache::default()).unwrap();
    assert_eq!(view::state_hash(&data, &world, None).unwrap(), FULL_HASH);
    assert!(data.autonomy_enabled && data.has_population_context && data.resource_service_enabled && !data.partial);
    assert_eq!(data.minute_index, 30);
    // People and facilities in ID order; each person's zone is its index.
    assert_eq!(data.agents.iter().map(|agent| &*agent.id).collect::<Vec<_>>(), ["A1", "A2"]);
    assert_eq!(data.zones.iter().map(|zone| &*zone.id).collect::<Vec<_>>(), ["hall", "room"]);
    let [a1, a2] = [&data.agents[0], &data.agents[1]];
    assert_eq!((a1.zone, a2.zone), (1, 0));
    // The household of the ledger over the profile; the person context of the configs.
    assert_eq!((a1.group_id.as_deref(), a2.group_id.as_deref()), (Some("h1"), Some("g1")));
    assert_eq!(texts(&a1.person.roles), ["parent"]);
    assert_eq!(texts(&a1.person.care_for_ids), ["A2"]);
    assert_eq!(a1.person.usual_wake_minute, Some(420));
    assert!(!a2.person.personal_context_present);
    // The queue, the request and its cooldown at minute 30.
    assert_eq!((a1.queue_task_id.as_deref(), a1.queue_kind.as_deref(), a1.queue_wait_minutes), (Some("t1"), Some("toilet_queue"), 5));
    assert_eq!((a2.support_request_status, a2.support_wait_minutes, a2.support_cooldown_remaining_minutes), (Some(RequestStatus::Pending), 10, 10));
    // Information, misinformation and distress.
    assert_eq!((a1.information_query_pressure, a2.information_query_pressure), (0.125, 0.5));
    assert_eq!((a1.misinformation_belief_pressure, a2.misinformation_belief_pressure), (0.0, 0.5));
    let source = &a2.misinformation_effect_sources[0];
    assert_eq!((&*source.message_id, &*source.origin_event_id, &*source.last_event_id, &*source.source_id, source.time), ("m1", "e-m1", "e-b1", "rumor", 3));
    assert_eq!((a1.chronic_distress, a2.chronic_distress), (0.0, 0.25));
    assert_eq!(a1.body, view::NO_BODY);
    // The hall: two stressors summed by fsum, and 30 °C four degrees above the comfort band.
    let hall = &data.zones[0];
    assert_eq!(hall.environmental_stress_per_minute.to_bits(), 0.30400000000000005_f64.to_bits());
    assert_eq!(texts(&hall.environmental_stressor_event_ids), ["e-s1", "e-s2"]);
    assert_eq!(hall.environmental_assumption_source_id.as_deref(), Some("ASM-H19-ZONE-ENVIRONMENT-V2"));
    assert_eq!((hall.temperature_c, hall.rest_efficiency_multiplier, hall.sanitation_level), (30.0, 0.9, 1.0));
    // The room: the lighting modifier, with the limiting rest formula.
    let room = &data.zones[1];
    assert_eq!((room.lighting_level, room.rest_efficiency_multiplier, room.environmental_stress_per_minute), (0.25, 0.95, 0.0));
    assert_eq!(texts(&room.condition_modifier_event_ids), ["e-z1"]);
    assert_eq!(texts(&room.condition_assumption_ids), ["ASM-H19-ZONE-ENVIRONMENT-V2", "ASM-ZONE-REST-LIMITING-V1", "src-z"]);
    assert_eq!(room.ventilation_hygiene_growth_per_minute.to_bits(), 3.0000000000000008e-05_f64.to_bits());
    // The water outage of the hall reaches the water station (its kind folded), the outage the toilet.
    assert_eq!(texts(&data.facilities[0].facility_event_context_ids), ["e-o1"]);
    assert_eq!(texts(&data.facilities[1].facility_event_context_ids), ["e-w1"]);
    // The document the hash is of starts with the life context, keys sorted.
    let document = String::from_utf8(view::document(&data, &world, None).unwrap()).unwrap();
    assert!(document.starts_with(r#"{"agent_life_context":[{"agent_id":"A1","care_for_ids":["A2"],"caregiver_ids":[],"discontent":0.0,"#));
    assert!(document.ends_with(r#""minute_index":30,"resource_service_config":{"a":1.5,"b":[1,2.5,"x"]}}"#));
    assert!(document.contains(r#""information_spread_config":{"query_pressure_per_claim":0.25}"#));
}

#[test]
fn without_information_and_context_the_hash_is_of_the_shorter_document() {
    let world = world(false);
    let data = build(&world, &mut ViewCache::default()).unwrap();
    assert_eq!(view::state_hash(&data, &world, None).unwrap(), PLAIN_HASH);
    assert!(!data.autonomy_enabled && !data.has_population_context);
    let document = String::from_utf8(view::document(&data, &world, None).unwrap()).unwrap();
    assert!(document.starts_with(r#"{"agents":[{"activity":"queued","group_id":null,"id":"A1","mobility":"wheelchair","needs":{"#));
    assert!(!document.contains("information"));
    // The extensions' flag alone makes the view autonomous.
    let selection = life_state();
    let inputs = Inputs { selection: &selection, configs: None };
    assert!(view::build(&world, &inputs, &mut ViewCache::default()).unwrap().finish(true).autonomy_enabled);
}

#[test]
fn configs_given_past_the_slot_are_read_instead_of_the_row() {
    let world = world(false);
    let mut given = world.row::<ValueRow>(MODEL_CONFIGS).unwrap().0.clone();
    let V::Map(map) = &mut given else { panic!("a map") };
    map.insert(text("resource_service"), v(json!({"a": 2.5})));
    let selection = life_state();
    let inputs = Inputs { selection: &selection, configs: Some(&given) };
    let data = view::build(&world, &inputs, &mut ViewCache::default()).unwrap().finish(false);
    let document = String::from_utf8(view::document(&data, &world, Some(&given)).unwrap()).unwrap();
    assert!(document.ends_with(r#""resource_service_config":{"a":2.5}}"#));
    assert_ne!(view::state_hash(&data, &world, Some(&given)).unwrap(), PLAIN_HASH);
    assert_eq!(view::state_hash(&data, &world, None).unwrap(), PLAIN_HASH);
    let not_a_map = V::List(Vec::new());
    let inputs = Inputs { selection: &selection, configs: Some(&not_a_map) };
    assert_eq!(view::build(&world, &inputs, &mut ViewCache::default()).err(), Some(unsupported("the configs are not a plain dict")));
}

#[test]
fn a_refreshed_view_reads_what_the_update_phase_writes_and_keeps_the_rest() {
    let mut world = world(true);
    let prior = build(&world, &mut ViewCache::default()).unwrap();
    {
        let rows = world.entities.get_mut();
        let a1 = rows.agents.get_mut("A1").unwrap();
        a1.stress = 0.75;
        a1.needs.hunger = 0.625;
        a1.zone_id = text("hall");
    }
    {
        let slot = world.slots.get("psychological_distress_v1").unwrap();
        let mut slot = slot.try_borrow_mut().unwrap();
        let row = slot.row_written::<DistressRow>().unwrap();
        row.agents.get_mut("A2").unwrap().long_term_distress = 0.5;
    }
    let refreshed = view::refresh(&prior, &world).unwrap();
    let a1 = &refreshed.agents[0];
    // Needs, stress, trust, distress and the body are read again; the zone is the prior view's.
    assert_eq!((a1.stress, a1.needs.hunger, a1.zone), (0.75, 0.625, 1));
    assert_eq!(refreshed.agents[1].chronic_distress, 0.5);
    assert!(Arc::ptr_eq(&refreshed.zones[0], &prior.zones[0]));
    assert_ne!(view::state_hash(&refreshed, &world, None).unwrap(), FULL_HASH);
    assert_eq!(view::state_hash(&prior, &world, None).unwrap(), FULL_HASH);
    // A person of the prior view who left is refused.
    world.entities.get_mut().agents = Table::default();
    assert_eq!(view::refresh(&prior, &world).err(), Some(unsupported("a person of the prior view has left")));
}

#[test]
fn what_is_kept_follows_the_stamps_of_what_it_was_read_from() {
    let world = world(true);
    let mut cache = ViewCache::default();
    let first = build(&world, &mut cache).unwrap();
    assert_eq!(cache.kept(), (1, 2));
    let again = build(&world, &mut cache).unwrap();
    assert_eq!(view::state_hash(&again, &world, None).unwrap(), view::state_hash(&first, &world, None).unwrap());
    // A write to the modifiers gives their slot a new stamp: the zones are read again.
    {
        let slot = world.slots.get("zone_environment_v1").unwrap();
        let mut slot = slot.try_borrow_mut().unwrap();
        let row = slot.row_written::<ValueRow>().unwrap();
        let V::Map(state) = &mut row.0 else { panic!("a map") };
        let V::Map(modifiers) = state.get_mut("modifiers").unwrap() else { panic!("a map") };
        let V::Map(record) = modifiers.get_mut("z1").unwrap() else { panic!("a map") };
        record.insert(text("parameters"), v(json!({"level": 0.75})));
    }
    let changed = build(&world, &mut cache).unwrap();
    assert_eq!(changed.zones[1].lighting_level, 0.75);
    // A context changed in place: the configs' stamp is new, and the person parses again.
    {
        let slot = world.slots.get(MODEL_CONFIGS).unwrap();
        let mut slot = slot.try_borrow_mut().unwrap();
        let row = slot.row_written::<ValueRow>().unwrap();
        let V::Map(configs) = &mut row.0 else { panic!("a map") };
        configs.insert(text("population_context"), v(json!({"people": {"A1": {"roles": ["child"]}}})));
    }
    let changed = build(&world, &mut cache).unwrap();
    assert_eq!(texts(&changed.agents[0].person.roles), ["child"]);
    assert_eq!(changed.agents[0].person.usual_wake_minute, None);
}

#[test]
fn what_the_view_cannot_read_it_refuses_before_making_anything() {
    let mut cache = ViewCache::default();
    let refused = |world: &World, cache: &mut ViewCache| build(world, cache).err();

    // A tainted namespace, with why its row could not hold it.
    let world = world(true);
    {
        let slot = world.slots.get("psychological_distress_v1").unwrap();
        let mut slot = slot.try_borrow_mut().unwrap();
        slot.misfit("not a float".to_owned());
    }
    assert_eq!(refused(&world, &mut cache), Some(unsupported("psychological_distress_v1 is tainted: not a float")));

    // A person in a zone the world does not have.
    let mut world = self::world(false);
    world.entities.get_mut().agents.get_mut("A1").unwrap().zone_id = text("nowhere");
    assert_eq!(refused(&world, &mut cache), Some(unsupported("a person is in an unknown zone")));

    // A household that is no str.
    let mut world = self::world(false);
    add_value(&mut world, "population_presence_v1", Place::Namespace, json!({"people": {"A1": {"household_id": 7}}}));
    assert_eq!(refused(&world, &mut cache), Some(unsupported("a view value is not a str")));

    // An outage whose facilities are no container: Python's own TypeError.
    let world = self::world(false);
    {
        let slot = world.slots.get("infrastructure_v1").unwrap();
        let mut slot = slot.try_borrow_mut().unwrap();
        let row = slot.row_written::<ValueRow>().unwrap();
        row.0 = v(json!({"environmental_stressors": {}, "outages": {"o": {"active": true, "affected_facility_ids": 3, "start_event_id": "e"}}}));
    }
    assert_eq!(refused(&world, &mut cache), Some(Error::Type("argument of type 'int' is not iterable".to_owned())));

    // An operational state stored as None.
    let world = self::world(false);
    {
        let slot = world.slots.get("operational_events_v1").unwrap();
        slot.try_borrow_mut().unwrap().row = Some(Box::new(ValueRow(V::Null)));
    }
    assert_eq!(refused(&world, &mut cache), Some(unsupported("the operational state is None")));
}

#[test]
fn the_selection_says_whether_misinformation_applies() {
    let selection = |model: &str| Ok(Arc::new(Selection::new(vec![(text("psychology"), text(model))])));
    assert_eq!(view::misinformation_applies(&selection("life-state-feedback-v1")), Ok(true));
    assert_eq!(view::misinformation_applies(&selection("mean-reverting-life-state-v1")), Ok(true));
    assert_eq!(view::misinformation_applies(&selection("bounded-stress-v1")), Ok(false));
    assert_eq!(view::misinformation_applies(&Ok(Arc::new(Selection::new(Vec::new())))), Err(Error::Key("psychology".to_owned())));
    assert_eq!(view::misinformation_applies(&Err(Fault::refused("not a selection"))), Err(unsupported("a model ID is not a str")));

    // Without a life-state model, the false claims are not read.
    let world = world(true);
    let other = selection("bounded-stress-v1");
    let inputs = Inputs { selection: &other, configs: None };
    let data = view::build(&world, &inputs, &mut ViewCache::default()).unwrap().finish(false);
    assert_eq!(data.agents[1].misinformation_belief_pressure, 0.0);
    assert!(data.agents[1].misinformation_effect_sources.is_empty());
    assert_eq!(data.agents[1].information_query_pressure, 0.5);
}

#[test]
fn a_person_context_is_read_as_the_contract_reads_it() {
    let fields = person_context::parse(&v(json!({
        "household_id": null, "parent_ids": ["P1"], "requires_review": "yes", "roles": ["child"],
        "usual_sleep_minute": 1380, "required_sleep_minutes": 480, "pet_count": 2
    })))
    .unwrap();
    assert_eq!(
        fields,
        PersonFields {
            parent_ids: vec![text("P1")],
            roles: vec![text("child")],
            usual_sleep_minute: Some(1380),
            required_sleep_minutes: Some(480),
            personal_context_present: true,
            ..PersonFields::default()
        }
    );
    let refused = unsupported(person_context::REFUSED);
    for bad in [
        json!({"pet_count": -1}),
        json!({"roles": ["other"]}),
        json!({"parent_ids": ["A", "B", "C", "D", "E"]}),
        json!({"usual_wake_minute": 1440}),
        json!({"usual_wake_minute": 1.0}),
        json!({"care_support_needs": ["hygiene_support", "hygiene_support"]}),
        json!({"requires_review": "maybe"}),
        json!({"other_field": 1}),
        json!([]),
    ] {
        assert_eq!(person_context::parse(&v(bad.clone())), Err(refused.clone()), "{bad}");
    }
}

#[test]
fn a_zone_without_the_policy_is_neutral_and_a_bad_modifier_is_refused() {
    let configs = |value: Value| match v(value) {
        V::Map(map) => map,
        _ => panic!("a map"),
    };
    let neutral = ZoneInputs::read(&configs(json!({})), Some(&v(json!({"modifiers": {}})))).unwrap().conditions("hall").unwrap();
    assert_eq!((neutral.ventilation_effectiveness, neutral.rest_efficiency_multiplier, neutral.min_rest_decision_minutes), (0.7, 1.0, 1));
    let config = configs(json!({"zone_environment": {}}));
    let missing = ZoneInputs::read(&config, Some(&v(json!({"schema_version": 1}))));
    assert_eq!(missing.err(), Some(unsupported("the zone environment state has no modifiers")));
    let bad = ZoneInputs::read(&config, Some(&v(json!({"modifiers": {"m": {"zone_id": "hall", "start_minute": 0}}})))).unwrap();
    assert_eq!(bad.conditions("hall").err(), Some(unsupported("a zone modifier is not what the rule reads")));
    let bad_config = configs(json!({"zone_environment": {"noise_rest_efficiency_loss": 0.95}}));
    assert_eq!(
        ZoneInputs::read(&bad_config, Some(&v(json!({"modifiers": {}})))).err(),
        Some(unsupported("the zone environment config does not parse"))
    );
    let unknown = configs(json!({"zone_environment": {}, "policy_registry": {"zone_rest": "zone-rest-other-v1"}}));
    assert_eq!(ZoneInputs::read(&unknown, Some(&v(json!({"modifiers": {}})))).err(), Some(unsupported("a zone formula of another module")));
}

#[test]
fn a_water_station_is_known_by_its_folded_kind() {
    assert!(view::is_water_station("Water_Station"));
    assert!(view::is_water_station("main-WATER_STATION-2"));
    assert!(!view::is_water_station("water station"));
    // `ſ` (long s) folds to `s` only in a full case folding.
    assert!(view::is_water_station("water_ſtation"));
}
