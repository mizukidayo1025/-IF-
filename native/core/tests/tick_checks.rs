//! Lane P8 d4: the end-of-tick check (`rules::after_tick`) and its entity rules
//! (`rules::tick_validation`) on a world built here with no Python, and the marks of the written
//! slots the memo reads (`Table::marked`). The PyO3 crate's kernels are glue over these, so the gate
//! and the offline tests run them on every tick (`tests/integration/test_after_tick_kernel.py`,
//! `test_entity_rules_kernel.py` compare them with the Python check); these say what each reads,
//! what it asks of Python (`Checks`), and what the memo keeps and skips.

use std::sync::Arc;

use indexmap::IndexMap;
use sheltersim_core::cell::Shard;
use sheltersim_core::config::{ResourceNumbers, TickPlan};
use sheltersim_core::entities::{Activity, AgentRow, AnyTable, Entities, FacilityRow, Mobility, NeedRow, ProfileRow, RequestRow, RequestStatus, Table, ZoneRow};
use sheltersim_core::error::{Error, Result};
use sheltersim_core::events::log::EventLog;
use sheltersim_core::events::queue::Queue;
use sheltersim_core::events::{EventKind, EventRow};
use sheltersim_core::row::{IdMap, ValueRow};
use sheltersim_core::rules::after_tick::{self, Checks, HeldNamespaces, Inputs, Memo, READ};
use sheltersim_core::rules::kit::Held;
use sheltersim_core::rules::tick_validation::{self, Dynamics, EntityInputs, Requests, NO_ROW};
use sheltersim_core::stamp::Stamp;
use sheltersim_core::value::V;
use sheltersim_core::world::{Fit, Place, Seed, World, WorldCell, ID_COUNTERS, RESOURCES};

fn text(value: &str) -> Arc<str> {
    Arc::from(value)
}

fn refusal(reason: &str) -> Error {
    Error::Unsupported(reason.to_owned().into())
}

fn put<R: Clone>(table: &mut Table<R>, id: &str, row: R) -> u32 {
    let slot = table.reserve_slot();
    table.fill_slot(slot, row);
    assert_eq!(table.bind(text(id), slot), None);
    slot
}

fn needs() -> NeedRow {
    NeedRow { toilet_urgency: 0.1, hunger: 0.2, fatigue: 0.3, hydration_need: 0.0 }
}

fn agent(id: &str, zone: &str) -> AgentRow {
    AgentRow {
        profile: ProfileRow {
            id: text(id),
            group_id: None,
            initial_zone_id: text(zone),
            mobility: Mobility::Independent,
            support_need: 0.0,
            stress_sensitivity: 1.0,
            rng_key: text(id),
            initial_needs: needs(),
            provenance: V::Map(IndexMap::new()),
        },
        zone_id: text(zone),
        activity: Activity::Idle,
        target_id: None,
        needs: needs(),
        stress: 0.2,
        trust: 0.5,
        next_decision_at: 0,
        support_request_id: None,
        support_cooldown_until: 0,
    }
}

fn request(id: &str, agent: &str, status: RequestStatus) -> RequestRow {
    RequestRow {
        id: text(id),
        agent_id: text(agent),
        requested_minute: 1,
        timeout_minute: 30,
        status,
        assigned_staff_id: None,
        task_id: None,
        resolved_minute: Some(3),
        cooldown_until: 0,
        last_assignment_attempt_minute: None,
        last_assignment_wait_reason_code: None,
    }
}

fn add_slot(world: &mut World, name: &str, place: Place, row: Box<dyn sheltersim_core::row::Row>) {
    let index = world.slots.add(text(name), place).unwrap();
    world.slots.at(index).unwrap().try_borrow_mut().unwrap().row = Some(row);
}

/// A WorldState v5 of 600 minutes at minute 7: a1 and a2 idle in z0 (capacity 3), a toilet in z0,
/// two settled requests of a1 (r1, r2), an empty queue and log, the inventory and the counters.
fn world() -> World {
    let mut world = World::default();
    {
        let scalars = world.scalars.get_mut();
        scalars.duration_minutes = Fit::Ok(600);
        scalars.minute_index = Fit::Ok(7);
        scalars.schema_version = Fit::Ok(5);
        scalars.seed = Fit::Ok(Seed::from_text("2026").unwrap());
    }
    let mut entities = Entities::default();
    put(&mut entities.agents, "a1", agent("a1", "z0"));
    put(&mut entities.agents, "a2", agent("a2", "z0"));
    put(&mut entities.zones, "z0", ZoneRow { id: text("z0"), name: text("z0"), capacity: Some(3), display_x: None, display_y: None });
    put(
        &mut entities.facilities,
        "toilet-1",
        FacilityRow {
            id: text("toilet-1"),
            kind: text("toilet"),
            zone_id: text("z0"),
            capacity: 1,
            working_units: 1,
            active_agent_ids: vec![],
            queue_agent_ids: vec![],
            resource_active_agent_ids: vec![],
            resource_queue_agent_ids: vec![],
        },
    );
    put(&mut entities.support_requests, "r1", request("r1", "a1", RequestStatus::Resolved));
    put(&mut entities.support_requests, "r2", request("r2", "a1", RequestStatus::Unresolved));
    *world.entities.get_mut() = entities;
    let mut resources: IdMap<i64> = IdMap::default();
    resources.insert(text("water_ml"), 1000);
    add_slot(&mut world, RESOURCES, Place::Field, Box::new(resources));
    add_slot(&mut world, ID_COUNTERS, Place::Field, Box::new(ValueRow(V::Map([(text("event"), V::Int(10))].into_iter().collect()))));
    world.hold_queue(Arc::new(Queue::new()));
    world.hold_log(Arc::new(EventLog::new()));
    world
}

fn plan() -> TickPlan {
    TickPlan { operational_enabled: Some(false), ..TickPlan::default() }
}

fn absent() -> HeldNamespaces {
    let mut held = HeldNamespaces::default();
    for name in READ {
        held.insert(text(name), Held::Absent);
    }
    held
}

fn entity_inputs<'a>(requests: Requests<'a>, absent: &'a Held, routes: &'a sheltersim_core::rules::kit::Kept<Option<sheltersim_core::config::movement::RouteCapacities>>) -> EntityInputs<'a> {
    EntityInputs { requests, resource_config: None, route_capacities: routes, ledger: absent, service: absent, spaces: absent, overlay: absent }
}

fn entity_rules(world: &World) -> Result<Vec<(Arc<str>, i64)>> {
    let (absent, routes) = (Held::Absent, Ok(Arc::new(None)));
    tick_validation::entity_rules(world, &entity_inputs(Requests::All, &absent, &routes), &mut |_| {})
}

/// The Python callables of the check: they say yes, and record what was asked.
#[derive(Default)]
struct Asked {
    namespaces: Vec<String>,
    body: u32,
    shared: u32,
    effects: u32,
    turned_down: Option<&'static str>,
}

impl Checks for Asked {
    fn namespace(&mut self, name: &str) -> Result<Option<bool>> {
        self.namespaces.push(name.to_owned());
        Ok(Some(self.turned_down != Some(after_tick::READ.iter().copied().find(|known| *known == name).unwrap_or(""))))
    }
    fn body_rows(&mut self) -> Result<bool> {
        self.body += 1;
        Ok(true)
    }
    fn shared_operations(&mut self) -> Result<bool> {
        self.shared += 1;
        Ok(true)
    }
    fn effects(&mut self) -> Result<()> {
        self.effects += 1;
        Ok(())
    }
    fn body_config_ids(&mut self) -> Result<Vec<Arc<str>>> {
        Ok(Vec::new())
    }
}

/// The world with a population state, a lifecycle state and an overlay with nothing pending.
fn world_with_states() -> World {
    let mut world = world();
    let empty = |parts: &[&str]| V::Map(parts.iter().map(|part| (text(part), V::List(vec![]))).collect());
    add_slot(&mut world, tick_validation::DYNAMICS, Place::Namespace, Box::new(ValueRow(population(vec!["a1", "a2"], vec![]))));
    add_slot(&mut world, tick_validation::LIFECYCLE, Place::Namespace, Box::new(ValueRow(empty(&["pending_relocations"]))));
    add_slot(&mut world, tick_validation::OVERLAY, Place::Namespace, Box::new(ValueRow(V::Map(IndexMap::new()))));
    world
}

fn check(cell: &Shard<World>, memo: &mut Memo, asked: &mut Asked, configs: Stamp) -> Result<()> {
    let (plan, mut held) = (plan(), absent());
    for name in [tick_validation::DYNAMICS, tick_validation::LIFECYCLE, tick_validation::OVERLAY] {
        if cell.try_borrow().unwrap().slots.get(name).is_some() {
            held.insert(text(name), Held::Held);
        }
    }
    let (routes, schema) = (Ok(Arc::new(None)), Ok(Arc::new(None)));
    let inputs = Inputs { plan: &plan, configs, held: &held, route_capacities: &routes, overlay_schema: &schema };
    after_tick::check(cell, memo, &inputs, asked)
}

#[test]
fn the_table_marks_each_written_or_filled_slot_once_in_order() {
    let mut table: Table<RequestRow> = Table::default();
    let first = put(&mut table, "r1", request("r1", "a1", RequestStatus::Resolved));
    let second = put(&mut table, "r2", request("r2", "a1", RequestStatus::Resolved));
    assert_eq!(table.take_marked(), vec![first, second]);
    assert!(table.marked().is_empty());
    // Reads mark nothing.
    assert!(table.get("r1").is_some() && table.row_at(second).is_some());
    assert!(table.marked().is_empty());
    // A rule of the core, then Python, then the core again: one mark per slot.
    table.get_mut("r2").unwrap().cooldown_until = 5;
    table.row_and_view_mut(first).unwrap().0.cooldown_until = 6;
    table.slot_mut(second).unwrap().cooldown_until = 7;
    assert_eq!(table.marked(), &[second, first]);
    assert!(table.is_marked(first) && table.is_marked(second));
    // A row put under a key marks its slot too.
    table.take_marked();
    let third = table.insert(text("r3"), request("r3", "a2", RequestStatus::Pending));
    assert_eq!(table.take_marked(), vec![third]);
    assert!(!table.is_marked(third));
}

#[test]
fn the_entity_rules_read_the_rows() {
    let world = world();
    assert_eq!(entity_rules(&world), Ok(vec![]));
    // An agent's value is read from its row.
    world.rows_mut().unwrap().agents.get_mut("a1").unwrap().stress = 1.5;
    assert_eq!(entity_rules(&world), Err(refusal("an agent's value is the Python rule's")));
    world.rows_mut().unwrap().agents.get_mut("a1").unwrap().stress = 0.2;
    // A v5 world with a resource config, or a v6 world without one, is the Python rule's.
    let config = ResourceNumbers { water_ml_per_service: 250, food_meal_units_per_service: 1, water_staff_skill: text("water"), meal_staff_skill: text("meal") };
    let (absent, routes) = (Held::Absent, Ok(Arc::new(None)));
    let mut inputs = entity_inputs(Requests::All, &absent, &routes);
    inputs.resource_config = Some(&config);
    assert_eq!(tick_validation::entity_rules(&world, &inputs, &mut |_| {}), Err(refusal("a resource service config for a WorldState v5")));
    // A zone over its capacity, or with spaces, is left with its count for the overlay's rule.
    world.rows_mut().unwrap().zones.get_mut("z0").unwrap().capacity = Some(1);
    assert_eq!(entity_rules(&world), Ok(vec![(text("z0"), 2)]));
}

#[test]
fn the_entity_rules_read_requests_inventory_counters_and_events() {
    let mut world = world();
    // A settled request names someone unknown.
    world.rows_mut().unwrap().support_requests.get_mut("r1").unwrap().agent_id = text("nobody");
    assert_eq!(entity_rules(&world), Err(refusal("a request's agent or timing is the Python rule's")));
    // Only the requests given are read.
    let (absent, routes) = (Held::Absent, Ok(Arc::new(None)));
    let r2 = world.rows().unwrap().support_requests.slot_of("r2").unwrap();
    let slots = [r2];
    let inputs = entity_inputs(Requests::Slots(&slots), &absent, &routes);
    assert_eq!(tick_validation::entity_rules(&world, &inputs, &mut |_| {}), Ok(vec![]));
    world.rows_mut().unwrap().support_requests.get_mut("r1").unwrap().agent_id = text("a1");
    // The inventory and the counters.
    world.slots.row_mut::<IdMap<i64>>(RESOURCES).unwrap().insert(text("water_ml"), -1);
    assert_eq!(entity_rules(&world), Err(refusal("the inventory is the Python rule's")));
    world.slots.row_mut::<IdMap<i64>>(RESOURCES).unwrap().insert(text("water_ml"), 1);
    // A pending event that is only a Python object, then one whose sequence the counter is not past.
    world.queue_mut().unwrap().add(text("e1"), (8, 0, 12), None);
    assert_eq!(entity_rules(&world), Err(refusal(NO_ROW)));
    let mut queue = Queue::new();
    let row = EventRow {
        event_id: text("e1"),
        kind: EventKind::Completion,
        due_minute: 8,
        priority: 0,
        sequence: 12,
        event_type: text("probe_event"),
        payload: V::Map(IndexMap::new()),
        source_id: None,
        causation_id: None,
        provenance: V::Map(IndexMap::new()),
    };
    assert_eq!(queue.schedule_row(row), 0);
    world.hold_queue(Arc::new(queue));
    let mut seen = Vec::new();
    let (absent, routes) = (Held::Absent, Ok(Arc::new(None)));
    let result = tick_validation::entity_rules(&world, &entity_inputs(Requests::All, &absent, &routes), &mut |kind| seen.push(kind.to_owned()));
    assert_eq!(result, Err(refusal("the event counter is not past the sequences")));
    assert_eq!(seen, vec!["probe_event".to_owned()]);
}

#[test]
fn the_check_asks_the_validators_and_the_memo_skips_what_stands() {
    let cell = Shard::new(world_with_states());
    let configs = Stamp::next();
    let mut memo = Memo::default();
    let mut asked = Asked::default();
    check(&cell, &mut memo, &mut asked, configs).unwrap();
    assert_eq!(
        asked.namespaces,
        [
            "agent_dynamics_v1",
            "population_dynamics_v1",
            "population_lifecycle_v1",
            "social_network_v1",
            "staff_dynamics_v1",
            "staff_operations_v1",
            "temporary_spaces_v1",
            "movement_v1",
            "geometry_overlay_v1",
        ]
    );
    assert_eq!((asked.body, asked.shared, asked.effects), (0, 0, 1));
    assert_eq!(memo.minute, 7);
    assert_eq!(memo.configs, Some(configs));
    assert_eq!(memo.requests, vec![text("r1"), text("r2")]);
    assert!(memo.open.is_empty());
    assert_eq!(memo.unchanged, [false; 5]);
    // The marks of the requests' slots were taken.
    assert!(cell.try_borrow().unwrap().rows().unwrap().support_requests.marked().is_empty());

    // Nothing changed: the namespaces whose state, people and configs stand are skipped.
    let mut again = Asked::default();
    check(&cell, &mut memo, &mut again, configs).unwrap();
    assert!(!again.namespaces.iter().any(|name| ["population_dynamics_v1", "population_lifecycle_v1", "geometry_overlay_v1"].contains(&name.as_str())));
    assert_eq!(memo.unchanged, [true, false, true, true, true]);
    // A write to a state the memo compares gives its slot a new stamp: its validator is asked again.
    cell.with(|world| {
        world.slots.row_mut::<ValueRow>(tick_validation::DYNAMICS).unwrap();
    })
    .unwrap();
    let mut written = Asked::default();
    check(&cell, &mut memo, &mut written, configs).unwrap();
    assert!(written.namespaces.iter().any(|name| name == "population_dynamics_v1"));
    assert!(!written.namespaces.iter().any(|name| name == "population_lifecycle_v1"));

    // New configs: the overlay is asked again, and the memo keeps their stamp.
    let newer = Stamp::next();
    let mut configs_changed = Asked::default();
    check(&cell, &mut memo, &mut configs_changed, newer).unwrap();
    assert!(configs_changed.namespaces.iter().any(|name| name == "geometry_overlay_v1"));
    assert_eq!(memo.configs, Some(newer));
}

#[test]
fn a_written_settled_request_is_read_again_and_a_refusal_keeps_the_memo() {
    let cell = Shard::new(world());
    let configs = Stamp::next();
    let mut memo = Memo::default();
    check(&cell, &mut memo, &mut Asked::default(), configs).unwrap();
    // Written by a rule of the core: its slot is marked, the check reads it and turns it down.
    cell.with(|world| world.rows_mut().unwrap().support_requests.get_mut("r2").unwrap().requested_minute = 99).unwrap();
    let before = memo.clone();
    assert_eq!(check(&cell, &mut memo, &mut Asked::default(), configs), Err(refusal("a request's agent or timing is the Python rule's")));
    assert_eq!((memo.requests.clone(), memo.open.clone(), memo.minute), (before.requests, before.open, before.minute));
    // The marks stay for the next check.
    assert!(!cell.try_borrow().unwrap().rows().unwrap().support_requests.marked().is_empty());
    cell.with(|world| world.rows_mut().unwrap().support_requests.get_mut("r2").unwrap().requested_minute = 2).unwrap();
    check(&cell, &mut memo, &mut Asked::default(), configs).unwrap();
}

#[test]
fn a_validator_that_says_no_refuses_with_its_name() {
    let cell = Shard::new(world());
    let mut memo = Memo::default();
    let mut asked = Asked { turned_down: Some("temporary_spaces_v1"), ..Asked::default() };
    let result = check(&cell, &mut memo, &mut asked, Stamp::next());
    assert_eq!(result, Err(refusal("temporary_spaces_v1: its kernel did not take it")));
    assert_eq!(memo.minute, -1);
    assert_eq!(asked.effects, 0);
}

#[test]
fn the_unsettled_requests_are_the_open_and_the_added_ones_unless_one_reopened() {
    let cell = Shard::new(world());
    let configs = Stamp::next();
    let mut memo = Memo::default();
    check(&cell, &mut memo, &mut Asked::default(), configs).unwrap();
    let unsettled = |memo: &Memo| cell.with(|world| memo.unsettled_requests(world).unwrap()).unwrap();
    assert_eq!(unsettled(&memo), Some(vec![]));
    // A new request comes last.
    cell.with(|world| {
        world.rows_mut().unwrap().support_requests.insert(text("r3"), request("r3", "a2", RequestStatus::Pending));
    })
    .unwrap();
    assert_eq!(unsettled(&memo), Some(vec![text("r3")]));
    // A settled one opened again: every request must be read.
    cell.with(|world| world.rows_mut().unwrap().support_requests.get_mut("r1").unwrap().status = RequestStatus::Pending).unwrap();
    assert_eq!(unsettled(&memo), None);
    // A request taken out of the table: the memo's keys no longer start it.
    cell.with(|world| {
        let mut rows = world.rows_mut().unwrap();
        rows.support_requests.get_mut("r1").unwrap().status = RequestStatus::Resolved;
        rows.support_requests.remove_row("r1");
    })
    .unwrap();
    assert_eq!(unsettled(&memo), None);
}

fn population(initial: Vec<&str>, pending: Vec<V>) -> V {
    let list = |items: Vec<V>| V::List(items);
    V::Map(
        [
            ("schema_version", V::Int(1)),
            ("policy_version", V::Str(text("capacity-aware-arrivals-departures-v1"))),
            ("source_id", V::Str(text("ASM-POPULATION-DYNAMICS-V1"))),
            ("initial_agent_ids", list(initial.into_iter().map(|id| V::Str(text(id))).collect())),
            ("profile_traits", V::Map(IndexMap::new())),
            ("pending_arrivals", list(pending)),
            ("arrivals", list(vec![])),
            ("departures", list(vec![])),
        ]
        .into_iter()
        .map(|(key, value)| (text(key), value))
        .collect(),
    )
}

#[test]
fn the_population_validators_read_the_rows() {
    let mut world = world();
    assert_eq!(tick_validation::population_dynamics(&world, &Held::Absent), Ok(Dynamics::Taken));
    add_slot(&mut world, tick_validation::DYNAMICS, Place::Namespace, Box::new(ValueRow(population(vec!["a1", "a2"], vec![]))));
    assert_eq!(tick_validation::population_dynamics(&world, &Held::Held), Ok(Dynamics::Taken));
    assert_eq!(tick_validation::population_dynamics(&world, &Held::Other("another tree")), Err(refusal("another tree")));
    *world.slots.row_mut::<ValueRow>(tick_validation::DYNAMICS).unwrap() = ValueRow(population(vec!["a1"], vec![]));
    assert_eq!(tick_validation::population_dynamics(&world, &Held::Held), Err(refusal("an active agent has no profile")));
    *world.slots.row_mut::<ValueRow>(tick_validation::DYNAMICS).unwrap() = ValueRow(population(vec!["a1"], vec![V::Map(IndexMap::new())]));
    assert_eq!(tick_validation::population_dynamics(&world, &Held::Held), Ok(Dynamics::Validate));

    // The presence ledger: each admitted person has a record, and an archive path is read in the
    // lifecycle state.
    let person = |id: &str, path: Option<Vec<&str>>| {
        let mut record: IndexMap<Arc<str>, V> =
            [("agent_id", V::Str(text(id))), ("status", V::Str(text("admitted"))), ("transition_zone_id", V::Str(text("z0"))), ("since_minute", V::Int(0))]
            .into_iter()
            .map(|(key, value)| (text(key), value))
            .collect();
        if let Some(path) = path {
            record.insert(text("archive_path"), V::List(path.into_iter().map(|part| V::Str(text(part))).collect()));
        }
        V::Map(record)
    };
    let presence = |people: Vec<(&str, V)>| {
        let people: IndexMap<Arc<str>, V> = people.into_iter().map(|(key, value)| (text(key), value)).collect();
        V::Map([(text("schema_version"), V::Int(1)), (text("people"), V::Map(people))].into_iter().collect())
    };
    add_slot(&mut world, tick_validation::PRESENCE, Place::Namespace, Box::new(ValueRow(presence(vec![("a1", person("a1", None)), ("a2", person("a2", None))]))));
    assert_eq!(tick_validation::validate_presence(&world, &Held::Held, &Held::Absent), Ok(()));
    *world.slots.row_mut::<ValueRow>(tick_validation::PRESENCE).unwrap() = ValueRow(presence(vec![("a1", person("a1", None))]));
    assert_eq!(tick_validation::validate_presence(&world, &Held::Held, &Held::Absent), Err(refusal("the presence omits an admitted person")));
    let archived = presence(vec![("a1", person("a1", Some(vec!["population_lifecycle_v1", "people", "a1"]))), ("a2", person("a2", None))]);
    *world.slots.row_mut::<ValueRow>(tick_validation::PRESENCE).unwrap() = ValueRow(archived);
    assert_eq!(tick_validation::validate_presence(&world, &Held::Held, &Held::Absent), Err(refusal("a presence archive path is missing")));
    let lifecycle = V::Map([(text("people"), V::Map([(text("a1"), V::Map(IndexMap::new()))].into_iter().collect()))].into_iter().collect());
    add_slot(&mut world, tick_validation::LIFECYCLE, Place::Namespace, Box::new(ValueRow(lifecycle)));
    assert_eq!(tick_validation::validate_presence(&world, &Held::Held, &Held::Held), Ok(()));
}
