//! The proposals of the behavior rule (`rules::behavior::due`, the kernel of `propose_due`) and the
//! observed state of a person (`behavior::observed_value`, of `observed_state`), on the worlds of
//! `integration/test_behavior_row.py`, without Python (Issue #225, lane M10b).
//!
//! `data/behavior_rows.json` (`tests/support/core_fixtures_reads.py`) holds the worlds the drawn
//! worlds start from (the generated Scenario and the small replay at minute 0, with their body
//! state), the world of the requests' test, and what that file drew with `random.Random`: each
//! drawn world's values (`draw_world`, numbers only) and its policies (the values the Run reads of
//! them), with the seeds of their draws. A test writes the drawn values into the world (`drawn`),
//! makes the view the propose phase makes (`view_for_proposals` of a new controller: the core's
//! `world_view::build` with a new cache, and its audit hash), and makes the policy's objects as the
//! Run read them (`Behavior::of`).
//!
//! Each value the kernel made is added to `Bits` as `json.dumps` of `_exact` of the Python object
//! the PyO3 crate makes of it (an `ActionProposal`, the observed dict): each type's name, each
//! dataclass's fields in the order the crate sets them, each float as `float.hex`. The digest is the
//! one the pytest file pinned, taken from the deleted Python rules (45a9bc9) or, for what model
//! version 2 changed, from the kernels of that model (that file's docstring).

mod support;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use indexmap::IndexMap;
use serde_json::Value;
use sheltersim_core::cell::Shard;
use sheltersim_core::checkpoint::sha256_hex;
use sheltersim_core::config::{Fault, Section, SectionId, Selection};
use sheltersim_core::entities::Activity;
use sheltersim_core::host::Phase;
use sheltersim_core::json::Encoder;
use sheltersim_core::row::{StrEnum, ValueRow};
use sheltersim_core::rules::behavior::{self, Proposal, Scored};
use sheltersim_core::rules::tick::{self, Prior, Seed, Tick};
use sheltersim_core::rules::world_view::{self as view, Inputs, ViewCache, ViewData};
use sheltersim_core::run::behavior::Behavior;
use sheltersim_core::run::held::{Class, Held, Obj};
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, World, WorldCell, MODEL_CONFIGS};

use support::dumps::dumps;
use support::load::Loaded;
use support::sections;
use support::{cases, load, texts};

const FILE: &str = "behavior_rows";
const BEHAVIOR: &str = "sheltersim.policies.behavior";
/// The values the Run reads of a `UtilityBehaviorPolicy`, in the order of a policy's record.
const UTILITY_FIELDS: [&str; 16] = [
    "policy_version",
    "temperature",
    "decision_interval_minutes",
    "decision_interval_minutes_by_agent",
    "temperature_by_agent",
    "sleep_debt_utility_weight",
    "resource_service_enabled",
    "hydration_action_threshold",
    "hunger_action_threshold",
    "water_utility_base",
    "water_utility_need_weight",
    "meal_utility_base",
    "meal_utility_need_weight",
    "information_query_threshold",
    "information_query_utility_weight",
    "autonomy_enabled",
];
const ACTIVITIES: [&str; 4] = ["idle", "resting", "sleeping", "moving"];
const HOUSEHOLDS: [Option<&str>; 4] = [None, Some(""), Some("G1"), Some("G2")];
const BODY_VALUES: [&str; 6] = ["sleep_debt_minutes", "uncertainty", "reassurance", "discontent", "negative_affect", "hygiene_need"];
const MODIFIER_ACTIONS: [&str; 4] = ["sleep", "rest", "wait", "sleep"];

fn text(value: &str) -> V {
    V::Str(Arc::from(value))
}

fn map(entries: Vec<(&str, V)>) -> V {
    V::Map(entries.into_iter().map(|(key, value)| (Arc::from(key), value)).collect())
}

// ---- `_exact` and `_Bits` ------------------------------------------------------------------------

/// `float.hex` of CPython.
fn hex(value: f64) -> String {
    let sign = if value.is_sign_negative() { "-" } else { "" };
    if value == 0.0 {
        return format!("{sign}0x0.0p+0");
    }
    let bits = value.to_bits();
    let exponent = ((bits >> 52) & 0x7ff) as i64;
    let fraction = bits & ((1 << 52) - 1);
    let (lead, exponent) = if exponent == 0 { (0, -1022) } else { (1, exponent - 1023) };
    format!("{sign}0x{lead}.{fraction:013x}p{exponent:+}")
}

fn typed(name: &str, value: V) -> V {
    V::List(vec![text(name), value])
}

fn float(value: f64) -> V {
    text(&hex(value))
}

fn int(value: i64) -> V {
    typed("int", V::Int(value))
}

fn string(value: &str) -> V {
    typed("str", text(value))
}

/// `_exact` of the Python value a `V` becomes (a list is a list).
fn exact(value: &V) -> V {
    match value {
        V::Null => V::Null,
        V::Bool(flag) => V::Bool(*flag),
        V::Int(number) => int(*number),
        V::Float(number) => float(*number),
        V::Str(value) => string(value),
        V::List(items) => V::List(std::iter::once(text("list")).chain(items.iter().map(exact)).collect()),
        V::Map(items) => V::List(std::iter::once(text("dict")).chain(items.iter().map(|(key, item)| V::List(vec![V::Str(key.clone()), exact(item)]))).collect()),
        V::Big(_) => panic!("an int past 64 bits"),
    }
}

/// `_exact` of a dataclass: its class's name, then each field and its value.
fn dataclass(name: &str, fields: Vec<(&str, V)>) -> V {
    V::List(std::iter::once(text(name)).chain(fields.into_iter().map(|(field, value)| V::List(vec![text(field), value]))).collect())
}

fn tuple(items: Vec<V>) -> V {
    V::List(std::iter::once(text("tuple")).chain(items).collect())
}

fn action(name: &str) -> V {
    V::List(vec![text("ActionKind"), text(name)])
}

fn candidate(data: &ViewData, proposal: &Proposal, item: &Scored) -> V {
    dataclass(
        "ActionCandidate",
        vec![
            ("action", action(item.action.value())),
            ("eligible", V::Bool(item.eligible)),
            ("trigger", string(&proposal.trigger_of(data, item).unwrap())),
            ("utility", float(item.utility)),
            ("urgency_priority", int(item.priority)),
            ("probability", float(item.probability)),
        ],
    )
}

/// `_exact` of the `ActionProposal` the PyO3 crate makes (`proposal_of_data`).
fn proposal(data: &ViewData, proposal: &Proposal) -> V {
    let outcome = &proposal.outcome;
    let key = proposal.rng_key(data).into_iter().map(|part| match part {
        Ok(part) => string(part),
        Err(number) => int(number),
    });
    dataclass(
        "ActionProposal",
        vec![
            ("agent_id", string(&data.agents[proposal.index].id)),
            ("minute_index", int(data.minute_index)),
            ("observed_state_hash", string(&proposal.common.state_hash)),
            ("action", action(outcome.action.value())),
            ("candidates", tuple(outcome.candidates.iter().map(|item| candidate(data, proposal, item)).collect())),
            ("trigger", string("scheduled_decision")),
            ("choice_value", float(outcome.choice_value)),
            ("rng_key", tuple(key.collect())),
            ("selection_probability", float(outcome.selection_probability)),
            ("policy_version", string(&proposal.common.policy_version)),
            ("next_decision_at", int(outcome.next_decision_at)),
            ("target_id", outcome.target.map_or(V::Null, |target| string(&data.agents[target].id))),
        ],
    )
}

/// The pytest file's `_Bits`: the sha256 of `json.dumps(_exact(value))` of each value added.
#[derive(Default)]
struct Bits(String);

impl Bits {
    fn add(&mut self, value: &V) {
        self.0.push_str(&dumps(value));
    }

    fn check(&self, digest: &str) {
        assert_eq!(&sha256_hex(self.0.as_bytes())[..16], digest);
    }
}

// ---- The world, the view and the policy --------------------------------------------------------------

/// The people of a world in ID order.
fn people(world: &World) -> Vec<Arc<str>> {
    let mut ids: Vec<Arc<str>> = world.rows().unwrap().agents.iter().map(|(id, _)| id.clone()).collect();
    ids.sort_unstable();
    ids
}

/// A namespace of `model_state` as a `V`, from its row's JSON.
fn namespace(world: &World, name: &str) -> V {
    let slot = world.slots.get(name).unwrap_or_else(|| panic!("no {name}")).try_borrow().unwrap();
    let mut encoder = Encoder::with_capacity(1024);
    slot.row.as_deref().unwrap_or_else(|| panic!("{name} has no row")).write_json(&mut encoder).unwrap();
    load::v(&serde_json::from_slice(&encoder.out).unwrap())
}

/// `model_state[name] = value`, with a new stamp.
fn put(world: &mut World, name: &str, value: V) {
    load::put_namespace(world, name, value);
    world.slots.get(name).unwrap().try_borrow_mut().unwrap().touch();
}

fn entries(value: &mut V) -> &mut IndexMap<Arc<str>, V> {
    match value {
        V::Map(map) => map,
        other => panic!("not a map: {other:?}"),
    }
}

fn number(value: &Value) -> V {
    load::v(value)
}

fn f(value: &Value) -> f64 {
    value.as_f64().unwrap()
}

/// Write what `draw_world` drew (`drawn_record`) into the world it started from.
fn drawn(world: &mut World, record: &str) {
    let record: Value = serde_json::from_str(record).unwrap();
    let ids = people(world);
    let zones = {
        let mut zones: Vec<Arc<str>> = world.rows().unwrap().zones.iter().map(|(id, _)| id.clone()).collect();
        zones.sort_unstable();
        zones
    };
    world.scalars.get_mut().minute_index = Fit::Ok(record[0].as_i64().unwrap());
    let rows = record[1].as_array().unwrap();
    assert_eq!(rows.len(), ids.len());
    {
        let mut entities = world.rows_mut().unwrap();
        for (id, values) in ids.iter().zip(rows) {
            let agent = entities.agents.get_mut(id).unwrap();
            agent.needs.toilet_urgency = f(&values[0]);
            agent.needs.hunger = f(&values[1]);
            agent.needs.fatigue = f(&values[2]);
            agent.needs.hydration_need = f(&values[3]);
            agent.stress = f(&values[4]);
            agent.trust = f(&values[5]);
            agent.next_decision_at = values[6].as_i64().unwrap();
            agent.activity = Activity::from_text(ACTIVITIES[values[7].as_u64().unwrap() as usize]).unwrap();
        }
    }
    let mut body = namespace(world, "body_state_v1");
    {
        let agents = entries(entries(&mut body).get_mut("agents").unwrap());
        for (id, values) in ids.iter().zip(rows) {
            let body = entries(agents.get_mut(id).unwrap());
            for (name, value) in BODY_VALUES.iter().zip(&values.as_array().unwrap()[8..]) {
                body.insert(Arc::from(*name), number(value));
            }
        }
    }
    put(world, "body_state_v1", body);
    let mut presence = IndexMap::new();
    for (id, household) in ids.iter().zip(record[2].as_array().unwrap()) {
        if let Ok(index) = usize::try_from(household.as_i64().unwrap()) {
            let household = HOUSEHOLDS[index].map_or(V::Null, text);
            presence.insert(id.clone(), map(vec![("household_id", household)]));
        }
    }
    put(world, "population_presence_v1", map(vec![("people", V::Map(presence))]));
    if let Some(context) = record[3].as_array() {
        let id_list = |value: &Value| V::List(value.as_array().unwrap().iter().map(|index| V::Str(ids[index.as_u64().unwrap() as usize].clone())).collect());
        let mut people = IndexMap::new();
        for person in context {
            let fields = vec![
                ("care_for_ids", id_list(&person[1])),
                ("caregiver_ids", id_list(&person[2])),
                ("parent_ids", id_list(&person[3])),
                ("usual_wake_minute", number(&person[4])),
                ("usual_sleep_minute", number(&person[5])),
                ("required_sleep_minutes", number(&person[6])),
            ];
            people.insert(ids[person[0].as_u64().unwrap() as usize].clone(), map(fields));
        }
        let mut configs = world.row_mut::<ValueRow>(MODEL_CONFIGS).unwrap();
        let section = entries(&mut configs.0).entry(Arc::from("population_context")).or_insert_with(|| map(vec![("people", map(vec![]))]));
        entries(section).insert(Arc::from("people"), V::Map(people));
        drop(configs);
        world.slots.get(MODEL_CONFIGS).unwrap().try_borrow_mut().unwrap().touch();
    }
    if let Some(added) = record[4].as_array() {
        let mut environment = namespace(world, "zone_environment_v1");
        let modifiers = entries(entries(&mut environment).get_mut("modifiers").unwrap());
        assert!(modifiers.is_empty());
        for modifier in added {
            let number_of = modifier[0].as_u64().unwrap() as usize;
            let action = MODIFIER_ACTIONS[number_of];
            let assumptions = ["ASM-A", "ASM-B"][..number_of % 3].iter().map(|id| text(id)).collect();
            let parameters = map(vec![
                ("action", text(action)),
                ("probability_multiplier", number(&modifier[2])),
                ("duration_multiplier", number(&modifier[3])),
            ]);
            let made = map(vec![
                ("zone_id", V::Str(zones[modifier[1].as_u64().unwrap() as usize].clone())),
                ("start_minute", V::Int(0)),
                ("op", text("modify_demand_rate")),
                ("parameters", parameters),
                ("start_event_id", text(&format!("e{number_of}"))),
                ("source_id", text("s")),
                ("assumption_ids", V::List(assumptions)),
            ]);
            modifiers.insert(Arc::from(format!("m{number_of}")), made);
        }
        put(world, "zone_environment_v1", environment);
    }
}

/// The selection of the world's configs, as Python parsed it (the export's section).
fn selection(entry: &Value) -> Result<Arc<Selection>, Fault> {
    match sections::section(entry, SectionId::Selection).selection.get().unwrap() {
        Section::Ok(selection) => Ok(selection.clone()),
        Section::Fault(fault) => Err(fault.clone()),
        Section::Absent => panic!("no selection"),
    }
}

fn extensions_enabled(entry: &Value) -> bool {
    match sections::section(entry, SectionId::ExtensionsEnabled).extensions_enabled.get().unwrap() {
        Section::Ok(enabled) => **enabled,
        _ => panic!("no flag of the extensions"),
    }
}

/// `view_for_proposals` of a new controller: a view built with a new cache, and its audit hash.
fn view_of(entry: &Value, cell: &Shard<World>) -> (ViewData, Arc<str>) {
    let selection = selection(entry);
    let enabled = extensions_enabled(entry);
    cell.with(|world| {
        let inputs = Inputs { selection: &selection, configs: None };
        let data = view::build(world, &inputs, &mut ViewCache::default()).unwrap().finish(enabled);
        let hash = view::state_hash(&data, world, None).unwrap();
        (data, Arc::from(hash))
    })
    .unwrap()
}

fn behavior_object(qualname: &str, attrs: Vec<(&str, Result<Held, Fault>)>) -> Arc<Obj> {
    let mut object = Obj::new(Class::named(BEHAVIOR, qualname));
    for (name, value) in attrs {
        object.attrs.insert(Arc::from(name), value);
    }
    Arc::new(object)
}

fn utility(values: &Value, as_policy: bool) -> Arc<Obj> {
    let values = values.as_array().unwrap();
    let mut attrs: Vec<(&str, Result<Held, Fault>)> = UTILITY_FIELDS.iter().zip(values).map(|(name, value)| (*name, Ok(Held::Value(load::v(value))))).collect();
    if as_policy {
        let no_scorer = load::fault(&serde_json::json!(["builtins.AttributeError", "'UtilityBehaviorPolicy' object has no attribute 'scorer'"]));
        attrs.insert(1, ("scorer", Err(no_scorer)));
    }
    behavior_object("UtilityBehaviorPolicy", attrs)
}

/// What the Run reads of a controller whose `decisions` run the policy of this record
/// (`PolicyRecords`): its values, or a priority rule's version and its scorer's values.
fn behavior(record: &str) -> Behavior {
    let record: Value = serde_json::from_str(record).unwrap();
    let policy = match record.as_array().unwrap().len() {
        2 => behavior_object("PriorityRuleBehaviorPolicy", vec![("policy_version", Ok(Held::Value(load::v(&record[0])))), ("scorer", Ok(Held::Object(utility(&record[1], false))))]),
        _ => utility(&record, true),
    };
    let runner = behavior_object("BehaviorDecisionRunner", vec![("policy", Ok(Held::Object(policy)))]);
    let mut controller = Obj::new(Class::named("types", "SimpleNamespace"));
    controller.attrs.insert(Arc::from("decisions"), Ok(Held::Object(runner)));
    Behavior::of(&controller)
}

/// `propose_due`: the kernel's proposals, added to `bits` (an empty tuple when nobody is due).
fn proposals(bits: &mut Bits, data: &ViewData, hash: &Arc<str>, policy: &str, seed: &str) -> Vec<Proposal> {
    let behavior = behavior(policy);
    let scoring = behavior::scoring_of(&behavior).unwrap_or_else(|bad| panic!("{bad:?}"));
    let made = behavior::due(data, &scoring, seed, hash.clone()).unwrap_or_else(|bad| panic!("{bad:?}")).unwrap_or_default();
    bits.add(&tuple(made.iter().map(|item| proposal(data, item)).collect()));
    made
}

/// `observed_state`: the kernel's observed state of a person, added to `bits`.
fn observed(bits: &mut Bits, data: &ViewData, agent_id: &str) -> V {
    let value = behavior::observed_value(data, data.index_of(agent_id).expect("a person of the view"));
    bits.add(&exact(&value));
    value
}

/// A world the drawn world starts from, in a cell, with its entry.
fn start(name: &str) -> (Value, Shard<World>) {
    let entry = cases::entry(FILE, name);
    let cell = texts::cell(&entry);
    (entry, cell)
}

/// The world's people in the order of Python's dict (`world.agents`).
fn agents_of(entry: &Value) -> Vec<String> {
    entry["agents"].as_array().unwrap().iter().map(|id| id.as_str().unwrap().to_owned()).collect()
}

/// A drawn world on the world `name` starts from, and its view.
fn drawn_view(name: &str, record: &str) -> (Value, ViewData, Arc<str>) {
    let (entry, cell) = start(name);
    cell.with(|world| drawn(world, record)).unwrap();
    let (data, hash) = view_of(&entry, &cell);
    (entry, data, hash)
}

#[test]
fn each_proposal_of_a_drawn_world_has_the_bits_of_python() {
    let digests = [
        "2fae7a31edbc3c16",
        "fd8741e8bc7a3693",
        "4156e098c70b0e82",
        "b84771aeec0cadd1",
        "31140fd72ef0f3a3",
        "69f5cb1c3f6cd3f6",
        "758fadbbe877fb40",
        "f365645fc57ad062",
        "45a72a5cec4be3f5",
        "015eba747f860aa6",
        "295a09cee324245d",
        "98b947d8e7144135",
        "287fd8ba0f8f7cd0",
        "34e963c729fc7d55",
        "cfdcc5e644625650",
        "57c04f8a579696f7",
        "681c23730e464b81",
        "8356521e966b2ce0",
        "ed29383f6605becb",
        "499c3c2d73cf4264",
        "09f53e4bfb031ffb",
        "89dc8cbe38936da4",
        "96db2d073a3665a0",
        "edcc9b9e0b1303c1",
        "2e11073734139790",
        "8339df839444504d",
        "78447961a6b236d2",
        "530c552f9d011ce4",
        "e535c9405f0e2425",
        "da5ecfb298102457",
    ];
    let data = load::data(FILE);
    for (seed, digest) in digests.iter().enumerate() {
        let case = &data["drawn"][seed.to_string()];
        let name = case["scenario"].as_str().unwrap();
        // The generated scenario has the water and meal actions, the small one doesn't.
        assert_eq!(name == "small", seed % 3 == 0);
        let (entry, view, hash) = drawn_view(name, case["world"].as_str().unwrap());
        assert_eq!(view.resource_service_enabled, seed % 3 != 0);
        let mut bits = Bits::default();
        for policy in case["policies"].as_array().unwrap() {
            proposals(&mut bits, &view, &hash, policy[0].as_str().unwrap(), &policy[1].to_string());
        }
        for agent_id in agents_of(&entry) {
            observed(&mut bits, &view, &agent_id);
        }
        bits.check(digest);
    }
}

#[test]
fn a_seed_past_64_bits_has_the_bits_of_python() {
    // A Scenario's seed is any int from 0, and CounterRNG keys its draws with `str(seed)`.
    let data = load::data(FILE);
    let case = &data["past_64"];
    let (_, view, hash) = drawn_view("generated", case["world"].as_str().unwrap());
    let seeds = [("9223372036854775808", "4eeb7955eaf7a746"), ("18446744073709551623", "b05281eb8a1f1846"), ("1000000000000000000000000000000", "09fcdfd963ea511b")];
    for (seed, digest) in seeds {
        let mut bits = Bits::default();
        assert!(!proposals(&mut bits, &view, &hash, case["policy"].as_str().unwrap(), seed).is_empty());
        bits.check(digest);
    }
}

fn get<'a>(value: &'a V, path: &[&str]) -> &'a V {
    path.iter().fold(value, |value, key| match value {
        V::Map(map) => map.get(*key).unwrap_or_else(|| panic!("no {key}")),
        other => panic!("not a map: {other:?}"),
    })
}

#[test]
fn requests_cooldowns_and_queued_tasks_are_read_as_python_reads_them() {
    // No gate scenario has someone due with a request or a queued task, so these are here.
    let data = load::data(FILE);
    let entry = cases::resolve(&data["worlds"], "requests");
    let cell = texts::cell(&entry);
    let (view, hash) = view_of(&entry, &cell);
    let people: Vec<String> = cell.with(|world| people(world).iter().map(|id| id.to_string()).collect()).unwrap();
    let mut bits = Bits::default();
    for policy in data["requests"].as_array().unwrap() {
        let made = proposals(&mut bits, &view, &hash, policy.as_str().unwrap(), "11");
        let proposed: Vec<&str> = made.iter().map(|item| &*view.agents[item.index].id).collect();
        assert_eq!(proposed, people.iter().map(String::as_str).collect::<Vec<_>>());
    }
    let snapshots: Vec<V> = people.iter().map(|agent_id| observed(&mut bits, &view, agent_id)).collect();
    bits.check("58c69158d7eb36d8");
    assert_eq!(get(&snapshots[0], &["support", "status"]), &text("pending"));
    assert_eq!(get(&snapshots[0], &["support", "wait_minutes"]), &V::Int(35));
    assert_eq!(get(&snapshots[4], &["support", "request_id"]), &text("nothing"));
    assert_eq!(get(&snapshots[5], &["support", "cooldown_remaining_minutes"]), &V::Int(15));
    assert_eq!(get(&snapshots[6], &["support", "cooldown_remaining_minutes"]), &V::Int(0));
    assert_eq!(get(&snapshots[7], &["queue", "wait_minutes"]), &V::Int(30));
    let queue = map(vec![("task_id", text("T2")), ("kind", text("resource_service_queue")), ("target_id", V::Null), ("wait_minutes", V::Int(0))]);
    assert_eq!(get(&snapshots[8], &["queue"]), &queue);
    // The first of two queued tasks: the order they were added in.
    assert_eq!(get(&snapshots[9], &["queue", "task_id"]), &text("T3"));
}

#[test]
fn drawn_worlds_reach_what_they_are_for() {
    // The comparison above means little if the branches stay unreached.
    let data = load::data(FILE);
    let mut actions = std::collections::BTreeSet::new();
    let mut with_target = std::collections::BTreeSet::new();
    let (mut demand, mut ties) = (0, 0);
    let mut bits = Bits::default();
    for seed in 0..60 {
        let case = &data["reach"][seed.to_string()];
        let record = match case["world"].as_str() {
            Some(record) => record,
            None => data["drawn"][seed.to_string()]["world"].as_str().unwrap(),
        };
        let (_, view, hash) = drawn_view("generated", record);
        for made in proposals(&mut bits, &view, &hash, case["policy"].as_str().unwrap(), "11") {
            let name = made.outcome.action.value();
            actions.insert(name);
            if made.outcome.target.is_some() {
                with_target.insert(name);
            }
            ties += usize::from(made.outcome.tie_broken);
            demand += usize::from(made.outcome.candidates.iter().any(|item| made.trigger_of(&view, item).unwrap().contains("experimental zone")));
        }
    }
    let wanted = ["wait", "request_toilet", "rest", "sleep", "seek_help", "seek_information", "socialize", "care_family", "help_others", "fetch_water", "collect_meal"];
    assert!(wanted.iter().all(|name| actions.contains(name)), "{actions:?}");
    assert_eq!(with_target.into_iter().collect::<Vec<_>>(), ["care_family", "help_others", "socialize"]);
    assert!(ties > 0 && demand > 0);
    bits.check("6057d790ccb496c7");
}

/// `proposal_view` of the pytest files at the world's minute, before the propose phase: the update
/// phase's view (the Run's scratch) with what that phase wrote read again, or a new view; with its
/// audit hash. The Run keeps its view for the propose phase.
fn proposal_view(loaded: &Loaded, entry: &Value) -> (ViewData, Arc<str>) {
    let minute = loaded.cell.with(|world| world.minute_index().unwrap()).unwrap();
    let prior = match loaded.run.borrow().scratch.get::<Prior>() {
        Some(Prior::View(data)) if data.minute_index == minute => Some(data.clone()),
        Some(Prior::Python { .. }) => panic!("a view of Python's"),
        _ => None,
    };
    let Some(prior) = prior else {
        return view_of(entry, &loaded.cell);
    };
    loaded
        .cell
        .with(|world| {
            let data = view::refresh(&prior, world).unwrap();
            let hash = view::state_hash(&data, world, None).unwrap();
            (data, Arc::from(hash))
        })
        .unwrap()
}

/// What the test counts: the calls, the proposals and the observed states.
#[derive(Default)]
struct Counts {
    calls: usize,
    proposals: usize,
    observed: usize,
}

#[test]
fn each_proposal_of_a_run_has_the_bits_of_python() {
    // The steps' kernels on the view each tick's propose phase makes, before that phase, through the
    // Run's ticks (`TickEngine.step`: `run_tick`, then the minute moved on).
    let entry = cases::entry(FILE, "run");
    let loaded = Rc::new(load::loaded(&entry));
    let mut host = load::following_host(&entry, &loaded);
    let bits = Rc::new(RefCell::new(Bits::default()));
    let counts = Rc::new(RefCell::new(Counts::default()));
    let seed = match &loaded.seed {
        Seed::Counter { text: Some(text), .. } => text.to_string(),
        other => panic!("not a CounterRNG: {other:?}"),
    };
    {
        let (loaded, bits, counts, entry) = (Rc::clone(&loaded), Rc::clone(&bits), Rc::clone(&counts), entry.clone());
        host.on_phase = Some(Box::new(move |phase, before| {
            if phase != Phase::ProposeActions || !before {
                return;
            }
            let (view, hash) = proposal_view(&loaded, &entry);
            let scoring = behavior::scoring_of(&loaded.policies.behavior).unwrap_or_else(|bad| panic!("{bad:?}"));
            let made = behavior::due(&view, &scoring, &seed, hash).unwrap_or_else(|bad| panic!("{bad:?}")).unwrap_or_default();
            let mut bits = bits.borrow_mut();
            bits.add(&tuple(made.iter().map(|item| proposal(&view, item)).collect()));
            let mut counts = counts.borrow_mut();
            counts.calls += 1;
            counts.proposals += made.len();
            for item in &made {
                observed(&mut bits, &view, &view.agents[item.index].id);
                counts.observed += 1;
            }
        }));
    }
    let duration = loaded.cell.with(|world| world.duration_minutes().unwrap()).unwrap();
    assert_eq!(duration, 90);
    for minute in 0..duration {
        let mut tick = Tick {
            cell: &loaded.cell,
            host: &mut host,
            run: &loaded.run,
            policies: loaded.policies.clone(),
            minute,
            seed: loaded.seed.clone(),
            neighbors: loaded.neighbors.clone(),
        };
        if let Err(failed) = tick::run_tick(&mut tick) {
            panic!("minute {minute}: {:?} {:?}", failed.label, failed.error);
        }
        loaded.cell.with(|world| world.scalars.get_mut().minute_index = Fit::Ok(minute + 1)).unwrap();
    }
    drop(host);
    let counts = counts.borrow();
    assert_eq!(counts.calls, 90);
    assert!(counts.proposals == counts.observed && counts.observed > 0);
    bits.borrow().check("39e0d6ea65c6015b");
}
